// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The workspace: layout, resolution, the commit protocol with carry-forward, conflicts,
//! revert, crash recovery, history, the reserved ui_state.json and determinism.

#![allow(dead_code, unused_imports)]
mod common;

use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use common::{entries, inv, new_workspace, temp_dir, top_level_keys};
use rusteda_workspace::hash::hash_dir;
use rusteda_workspace::{
    BaseRev, Committed, Corruption, DroppedEntry, Error, ErrorCode, Hash64, JsonMap, LibraryEntry,
    RbinWriter, Rev, SourceEntry, TableValues, UiState, Upstream, Workspace, exit,
};
use serde_json::{Value, json};

/// Values per table from `(table, key, value)` triples.
fn per_table(values: &[(&str, &str, Value)]) -> TableValues {
    let mut out = TableValues::new();
    for (table, key, value) in values {
        out.entry(table.to_string())
            .or_default()
            .insert(key.to_string(), value.clone());
    }
    out
}

/// A small rbin table whose content depends on `seed`.
fn table(kind: &str, seed: u32) -> RbinWriter<'static> {
    let mut w = RbinWriter::new(kind, 1).unwrap();
    w.section_vec(1, (0..64u32).map(|i| i * seed).collect::<Vec<u32>>())
        .unwrap();
    w
}

fn lib(name: &str, corner: &str, hash: u64) -> LibraryEntry {
    LibraryEntry {
        name: name.to_string(),
        corner: corner.to_string(),
        hash: Hash64(hash),
        source: format!("libs/{name}.lib"),
        compiled: format!("cache/{name}-{}.rlib", Hash64(hash).hex()),
    }
}

fn base(ws: &Workspace, spec: &str) -> BaseRev {
    ws.resolve_base(spec).unwrap()
}

/// Commits one bytes table `name` (no upstream) on HEAD.
fn commit_bytes(ws: &Workspace, name: &str, content: &[u8]) -> Committed {
    let head = ws.head().unwrap();
    let mut txn = ws
        .begin(base(ws, &head.to_string()), &inv("tool put"))
        .unwrap();
    txn.add_bytes(name, &format!("{name}.bin"), content, Upstream::new())
        .unwrap();
    txn.commit(JsonMap::new()).unwrap()
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    std::os::unix::fs::MetadataExt::ino(&fs::metadata(path).unwrap())
}

// WS-REQ-0042: one line per workspace-changing command, keys in contract order.
#[test]
fn history_lines() {
    let ws = new_workspace("history");
    let mut txn = ws.begin(base(&ws, "rev0"), &inv("tool put")).unwrap();
    txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    txn.commit(JsonMap::from([
        ("cells".into(), json!(5)),
        ("area_um2".into(), json!(1.25)),
    ]))
    .unwrap();
    ws.log_rejected(
        &inv("rusteda import --frob"),
        None,
        &Error::usage("unknown flag --frob"),
    )
    .unwrap();
    ws.revert(
        base(&ws, "rev1"),
        Rev(0),
        &inv("rusteda revert --base-rev rev1 --to rev0"),
    )
    .unwrap();

    let text = fs::read_to_string(ws.root().join("history.jsonl")).unwrap();
    let keys = [
        "schema",
        "time_unix_ms",
        "tool",
        "tool_version",
        "command",
        "argv",
        "base_rev",
        "new_rev",
        "head_after",
        "ok",
        "exit",
        "error",
        "metrics",
        "metric_deltas",
    ];
    let lines: Vec<Value> = text
        .lines()
        .map(|line| {
            assert_eq!(top_level_keys(line), keys, "{line}");
            assert!(
                !line.contains('\n') && !line.contains(": "),
                "compact: {line}"
            );
            serde_json::from_str(line).unwrap()
        })
        .collect();
    assert_eq!(lines.len(), 4);
    assert!(lines.iter().all(|l| l["schema"] == "rusteda.history.v1"));
    assert_eq!(lines[1]["metrics"], json!({"area_um2": 1.25, "cells": 5}));
    assert_eq!(lines[2]["base_rev"], Value::Null);
    assert_eq!(lines[2]["head_after"], "rev1");
    assert_eq!(
        lines[2]["error"],
        json!({"code": "USAGE", "message": "unknown flag --frob"})
    );
    assert_eq!(
        (lines[2]["ok"].clone(), lines[2]["exit"].clone()),
        (json!(false), json!(1))
    );
    assert_eq!(lines[3]["new_rev"], Value::Null);
    assert_eq!(lines[3]["head_after"], "rev0");
    assert!(lines[0]["time_unix_ms"].as_u64().unwrap() > 1_600_000_000_000);

    // A line of a newer major schema is not read.
    let mut text = text;
    text.push_str(&serde_json::to_string(&json!({"schema": "rusteda.history.v2"})).unwrap());
    text.push('\n');
    fs::write(ws.root().join("history.jsonl"), text).unwrap();
    assert_eq!(ws.history().unwrap_err().code(), ErrorCode::SchemaTooNew);
}

// WS-REQ-0043
#[test]
fn ui_state_selection() {
    let ws = new_workspace("ui");
    commit_bytes(&ws, "a", b"a");
    let err = ws.resolve_selection("sel4", Rev(1)).unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownHandle);
    assert!(err.to_string().contains("no ui_state.json"), "{err}");
    assert_eq!(ws.ui_state().unwrap(), None);

    let state = UiState::new(
        Rev(1),
        "sel4",
        vec!["inst8".into(), "inst9".into()],
        vec!["path1".into()],
    );
    ws.write_ui_state(&state).unwrap();
    let text = fs::read_to_string(ws.root().join("ui_state.json")).unwrap();
    assert_eq!(
        top_level_keys(&text),
        [
            "schema",
            "revision",
            "selection_handle",
            "selected_instances",
            "selected_paths"
        ]
    );
    assert_eq!(ws.resolve_selection("sel4", Rev(1)).unwrap(), state);
    assert_eq!(ws.resolve_selection("sel0004", Rev(1)).unwrap(), state);
    let err = ws.resolve_selection("sel4", Rev(0)).unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownHandle);
    assert!(
        err.to_string().contains("sel4 belongs to rev1, not rev0"),
        "{err}"
    );
    let err = ws.resolve_selection("sel5", Rev(1)).unwrap_err();
    assert!(err.to_string().contains("holds selection 'sel4'"), "{err}");
    assert_eq!(
        ws.resolve_selection("path4", Rev(1)).unwrap_err().code(),
        ErrorCode::UnknownHandle
    );
    assert_eq!(
        ws.resolve_selection("sel#4", Rev(1)).unwrap_err().code(),
        ErrorCode::UnknownHandle
    );

    fs::write(
        ws.root().join("ui_state.json"),
        text.replace("ui_state.v1", "ui_state.v2"),
    )
    .unwrap();
    assert_eq!(
        ws.resolve_selection("sel4", Rev(1)).unwrap_err().code(),
        ErrorCode::SchemaTooNew
    );
}

// WS-REQ-0043 (review CW-6): handles read from ui_state.json, a file of another program, are
// shell-safe, or the file is rejected before any of them reaches the command line.
#[test]
fn ui_state_handles_are_checked() {
    let ws = new_workspace("ui-handles");
    let path = ws.root().join("ui_state.json");
    let state = |selection: &str, instances: &str, paths: &str| {
        format!(
            "{{\"schema\":\"rusteda.ui_state.v1\",\"revision\":\"rev0\",\
             \"selection_handle\":\"{selection}\",\"selected_instances\":[{instances}],\
             \"selected_paths\":[{paths}]}}"
        )
    };
    fs::write(&path, state("sel4", r#""inst841","inst9""#, r#""path1""#)).unwrap();
    let ok = ws.resolve_selection("sel4", Rev(0)).unwrap();
    assert_eq!(ok.selected_instances, ["inst841", "inst9"]);
    for (selection, instances, paths) in [
        ("sel4", r##""inst 1","$HOME","#p1""##, r#""path;rm""#),
        ("sel4", r#""$HOME""#, ""),
        ("sel4", "", r##""#p1""##),
        ("sel4", "", r#""path;rm""#),
        ("sel4", r#""inst`id`1""#, ""),
        ("sel4", r#""""#, ""),
        ("sel$4", "", ""),
        ("path4", "", ""),
    ] {
        fs::write(&path, state(selection, instances, paths)).unwrap();
        let err = ws.resolve_selection("sel4", Rev(0)).unwrap_err();
        assert_eq!(
            err.code(),
            ErrorCode::CorruptTable,
            "{selection} {instances} {paths}"
        );
        assert_eq!(ws.ui_state().unwrap_err().code(), ErrorCode::CorruptTable);
        assert!(err.help().unwrap().contains("ui_state.json"), "{err}");
    }
    let bad = UiState::new(Rev(0), "sel4", vec!["$HOME".into()], vec![]);
    assert_eq!(
        ws.write_ui_state(&bad).unwrap_err().code(),
        ErrorCode::Internal
    );
}

// WS-REQ-0046 (review CW-1): a command that commits with a negative or inconclusive verdict
// records its exit code, 3 or 5, with ok: true; other codes are refused.
#[test]
fn committed_verdict_exit_codes() {
    let ws = new_workspace("verdict-exit");
    for (code, verdict) in [
        (exit::NEGATIVE_VERDICT, "not-equivalent"),
        (exit::INCONCLUSIVE, "undecided"),
        (exit::OK, "equivalent"),
    ] {
        let head = ws.head().unwrap();
        let mut txn = ws
            .begin(base(&ws, &head.to_string()), &inv("rustsys check"))
            .unwrap();
        let body = format!("{{\"verdict\":\"{verdict}\"}}");
        txn.add_bytes("check", "check.json", body.as_bytes(), Upstream::new())
            .unwrap();
        txn.set_status("check", "equivalence", verdict).unwrap();
        if code != exit::OK {
            txn.set_exit(code).unwrap();
        }
        let c = txn.commit(JsonMap::new()).unwrap();
        assert!(c.history.ok, "{verdict}");
        assert_eq!((c.history.exit, &c.history.error), (code, &None));
        assert_eq!(ws.history().unwrap().last(), Some(&c.history));
    }
    let lines = ws.history().unwrap();
    let exits: Vec<i32> = lines.iter().map(|l| l.exit).collect();
    assert_eq!(exits, [0, 3, 5, 0]);

    let mut txn = ws.begin(base(&ws, "rev3"), &inv("rustsys check")).unwrap();
    for bad in [1, 2, 4, 130, -1] {
        assert_eq!(
            txn.set_exit(bad).unwrap_err().code(),
            ErrorCode::Internal,
            "{bad}"
        );
    }
    txn.abort().unwrap();
}

// WS-REQ-0045 (review CW-2): summary and status values belong to their table: they are set
// only for tables the transaction adds, carried with the table and dropped with it.
// WS-REQ-0044: the metric deltas of a table compare with its previous version, also across
// an optimization that dropped it (ruststa run -> rustsys opt-timing -> ruststa run).
#[test]
fn values_follow_their_tables_through_the_closed_loop() {
    let ws = new_workspace("values-follow-tables");
    let bytes = |txn: &mut rusteda_workspace::Txn, name: &str, body: &[u8], up: Upstream| {
        txn.add_bytes(name, &format!("{name}.bin"), body, up)
            .unwrap();
    };

    // rev1: an imported netlist.
    let mut txn = ws
        .begin(base(&ws, "rev0"), &inv("rusteda import --netlist n.v"))
        .unwrap();
    bytes(&mut txn, "netlist", b"n1", Upstream::new());
    txn.set_summary("netlist", "cells", 100).unwrap();
    txn.set_summary("netlist", "area_um2", 50.0).unwrap();
    txn.commit(JsonMap::new()).unwrap();

    // rev2: rustsys check, computed from the netlist.
    let mut txn = ws
        .begin(base(&ws, "rev1"), &inv("rustsys check --base-rev rev1"))
        .unwrap();
    let up = txn.upstream(&["netlist"]).unwrap();
    bytes(&mut txn, "check", b"{}", up);
    txn.set_status("check", "equivalence", "equivalent")
        .unwrap();
    for (table, err) in [
        (
            "netlist",
            txn.set_status("netlist", "equivalence", "equivalent"),
        ),
        ("nope", txn.set_summary("nope", "cells", 1)),
    ] {
        assert_eq!(err.unwrap_err().code(), ErrorCode::Internal, "{table}");
    }
    txn.commit(JsonMap::new()).unwrap();

    // rev3: ruststa run; check is carried with its status.
    let mut txn = ws
        .begin(base(&ws, "rev2"), &inv("ruststa run --base-rev rev2"))
        .unwrap();
    let up = txn.upstream(&["netlist"]).unwrap();
    bytes(&mut txn, "sta", b"s1", up);
    txn.set_summary("sta", "wns_ns", -0.5).unwrap();
    txn.set_status("sta", "timing", "violated").unwrap();
    let c3 = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(c3.carried, ["check", "netlist"]);
    assert_eq!(
        c3.meta.status,
        per_table(&[
            ("check", "equivalence", json!("equivalent")),
            ("sta", "timing", json!("violated")),
        ])
    );
    assert_eq!(
        c3.meta.summary,
        per_table(&[
            ("netlist", "area_um2", json!(50.0)),
            ("netlist", "cells", json!(100)),
            ("sta", "wns_ns", json!(-0.5)),
        ])
    );

    // rev4: rustsys opt-timing writes a new netlist: check and sta are dropped, and their
    // status with them, so nothing claims equivalence or timing for the new netlist.
    let mut txn = ws
        .begin(
            base(&ws, "rev3"),
            &inv("rustsys opt-timing --base-rev rev3"),
        )
        .unwrap();
    bytes(&mut txn, "netlist", b"n2", Upstream::new());
    txn.set_summary("netlist", "cells", 104).unwrap();
    txn.set_summary("netlist", "area_um2", 51.5).unwrap();
    let c4 = txn.commit(JsonMap::new()).unwrap();
    let dropped: Vec<&str> = c4.meta.dropped.iter().map(|d| d.table.as_str()).collect();
    assert_eq!(dropped, ["check", "sta"]);
    assert!(c4.meta.status.is_empty(), "{:?}", c4.meta.status);
    assert_eq!(
        c4.meta.summary,
        per_table(&[
            ("netlist", "area_um2", json!(51.5)),
            ("netlist", "cells", json!(104)),
        ])
    );
    assert_eq!(
        c4.history.metric_deltas,
        per_table(&[
            ("netlist", "area_um2", json!(1.5)),
            ("netlist", "cells", json!(4)),
        ])
    );

    // rev5: ruststa run again: the WNS delta compares with the timing table of rev3.
    let mut txn = ws
        .begin(base(&ws, "rev4"), &inv("ruststa run --base-rev rev4"))
        .unwrap();
    let up = txn.upstream(&["netlist"]).unwrap();
    bytes(&mut txn, "sta", b"s2", up);
    txn.set_summary("sta", "wns_ns", -0.1).unwrap();
    txn.set_status("sta", "timing", "violated").unwrap();
    let c5 = txn
        .commit(JsonMap::from([("wns_ns".into(), json!(-0.1))]))
        .unwrap();
    let delta = c5.history.metric_deltas["sta"]["wns_ns"].as_f64().unwrap();
    assert!((delta - 0.4).abs() < 1e-12, "{delta}");
    assert_eq!(c5.history.metric_deltas.len(), 1);
    let line = ws.history().unwrap().pop().unwrap();
    assert_eq!(line.metric_deltas, c5.history.metric_deltas);
    assert_eq!(
        line.metrics,
        JsonMap::from([("wns_ns".into(), json!(-0.1))])
    );

    // The revert back to rev3 compares the two versions of the tables both revisions have.
    let reverted = ws
        .revert(base(&ws, "rev5"), Rev(3), &inv("rusteda revert"))
        .unwrap();
    assert_eq!(
        reverted.history.metric_deltas,
        per_table(&[
            ("netlist", "area_um2", json!(-1.5)),
            ("netlist", "cells", json!(-4)),
            ("sta", "wns_ns", json!(-0.4)),
        ])
    );
}

// WS-REQ-0014 (review CW-4): a netlist synthesized against one corner names
// `libraries.<corner>` as its upstream, so importing a library for another analysis corner
// keeps it; timing over all corners names `libraries` and is dropped.
#[test]
fn corner_pseudo_tables() {
    let ws = new_workspace("corners");
    let mut txn = ws
        .begin(
            base(&ws, "rev0"),
            &inv("rusteda import --lib tt.lib --rtl top.v"),
        )
        .unwrap();
    txn.add_library(lib("cells_tt", "default", 1)).unwrap();
    txn.add_bytes("rtl", "rtl.v", b"module top; endmodule", Upstream::new())
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();

    let mut txn = ws
        .begin(base(&ws, "rev1"), &inv("rustsys synth --base-rev rev1"))
        .unwrap();
    let up = txn.upstream(&["rtl", "libraries.default"]).unwrap();
    assert_eq!(
        up["libraries.default"],
        LibraryEntry::corner_hash(&[lib("cells_tt", "default", 1)], "default").unwrap()
    );
    txn.add_bytes("netlist", "netlist.bin", b"gates", up)
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();

    let mut txn = ws
        .begin(base(&ws, "rev2"), &inv("ruststa run --base-rev rev2"))
        .unwrap();
    let err = txn.upstream(&["libraries.ss"]).unwrap_err();
    assert_eq!(err.code(), ErrorCode::MissingTable);
    let up = txn.upstream(&["netlist", "libraries"]).unwrap();
    txn.add_bytes("sta", "sta.bin", b"timing", up).unwrap();
    txn.commit(JsonMap::new()).unwrap();

    // A second corner for timing: the netlist stays, the timing of all corners does not.
    let mut txn = ws
        .begin(
            base(&ws, "rev3"),
            &inv("rusteda import --lib ss.lib --corner ss"),
        )
        .unwrap();
    txn.add_library(lib("cells_ss", "ss", 2)).unwrap();
    assert!(txn.table_hash("libraries.ss").is_some());
    let c4 = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(c4.carried, ["netlist", "rtl"]);
    assert_eq!(
        c4.meta.dropped,
        [DroppedEntry {
            table: "sta".into(),
            reason: "upstream libraries changed".into()
        }]
    );
    assert_eq!(ws.manifest().unwrap().corners, ["default", "ss"]);
    let rev4 = ws.revision(c4.rev).unwrap();
    assert!(rev4.table_hash("libraries.ss").is_some());
    assert_eq!(rev4.table_hash("libraries.ff"), None);

    // A new library for the netlist's own corner drops it.
    let mut txn = ws
        .begin(base(&ws, "rev4"), &inv("rusteda import --lib tt2.lib"))
        .unwrap();
    txn.add_library(lib("cells_tt", "default", 3)).unwrap();
    let err = txn.upstream(&["netlist"]).unwrap_err();
    assert!(
        err.to_string()
            .contains("upstream libraries.default changed"),
        "{err}"
    );
    // A transaction that removes every library of a corner has no such pseudo-table.
    txn.set_libraries(vec![lib("cells_ss", "ss", 2)]).unwrap();
    let err = txn.upstream(&["libraries.default"]).unwrap_err();
    assert_eq!(err.code(), ErrorCode::MissingTable);
    assert!(err.to_string().contains("corner default"), "{err}");
    for bad in ["libraries.", "libraries.a b", "libraries.x.y"] {
        let up = Upstream::from([(bad.to_string(), Hash64(1))]);
        let err = txn.add_bytes("t", "t.bin", b"t", up).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal, "{bad}");
    }
    txn.abort().unwrap();
}

// A table changed on disk fails verification.
#[test]
fn tampering_is_detected() {
    let ws = new_workspace("tamper");
    let mut txn = ws.begin(base(&ws, "rev0"), &inv("tool put")).unwrap();
    let dir = txn.staging_path("rtl").unwrap();
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("a.v"), "x").unwrap();
    txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    let rev = ws.revision(c.rev).unwrap();
    rev.verify_table("a").unwrap();
    rev.verify_table("rtl").unwrap();
    fs::write(c.dir.join("a.bin"), b"b").unwrap();
    fs::write(c.dir.join("rtl/extra.v"), "y").unwrap();
    for name in ["a", "rtl"] {
        let err = rev.verify_table(name).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Corrupt {
                    what: Corruption::TableHash { .. },
                    ..
                }
            ),
            "{err}"
        );
    }
}

/// `created_unix_ms` / `time_unix_ms` values replaced by 0.
fn without_times(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("_unix_ms\":") {
        let (head, tail) = rest.split_at(at + "_unix_ms\":".len());
        out.push_str(head);
        let spaces = tail.len() - tail.trim_start().len();
        let digits = tail[spaces..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        out.push_str(&tail[..spaces]);
        out.push('0');
        rest = &tail[spaces + digits..];
    }
    out.push_str(rest);
    out
}

fn build(name: &str) -> Workspace {
    let ws = new_workspace(name);
    let mut txn = ws
        .begin(base(&ws, "rev0"), &inv("rusteda import --rtl top.v"))
        .unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir_all(rtl.join("inc")).unwrap();
    fs::write(rtl.join("top.v"), "module top; endmodule\n").unwrap();
    fs::write(rtl.join("inc/a.vh"), "`define A\n").unwrap();
    txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    txn.add_library(lib("cells", "default", 7)).unwrap();
    let upstream = txn.upstream(&["rtl", "libraries"]).unwrap();
    txn.add_rbin("netlist", "netlist.rbin", &table("NETLIST", 9), upstream)
        .unwrap();
    txn.add_source("rtl", "top.v", Hash64(5)).unwrap();
    txn.set_summary("netlist", "cells", 3).unwrap();
    txn.set_summary("netlist", "area_um2", 1.5).unwrap();
    txn.set_status("netlist", "equivalence", "unchecked")
        .unwrap();
    txn.set_message("import");
    txn.commit(JsonMap::from([("cells".into(), json!(3))]))
        .unwrap();
    let mut txn = ws
        .begin(base(&ws, "rev1"), &inv("ruststa run --base-rev rev1"))
        .unwrap();
    let upstream = txn.upstream(&["netlist", "libraries"]).unwrap();
    txn.add_rbin("sta", "sta.rbin", &table("STA", 11), upstream)
        .unwrap();
    txn.set_summary("sta", "wns_ns", -0.25).unwrap();
    txn.commit(JsonMap::new()).unwrap();
    ws
}

// WS-REQ-0100: identical inputs give identical bytes, except the times.
#[test]
fn determinism() {
    let a = build("det-a");
    let b = build("det-b");
    let read = |ws: &Workspace, path: &str| fs::read(ws.root().join(path)).unwrap();
    assert_eq!(read(&a, "manifest.json"), read(&b, "manifest.json"));
    for rev in 0..=2 {
        let dir = format!("revisions/{}", Rev(rev).dir_name());
        let meta_a = String::from_utf8(read(&a, &format!("{dir}/meta.json"))).unwrap();
        let meta_b = String::from_utf8(read(&b, &format!("{dir}/meta.json"))).unwrap();
        assert_eq!(without_times(&meta_a), without_times(&meta_b), "{dir}");
        assert_ne!(without_times(&meta_a), meta_a, "the time was normalized");
        let ra = a.revision(Rev(rev)).unwrap();
        for (name, table) in &ra.meta().tables {
            if table.path.ends_with(".rbin") {
                let path = format!("{dir}/{}", table.path);
                assert_eq!(read(&a, &path), read(&b, &path), "{name}");
            }
        }
    }
    let ha = fs::read_to_string(a.root().join("history.jsonl")).unwrap();
    let hb = fs::read_to_string(b.root().join("history.jsonl")).unwrap();
    assert_eq!(without_times(&ha), without_times(&hb));
}

/// Overwrites the first u32 of section 1 of a `table(..)` rbin in place.
#[cfg(unix)]
fn poke(path: &Path) {
    use std::io::{Seek, SeekFrom, Write};
    let r = rusteda_workspace::Rbin::read(path, "ADV", 1..=1).unwrap();
    let offset = r.section_info(1).unwrap().offset;
    let mut f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.seek(SeekFrom::Start(offset)).unwrap();
    f.write_all(&0xdead_beefu32.to_le_bytes()).unwrap();
}

// WS-REQ-0077 (review R2-C2): a staging file that is a hard link to a file outside the
// revisions (a user's source file) is refused by add_file and add_dir, so a committed table
// never shares its inode with a file that an edit in place would change; a copy is accepted,
// and later edits of the user's files leave the revision as it was committed.
#[cfg(unix)]
#[test]
fn staging_files_do_not_share_inodes_with_outside_files() {
    use std::os::unix::fs::MetadataExt;
    let ws = new_workspace("links-outside");
    let user = temp_dir("links-outside-user");
    table("ADV", 1).write_to(user.join("user.rbin")).unwrap();
    fs::write(user.join("top.v"), "module top; endmodule\n").unwrap();
    let mut txn = ws
        .begin(base(&ws, "rev0"), &inv("rusteda import --rtl top.v"))
        .unwrap();
    let staged = txn.staging_path("t.rbin").unwrap();
    fs::hard_link(user.join("user.rbin"), &staged).unwrap();
    let err = txn.add_file("t", "t.rbin", Upstream::new()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal, "{err}");
    assert!(err.to_string().contains("hard links"), "{err}");
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir_all(rtl.join("inc")).unwrap();
    fs::hard_link(user.join("top.v"), rtl.join("inc/top.v")).unwrap();
    let err = txn.add_dir("rtl", "rtl", Upstream::new()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal, "{err}");
    assert!(err.to_string().contains("inc/top.v"), "{err}");

    // Copies are new files.
    fs::remove_file(&staged).unwrap();
    fs::copy(user.join("user.rbin"), &staged).unwrap();
    txn.add_file("t", "t.rbin", Upstream::new()).unwrap();
    fs::remove_file(rtl.join("inc/top.v")).unwrap();
    fs::copy(user.join("top.v"), rtl.join("inc/top.v")).unwrap();
    txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(fs::metadata(c.dir.join("t.rbin")).unwrap().nlink(), 1);

    // The user edits the files in place: the committed revision does not change.
    poke(&user.join("user.rbin"));
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(user.join("top.v"))
        .unwrap();
    std::io::Write::write_all(&mut f, b"// edited after the import\n").unwrap();
    let rev = ws.revision(c.rev).unwrap();
    rev.verify_table("t").unwrap();
    rev.verify_table("rtl").unwrap();
    let mapped = rev.open_rbin("t", "ADV", 1..=1).unwrap();
    assert_eq!(mapped.section::<u32>(1).unwrap()[1], 1);
}

// WS-REQ-0077 (review R2-C2): hard links to files of the base revision stay allowed, so a new
// snapshot can share its unchanged files with the base; a file of another revision cannot be
// linked.
#[cfg(unix)]
#[test]
fn staging_files_may_link_files_of_the_base() {
    let ws = new_workspace("links-base");
    let mut txn = ws.begin(base(&ws, "rev0"), &inv("tool put")).unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir(&rtl).unwrap();
    fs::write(rtl.join("a.v"), "module a; endmodule\n").unwrap();
    fs::write(rtl.join("b.v"), "module b; endmodule\n").unwrap();
    txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    txn.add_rbin("old", "old.rbin", &table("ADV", 2), Upstream::new())
        .unwrap();
    let c1 = txn.commit(JsonMap::new()).unwrap();

    // rev2: a new snapshot that keeps a.v (a link to the base) and changes b.v.
    let mut txn = ws.begin(base(&ws, "rev1"), &inv("tool put")).unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir(&rtl).unwrap();
    fs::hard_link(c1.dir.join("rtl/a.v"), rtl.join("a.v")).unwrap();
    fs::write(rtl.join("b.v"), "module b(input x); endmodule\n").unwrap();
    txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    let copy = txn.staging_path("copy.rbin").unwrap();
    fs::hard_link(c1.dir.join("old.rbin"), &copy).unwrap();
    txn.add_file("copy", "copy.rbin", Upstream::new()).unwrap();
    txn.drop_table("old", "replaced by copy").unwrap();
    let c2 = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(
        inode(&c2.dir.join("rtl/a.v")),
        inode(&c1.dir.join("rtl/a.v"))
    );
    assert_ne!(
        inode(&c2.dir.join("rtl/b.v")),
        inode(&c1.dir.join("rtl/b.v"))
    );
    ws.revision(c2.rev).unwrap().verify_table("rtl").unwrap();

    // rev3 on rev2: rev1's rtl/b.v is in no table of the base any more.
    let mut txn = ws.begin(base(&ws, "rev2"), &inv("tool put")).unwrap();
    let linked = txn.staging_path("b.v").unwrap();
    fs::hard_link(c1.dir.join("rtl/b.v"), &linked).unwrap();
    let err = txn.add_file("b", "b.v", Upstream::new()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal, "{err}");
    txn.abort().unwrap();
}

// WS-REQ-0052, WS-REQ-0053 (review R2-C6): a fork is one state of the workspace, copied under the
// shared lock: HEAD and the revisions below next_rev, without staging or orphan directories; files
// of revisions and of the cache are hard links, the history and ui_state.json are copies. The
// fork goes its own way: its commits do not reach the source.
#[test]
fn fork_is_a_consistent_copy() {
    let ws = new_workspace("fork-source");
    let cached = ws
        .cache()
        .put_library("cells", b"library(cells) {}", "LIBERTY", 1..=1, |b| {
            let mut w = RbinWriter::new("LIBERTY", 1).unwrap();
            w.section_bytes(1, 1, b.to_vec()).unwrap();
            Ok::<_, String>(w.to_bytes())
        })
        .unwrap();
    let mut txn = ws
        .begin(base(&ws, "rev0"), &inv("rusteda import --lib cells.lib"))
        .unwrap();
    txn.add_library(cached.entry("default", "cells.lib"))
        .unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir(&rtl).unwrap();
    fs::write(rtl.join("top.v"), "module top; endmodule\n").unwrap();
    txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    txn.add_rbin("t", "t.rbin", &table("ADV", 3), Upstream::new())
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();
    ws.write_ui_state(&UiState::new(Rev(1), "sel1", vec!["inst1".into()], vec![]))
        .unwrap();
    // Leftovers: a transaction in progress and the directory of an interrupted commit.
    let pending = ws.begin(base(&ws, "rev1"), &inv("tool pending")).unwrap();
    fs::create_dir(ws.root().join("revisions/rev0007")).unwrap();
    fs::write(ws.root().join("revisions/rev0007/meta.json"), b"{}").unwrap();

    let dst = temp_dir("fork-copy").join("ws");
    let forked = ws.fork(&dst, &inv("rusteda fork")).unwrap();
    assert!(forked.warnings.is_empty(), "{:?}", forked.warnings);
    assert_eq!(forked.head, Rev(1));
    let fork = forked.workspace;
    assert_eq!(fork.root(), dst);
    assert_eq!(fork.manifest().unwrap(), ws.manifest().unwrap());
    assert_eq!(entries(&dst.join("revisions")), ["rev0000", "rev0001"]);
    assert_eq!(
        entries(&dst.join("cache")),
        entries(&ws.root().join("cache"))
    );
    assert_eq!(fork.ui_state().unwrap(), ws.ui_state().unwrap());
    let source_history = ws.history().unwrap();
    let fork_history = fork.history().unwrap();
    assert_eq!(fork_history[..source_history.len()], source_history[..]);
    assert_eq!(fork_history.len(), source_history.len() + 1);
    let line = fork_history.last().unwrap();
    assert_eq!(line, &forked.history);
    assert!(line.ok);
    assert_eq!(
        (
            line.command.as_str(),
            line.base_rev,
            line.new_rev,
            line.head_after
        ),
        ("rusteda fork", None, None, Rev(1))
    );
    #[cfg(unix)]
    {
        let source = ws.revision_dir(Rev(1));
        let copied = fork.revision_dir(Rev(1));
        assert_eq!(inode(&source.join("t.rbin")), inode(&copied.join("t.rbin")));
        assert_eq!(
            inode(&source.join("rtl/top.v")),
            inode(&copied.join("rtl/top.v"))
        );
        assert_eq!(inode(&cached.rlib_path), inode(&dst.join(&cached.rlib)));
    }

    // The fork works on its own.
    let head = fork.revision(fork.resolve_query("HEAD").unwrap()).unwrap();
    for name in ["t", "rtl"] {
        head.verify_table(name).unwrap();
    }
    head.open_library(&head.meta().libraries[0], "LIBERTY", 1..=1)
        .unwrap();
    let c = commit_bytes(&fork, "b", b"b");
    assert_eq!(c.rev, Rev(2));
    assert_eq!(ws.head().unwrap(), Rev(1), "the source does not move");
    assert_eq!(ws.history().unwrap(), source_history);
    pending.abort().unwrap();

    // Where a fork cannot go.
    let err = ws.fork(ws.root(), &inv("rusteda fork")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::WorkspaceExists);
    let err = ws.fork(&dst, &inv("rusteda fork")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::WorkspaceExists);
    let busy = temp_dir("fork-busy");
    fs::write(busy.join("notes.txt"), b"mine").unwrap();
    let err = ws.fork(&busy, &inv("rusteda fork")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InputRejected);
    assert_eq!(entries(&busy), ["notes.txt"]);
    let empty = temp_dir("fork-empty");
    ws.fork(&empty, &inv("rusteda fork")).unwrap();
    assert_eq!(Workspace::open(&empty).unwrap().head().unwrap(), Rev(1));
}

// WS-REQ-0052 (review R2-C6): forks made while another thread commits always hold the
// revision of their HEAD, so a query and a transaction on the fork's HEAD work.
#[test]
fn forks_made_during_commits() {
    let ws = new_workspace("fork-race");
    let committer = {
        let ws = Workspace::open(ws.root()).unwrap();
        thread::spawn(move || {
            for i in 0..40u32 {
                commit_bytes(&ws, "a", &i.to_le_bytes());
            }
        })
    };
    let forks = temp_dir("fork-race-copies");
    for i in 0..40 {
        let fork = ws
            .fork(forks.join(format!("f{i}")), &inv("rusteda fork"))
            .unwrap()
            .workspace;
        let head = fork.head().unwrap();
        fork.revision(fork.resolve_query("HEAD").unwrap())
            .unwrap_or_else(|e| panic!("fork {i}: HEAD {head}: {e}"));
        fork.begin(
            fork.resolve_base(&head.to_string()).unwrap(),
            &inv("tool next"),
        )
        .unwrap_or_else(|e| panic!("fork {i}: begin on {head}: {e}"))
        .abort()
        .unwrap();
    }
    committer.join().unwrap();
}

// WS-REQ-0045 (review CR2-1; contract §3 CA-STATUS): summary and status hold keys only for
// tables of the revision. A meta.json that breaks the rule, such as one with the flat status
// of contract draft 4's example, is CORRUPT_TABLE with a reason that names the rule; a
// carried table keeps the values of the base, and a missing table means an unchecked property.
#[test]
fn status_keys_are_tables_of_the_revision() {
    let ws = new_workspace("status-keys");
    let mut txn = ws.begin(base(&ws, "rev0"), &inv("rustsys check")).unwrap();
    txn.add_bytes("check", "check.json", b"{}", Upstream::new())
        .unwrap();
    txn.set_status("check", "equivalence", "equivalent")
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();
    let meta = ws.revision_dir(Rev(1)).join("meta.json");
    let text = fs::read_to_string(&meta).unwrap();
    let per_table =
        "\"status\": {\n    \"check\": {\n      \"equivalence\": \"equivalent\"\n    }\n  }";
    assert!(text.contains(per_table), "{text}");
    for (status, key) in [
        // The status-honesty example of contract draft 4, flat.
        (
            "\"status\": {\"equivalence\": \"unchecked\", \"timing\": \"not_analyzed\"}",
            "equivalence",
        ),
        (
            "\"status\": {\"equivalence\": {\"value\": \"unchecked\"}, \"check\": {}}",
            "equivalence",
        ),
        ("\"status\": {\"sta\": {\"timing\": \"met\"}}", "sta"),
    ] {
        fs::write(&meta, text.replace(per_table, status)).unwrap();
        let err = ws.revision(Rev(1)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable, "{status}");
        let message = err.to_string();
        assert!(message.contains(&format!("the key {key}")), "{message}");
        assert!(message.contains("CA-STATUS"), "{message}");
    }
    fs::write(&meta, &text).unwrap();
    // A carried table keeps the values of the base; they are not set again.
    let mut txn = ws.begin(base(&ws, "rev1"), &inv("ruststa run")).unwrap();
    let err = txn
        .set_status("check", "equivalence", "unchecked")
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal);
    assert!(err.to_string().contains("CA-STATUS"), "{err}");
    txn.add_bytes("sta", "sta.json", b"{}", Upstream::new())
        .unwrap();
    txn.set_status("sta", "timing", "met").unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(
        c.meta.status,
        per_table_values(&[
            ("check", "equivalence", json!("equivalent")),
            ("sta", "timing", json!("met")),
        ])
    );
    // Without the check table there is no equivalence status: it is unchecked.
    let mut txn = ws.begin(base(&ws, "rev2"), &inv("tool drop")).unwrap();
    txn.drop_table("check", "regenerate").unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    assert!(!c.meta.status.contains_key("check"));
    assert!(c.meta.status.contains_key("sta"));
}

/// [`per_table`] for the CA-STATUS test.
fn per_table_values(values: &[(&str, &str, Value)]) -> TableValues {
    per_table(values)
}

// WS-REQ-0026 (review CR2-2): a directory table (an rtl/ snapshot) is read through checked
// accessors: its files with their hashes, verified against meta.json, and one file at a time,
// without following symbolic links; read_table on it is INTERNAL, not a false corruption.
#[test]
fn directory_tables_have_checked_accessors() {
    let ws = new_workspace("dir-accessors");
    let mut txn = ws
        .begin(base(&ws, "rev0"), &inv("rusteda import --rtl top.v"))
        .unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir_all(rtl.join("inc")).unwrap();
    let files: [(&str, &[u8]); 3] = [
        ("inc/defs.vh", b"`define W 8\n"),
        ("rtl.json", br#"{"top":"top"}"#),
        ("top.v", b"module top; endmodule\n"),
    ];
    for (rel, bytes) in files {
        fs::write(rtl.join(rel), bytes).unwrap();
    }
    txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    txn.add_bytes("t", "t.json", b"{}", Upstream::new())
        .unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    let rev = ws.revision(c.rev).unwrap();

    let err = rev.read_table("rtl").unwrap_err();
    assert_eq!((err.code(), err.exit_code()), (ErrorCode::Internal, 2));
    assert!(err.to_string().contains("table_files"), "{err}");
    let listed = rev.table_files("rtl").unwrap();
    let expected: Vec<(String, Hash64)> = files
        .iter()
        .map(|(rel, bytes)| (rel.to_string(), Hash64::of(bytes)))
        .collect();
    assert_eq!(listed, expected);
    for (rel, bytes) in files {
        assert_eq!(rev.read_table_file("rtl", rel).unwrap(), bytes, "{rel}");
    }
    for bad in [
        "",
        "/etc/hostname",
        "../meta.json",
        "inc/../top.v",
        "./top.v",
        "inc//defs.vh",
    ] {
        let err = rev.read_table_file("rtl", bad).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal, "{bad}: {err}");
    }
    for absent in ["missing.v", "top.v/x", "inc/missing.vh"] {
        let err = rev.read_table_file("rtl", absent).unwrap_err();
        assert_eq!(err.code(), ErrorCode::IoError, "{absent}: {err}");
    }
    for err in [
        rev.table_files("t").unwrap_err(),
        rev.read_table_file("t", "x").unwrap_err(),
    ] {
        assert_eq!(err.code(), ErrorCode::Internal, "{err}");
    }
    assert_eq!(
        rev.table_files("nope").unwrap_err().code(),
        ErrorCode::MissingTable
    );

    // Tampering: a file added, or a directory on the way replaced by a link.
    fs::write(c.dir.join("rtl/extra.v"), "x").unwrap();
    let err = rev.table_files("rtl").unwrap_err();
    assert!(
        matches!(
            err,
            Error::Corrupt {
                what: Corruption::TableHash { .. },
                ..
            }
        ),
        "{err}"
    );
    fs::remove_file(c.dir.join("rtl/extra.v")).unwrap();
    #[cfg(unix)]
    {
        let elsewhere = temp_dir("dir-accessors-elsewhere");
        fs::write(elsewhere.join("defs.vh"), b"`define W 8\n").unwrap();
        fs::remove_dir_all(c.dir.join("rtl/inc")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, c.dir.join("rtl/inc")).unwrap();
        let err = rev.read_table_file("rtl", "inc/defs.vh").unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable, "{err}");
        let err = rev.table_files("rtl").unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable, "{err}");
    }
}
