// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Failures around the commit point and damaged or hostile workspace files: a history line
//! that cannot be written after the manifest rename (WS-REQ-0076), table paths of `meta.json`
//! that leave the revision (WS-REQ-0024), and tables that are not regular files
//! (WS-REQ-0025).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::{inv, new_workspace, temp_dir};
use rusteda_workspace::{
    Corruption, DiagCode, Diagnostic, Error, ErrorCode, JsonMap, Rbin, RbinWriter, Rev, Upstream,
    Workspace, codes,
};

/// Makes every history append fail until [`repair_history`]: `history.jsonl` becomes a
/// directory, which no process can open for writing, not even one running as root.
fn break_history(root: &Path) -> (PathBuf, Vec<u8>) {
    let history = root.join("history.jsonl");
    let saved = fs::read(&history).unwrap();
    fs::remove_file(&history).unwrap();
    fs::create_dir(&history).unwrap();
    (history, saved)
}

fn repair_history((history, saved): (PathBuf, Vec<u8>)) {
    fs::remove_dir(&history).unwrap();
    fs::write(&history, saved).unwrap();
}

fn codes_of(warnings: &[Diagnostic]) -> Vec<DiagCode> {
    warnings.iter().map(|w| w.code).collect()
}

// WS-REQ-0076 (review W1): the manifest rename is the commit point. A history line that cannot
// be written after it does not turn the commit into a failure: the result is the new revision,
// with a warning, and no false failure line is written.
#[test]
fn commit_with_an_unwritable_history() {
    let ws = new_workspace("history-commit");
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool put"))
        .unwrap();
    txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    let broken = break_history(ws.root());
    let committed = txn.commit(JsonMap::new()).expect("the commit happened");
    assert_eq!(committed.rev, Rev(1));
    assert_eq!(ws.head().unwrap(), Rev(1));
    assert_eq!(codes_of(&committed.warnings), [codes::HISTORY_NOT_WRITTEN]);
    let warning = &committed.warnings[0];
    assert!(
        warning
            .message
            .starts_with("rev1 is committed and is HEAD, but writing its history line failed"),
        "{warning:?}"
    );
    assert!(warning.help.is_some());
    // The line the history should have: a successful commit of rev1.
    let line = &committed.history;
    assert!(line.ok);
    assert_eq!((line.new_rev, line.head_after), (Some(Rev(1)), Rev(1)));

    repair_history(broken);
    assert_eq!(ws.history().unwrap().len(), 1, "only the init line");
    // The workspace goes on from rev1, and later lines are written again.
    let mut txn = ws
        .begin(ws.resolve_base("rev1").unwrap(), &inv("tool add"))
        .unwrap();
    txn.add_bytes("b", "b.bin", b"b", Upstream::new()).unwrap();
    let next = txn.commit(JsonMap::new()).unwrap();
    assert!(next.warnings.is_empty());
    assert_eq!(next.meta.parent, Some(Rev(1)));
    let lines = ws.history().unwrap();
    assert!(lines.iter().all(|l| l.ok), "no failure line");
    assert_eq!(lines.last().unwrap().new_rev, Some(Rev(2)));
}

// WS-REQ-0076 (review W1): the same for revert: HEAD has moved, so the revert succeeds.
#[test]
fn revert_with_an_unwritable_history() {
    let ws = new_workspace("history-revert");
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool put"))
        .unwrap();
    txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    txn.commit(JsonMap::new()).unwrap();
    let broken = break_history(ws.root());
    let reverted = ws
        .revert(
            ws.resolve_base("rev1").unwrap(),
            Rev(0),
            &inv("rusteda revert --base-rev rev1 --to rev0"),
        )
        .expect("HEAD moved");
    assert_eq!((reverted.previous, reverted.head), (Rev(1), Rev(0)));
    assert_eq!(ws.head().unwrap(), Rev(0));
    assert_eq!(codes_of(&reverted.warnings), [codes::HISTORY_NOT_WRITTEN]);
    assert!(
        reverted.warnings[0]
            .message
            .starts_with("HEAD is rev0, but"),
        "{:?}",
        reverted.warnings
    );
    repair_history(broken);
    let lines = ws.history().unwrap();
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l.ok), "no failure line");
    // A retry with the old base is a conflict, as it should be: HEAD did move.
    let err = ws
        .revert(
            ws.resolve_base("rev1").unwrap(),
            Rev(0),
            &inv("rusteda revert"),
        )
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::RevisionConflict);
}

// WS-REQ-0076 (review W1): init that cannot write its history line has still created the
// workspace; it does not report an error that a second init would contradict.
#[test]
fn init_with_an_unwritable_history() {
    let root = temp_dir("history-init").join("ws");
    fs::create_dir_all(root.join("history.jsonl")).unwrap();
    let init = Workspace::init(&root, &inv("rusteda init")).expect("the workspace is created");
    assert_eq!(codes_of(&init.warnings), [codes::HISTORY_NOT_WRITTEN]);
    assert!(
        init.warnings[0]
            .message
            .starts_with("the workspace is created, but"),
        "{:?}",
        init.warnings
    );
    let ws = Workspace::open(&root).unwrap();
    assert_eq!(ws.head().unwrap(), Rev(0));
    assert_eq!(ws.revisions().unwrap(), [Rev(0)]);
    fs::remove_dir(root.join("history.jsonl")).unwrap();
    let err = Workspace::init(&root, &inv("rusteda init")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::WorkspaceExists);
}

// WS-REQ-0024 (review W2): a table path from meta.json names one entry of its revision
// directory; `..`, absolute paths, subdirectories and meta.json itself make the revision
// CORRUPT_TABLE before any table is opened.
#[test]
fn table_paths_stay_in_their_revision() {
    let ws = new_workspace("meta-paths");
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool put"))
        .unwrap();
    txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    txn.add_bytes("b", "b.bin", b"b", Upstream::new()).unwrap();
    txn.commit(JsonMap::new()).unwrap();
    let meta = ws.revision_dir(Rev(1)).join("meta.json");
    let text = fs::read_to_string(&meta).unwrap();
    for bad in [
        "../../manifest.json",
        "/etc/hostname",
        "..",
        ".",
        "",
        "meta.json",
        "sub/a.bin",
        ".hidden",
        "b.bin",
    ] {
        let quoted = serde_json::to_string(bad).unwrap();
        fs::write(
            &meta,
            text.replace("\"path\": \"a.bin\"", &format!("\"path\": {quoted}")),
        )
        .unwrap();
        let err = ws.revision(Rev(1)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable, "{bad}: {err}");
        assert!(err.to_string().contains("meta.json"), "{err}");
        let base = ws.resolve_base("rev1").unwrap();
        let err = ws.begin(base, &inv("tool add")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable, "{bad}");
    }
    fs::write(&meta, &text).unwrap();
    assert_eq!(ws.revision(Rev(1)).unwrap().read_table("a").unwrap(), b"a");
}

/// Replaces `file` by a FIFO with `mkfifo`; false where there is none.
#[cfg(unix)]
fn make_fifo(file: &Path) -> bool {
    fs::remove_file(file).unwrap();
    std::process::Command::new("mkfifo")
        .arg(file)
        .status()
        .is_ok_and(|s| s.success())
}

// WS-REQ-0025 (review W2): a table that is a FIFO is rejected at once by every reader,
// instead of blocking the open; the carry-forward of a commit does not block on it either.
#[cfg(unix)]
#[test]
fn fifo_tables_never_block() {
    let ws = new_workspace("fifo-tables");
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool put"))
        .unwrap();
    let mut w = RbinWriter::new("ADV", 1).unwrap();
    w.section_vec(1, vec![1u32, 2, 3]).unwrap();
    txn.add_rbin("t", "t.rbin", &w, Upstream::new()).unwrap();
    txn.add_bytes("a", "a.json", b"{}", Upstream::new())
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();
    let dir = ws.revision_dir(Rev(1));
    for file in ["t.rbin", "a.json"] {
        if !make_fifo(&dir.join(file)) {
            eprintln!("mkfifo is not available; skipped");
            return;
        }
    }
    let (tx, rx) = mpsc::channel();
    let reader = ws.clone();
    thread::spawn(move || {
        let r = reader.revision(Rev(1)).unwrap();
        let path = r.table_path("t").unwrap();
        let mut txn = reader
            .begin(reader.resolve_base("rev1").unwrap(), &inv("tool add"))
            .unwrap();
        txn.add_bytes("b", "b.bin", b"b", Upstream::new()).unwrap();
        let outcomes = [
            r.open_rbin("t", "ADV", 1..=1).map(drop),
            Rbin::read(&path, "ADV", 1..=1).map(drop),
            r.read_table("a").map(drop),
            r.verify_table("t"),
            r.verify_table("a"),
            txn.commit(JsonMap::new()).map(drop),
        ];
        let codes: Vec<_> = outcomes
            .into_iter()
            .map(|o| o.map_err(|e| (e.code(), e.to_string())))
            .collect();
        tx.send(codes).unwrap();
    });
    let Ok(outcomes) = rx.recv_timeout(Duration::from_secs(20)) else {
        // Unblock a reader stuck in open() so that the test process can end.
        for file in ["t.rbin", "a.json"] {
            let _ = fs::OpenOptions::new().write(true).open(dir.join(file));
        }
        panic!("a reader blocked on a FIFO table");
    };
    let (readers, commit) = outcomes.split_at(5);
    for outcome in readers {
        let (code, message) = outcome.clone().unwrap_err();
        assert_eq!(code, ErrorCode::CorruptTable, "{message}");
        assert!(message.contains("not a regular file"), "{message}");
    }
    // The commit cannot carry a FIFO forward: it fails, without blocking, and HEAD stays.
    assert_eq!(commit[0].clone().unwrap_err().0, ErrorCode::IoError);
    assert_eq!(ws.head().unwrap(), Rev(1));
}

// WS-REQ-0025 (review W2): a table replaced by a symbolic link, even to a device that never
// ends, is not followed.
#[cfg(unix)]
#[test]
fn symlinked_tables_are_not_followed() {
    let ws = new_workspace("symlink-tables");
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool put"))
        .unwrap();
    let mut w = RbinWriter::new("ADV", 1).unwrap();
    w.section_vec(1, vec![7u64]).unwrap();
    txn.add_rbin("t", "t.rbin", &w, Upstream::new()).unwrap();
    txn.add_bytes("a", "a.json", b"{}", Upstream::new())
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();
    let dir = ws.revision_dir(Rev(1));
    for (file, target) in [("t.rbin", "../../manifest.json"), ("a.json", "/dev/zero")] {
        fs::remove_file(dir.join(file)).unwrap();
        std::os::unix::fs::symlink(target, dir.join(file)).unwrap();
    }
    let r = ws.revision(Rev(1)).unwrap();
    let errors = [
        r.open_rbin("t", "ADV", 1..=1).map(drop).unwrap_err(),
        r.read_table("a").map(drop).unwrap_err(),
        r.verify_table("a").unwrap_err(),
        r.verify_table("t").unwrap_err(),
    ];
    for err in errors {
        assert_eq!(err.code(), ErrorCode::CorruptTable, "{err}");
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Whether this process is held to file permissions (not so as root): a file cannot be made in
/// a read-only directory.
#[cfg(unix)]
fn permissions_hold(dir: &Path) -> bool {
    let probe = dir.join("permission-probe");
    fs::create_dir_all(&probe).unwrap();
    chmod(&probe, 0o555);
    let held = fs::write(probe.join("x"), b"").is_err();
    chmod(&probe, 0o755);
    fs::remove_dir_all(&probe).unwrap();
    held
}

// WS-REQ-0069, WS-REQ-0079 (review R2-C3): Txn::reject writes the history line first and then
// removes the staging directory. A read-only subdirectory, as a copy of a read-only source tree
// has, is removed all the same; a staging directory that cannot be removed at all stays behind,
// ignored by readers, and costs the rejected command no history line.
#[cfg(unix)]
#[test]
fn reject_logs_before_it_cleans_up() {
    let ws = new_workspace("reject-cleanup");
    let error = Error::input_rejected("ip.v: syntax error");
    let txn = ws
        .begin(
            ws.resolve_base("rev0").unwrap(),
            &inv("rusteda import --rtl ip"),
        )
        .unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir_all(rtl.join("vendor/deep")).unwrap();
    fs::write(rtl.join("vendor/deep/ip.v"), b"module ip; endmodule\n").unwrap();
    chmod(&rtl.join("vendor/deep"), 0o555);
    chmod(&rtl.join("vendor"), 0o555);
    let staging = txn.staging_dir().to_path_buf();
    let before = ws.history().unwrap().len();
    let line = txn.reject(&error).unwrap();
    let lines = ws.history().unwrap();
    assert_eq!(lines.len(), before + 1);
    assert_eq!(lines.last(), Some(&line));
    assert_eq!(
        (line.ok, line.exit, line.base_rev),
        (false, 1, Some(Rev(0)))
    );
    assert!(
        !staging.exists(),
        "the read-only subdirectories are removed too"
    );

    let parent = temp_dir("reject-cleanup-probe");
    if !permissions_hold(&parent) {
        eprintln!("file permissions do not hold for this process; the rest is skipped");
        return;
    }
    // revisions/ read-only: the staging directory cannot be removed.
    let txn = ws
        .begin(
            ws.resolve_base("rev0").unwrap(),
            &inv("rusteda import --rtl ip"),
        )
        .unwrap();
    let staging = txn.staging_dir().to_path_buf();
    let revisions = ws.root().join("revisions");
    chmod(&revisions, 0o555);
    let outcome = txn.reject(&error);
    chmod(&revisions, 0o755);
    let line = outcome.expect("the history line is written");
    let lines = ws.history().unwrap();
    assert_eq!(lines.len(), before + 2);
    assert_eq!(lines.last(), Some(&line));
    assert!(staging.exists(), "the staging directory stays behind");
    assert_eq!(ws.revisions().unwrap(), [Rev(0)], "readers ignore it");
    let mut next = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool put"))
        .unwrap();
    next.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    assert_eq!(next.commit(JsonMap::new()).unwrap().rev, Rev(1));
    fs::remove_dir_all(&staging).unwrap();
}

// WS-REQ-0025, WS-REQ-0026 (review R2-C5, CR2-2): inside a directory table, an entry that is a
// symbolic link or a FIFO, or a name that is not UTF-8, is CORRUPT_TABLE (exit 2), as for any
// table of a revision, not IO_ERROR; the FIFO never blocks. In a staging directory the same
// entries are a bug of the tool: INTERNAL.
#[cfg(unix)]
#[test]
fn special_entries_of_directory_tables() {
    use std::os::unix::ffi::OsStrExt;
    let ws = new_workspace("dir-table-special");
    let mut txn = ws
        .begin(
            ws.resolve_base("rev0").unwrap(),
            &inv("rusteda import --rtl a.v"),
        )
        .unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir(&rtl).unwrap();
    fs::write(rtl.join("a.v"), b"module a; endmodule\n").unwrap();
    fs::write(rtl.join("b.v"), b"module b; endmodule\n").unwrap();
    std::os::unix::fs::symlink("a.v", rtl.join("link.v")).unwrap();
    let err = txn.add_dir("rtl", "rtl", Upstream::new()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal, "{err}");
    fs::remove_file(rtl.join("link.v")).unwrap();
    txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    let rev = ws.revision(c.rev).unwrap();
    rev.verify_table("rtl").unwrap();
    let dir = c.dir.join("rtl");

    let check = |what: &str, entry: &str| {
        for err in [
            rev.verify_table("rtl").unwrap_err(),
            rev.table_files("rtl").unwrap_err(),
        ] {
            assert_eq!(
                (err.code(), err.exit_code()),
                (ErrorCode::CorruptTable, 2),
                "{what}: {err}"
            );
            assert!(err.to_string().contains(entry), "{what}: {err}");
        }
    };
    fs::remove_file(dir.join("a.v")).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", dir.join("a.v")).unwrap();
    check("symlink", "a.v: not a regular file or a directory");
    fs::remove_file(dir.join("a.v")).unwrap();
    fs::write(dir.join("a.v"), b"module a; endmodule\n").unwrap();
    rev.verify_table("rtl").unwrap();

    if make_fifo(&dir.join("b.v")) {
        check("fifo", "b.v: not a regular file or a directory");
        fs::remove_file(dir.join("b.v")).unwrap();
        fs::write(dir.join("b.v"), b"module b; endmodule\n").unwrap();
    }
    let odd = dir.join(std::ffi::OsStr::from_bytes(b"odd\xff.v"));
    if fs::write(&odd, b"").is_ok() {
        check("name", "the name is not UTF-8");
        let err = rev.verify_table("rtl").unwrap_err();
        assert!(
            matches!(
                err,
                Error::Corrupt {
                    what: Corruption::NotUtf8,
                    ..
                }
            ),
            "{err}"
        );
        fs::remove_file(&odd).unwrap();
    }
    rev.verify_table("rtl").unwrap();
}
