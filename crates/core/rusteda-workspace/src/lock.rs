// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Locks on `workspace.lock` (WS-REQ-0050).
//!
//! Readers hold a shared lock while they read `manifest.json`; init, commit, revert and the
//! logging of rejected commands hold an exclusive lock. Every lock opens `workspace.lock` anew,
//! so each lock has its own open file description: two locks taken in one process, for example
//! by two threads, exclude each other exactly like locks of two processes. The locks are
//! advisory (`flock` through [`std::fs::File::lock`]); dropping a lock releases it, and so does
//! the end of the process.
//!
//! The lock is internal to the crate (WS-REQ-0051): every [`crate::Workspace`] method takes the
//! lock it needs and releases it before it returns, so no caller can hold a lock across calls
//! and deadlock against the next call of its own thread.
//!
//! A wait for a lock is not cancellable. A signal that interrupts it (`EINTR`, a handler
//! installed without `SA_RESTART`) does not end it: the standard library reports the
//! interruption instead of retrying, and the wait starts again (WS-REQ-0050).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use crate::error::{Error, Result};
use crate::workspace::layout;

/// A shared or exclusive lock on `<workspace>/workspace.lock`, released on drop.
#[derive(Debug)]
pub(crate) struct WorkspaceLock {
    file: File,
}

impl WorkspaceLock {
    /// Waits for a shared lock.
    pub(crate) fn shared(root: &Path) -> Result<WorkspaceLock> {
        let file = open(root, false)?;
        retry_interrupted(|| file.lock_shared())
            .map_err(|e| Error::io(root.join(layout::LOCK), e))?;
        Ok(WorkspaceLock { file })
    }

    /// Waits for an exclusive lock.
    pub(crate) fn exclusive(root: &Path) -> Result<WorkspaceLock> {
        let file = open(root, true)?;
        retry_interrupted(|| file.lock()).map_err(|e| Error::io(root.join(layout::LOCK), e))?;
        Ok(WorkspaceLock { file })
    }

    /// A shared lock if no exclusive lock is held, else `None`.
    #[cfg(test)]
    pub(crate) fn try_shared(root: &Path) -> Result<Option<WorkspaceLock>> {
        let file = open(root, false)?;
        match retry_would_block(|| file.try_lock_shared()) {
            Ok(true) => Ok(Some(WorkspaceLock { file })),
            Ok(false) => Ok(None),
            Err(e) => Err(Error::io(root.join(layout::LOCK), e)),
        }
    }

    /// An exclusive lock if no other lock is held, else `None`.
    #[cfg(test)]
    pub(crate) fn try_exclusive(root: &Path) -> Result<Option<WorkspaceLock>> {
        let file = open(root, true)?;
        match retry_would_block(|| file.try_lock()) {
            Ok(true) => Ok(Some(WorkspaceLock { file })),
            Ok(false) => Ok(None),
            Err(e) => Err(Error::io(root.join(layout::LOCK), e)),
        }
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        // Closing the file releases the lock too; unlocking first makes the release explicit.
        let _ = self.file.unlock();
    }
}

/// Runs a blocking lock call until it is not interrupted by a signal: `File::lock` and
/// `File::lock_shared` return `ErrorKind::Interrupted` on `EINTR` (WS-REQ-0050).
fn retry_interrupted<T>(mut call: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match call() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

/// Runs a non-blocking lock call until it is not interrupted: `Ok(true)` when the lock was
/// taken, `Ok(false)` when another lock holds it.
#[cfg(test)]
fn retry_would_block(
    mut call: impl FnMut() -> std::result::Result<(), std::fs::TryLockError>,
) -> io::Result<bool> {
    use std::fs::TryLockError;
    loop {
        match call() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(e)) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// Opens the lock file: read-only for a shared lock, so that a read-only workspace can be
/// queried, and read-write for an exclusive lock (emulated `flock` on network file systems
/// needs write access for it). The file is never created here: a lock on a re-created file
/// would not exclude a lock on the old one.
fn open(root: &Path, write: bool) -> Result<File> {
    let path = root.join(layout::LOCK);
    OpenOptions::new()
        .read(true)
        .write(write)
        .open(&path)
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound && !root.join(layout::MANIFEST).exists() {
                Error::NotAWorkspace {
                    path: root.to_path_buf(),
                }
            } else {
                Error::io(path, e)
            }
        })
}

#[cfg(test)]
mod tests {
    use std::fs::TryLockError;

    use super::*;
    use crate::error::ErrorCode;
    use crate::testutil::temp_dir;

    // WS-REQ-0050: separate open file descriptions of one process exclude each other.
    #[test]
    fn shared_and_exclusive() {
        let root = temp_dir("lock");
        std::fs::write(root.join(layout::LOCK), b"").unwrap();
        let a = WorkspaceLock::shared(&root).unwrap();
        let b = WorkspaceLock::try_shared(&root)
            .unwrap()
            .expect("shared locks coexist");
        assert!(WorkspaceLock::try_exclusive(&root).unwrap().is_none());
        drop(a);
        assert!(WorkspaceLock::try_exclusive(&root).unwrap().is_none());
        drop(b);
        let x = WorkspaceLock::try_exclusive(&root)
            .unwrap()
            .expect("no lock is held");
        assert!(WorkspaceLock::try_shared(&root).unwrap().is_none());
        assert!(WorkspaceLock::try_exclusive(&root).unwrap().is_none());
        drop(x);
        assert!(WorkspaceLock::try_shared(&root).unwrap().is_some());
        let x = WorkspaceLock::exclusive(&root).unwrap();
        assert!(WorkspaceLock::try_shared(&root).unwrap().is_none());
        drop(x);
    }

    // WS-REQ-0050 (review W3): a lock wait interrupted by a signal starts again instead of
    // failing; other errors end it. (The interruption itself cannot be produced here: that
    // needs a signal handler installed without SA_RESTART, which takes unsafe code.)
    #[test]
    fn interrupted_lock_waits_are_retried() {
        let mut calls = 0;
        let result = retry_interrupted(|| {
            calls += 1;
            if calls < 4 {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(result.unwrap(), 4);
        let mut calls = 0;
        let result: io::Result<()> = retry_interrupted(|| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 1);
        let mut calls = 0;
        let taken = retry_would_block(|| {
            calls += 1;
            if calls < 3 {
                Err(TryLockError::Error(io::ErrorKind::Interrupted.into()))
            } else {
                Err(TryLockError::WouldBlock)
            }
        });
        assert!(!taken.unwrap());
        assert_eq!(calls, 3);
    }

    // WS-REQ-0050: a shared lock needs only read access, so a read-only workspace can be queried.
    #[cfg(unix)]
    #[test]
    fn shared_lock_on_a_read_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_dir("lock-read-only");
        let path = root.join(layout::LOCK);
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let writable = OpenOptions::new().write(true).open(&path).is_ok();
        let shared = WorkspaceLock::shared(&root).unwrap();
        drop(shared);
        if !writable {
            // Not as root: the exclusive lock needs write access.
            assert_eq!(
                WorkspaceLock::exclusive(&root).unwrap_err().code(),
                ErrorCode::IoError
            );
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn missing_lock_file() {
        let root = temp_dir("lock-missing");
        let err = WorkspaceLock::shared(&root).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotAWorkspace);
        std::fs::write(root.join(layout::MANIFEST), b"{}").unwrap();
        let err = WorkspaceLock::exclusive(&root).unwrap_err();
        assert_eq!(err.code(), ErrorCode::IoError);
    }
}
