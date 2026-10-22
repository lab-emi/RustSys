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

// WS-REQ-0001
#[test]
fn init_creates_the_layout() {
    let ws = new_workspace("init");
    let root = ws.root();
    assert_eq!(
        entries(root),
        [
            "cache",
            "history.jsonl",
            "manifest.json",
            "revisions",
            "workspace.lock"
        ]
    );
    assert_eq!(
        fs::read_to_string(root.join("manifest.json")).unwrap(),
        "{\n  \"schema\": \"rusteda.manifest.v1\",\n  \"head\": \"rev0\",\n  \"next_rev\": 1,\n  \
         \"corners\": []\n}\n"
    );
    assert_eq!(fs::metadata(root.join("workspace.lock")).unwrap().len(), 0);
    assert!(entries(&root.join("cache")).is_empty());
    assert_eq!(entries(&root.join("revisions")), ["rev0000"]);
    assert_eq!(entries(&root.join("revisions/rev0000")), ["meta.json"]);
    let rev0 = ws.revision(Rev(0)).unwrap();
    let meta = rev0.meta();
    assert_eq!((meta.rev, meta.parent), (Rev(0), None));
    assert!(meta.tables.is_empty() && meta.libraries.is_empty() && meta.dropped.is_empty());
    assert_eq!(meta.command, "rusteda init");
    assert_eq!(ws.head().unwrap(), Rev(0));
    assert_eq!(ws.revisions().unwrap(), [Rev(0)]);
    let history = ws.history().unwrap();
    assert_eq!(history.len(), 1);
    let line = &history[0];
    assert!(line.ok);
    assert_eq!(
        (line.exit, line.base_rev, line.new_rev, line.head_after),
        (0, None, Some(Rev(0)), Rev(0))
    );
    assert_eq!(line.argv, ["rusteda", "init"]);
    assert_eq!(line.error, None);
}

// WS-REQ-0002
#[test]
fn init_refuses_an_existing_workspace_and_sets_leftovers_aside() {
    let ws = new_workspace("init-twice");
    let err = Workspace::init(ws.root(), &inv("rusteda init -w ws")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::WorkspaceExists);
    assert_eq!(err.exit_code(), 1);
    // WS-REQ-0069 (review CW-7): the rejected init is logged in the existing workspace.
    let history = ws.history().unwrap();
    assert_eq!(history.len(), 2);
    let line = &history[1];
    assert!(!line.ok);
    assert_eq!(
        (line.exit, line.base_rev, line.new_rev, line.head_after),
        (1, None, None, Rev(0))
    );
    assert_eq!(line.command, "rusteda init -w ws");
    assert_eq!(
        line.error.as_ref().unwrap().code,
        ErrorCode::WorkspaceExists
    );

    // A directory whose manifest.json is not a workspace manifest takes no line, and nothing
    // is created in it.
    let foreign = temp_dir("init-foreign");
    fs::write(
        foreign.join("manifest.json"),
        "{\"name\": \"some other tool\"}",
    )
    .unwrap();
    let err = Workspace::init(&foreign, &inv("rusteda init")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::WorkspaceExists);
    assert_eq!(entries(&foreign), ["manifest.json"]);

    // An interrupted init left a revision directory but no manifest; other files are kept.
    let root = temp_dir("init-leftover").join("ws");
    fs::create_dir_all(root.join("revisions/rev0000")).unwrap();
    fs::write(root.join("revisions/rev0000/meta.json"), "partial").unwrap();
    fs::write(root.join("notes.txt"), "user file").unwrap();
    let ws = Workspace::init(&root, &inv("rusteda init"))
        .unwrap()
        .workspace;
    let names = entries(&root.join("revisions"));
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[0].starts_with(".orphan-rev0000-"), "{names:?}");
    assert_eq!(names[1], "rev0000");
    assert_eq!(ws.revision(Rev(0)).unwrap().meta().rev, Rev(0));
    assert_eq!(
        fs::read_to_string(root.join("notes.txt")).unwrap(),
        "user file"
    );
}

// WS-REQ-0003, WS-REQ-0004
#[test]
fn open_errors() {
    let dir = temp_dir("open");
    assert_eq!(
        Workspace::open(dir.join("absent")).unwrap_err().code(),
        ErrorCode::NotAWorkspace
    );
    let err = Workspace::open(&dir).unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotAWorkspace);
    assert!(err.to_string().contains("has no manifest.json"), "{err}");

    let ws = new_workspace("open-newer");
    let manifest = ws.root().join("manifest.json");
    fs::write(
        &manifest,
        r#"{"schema":"rusteda.manifest.v2","head":"rev0","next_rev":1,"corners":[],"more":1}"#,
    )
    .unwrap();
    let err = Workspace::open(ws.root()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::SchemaTooNew);
    assert!(err.to_string().contains("rusteda.manifest.v2"), "{err}");
    // A newer minor version is read.
    fs::write(
        &manifest,
        r#"{"schema":"rusteda.manifest.v1.4","head":"rev0","next_rev":1,"corners":[],"more":1}"#,
    )
    .unwrap();
    Workspace::open(ws.root()).unwrap();
    fs::write(&manifest, "{").unwrap();
    assert_eq!(
        Workspace::open(ws.root()).unwrap_err().code(),
        ErrorCode::CorruptTable
    );
    fs::write(
        &manifest,
        r#"{"schema":"rusteda.manifest.v1","head":"rev3","next_rev":1,"corners":[]}"#,
    )
    .unwrap();
    assert_eq!(
        Workspace::open(ws.root()).unwrap_err().code(),
        ErrorCode::CorruptTable
    );
}

// WS-REQ-0021, WS-REQ-0023
#[test]
fn resolution() {
    let ws = new_workspace("resolve");
    assert_eq!(ws.resolve_query("HEAD").unwrap(), Rev(0));
    assert_eq!(ws.resolve_query("rev0").unwrap(), Rev(0));
    assert_eq!(ws.resolve_query("rev0000").unwrap(), Rev(0));
    for bad in ["rev1", "head", "r1", "", "rev-1", "HEAD~1"] {
        assert_eq!(
            ws.resolve_query(bad).unwrap_err().code(),
            ErrorCode::UnknownRevision,
            "{bad}"
        );
    }
    let err = ws.resolve_base("HEAD").unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownRevision);
    assert!(
        err.to_string()
            .contains("HEAD is accepted only by --rev, --from and --to"),
        "{err}"
    );
    assert_eq!(ws.resolve_base("rev0").unwrap().rev(), Rev(0));
    let err = ws.resolve_base("rev1").unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownRevision);
    assert_eq!(
        err.to_string(),
        "unknown revision 'rev1': no such revision (HEAD is rev0)"
    );
    assert_eq!(
        ws.revision(Rev(1)).unwrap_err().code(),
        ErrorCode::UnknownRevision
    );

    let meta = ws.revision_dir(Rev(0)).join("meta.json");
    let text = fs::read_to_string(&meta).unwrap();
    fs::write(
        &meta,
        text.replace("rusteda.revision.v1", "rusteda.revision.v3"),
    )
    .unwrap();
    assert_eq!(
        ws.revision(Rev(0)).unwrap_err().code(),
        ErrorCode::SchemaTooNew
    );
}

#[test]
fn missing_tables() {
    let ws = new_workspace("missing");
    let rev0 = ws.revision(Rev(0)).unwrap();
    let err = rev0.table("sta").unwrap_err();
    assert_eq!(err.code(), ErrorCode::MissingTable);
    assert_eq!(err.to_string(), "rev0 has no table 'sta'");
    assert_eq!(
        rev0.open_rbin("sta", "STA", 1..=1).unwrap_err().code(),
        ErrorCode::MissingTable
    );
    assert_eq!(rev0.table_hash("libraries"), Some(Hash64::of(b"")));
    assert_eq!(rev0.table_hash("sta"), None);
}

// WS-REQ-0061, WS-REQ-0064 to WS-REQ-0067, WS-REQ-0074: five revisions; the last one drops a chain of three
// dependent tables (rtl <- netlist <- sta) over two rounds of the fixpoint.
#[test]
fn commit_chain_of_five_revisions() {
    let ws = new_workspace("chain");
    let revisions = ws.root().join("revisions");

    // rev1: an rtl snapshot (a directory table) and a library.
    let mut txn = ws
        .begin(
            base(&ws, "rev0"),
            &inv("rusteda import --rtl top.v --lib a.lib"),
        )
        .unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir_all(rtl.join("inc")).unwrap();
    fs::write(rtl.join("top.v"), "module top; endmodule\n").unwrap();
    fs::write(rtl.join("inc/defs.vh"), "`define W 8\n").unwrap();
    fs::write(rtl.join("rtl.json"), r#"{"top":"top"}"#).unwrap();
    let rtl_hash = txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    txn.add_source("rtl", "src/top.v", Hash64::of(b"module top; endmodule\n"))
        .unwrap();
    txn.add_library(lib("cells_a", "default", 1)).unwrap();
    txn.set_summary("rtl", "rtl_files", 2).unwrap();
    txn.set_message("first import");
    let c1 = txn.commit(JsonMap::new()).unwrap();
    assert_eq!((c1.rev, c1.base), (Rev(1), Rev(0)));
    assert!(c1.carried.is_empty() && c1.meta.dropped.is_empty());
    assert_eq!(c1.meta.tables["rtl"].hash, rtl_hash);
    assert_eq!(hash_dir(&c1.dir.join("rtl")).unwrap(), rtl_hash);
    assert_eq!(c1.meta.message.as_deref(), Some("first import"));

    // rev2: synthesis from rtl and the libraries.
    let mut txn = ws
        .begin(base(&ws, "rev1"), &inv("rustsys synth --base-rev rev1"))
        .unwrap();
    let upstream = txn.upstream(&["rtl", "libraries"]).unwrap();
    assert_eq!(upstream["rtl"], rtl_hash);
    assert_eq!(
        upstream["libraries"],
        LibraryEntry::list_hash(&[lib("cells_a", "default", 1)])
    );
    txn.add_rbin(
        "netlist",
        "netlist.rbin",
        &table("NETLIST", 2),
        upstream.clone(),
    )
    .unwrap();
    txn.add_bytes("synth", "synth.json", br#"{"cells":100}"#, upstream)
        .unwrap();
    txn.set_summary("netlist", "cells", 100).unwrap();
    txn.set_summary("netlist", "area_um2", 50.5).unwrap();
    txn.set_status("synth", "equivalence", "unchecked").unwrap();
    let c2 = txn
        .commit(JsonMap::from([("cells".into(), json!(100))]))
        .unwrap();
    assert_eq!(c2.carried, ["rtl"]);
    assert_eq!(
        c2.meta.sources.len(),
        1,
        "the rtl source is carried with rtl"
    );

    // rev3: constraints, written by the tool into the staging directory; no upstream.
    let mut txn = ws
        .begin(base(&ws, "rev2"), &inv("rusteda import --sdc top.sdc"))
        .unwrap();
    table("CONSTR", 3)
        .write_to(txn.staging_path("constraints.rbin").unwrap())
        .unwrap();
    let constraints_hash = txn
        .add_file("constraints", "constraints.rbin", Upstream::new())
        .unwrap();
    txn.add_source(
        "constraints",
        "top.sdc",
        Hash64::of(b"create_clock -period 2 clk"),
    )
    .unwrap();
    let c3 = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(c3.carried, ["netlist", "rtl", "synth"]);
    assert_eq!(c3.meta.libraries, c2.meta.libraries);
    // WS-REQ-0045: the summary and status of carried tables are carried with them.
    assert_eq!(c3.meta.summary, c2.meta.summary);
    assert_eq!(c3.meta.status, c2.meta.status);
    assert!(c3.history.metric_deltas.is_empty());

    // rev4: timing from netlist, constraints and libraries.
    let mut txn = ws
        .begin(base(&ws, "rev3"), &inv("ruststa run --base-rev rev3"))
        .unwrap();
    let upstream = txn
        .upstream(&["netlist", "constraints", "libraries"])
        .unwrap();
    txn.add_rbin("sta", "sta.rbin", &table("STA", 4), upstream)
        .unwrap();
    txn.set_summary("sta", "wns_ns", -0.5).unwrap();
    txn.set_status("sta", "timing", "violated").unwrap();
    let c4 = txn
        .commit(JsonMap::from([("wns_ns".into(), json!(-0.5))]))
        .unwrap();
    assert_eq!(c4.carried, ["constraints", "netlist", "rtl", "synth"]);
    assert!(
        c4.history.metric_deltas.is_empty(),
        "the first timing table has no previous version"
    );
    assert_eq!(
        c4.meta.summary,
        per_table(&[
            ("netlist", "area_um2", json!(50.5)),
            ("netlist", "cells", json!(100)),
            ("rtl", "rtl_files", json!(2)),
            ("sta", "wns_ns", json!(-0.5)),
        ])
    );
    assert_eq!(
        c4.meta.status,
        per_table(&[
            ("sta", "timing", json!("violated")),
            ("synth", "equivalence", json!("unchecked")),
        ])
    );

    // rev5: new rtl. netlist and synth came from the old rtl (round 1), sta from the dropped
    // netlist (round 2); constraints do not depend on rtl and stay.
    let mut txn = ws
        .begin(base(&ws, "rev4"), &inv("rusteda import --rtl top.v"))
        .unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    fs::create_dir_all(&rtl).unwrap();
    fs::write(rtl.join("top.v"), "module top(input a); endmodule\n").unwrap();
    let new_rtl = txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    assert_ne!(new_rtl, rtl_hash);
    assert_eq!(
        txn.table_hash("netlist"),
        None,
        "invalidated as the transaction stands"
    );
    assert_eq!(txn.table_hash("constraints"), Some(constraints_hash));
    // WS-REQ-0075: a table dropped by this transaction is missing, with the reason.
    let err = txn.upstream(&["sta"]).unwrap_err();
    assert_eq!(err.code(), ErrorCode::MissingTable);
    assert_eq!(
        err.to_string(),
        "table 'sta' does not carry into the new revision: upstream netlist is not in this \
         revision"
    );
    let err = txn.upstream(&["placement"]).unwrap_err();
    assert_eq!(err.to_string(), "rev4 has no table 'placement'");
    let c5 = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(c5.carried, ["constraints"]);
    assert_eq!(
        c5.meta.dropped,
        [
            DroppedEntry {
                table: "netlist".into(),
                reason: "upstream rtl changed".into()
            },
            DroppedEntry {
                table: "synth".into(),
                reason: "upstream rtl changed".into()
            },
            DroppedEntry {
                table: "sta".into(),
                reason: "upstream netlist is not in this revision".into()
            },
        ]
    );
    assert_eq!(
        c5.meta.tables.keys().collect::<Vec<_>>(),
        ["constraints", "rtl"]
    );
    // The values of the dropped tables are gone with them; the new rtl has none yet.
    assert!(c5.meta.summary.is_empty(), "{:?}", c5.meta.summary);
    assert!(c5.meta.status.is_empty(), "{:?}", c5.meta.status);
    assert_eq!(
        c5.meta.sources,
        [SourceEntry {
            table: "constraints".into(),
            path: "top.sdc".into(),
            hash: Hash64::of(b"create_clock -period 2 clk"),
        }]
    );
    assert_eq!(entries(&c5.dir), ["constraints.rbin", "meta.json", "rtl"]);

    // The chain.
    let manifest = ws.manifest().unwrap();
    assert_eq!((manifest.head, manifest.next_rev), (Rev(5), 6));
    assert_eq!(manifest.corners, ["default"]);
    for (rev, parent) in [(1, 0), (2, 1), (3, 2), (4, 3), (5, 4)] {
        assert_eq!(
            ws.revision(Rev(rev)).unwrap().meta().parent,
            Some(Rev(parent))
        );
    }
    assert_eq!(
        entries(&revisions),
        [
            "rev0000", "rev0001", "rev0002", "rev0003", "rev0004", "rev0005"
        ],
        "no staging directory is left"
    );
    assert_eq!(
        ws.revisions().unwrap(),
        (0..=5).map(Rev).collect::<Vec<_>>()
    );

    // WS-REQ-0065: carried files are hard links of the first copy.
    #[cfg(unix)]
    {
        let at = |rev: u32, path: &str| inode(&revisions.join(Rev(rev).dir_name()).join(path));
        assert_eq!(at(2, "netlist.rbin"), at(4, "netlist.rbin"));
        assert_eq!(at(1, "rtl/inc/defs.vh"), at(4, "rtl/inc/defs.vh"));
        assert_eq!(at(3, "constraints.rbin"), at(5, "constraints.rbin"));
        assert_ne!(at(4, "rtl/top.v"), at(5, "rtl/top.v"));
    }

    // Every table verifies against meta.json; tables open by name, zero-copy.
    for rev in 1..=5 {
        let r = ws.revision(Rev(rev)).unwrap();
        for name in r.meta().tables.keys() {
            r.verify_table(name).unwrap();
        }
    }
    let sta = ws
        .revision(Rev(4))
        .unwrap()
        .open_rbin("sta", "STA", 1..=1)
        .unwrap();
    assert_eq!(sta.section::<u32>(1).unwrap()[3], 12);
    let synth = ws.revision(Rev(3)).unwrap().read_table("synth").unwrap();
    assert_eq!(synth, br#"{"cells":100}"#);

    // History: init and five commits.
    let history = ws.history().unwrap();
    assert_eq!(history.len(), 6);
    let made: Vec<Option<Rev>> = history.iter().map(|h| h.new_rev).collect();
    assert_eq!(made, (0..=5).map(|r| Some(Rev(r))).collect::<Vec<_>>());
    assert!(
        history
            .iter()
            .all(|h| h.ok && h.exit == 0 && h.error.is_none())
    );
    assert_eq!(
        history[2].metrics,
        JsonMap::from([("cells".into(), json!(100))])
    );
    assert!(
        history[2].metric_deltas.is_empty(),
        "netlist and synth are new in rev2"
    );
    assert_eq!(history[2].base_rev, Some(Rev(1)));
    assert_eq!(history[2].command, "rustsys synth --base-rev rev1");
}

// WS-REQ-0013, WS-REQ-0040, WS-REQ-0073: libraries are a pseudo-table; corners follow HEAD.
#[test]
fn libraries_drive_invalidation_and_corners() {
    let ws = new_workspace("libraries");
    let mut txn = ws
        .begin(base(&ws, "rev0"), &inv("rusteda import --lib a.lib"))
        .unwrap();
    txn.add_library(lib("a", "default", 1)).unwrap();
    let upstream = txn.upstream(&["libraries"]).unwrap();
    txn.add_bytes("synth", "synth.json", b"{}", upstream)
        .unwrap();
    txn.add_bytes("notes", "notes.txt", b"n", Upstream::new())
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();

    let mut txn = ws
        .begin(
            base(&ws, "rev1"),
            &inv("rusteda import --lib b.lib --corner ss"),
        )
        .unwrap();
    txn.add_library(lib("b", "ss", 2)).unwrap();
    let c2 = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(c2.carried, ["notes"]);
    assert_eq!(
        c2.meta.dropped,
        [DroppedEntry {
            table: "synth".into(),
            reason: "upstream libraries changed".into()
        }]
    );
    assert_eq!(ws.manifest().unwrap().corners, ["default", "ss"]);

    let mut txn = ws
        .begin(base(&ws, "rev2"), &inv("rusteda import --lib a.lib"))
        .unwrap();
    txn.add_library(lib("a", "default", 3)).unwrap();
    assert_eq!(
        txn.libraries(),
        [lib("a", "default", 3), lib("b", "ss", 2)],
        "replaced in place"
    );
    let err = txn
        .set_libraries(vec![lib("a", "default", 1), lib("a", "default", 2)])
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal);
    let err = txn.add_library(lib("c", "bad corner", 1)).unwrap_err();
    assert_eq!(err.code(), ErrorCode::InputRejected);
    txn.set_libraries(vec![lib("b", "ss", 2)]).unwrap();
    txn.commit(JsonMap::new()).unwrap();
    assert_eq!(ws.manifest().unwrap().corners, ["ss"]);

    let to = ws.resolve_query("rev1").unwrap();
    ws.revert(
        base(&ws, "rev3"),
        to,
        &inv("rusteda revert --base-rev rev3 --to rev1"),
    )
    .unwrap();
    assert_eq!(ws.manifest().unwrap().corners, ["default"]);
}

// WS-REQ-0071: a new table whose upstream does not hold fails the commit, which is logged.
#[test]
fn new_tables_must_match_their_upstream() {
    let ws = new_workspace("upstream-check");
    let a = commit_bytes(&ws, "a", b"a1").meta.tables["a"].hash;
    let revisions = ws.root().join("revisions");

    let cases: [(&str, Upstream); 3] = [
        (
            "stale hash",
            Upstream::from([("a".to_string(), Hash64(12345))]),
        ),
        ("no such table", Upstream::from([("zz".to_string(), a)])),
        ("dropped table", Upstream::from([("a".to_string(), a)])),
    ];
    for (case, upstream) in cases {
        let mut txn = ws.begin(base(&ws, "rev1"), &inv("tool derive")).unwrap();
        if case == "dropped table" {
            txn.drop_table("a", "replaced").unwrap();
        }
        txn.add_bytes("b", "b.bin", b"b", upstream).unwrap();
        let staging = txn.staging_dir().to_path_buf();
        let err = txn.commit(JsonMap::new()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal, "{case}: {err}");
        assert!(!staging.exists(), "{case}");
        let last = ws.history().unwrap().pop().unwrap();
        assert!(!last.ok);
        assert_eq!(
            (last.exit, last.base_rev, last.head_after),
            (2, Some(Rev(1)), Rev(1))
        );
        assert_eq!(last.error.unwrap().code, ErrorCode::Internal);
    }
    assert_eq!(ws.head().unwrap(), Rev(1));
    assert_eq!(entries(&revisions), ["rev0000", "rev0001"]);
}

// WS-REQ-0072
#[test]
fn staging_holds_only_tables() {
    let ws = new_workspace("strays");
    let mut txn = ws.begin(base(&ws, "rev0"), &inv("tool put")).unwrap();
    fs::write(txn.staging_dir().join("scratch.tmp"), b"x").unwrap();
    txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    let err = txn.commit(JsonMap::new()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal);
    assert!(
        err.to_string().contains("scratch.tmp is not a table"),
        "{err}"
    );

    let mut txn = ws.begin(base(&ws, "rev0"), &inv("tool put")).unwrap();
    for (name, path) in [
        ("libraries", "x.bin"),
        ("a b", "x.bin"),
        ("a", "meta.json"),
        ("a", ".x"),
        ("a", "d/x"),
    ] {
        let err = txn
            .add_bytes(name, path, b"x", Upstream::new())
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal, "{name} {path}");
    }
    txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    assert!(
        txn.add_bytes("a", "other.bin", b"a", Upstream::new())
            .is_err(),
        "twice"
    );
    assert!(
        txn.add_bytes("b", "a.bin", b"b", Upstream::new()).is_err(),
        "same path"
    );
    assert!(txn.add_file("c", "absent.bin", Upstream::new()).is_err());
    assert!(
        txn.add_dir("c", "a.bin", Upstream::new()).is_err(),
        "not a directory"
    );
    let self_upstream = Upstream::from([("d".to_string(), Hash64(1))]);
    assert!(txn.add_bytes("d", "d.bin", b"d", self_upstream).is_err());
    txn.abort().unwrap();
}

// Explicit drops.
#[test]
fn explicit_drops() {
    let ws = new_workspace("drops");
    let mut txn = ws.begin(base(&ws, "rev0"), &inv("tool put")).unwrap();
    let a = txn.add_bytes("a", "a.bin", b"a", Upstream::new()).unwrap();
    txn.add_bytes("b", "b.bin", b"b", Upstream::from([("a".to_string(), a)]))
        .unwrap();
    txn.add_bytes("c", "c.bin", b"c", Upstream::new()).unwrap();
    txn.commit(JsonMap::new()).unwrap();

    let mut txn = ws
        .begin(base(&ws, "rev1"), &inv("rusteda import --netlist n.v"))
        .unwrap();
    assert_eq!(
        txn.drop_table("zz", "x").unwrap_err().code(),
        ErrorCode::MissingTable
    );
    txn.drop_table("a", "superseded by the imported netlist")
        .unwrap();
    assert!(txn.add_bytes("a", "a2.bin", b"a", Upstream::new()).is_err());
    txn.add_bytes("n", "n.bin", b"n", Upstream::new()).unwrap();
    assert!(txn.drop_table("n", "x").is_err());
    let c = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(c.carried, ["c"]);
    assert_eq!(
        c.meta.dropped,
        [
            DroppedEntry {
                table: "a".into(),
                reason: "superseded by the imported netlist".into()
            },
            DroppedEntry {
                table: "b".into(),
                reason: "upstream a is not in this revision".into()
            },
        ]
    );
}

#[test]
fn abort_reject_and_drop_remove_staging() {
    let ws = new_workspace("abort");
    let txn = ws.begin(base(&ws, "rev0"), &inv("tool put")).unwrap();
    let staging = txn.staging_dir().to_path_buf();
    assert!(staging.is_dir());
    assert!(
        staging
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(".staging-")
    );
    txn.abort().unwrap();
    assert!(!staging.exists());

    let txn = ws.begin(base(&ws, "rev0"), &inv("tool put")).unwrap();
    let staging = txn.staging_dir().to_path_buf();
    drop(txn);
    assert!(!staging.exists());

    let mut txn = ws
        .begin(base(&ws, "rev0"), &inv("rusteda import --netlist bad.v"))
        .unwrap();
    txn.add_bytes("netlist", "netlist.rbin", b"x", Upstream::new())
        .unwrap();
    let staging = txn.staging_dir().to_path_buf();
    let line = txn
        .reject(&Error::input_rejected("bad.v:3:1: x/z literal"))
        .unwrap();
    assert!(!staging.exists());
    assert!(!line.ok);
    assert_eq!(
        (line.exit, line.base_rev, line.new_rev, line.head_after),
        (1, Some(Rev(0)), None, Rev(0))
    );
    assert_eq!(line.error.as_ref().unwrap().code, ErrorCode::InputRejected);
    assert_eq!(ws.history().unwrap().last(), Some(&line));
    assert_eq!(entries(&ws.root().join("revisions")), ["rev0000"]);
}

// WS-REQ-0060, WS-REQ-0069 (review CR2-6): a stale base fails before any work, and begin logs
// the conflict itself, so the natural `ws.begin(base, &inv)?` loses no history line; so does any
// other failure of begin.
#[test]
fn conflict_before_any_work() {
    let ws = new_workspace("conflict-early");
    commit_bytes(&ws, "a", b"a");
    let revisions = ws.root().join("revisions");
    let before = entries(&revisions);
    let lines_before = ws.history().unwrap().len();
    let stale = base(&ws, "rev0");
    let command = inv("rustsys synth --base-rev rev0");
    let err = ws.begin(stale, &command).unwrap_err();
    assert!(
        matches!(
            err,
            Error::RevisionConflict {
                base: Rev(0),
                head: Rev(1)
            }
        ),
        "{err}"
    );
    assert_eq!(err.exit_code(), 4);
    assert_eq!(entries(&revisions), before, "no staging directory");
    let lines = ws.history().unwrap();
    assert_eq!(lines.len(), lines_before + 1, "begin logged the conflict");
    let line = lines.last().unwrap();
    assert!(!line.ok);
    assert_eq!(line.command, command.command);
    assert_eq!(
        (line.exit, line.base_rev, line.new_rev, line.head_after),
        (4, Some(Rev(0)), None, Rev(1))
    );
    assert_eq!(
        line.error.as_ref().unwrap().code,
        ErrorCode::RevisionConflict
    );

    // A base whose meta.json is damaged fails begin, and is logged the same way.
    let meta = ws.revision_dir(Rev(1)).join("meta.json");
    let text = fs::read_to_string(&meta).unwrap();
    fs::write(&meta, b"{").unwrap();
    let err = ws.begin(base(&ws, "rev1"), &command).unwrap_err();
    assert_eq!(err.code(), ErrorCode::CorruptTable);
    let lines = ws.history().unwrap();
    assert_eq!(lines.len(), lines_before + 2);
    let line = lines.last().unwrap();
    assert_eq!((line.exit, line.base_rev), (2, Some(Rev(1))));
    assert_eq!(line.error.as_ref().unwrap().code, ErrorCode::CorruptTable);
    fs::write(&meta, text).unwrap();
    assert_eq!(entries(&revisions), before, "no staging directory");
}

// WS-REQ-0050, WS-REQ-0062: two transactions on one base in two threads, each lock on its own
// open file description: exactly one commits, the other is a logged conflict.
#[test]
fn conflict_at_commit_between_threads() {
    let ws = new_workspace("conflict-threads");
    let base = base(&ws, "rev0");
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|i| {
            let ws = Workspace::open(ws.root()).unwrap();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut txn = ws.begin(base, &inv(&format!("tool{i} run"))).unwrap();
                txn.add_bytes(
                    "t",
                    "t.bin",
                    format!("thread {i}").as_bytes(),
                    Upstream::new(),
                )
                .unwrap();
                let staging = txn.staging_dir().to_path_buf();
                barrier.wait();
                (txn.commit(JsonMap::new()), staging)
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(r, _)| r.is_ok()).count(), 1);
    let (winner, _) = results.iter().find(|(r, _)| r.is_ok()).unwrap();
    let winner = winner.as_ref().unwrap();
    let (loser, loser_staging) = results.iter().find(|(r, _)| r.is_err()).unwrap();
    let err = loser.as_ref().unwrap_err();
    assert!(
        matches!(
            err,
            Error::RevisionConflict {
                base: Rev(0),
                head: Rev(1)
            }
        ),
        "{err}"
    );
    assert!(!loser_staging.exists());

    assert_eq!(winner.rev, Rev(1));
    assert_eq!(ws.head().unwrap(), Rev(1));
    assert_eq!(ws.manifest().unwrap().next_rev, 2);
    assert_eq!(
        entries(&ws.root().join("revisions")),
        ["rev0000", "rev0001"]
    );
    let history = ws.history().unwrap();
    assert_eq!(history.len(), 3);
    let ok: Vec<_> = history[1..].iter().filter(|h| h.ok).collect();
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].command, winner.meta.command);
    let conflict = history[1..].iter().find(|h| !h.ok).unwrap();
    assert_eq!(
        (
            conflict.exit,
            conflict.base_rev,
            conflict.new_rev,
            conflict.head_after
        ),
        (4, Some(Rev(0)), None, Rev(1))
    );
    assert_eq!(
        conflict.error.as_ref().unwrap().code,
        ErrorCode::RevisionConflict
    );
    assert_ne!(conflict.command, winner.meta.command);
}

// WS-REQ-0068: revert moves HEAD without a revision; numbering continues after it.
#[test]
fn revert_then_commit() {
    let ws = new_workspace("revert");
    for (i, corner) in [(1u64, "default"), (2, "ss"), (3, "ss")] {
        let head = ws.head().unwrap();
        let mut txn = ws
            .begin(base(&ws, &head.to_string()), &inv("tool put"))
            .unwrap();
        txn.add_bytes("a", "a.bin", format!("a{i}").as_bytes(), Upstream::new())
            .unwrap();
        txn.set_libraries(vec![lib("x", corner, i)]).unwrap();
        txn.set_summary("a", "cells", 10 * i).unwrap();
        txn.commit(JsonMap::new()).unwrap();
    }
    let a1 = ws.revision(Rev(1)).unwrap().table("a").unwrap().hash;
    let command = inv("rusteda revert --base-rev rev3 --to rev1");
    let to = ws.resolve_query("rev1").unwrap();
    let reverted = ws.revert(base(&ws, "rev3"), to, &command).unwrap();
    assert_eq!((reverted.previous, reverted.head), (Rev(3), Rev(1)));
    let manifest = ws.manifest().unwrap();
    assert_eq!((manifest.head, manifest.next_rev), (Rev(1), 4));
    assert_eq!(manifest.corners, ["default"]);
    let line = &reverted.history;
    assert!(line.ok);
    assert_eq!(
        (line.base_rev, line.new_rev, line.head_after),
        (Some(Rev(3)), None, Rev(1))
    );
    assert_eq!(line.metric_deltas, per_table(&[("a", "cells", json!(-20))]));
    assert!(reverted.warnings.is_empty());
    assert_eq!(ws.history().unwrap().last(), Some(line));
    assert_eq!(
        entries(&ws.root().join("revisions")).len(),
        4,
        "no new revision"
    );

    // A stale base and an unknown target are rejected, and logged.
    let err = ws
        .revert(BaseRev::parse("rev3").unwrap(), Rev(2), &command)
        .unwrap_err();
    assert!(matches!(
        err,
        Error::RevisionConflict {
            base: Rev(3),
            head: Rev(1)
        }
    ));
    let last = ws.history().unwrap().pop().unwrap();
    assert_eq!((last.ok, last.exit, last.head_after), (false, 4, Rev(1)));
    let err = ws.revert(base(&ws, "rev1"), Rev(9), &command).unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownRevision);
    let last = ws.history().unwrap().pop().unwrap();
    assert_eq!((last.ok, last.exit), (false, 1));
    assert_eq!(last.error.unwrap().code, ErrorCode::UnknownRevision);

    // A commit on the reverted HEAD: parent rev1, number 4, rev1's tables carried.
    let mut txn = ws.begin(base(&ws, "rev1"), &inv("tool add")).unwrap();
    txn.add_bytes("b", "b.bin", b"b", Upstream::new()).unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    assert_eq!((c.rev, c.meta.parent), (Rev(4), Some(Rev(1))));
    assert_eq!(c.meta.tables["a"].hash, a1);
    assert_eq!(ws.manifest().unwrap().next_rev, 5);
    assert_eq!(
        ws.revision(Rev(3)).unwrap().meta().rev,
        Rev(3),
        "later revisions stay readable"
    );

    // Revert forward to a later revision works too.
    ws.revert(
        base(&ws, "rev4"),
        Rev(3),
        &inv("rusteda revert --base-rev rev4 --to rev3"),
    )
    .unwrap();
    assert_eq!(ws.head().unwrap(), Rev(3));
    assert_eq!(ws.manifest().unwrap().corners, ["ss"]);
}

// WS-REQ-0063, WS-REQ-0066, WS-REQ-0070: leftovers of crashes in the windows the commit order
// leaves (staging written, revision renamed before the manifest update, history line torn).
#[test]
fn crash_leftovers() {
    let ws = new_workspace("crash");
    commit_bytes(&ws, "a", b"a");
    let revisions = ws.root().join("revisions");

    // A transaction that died before committing left its staging directory.
    let stale = revisions.join(".staging-999999-1");
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("sta.rbin"), b"partial").unwrap();
    // A commit that died after renaming its directory, before the manifest update, left
    // rev0002: complete, never published.
    let orphan = revisions.join("rev0002");
    fs::create_dir(&orphan).unwrap();
    let meta = fs::read_to_string(revisions.join("rev0001/meta.json")).unwrap();
    let meta = meta
        .replace("\"rev\": \"rev1\"", "\"rev\": \"rev2\"")
        .replace("\"parent\": \"rev0\"", "\"parent\": \"rev1\"");
    fs::write(orphan.join("meta.json"), meta).unwrap();
    fs::copy(revisions.join("rev0001/a.bin"), orphan.join("a.bin")).unwrap();

    assert_eq!(
        ws.resolve_query("rev2").unwrap_err().code(),
        ErrorCode::UnknownRevision
    );
    assert_eq!(ws.revisions().unwrap(), [Rev(0), Rev(1)]);
    assert_eq!(ws.head().unwrap(), Rev(1));

    let c = commit_bytes(&ws, "b", b"b");
    assert_eq!(c.rev, Rev(3), "allocated past the orphan");
    assert_eq!(c.meta.parent, Some(Rev(1)));
    let manifest = ws.manifest().unwrap();
    assert_eq!((manifest.head, manifest.next_rev), (Rev(3), 4));
    let names = entries(&revisions);
    assert!(
        names.iter().any(|n| n.starts_with(".orphan-rev0002-")),
        "{names:?}"
    );
    assert!(
        names.contains(&".staging-999999-1".to_string()),
        "{names:?}"
    );
    assert!(!names.contains(&"rev0002".to_string()));
    assert_eq!(fs::read(stale.join("sta.rbin")).unwrap(), b"partial");
    assert_eq!(
        ws.resolve_query("rev2").unwrap_err().code(),
        ErrorCode::UnknownRevision
    );
    assert_eq!(ws.revisions().unwrap(), [Rev(0), Rev(1), Rev(3)]);

    // A history line torn by a crash is skipped and never merges with the next line.
    let history = ws.root().join("history.jsonl");
    let mut text = fs::read_to_string(&history).unwrap();
    text.push_str("{\"schema\":\"rusteda.history.v1\",\"time_un");
    fs::write(&history, &text).unwrap();
    assert_eq!(ws.history().unwrap().len(), 3);
    let c = commit_bytes(&ws, "c", b"c");
    let lines = ws.history().unwrap();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[3].new_rev, Some(c.rev));
    let raw = fs::read_to_string(&history).unwrap();
    assert!(
        raw.contains("\"time_un\n{\"schema\""),
        "the torn line was ended first"
    );
}
