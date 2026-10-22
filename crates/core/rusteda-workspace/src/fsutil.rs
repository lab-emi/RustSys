// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! File-system helpers: durable writes, atomic replacement, directory fsync, hard links
//! (WS-REQ-0066), and opening regular files only (WS-REQ-0025).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Corruption, Error, Result};

/// Milliseconds since the Unix epoch.
pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// `<pid>-<nanos>`: the current time in nanoseconds, made unique within the process.
pub(crate) fn unique_suffix() -> String {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let mut last = LAST.load(Ordering::Relaxed);
    let nanos = loop {
        let next = now.max(last + 1);
        match LAST.compare_exchange_weak(last, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break next,
            Err(seen) => last = seen,
        }
    };
    format!("{}-{nanos}", std::process::id())
}

/// Creates `parent/<prefix><pid>-<nanos>`, a directory that did not exist before.
pub(crate) fn create_unique_dir(parent: &Path, prefix: &str) -> Result<PathBuf> {
    loop {
        let dir = parent.join(format!("{prefix}{}", unique_suffix()));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::io(dir, e)),
        }
    }
}

pub(crate) fn create_dir_all(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))
}

/// Creates `dir` and its missing ancestors, like [`create_dir_all`], and makes the new entries
/// durable: the parent of every directory this call found missing is fsynced, from the top
/// down, so a crash after it returns loses none of them (WS-REQ-0066). An existing `dir` is
/// left as it is.
pub(crate) fn create_dir_all_durable(dir: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut at = dir;
    loop {
        match fs::symlink_metadata(at) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => missing.push(at),
            Err(e) => return Err(Error::io(at, e)),
        }
        match at.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => at = parent,
            _ => break,
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    create_dir_all(dir)?;
    for created in missing.iter().rev() {
        fsync_dir(parent_dir(created))?;
    }
    Ok(())
}

pub(crate) fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).map_err(|e| Error::io(from, e))
}

/// Creates a file that must not exist, writes `bytes` and fsyncs it (WS-REQ-0036). A partial
/// file is removed on failure.
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| Error::io(path, e))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(e) = written {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(Error::io(path, e));
    }
    Ok(())
}

/// What [`replace`] did after its rename succeeded.
#[derive(Debug)]
#[must_use]
pub(crate) enum Replaced {
    /// The file is replaced and its directory entry is durable.
    Durable,
    /// The file is replaced, and readers see the new content, but the directory fsync failed:
    /// a crash may still bring back the old file.
    NotDurable(Error),
}

/// Replaces `path` atomically: a temporary file in the same directory, fsync, rename over
/// `path`, fsync of the directory. An error means that nothing changed. Once the rename has
/// succeeded the replacement has happened, so a failed directory fsync is reported as
/// [`Replaced::NotDurable`] and not as an error (WS-REQ-0076).
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> Result<Replaced> {
    let dir = parent_dir(path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", unique_suffix()));
    write_new(&tmp, bytes)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(path, e));
    }
    Ok(match sync_after_rename(dir) {
        Ok(()) => Replaced::Durable,
        Err(e) => Replaced::NotDurable(e),
    })
}

/// [`replace`] for files that may simply be written again: a failed directory fsync is an
/// error too.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    match replace(path, bytes)? {
        Replaced::Durable => Ok(()),
        Replaced::NotDurable(e) => Err(e),
    }
}

/// The directory fsync that follows the rename of [`replace`].
fn sync_after_rename(dir: &Path) -> Result<()> {
    #[cfg(test)]
    if failpoint::SYNC_AFTER_RENAME.replace(false) {
        return Err(Error::io(
            dir,
            io::Error::other("injected directory fsync failure"),
        ));
    }
    fsync_dir(dir)
}

/// Fault injection and observation for the unit tests.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod failpoint {
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};

    thread_local! {
        /// When set, the next directory fsync after a [`super::replace`] rename on this thread
        /// fails.
        pub(crate) static SYNC_AFTER_RENAME: Cell<bool> = const { Cell::new(false) };
        /// When set, the directories fsynced on this thread, in order.
        static FSYNCED_DIRS: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
        /// When set, the next fork on this thread fails after its copy, before it publishes.
        pub(crate) static FORK_AFTER_COPY: Cell<bool> = const { Cell::new(false) };
    }

    /// Makes the next directory fsync after a rename of `replace` on this thread fail.
    pub(crate) fn fail_next_sync_after_rename() {
        SYNC_AFTER_RENAME.set(true);
    }

    /// Makes the next fork on this thread fail after it copied the workspace.
    pub(crate) fn fail_next_fork_after_copy() {
        FORK_AFTER_COPY.set(true);
    }

    /// Starts recording the directories fsynced on this thread.
    pub(crate) fn record_fsynced_dirs() {
        FSYNCED_DIRS.set(Some(Vec::new()));
    }

    /// The directories fsynced on this thread since [`record_fsynced_dirs`]; stops recording.
    pub(crate) fn take_fsynced_dirs() -> Vec<PathBuf> {
        FSYNCED_DIRS.take().unwrap_or_default()
    }

    pub(super) fn note_fsynced_dir(dir: &Path) {
        FSYNCED_DIRS.with_borrow_mut(|dirs| {
            if let Some(dirs) = dirs {
                dirs.push(dir.to_path_buf());
            }
        });
    }
}

/// Opens a file for reading only if it is a regular file (WS-REQ-0025): the type is checked
/// before opening (a FIFO would block the open; symbolic links are not followed) and again on
/// the open handle.
pub(crate) fn open_regular(path: &Path) -> Result<File> {
    check_regular(path)?;
    let file = File::open(path).map_err(|e| Error::io(path, e))?;
    let meta = file.metadata().map_err(|e| Error::io(path, e))?;
    if !meta.is_file() {
        return Err(not_a_regular_file(path));
    }
    Ok(file)
}

/// Checks that `path` itself, not following a symbolic link, is a regular file
/// (WS-REQ-0025); a missing file is an `IO_ERROR` of kind `NotFound`.
pub(crate) fn check_regular(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    if !meta.file_type().is_file() {
        return Err(not_a_regular_file(path));
    }
    Ok(())
}

fn not_a_regular_file(path: &Path) -> Error {
    Error::corrupt(
        path,
        Corruption::FileType {
            expected: "a regular file",
        },
    )
}

/// Reads a whole regular file ([`open_regular`]).
pub(crate) fn read_regular(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut open_regular(path)?, &mut bytes).map_err(|e| Error::io(path, e))?;
    Ok(bytes)
}

/// [`read_regular`], or `None` when the file does not exist.
pub(crate) fn read_regular_if_exists(path: &Path) -> Result<Option<Vec<u8>>> {
    match read_regular(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The directory that holds `path`; `.` for a bare file name.
pub(crate) fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Makes the entries of a directory durable. Not needed (and not possible through `File`) on
/// Windows.
pub(crate) fn fsync_dir(dir: &Path) -> Result<()> {
    #[cfg(test)]
    failpoint::note_fsynced_dir(dir);
    #[cfg(unix)]
    {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| Error::io(dir, e))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Fsyncs a file, or every file and directory of a tree.
pub(crate) fn fsync_tree(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    if meta.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| Error::io(path, e))? {
            let entry = entry.map_err(|e| Error::io(path, e))?;
            fsync_tree(&entry.path())?;
        }
        fsync_dir(path)
    } else {
        sync_file(path)
    }
}

/// Fsyncs every directory of a tree (not the files), deepest first.
pub(crate) fn fsync_dirs(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        if entry
            .file_type()
            .map_err(|e| Error::io(entry.path(), e))?
            .is_dir()
        {
            fsync_dirs(&entry.path())?;
        }
    }
    fsync_dir(dir)
}

fn sync_file(path: &Path) -> Result<()> {
    // Write access where possible: Windows flushes only handles opened for writing. A file the
    // tool made read-only is synced through a read-only handle (enough on Unix).
    OpenOptions::new()
        .write(true)
        .open(path)
        .or_else(|_| File::open(path))
        .and_then(|f| f.sync_all())
        .map_err(|e| Error::io(path, e))
}

/// Hard-links `src` to `dst`, a file or a directory tree whose files are linked one by one;
/// a file that cannot be linked (another file system, no hard links) is copied and fsynced
/// (WS-REQ-0065). `dst` must not exist.
pub(crate) fn link_or_copy(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src).map_err(|e| Error::io(src, e))?;
    if meta.is_dir() {
        fs::create_dir(dst).map_err(|e| Error::io(dst, e))?;
        for entry in fs::read_dir(src).map_err(|e| Error::io(src, e))? {
            let entry = entry.map_err(|e| Error::io(src, e))?;
            link_or_copy(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if !meta.is_file() {
        return Err(Error::io(
            src,
            io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular file or directory",
            ),
        ));
    }
    match fs::hard_link(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(Error::io(dst, e)),
        Err(_) => {
            if dst.exists() {
                return Err(Error::io(
                    dst,
                    io::Error::from(io::ErrorKind::AlreadyExists),
                ));
            }
            fs::copy(src, dst).map_err(|e| Error::io(dst, e))?;
            sync_file(dst)
        }
    }
}

/// Removes a directory tree of the crate's own (a staging directory); a missing tree is fine.
/// A subdirectory without write permission (a copy of a read-only source tree) would stop
/// `remove_dir_all`, so on Unix such directories are made writable by their owner and the
/// removal is tried again. Symbolic links are never followed: only entries that are
/// directories themselves get new permissions.
pub(crate) fn remove_tree(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {}
    }
    #[cfg(unix)]
    make_dirs_writable(dir);
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Adds owner read, write and search permission to `dir` and every directory below it.
#[cfg(unix)]
fn make_dirs_writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o700));
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                make_dirs_writable(&entry.path());
            }
        }
    }
}

/// Removes a directory tree of the crate's own ([`remove_tree`]), ignoring errors.
pub(crate) fn remove_dir_best_effort(dir: &Path) {
    let _ = remove_tree(dir);
}

/// The (device, inode) pairs of the regular files below `dir`, symbolic links not followed:
/// the files a staging file may share its inode with (WS-REQ-0077).
#[cfg(unix)]
pub(crate) fn file_ids_below(dir: &Path) -> Result<std::collections::BTreeSet<(u64, u64)>> {
    use std::os::unix::fs::MetadataExt;
    fn walk(dir: &Path, ids: &mut std::collections::BTreeSet<(u64, u64)>) -> Result<()> {
        for entry in fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
            let entry = entry.map_err(|e| Error::io(dir, e))?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
            if meta.is_dir() {
                walk(&path, ids)?;
            } else if meta.is_file() {
                ids.insert((meta.dev(), meta.ino()));
            }
        }
        Ok(())
    }
    let mut ids = std::collections::BTreeSet::new();
    walk(dir, &mut ids)?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    #[test]
    fn unique_suffixes() {
        let a = unique_suffix();
        let b = unique_suffix();
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("{}-", std::process::id())));
    }

    #[test]
    fn atomic_and_new_writes() {
        let dir = temp_dir("fsutil-write");
        let f = dir.join("m.json");
        write_atomic(&f, b"one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "no temporary file is left: {names:?}");
        let err = write_new(&f, b"three").unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::IoError);
        assert_eq!(fs::read(&f).unwrap(), b"two");
    }

    // WS-REQ-0076 (review W1): once the rename succeeded, a failed directory fsync does not
    // make `replace` fail: the file is replaced.
    #[test]
    fn replace_after_the_rename() {
        let dir = temp_dir("fsutil-replace");
        let f = dir.join("m.json");
        assert!(matches!(replace(&f, b"one").unwrap(), Replaced::Durable));
        failpoint::fail_next_sync_after_rename();
        let Replaced::NotDurable(e) = replace(&f, b"two").unwrap() else {
            panic!("the injected fsync failure is reported");
        };
        assert!(e.to_string().contains("injected"), "{e}");
        assert_eq!(fs::read(&f).unwrap(), b"two");
        // The strict form reports it as an error, also after the rename.
        failpoint::fail_next_sync_after_rename();
        let err = write_atomic(&f, b"three").unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::IoError);
        assert_eq!(fs::read(&f).unwrap(), b"three");
        assert!(matches!(replace(&f, b"four").unwrap(), Replaced::Durable));
    }

    // WS-REQ-0025 (review W2): only regular files are opened.
    #[cfg(unix)]
    #[test]
    fn open_regular_files_only() {
        let dir = temp_dir("fsutil-open-regular");
        fs::write(dir.join("f"), b"x").unwrap();
        open_regular(&dir.join("f")).unwrap();
        std::os::unix::fs::symlink("f", dir.join("link")).unwrap();
        for path in [dir.join("link"), dir.clone()] {
            let err = open_regular(&path).unwrap_err();
            assert_eq!(
                err.code(),
                crate::ErrorCode::CorruptTable,
                "{}",
                path.display()
            );
            assert!(err.to_string().ends_with("not a regular file"), "{err}");
        }
        let err = open_regular(&dir.join("absent")).unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::IoError);
    }

    // WS-REQ-0079 (review R2-C3): a staging tree with a read-only subdirectory is removed.
    #[cfg(unix)]
    #[test]
    fn remove_read_only_trees() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("fsutil-remove-read-only");
        let tree = dir.join("staging");
        fs::create_dir_all(tree.join("rtl/vendor/deep")).unwrap();
        fs::write(tree.join("rtl/vendor/deep/ip.v"), b"module ip; endmodule\n").unwrap();
        fs::write(tree.join("rtl/vendor/ip.v"), b"module ip; endmodule\n").unwrap();
        // A symbolic link to a directory outside the tree keeps the outside's permissions.
        let outside = dir.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o555)).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("rtl/link")).unwrap();
        for sub in ["rtl/vendor/deep", "rtl/vendor"] {
            fs::set_permissions(tree.join(sub), fs::Permissions::from_mode(0o555)).unwrap();
        }
        remove_tree(&tree).unwrap();
        assert!(!tree.exists());
        let mode = fs::metadata(&outside).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o555, "the link target is untouched");
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        remove_tree(&tree).unwrap(); // gone already: fine
    }

    #[test]
    fn link_trees() {
        let dir = temp_dir("fsutil-link");
        let src = dir.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a"), b"a").unwrap();
        fs::write(src.join("sub/b"), b"b").unwrap();
        let dst = dir.join("dst");
        link_or_copy(&src, &dst).unwrap();
        assert_eq!(fs::read(dst.join("sub/b")).unwrap(), b"b");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let ino = |p: &Path| fs::metadata(p).unwrap().ino();
            assert_eq!(ino(&src.join("a")), ino(&dst.join("a")));
        }
        assert!(link_or_copy(&src.join("a"), &dst.join("a")).is_err());
        fsync_tree(&dst).unwrap();
        fsync_dirs(&dst).unwrap();
    }
}
