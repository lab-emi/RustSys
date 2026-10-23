// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Tiny (<1 MiB graph) workspace lifecycle checks; external FPGA simulation lives
//! in verification/fpga. Run with two test threads under the normal memory guard.

use rusteda_workspace::{Hash64, Invocation, Workspace};
use rustsys_cli::ws::{RtlFileEntry, RtlManifest};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn seed(tag: &str) -> (PathBuf, Workspace) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fpga-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let inv = Invocation::new("test", "0", "test", vec!["test"]);
    let ws = Workspace::init(&dir, &inv).unwrap().workspace;
    let source = "module top(input clk,rst_n,input [5:0] d, output reg q); always @(posedge clk or negedge rst_n) if(!rst_n) q<=0; else q<=^d; endmodule";
    let mut txn = ws.begin(ws.resolve_base("rev0").unwrap(), &inv).unwrap();
    let rtl = txn.staging_path("rtl").unwrap();
    std::fs::create_dir(&rtl).unwrap();
    std::fs::write(rtl.join("top.v"), source).unwrap();
    let manifest = RtlManifest {
        top: "top".into(),
        defines: BTreeMap::new(),
        params: BTreeMap::new(),
        files: vec![RtlFileEntry {
            path: "top.v".into(),
            hash: Hash64::of(source.as_bytes()),
        }],
        predefined_macros: true,
        format: None,
    };
    std::fs::write(rtl.join("rtl.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    txn.add_dir("rtl", "rtl", Default::default()).unwrap();
    txn.commit(Default::default()).unwrap();
    (dir, ws)
}

fn run(dir: &Path, words: &[&str]) -> (bool, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_rustsys"))
        .args(["--format", "json", "--threads", "2", "-w"])
        .arg(dir)
        .arg("synth-fpga")
        .args(words)
        .output()
        .unwrap();
    (
        out.status.success(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}

#[test]
fn native_fpga_table_requires_no_liberty_and_rejects_stale_writes() {
    let (dir, ws) = seed("lifecycle");
    let (ok, value) = run(&dir, &["--base-rev", "rev1"]);
    assert!(ok, "{value}");
    assert_eq!(value["new_rev"], "rev2");
    assert_eq!(value["report"]["equivalence_status"], "unchecked");
    assert_eq!(value["report"]["flip_flops"], 1);
    let path = PathBuf::from(value["artifacts"]["netlist"].as_str().unwrap());
    assert!(path.is_file());
    assert!(path.with_file_name("luts.bin").metadata().unwrap().len() > 0);
    let (ok, value) = run(&dir, &["--base-rev", "rev1"]);
    assert!(!ok);
    assert_eq!(value["error"]["code"], "REVISION_CONFLICT");
    assert_eq!(ws.head().unwrap().to_string(), "rev2");
    let (ok, value) = run(&dir, &["--base-rev", "rev2", "--max-cut-merges", "1"]);
    assert!(!ok);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("FPGA_WORK_BUDGET")
    );
    assert_eq!(ws.head().unwrap().to_string(), "rev2");
    assert!(path.is_file());
}

#[test]
fn unsupported_device_does_not_commit() {
    let (dir, ws) = seed("unsupported");
    let (ok, value) = run(
        &dir,
        &["--base-rev", "rev1", "--part", "xczu3eg-sbva484-1-e"],
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], "INPUT_REJECTED");
    assert_eq!(ws.head().unwrap().to_string(), "rev1");
}
