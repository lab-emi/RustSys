// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The timing table in a workspace: staged by the builder, opened through the revision
//! (WS-REQ-0126, WS-REQ-0135).

mod common;

use common::{inv, new_workspace};
use rusteda_workspace::sta::{
    CheckKind, EndpointKind, PathSpec, PointKind, StaTable, StaTableBuilder, StoreMode, Transition,
};
use rusteda_workspace::{BaseRev, ErrorCode, JsonMap, Upstream};
use serde_json::json;

fn table() -> StaTableBuilder {
    let mut b = StaTableBuilder::new(StoreMode::Summary);
    let tt = b.corner("tt");
    b.clock("clk", 1.0, 0.0);
    let q = b.pin("u1", "Q");
    let a = b.pin("u2", "A");
    let y = b.pin("u2", "Y");
    let d = b.pin("u3", "D");
    b.endpoint(d, EndpointKind::FlipFlop)
        .endpoint_slack(d, tt, Some(-0.5), Some(0.1));
    b.path(PathSpec {
        corner: tt,
        kind: CheckKind::Setup,
        slack_ns: -0.5,
        required_ns: 0.9,
        points: vec![
            (q, Transition::Rise, PointKind::Cell, 0.4),
            (a, Transition::Rise, PointKind::Net, 0.4),
            (y, Transition::Fall, PointKind::Cell, 1.3),
            (d, Transition::Fall, PointKind::Net, 1.4),
        ],
    });
    b
}

// WS-REQ-0126, WS-REQ-0135: a staged table is opened through its revision with the summary
// values and the timing status of the timing table; a revision without one is MISSING_TABLE.
#[test]
fn staged_table_opens_through_its_revision() {
    let ws = new_workspace("sta-staged");
    let mut txn = ws
        .begin(BaseRev::parse("rev0").unwrap(), &inv("tool sta"))
        .unwrap();
    let hash = table().stage(&mut txn, Upstream::new()).unwrap();
    let committed = txn.commit(JsonMap::new()).unwrap();
    let rev = ws.revision(committed.rev).unwrap();
    assert_eq!(rev.table("sta").unwrap().path, "sta.rbin");
    assert_eq!(rev.table_hash("sta"), Some(hash));
    let summary = &rev.meta().summary["sta"];
    assert_eq!(summary["setup_wns_ns"], json!(-0.5));
    assert_eq!(summary["setup_violating_endpoints"], json!(1));
    assert_eq!(summary["total_endpoints"], json!(1));
    assert_eq!(rev.meta().status["sta"]["timing"], json!("violated"));
    let t = StaTable::open_rev(&rev).unwrap();
    t.verify_hash().unwrap();
    let p = t.path(0).unwrap();
    assert_eq!(p.handle(), "path1");
    assert_eq!(p.instances(), ["u1", "u2", "u3"]);
    let rev0 = ws.revision(BaseRev::parse("rev0").unwrap().rev()).unwrap();
    assert_eq!(
        StaTable::open_rev(&rev0).unwrap_err().code(),
        ErrorCode::MissingTable
    );
}

// WS-REQ-0126: the upstream map of the table is recorded in its rbin header, so the revision
// opens it, and a table computed from a table of the revision is dropped with it.
#[test]
fn staged_table_records_its_upstream() {
    let ws = new_workspace("sta-upstream");
    let mut txn = ws
        .begin(BaseRev::parse("rev0").unwrap(), &inv("tool import"))
        .unwrap();
    txn.add_bytes("netlist", "netlist.json", b"{}", Upstream::new())
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();
    let mut txn = ws
        .begin(BaseRev::parse("rev1").unwrap(), &inv("tool sta"))
        .unwrap();
    let up = txn.upstream(&["netlist"]).unwrap();
    table().stage(&mut txn, up).unwrap();
    let rev2 = ws
        .revision(txn.commit(JsonMap::new()).unwrap().rev)
        .unwrap();
    StaTable::open_rev(&rev2).unwrap();
    let mut txn = ws
        .begin(BaseRev::parse("rev2").unwrap(), &inv("tool edit"))
        .unwrap();
    txn.add_bytes("netlist", "netlist.json", b"{\"x\":1}", Upstream::new())
        .unwrap();
    let rev3 = ws
        .revision(txn.commit(JsonMap::new()).unwrap().rev)
        .unwrap();
    assert!(
        !rev3.has_table("sta"),
        "the timing table of another netlist is dropped"
    );
    assert_eq!(
        StaTable::open_rev(&rev3).unwrap_err().code(),
        ErrorCode::MissingTable
    );
}
