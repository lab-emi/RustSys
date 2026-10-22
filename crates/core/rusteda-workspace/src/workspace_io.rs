// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

use super::*;

/// The revision directories in `revisions/` (canonical `rev<NNNN>` names only; `.staging-*`,
/// `.orphan-*` and anything else is ignored, WS-REQ-0070), sorted.
pub(super) fn rev_dirs(revisions: &Path) -> Result<Vec<(Rev, String)>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(revisions).map_err(|e| Error::io(revisions, e))? {
        let entry = entry.map_err(|e| Error::io(revisions, e))?;
        if let Ok(name) = entry.file_name().into_string() {
            if let Some(rev) = Rev::from_dir_name(&name) {
                dirs.push((rev, name));
            }
        }
    }
    dirs.sort();
    Ok(dirs)
}

/// Renames revision directories to `.orphan-<name>-<pid>-<nanos>`.
pub(super) fn set_aside(revisions: &Path, dirs: &[(Rev, String)]) -> Result<()> {
    for (_, name) in dirs {
        let to = revisions.join(format!(
            "{}{name}-{}",
            layout::ORPHAN_PREFIX,
            fsutil::unique_suffix()
        ));
        fsutil::rename(&revisions.join(name), &to)?;
    }
    Ok(())
}

/// Where a fork of `dst` goes: the directory that holds it, and `dst` as that directory joined
/// with the last component of `dst`, the name that the fork's directory is renamed to
/// (WS-REQ-0053). A `dst` that does not end in a name (`.`, `..`, a root) is `INPUT_REJECTED`:
/// the fork would replace a directory that its path does not name.
pub(super) fn fork_location(dst: &Path) -> Result<(PathBuf, PathBuf)> {
    let mut components = dst.components();
    let Some(Component::Normal(name)) = components.next_back() else {
        return Err(fork_refused(
            dst,
            "the path does not end in the name of a new directory (e.g. ../exp1)",
        ));
    };
    let parent = components.as_path();
    if parent.as_os_str().is_empty() {
        return Ok((PathBuf::from("."), PathBuf::from(name)));
    }
    Ok((parent.to_path_buf(), parent.join(name)))
}

/// The early checks of a fork's destination, before the copy; the rename that publishes the
/// fork checks again (WS-REQ-0053). A workspace there is `WORKSPACE_EXISTS`; a file, a symbolic
/// link, a non-empty directory or a mount point is `INPUT_REJECTED`. Returns the permissions
/// of an empty directory there, which the fork's directory replaces.
pub(super) fn check_fork_destination(target: &Path) -> Result<Option<fs::Permissions>> {
    if fs::symlink_metadata(target.join(layout::MANIFEST)).is_ok() {
        return Err(Error::WorkspaceExists {
            path: target.to_path_buf(),
        });
    }
    let meta = match fs::symlink_metadata(target) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(target, e)),
    };
    if meta.file_type().is_symlink() {
        return Err(fork_refused(target, "it is a symbolic link"));
    }
    if !meta.is_dir() {
        return Err(fork_refused(target, "it is not a directory"));
    }
    let mut entries = fs::read_dir(target).map_err(|e| Error::io(target, e))?;
    if entries.next().is_some() {
        return Err(fork_refused(target, "the directory is not empty"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let parent = fsutil::parent_dir(target);
        let parent_meta = fs::metadata(parent).map_err(|e| Error::io(parent, e))?;
        if parent_meta.dev() != meta.dev() {
            return Err(fork_refused(target, MOUNT_POINT));
        }
    }
    Ok(Some(meta.permissions()))
}

/// Why a fork cannot replace a mount point.
const MOUNT_POINT: &str = "it is a mount point, which the fork's directory cannot replace; fork \
                           into a new directory below it";

/// `INPUT_REJECTED`: the workspace cannot be forked into `dst`.
pub(super) fn fork_refused(dst: &Path, reason: &str) -> Error {
    Error::input_rejected(format!(
        "cannot fork the workspace into {}: {reason}",
        dst.display()
    ))
}

/// The prefix of the directory that a fork into `target` is built in, `.<name>.fork-`: a dot
/// name next to `target`, which nothing reads. A long name is shortened, so that the directory
/// name stays within the limits of file systems.
pub(super) fn fork_prefix(target: &Path) -> String {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut end = name.len().min(64);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!(".{}.fork-", &name[..end])
}

/// Publishes a fork built in `aside` by renaming it onto `target` (WS-REQ-0053). The rename
/// creates `target`, or replaces it if it is an empty directory, in one step; it fails if
/// `target` is not empty, so a fork or an `init` that claimed `target` first keeps its files.
pub(super) fn publish_fork(aside: &Path, target: &Path) -> Result<()> {
    let renamed = fs::rename(aside, target);
    // Windows renames onto a directory only on some file systems: there an empty directory is
    // removed first (`remove_dir` removes only an empty one, so another fork's files stay).
    #[cfg(not(unix))]
    let renamed = renamed.or_else(|e| {
        let empty = fs::read_dir(target).is_ok_and(|mut entries| entries.next().is_none());
        if empty && fs::remove_dir(target).is_ok() {
            fs::rename(aside, target)
        } else {
            Err(e)
        }
    });
    renamed.map_err(|e| {
        if fs::symlink_metadata(target.join(layout::MANIFEST)).is_ok() {
            return Error::WorkspaceExists {
                path: target.to_path_buf(),
            };
        }
        match e.kind() {
            io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists => {
                fork_refused(target, "the directory is not empty")
            }
            io::ErrorKind::NotADirectory => fork_refused(target, "it is not a directory"),
            io::ErrorKind::CrossesDevices | io::ErrorKind::ResourceBusy => {
                fork_refused(target, MOUNT_POINT)
            }
            _ => Error::io(target, e),
        }
    })
}

pub(super) fn read_manifest(root: &Path) -> Result<Manifest> {
    let path = root.join(layout::MANIFEST);
    let Some(bytes) = fsutil::read_regular_if_exists(&path)? else {
        return Err(Error::NotAWorkspace {
            path: root.to_path_buf(),
        });
    };
    let manifest: Manifest = schema::parse_json(&path, &bytes, MANIFEST_SCHEMA)?;
    if manifest.head.0 >= manifest.next_rev {
        let reason = format!(
            "head {} is not below next_rev {}",
            manifest.head, manifest.next_rev
        );
        return Err(Error::corrupt(path, Corruption::Json { reason }));
    }
    Ok(manifest)
}

/// Replaces `manifest.json`: temporary file, fsync, rename, directory fsync (WS-REQ-0066). An
/// error means HEAD did not change; after the rename the outcome is `Ok` (WS-REQ-0076).
pub(super) fn write_manifest(root: &Path, manifest: &Manifest) -> Result<Replaced> {
    let text = schema::to_pretty_json(manifest)?;
    fsutil::replace(&root.join(layout::MANIFEST), text.as_bytes())
}

/// A history line. `outcome` is the exit code of a command that completed (0, 3 or 5) or the
/// error of one that failed.
pub(super) fn history_line(
    inv: &Invocation,
    base_rev: Option<Rev>,
    new_rev: Option<Rev>,
    head_after: Rev,
    outcome: std::result::Result<i32, &Error>,
    metrics: JsonMap,
) -> HistoryEntry {
    HistoryEntry {
        schema: HISTORY_SCHEMA.to_string(),
        time_unix_ms: fsutil::now_unix_ms(),
        tool: inv.tool.clone(),
        tool_version: inv.tool_version.clone(),
        command: inv.command.clone(),
        argv: inv.argv.clone(),
        base_rev,
        new_rev,
        head_after,
        ok: outcome.is_ok(),
        exit: outcome.unwrap_or_else(Error::exit_code),
        error: outcome.err().map(|e| HistoryError {
            code: e.code(),
            message: e.to_string(),
        }),
        metrics,
        metric_deltas: TableValues::new(),
    }
}

/// Appends one line to `history.jsonl` and fsyncs it, and its directory when the file is new
/// (WS-REQ-0042). If a crash tore the last line, a newline first ends it, so the torn text
/// never merges with this line.
pub(super) fn append_history(root: &Path, line: &HistoryEntry) -> Result<()> {
    let path = root.join(layout::HISTORY);
    let mut text = serde_json::to_string(line)
        .map_err(|e| Error::internal(format!("serializing a history line: {e}")))?;
    text.push('\n');
    let io_err = |e| Error::io(&path, e);
    let created = fs::symlink_metadata(&path).is_err();
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(&path)
        .map_err(io_err)?;
    let len = file.metadata().map_err(io_err)?.len();
    if len > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(len - 1)).map_err(io_err)?;
        file.read_exact(&mut last).map_err(io_err)?;
        if last[0] != b'\n' {
            text.insert(0, '\n');
        }
    }
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_data())
        .map_err(io_err)?;
    if created {
        fsutil::fsync_dir(fsutil::parent_dir(&path))?;
    }
    Ok(())
}

/// The warning of a change that happened although writing its history line failed
/// (WS-REQ-0076).
pub(super) fn history_not_written(what: &str, error: &Error) -> Diagnostic {
    Diagnostic::new(
        Severity::Warning,
        codes::HISTORY_NOT_WRITTEN,
        format!("{what}, but writing its history line failed: {error}"),
    )
    .with_help("the change itself is complete; make history.jsonl writable for later commands")
}

/// The warning of a change that happened although the directory fsync after the rename of its
/// commit point failed (WS-REQ-0076): the rename of `manifest.json`, or for a fork the rename
/// of its directory, `subject`.
pub(super) fn not_durable(what: &str, subject: &str, error: &Error) -> Diagnostic {
    Diagnostic::new(
        Severity::Warning,
        codes::NOT_DURABLE,
        format!("{what}, but making {subject} durable failed: {error}"),
    )
    .with_help("a crash before the file system writes it back may undo the change")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    fn table(path: &str, hash: u64, upstream: &[(&str, u64)]) -> TableEntry {
        TableEntry {
            path: path.to_string(),
            hash: Hash64(hash),
            upstream: upstream
                .iter()
                .map(|(n, h)| (n.to_string(), Hash64(*h)))
                .collect(),
        }
    }

    fn library(corner: &str, hash: u64) -> LibraryEntry {
        LibraryEntry {
            name: format!("cells_{corner}"),
            corner: corner.to_string(),
            hash: Hash64(hash),
            source: String::new(),
            compiled: String::new(),
        }
    }

    // WS-REQ-0064: a chain a <- b <- c is dropped round by round when a changes.
    #[test]
    fn carry_forward_fixpoint() {
        let libs = [library("default", 9)];
        let lh = LibraryEntry::list_hash(&libs).0;
        let base = BTreeMap::from([
            ("a".to_string(), table("a", 1, &[])),
            ("b".to_string(), table("b", 2, &[("a", 1)])),
            (
                "c".to_string(),
                table("c", 3, &[("b", 2), ("libraries", lh)]),
            ),
            ("d".to_string(), table("d", 4, &[("libraries", lh)])),
        ]);
        let none = BTreeMap::new();
        let no_drops = BTreeMap::new();
        let (carried, dropped) = carry_forward(&base, &none, &no_drops, &libs);
        assert_eq!(carried.len(), 4);
        assert!(dropped.is_empty());

        let added = BTreeMap::from([("a".to_string(), table("a", 5, &[]))]);
        let (carried, dropped) = carry_forward(&base, &added, &no_drops, &libs);
        assert_eq!(carried.keys().collect::<Vec<_>>(), ["d"]);
        assert_eq!(
            dropped,
            [
                DroppedEntry {
                    table: "b".into(),
                    reason: "upstream a changed".into()
                },
                DroppedEntry {
                    table: "c".into(),
                    reason: "upstream b is not in this revision".into()
                },
            ]
        );

        // The same content keeps everything: invalidation is by hash, not by command.
        let same = BTreeMap::from([("a".to_string(), table("a", 1, &[]))]);
        assert_eq!(carry_forward(&base, &same, &no_drops, &libs).0.len(), 3);

        let (carried, dropped) = carry_forward(&base, &none, &no_drops, &[library("default", 8)]);
        assert_eq!(carried.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(dropped.len(), 2);

        let explicit = BTreeMap::from([("a".to_string(), "replaced".to_string())]);
        let (carried, dropped) = carry_forward(&base, &none, &explicit, &libs);
        assert_eq!(carried.keys().collect::<Vec<_>>(), ["d"]);
        assert_eq!(
            dropped[0],
            DroppedEntry {
                table: "a".into(),
                reason: "replaced".into()
            }
        );
        assert_eq!(dropped.len(), 3);
    }

    // WS-REQ-0014 (review CW-4): a table computed from one corner survives a library added for
    // another corner, and is dropped when its own corner changes or disappears.
    #[test]
    fn carry_forward_with_corner_pseudo_tables() {
        let tt = [library("tt", 1)];
        let corner = LibraryEntry::corner_hash(&tt, "tt").unwrap().0;
        let all = LibraryEntry::list_hash(&tt).0;
        let base = BTreeMap::from([
            ("n".to_string(), table("n", 1, &[("libraries.tt", corner)])),
            ("s".to_string(), table("s", 2, &[("libraries", all)])),
        ]);
        let (none, no_drops) = (BTreeMap::new(), BTreeMap::new());
        let with_ss = [library("tt", 1), library("ss", 2)];
        let (carried, dropped) = carry_forward(&base, &none, &no_drops, &with_ss);
        assert_eq!(carried.keys().collect::<Vec<_>>(), ["n"]);
        assert_eq!(dropped[0].reason, "upstream libraries changed");
        let (carried, dropped) = carry_forward(&base, &none, &no_drops, &[library("tt", 3)]);
        assert!(carried.is_empty());
        assert_eq!(dropped[0].reason, "upstream libraries.tt changed");
        let (_, dropped) = carry_forward(&base, &none, &no_drops, &[library("ss", 2)]);
        assert_eq!(
            dropped[0].reason,
            "upstream libraries.tt is not in this revision"
        );
    }

    #[test]
    fn names_and_paths() {
        for ok in ["netlist", "sta", "rtl", "a-b_1"] {
            check_table_name(ok).unwrap();
        }
        for bad in ["", "libraries", "a.b", "a/b", "x y", "libraries.ss"] {
            assert!(check_table_name(bad).is_err(), "{bad}");
        }
        for ok in ["netlist.rbin", "rtl", "synth.json", "a_b-c.d"] {
            check_table_path(ok).unwrap();
        }
        for bad in [
            "",
            ".hidden",
            "meta.json",
            "a/b",
            "..",
            ".",
            "x y",
            "a\\b",
            "/etc/passwd",
            "../../manifest.json",
        ] {
            assert!(check_table_path(bad).is_err(), "{bad}");
        }
    }

    fn inv(command: &str) -> Invocation {
        let words: Vec<&str> = command.split(' ').collect();
        Invocation::new(words[0], "0.1.0", command, words)
    }

    fn codes_of(warnings: &[Diagnostic]) -> Vec<crate::DiagCode> {
        warnings.iter().map(|w| w.code).collect()
    }

    // WS-REQ-0076 (review W1): a directory fsync that fails after the manifest rename fails
    // neither init nor commit nor revert; the change is reported with a warning, and its
    // history line is the line of a successful command.
    #[test]
    fn failed_sync_after_the_manifest_rename() {
        use fsutil::failpoint::fail_next_sync_after_rename;
        let root = temp_dir("workspace-sync-after-rename").join("ws");
        fail_next_sync_after_rename();
        let init = Workspace::init(&root, &inv("rusteda init")).unwrap();
        assert_eq!(codes_of(&init.warnings), [codes::NOT_DURABLE]);
        assert!(
            init.warnings[0]
                .message
                .starts_with("the workspace is created, but")
        );
        let ws = init.workspace;
        assert_eq!(ws.head().unwrap(), Rev::ZERO);

        let mut txn = ws
            .begin(ws.resolve_base("rev0").unwrap(), &inv("tool put"))
            .unwrap();
        txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
        fail_next_sync_after_rename();
        let committed = txn.commit(JsonMap::new()).unwrap();
        assert_eq!(committed.rev, Rev(1));
        assert_eq!(codes_of(&committed.warnings), [codes::NOT_DURABLE]);
        let warning = &committed.warnings[0];
        assert_eq!(warning.severity, Severity::Warning);
        assert!(
            warning.message.contains("rev1 is committed and is HEAD"),
            "{warning:?}"
        );
        assert!(warning.message.contains("injected"), "{warning:?}");
        assert_eq!(ws.head().unwrap(), Rev(1));

        fail_next_sync_after_rename();
        let reverted = ws
            .revert(
                ws.resolve_base("rev1").unwrap(),
                Rev(0),
                &inv("rusteda revert"),
            )
            .unwrap();
        assert_eq!(codes_of(&reverted.warnings), [codes::NOT_DURABLE]);
        assert_eq!(ws.head().unwrap(), Rev(0));

        let lines = ws.history().unwrap();
        assert_eq!(lines.len(), 3, "one line per command, no failure lines");
        assert!(
            lines
                .iter()
                .all(|l| l.ok && l.exit == 0 && l.error.is_none())
        );
        assert_eq!(
            (lines[1].new_rev, lines[1].head_after),
            (Some(Rev(1)), Rev(1))
        );
        assert_eq!((lines[2].new_rev, lines[2].head_after), (None, Rev(0)));
    }
}
