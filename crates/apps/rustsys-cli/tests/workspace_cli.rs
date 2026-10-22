// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Integration tests for workspace-first CLI commands (`-w/--workspace`, `--format text|json`,
//! `rusteda.v1` envelopes, revisions, tables, handles, and post-mapping incremental flows).
//!
//! Covers: CLI-REQ-0200, CLI-REQ-0201, CLI-REQ-0202, CLI-REQ-0203, CLI-REQ-0204, CLI-REQ-0205.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusteda_formats::liberty;
use rusteda_workspace::{
    BaseRev, Hash64, Invocation, JsonMap, Rev, UiState, Upstream, Workspace, netlist, rlib,
};
use rustsys_cli::ws::{RtlFileEntry, RtlManifest};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;
use serde_json::Value;

struct WsEnv {
    dir: PathBuf,
    ws_dir: PathBuf,
}

impl WsEnv {
    fn new(tag: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("ws-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ws_dir = dir.join("ws");
        Self { dir, ws_dir }
    }

    /// Initializes the workspace (`rev0`), imports `Variant::Full` library (`rev1`), and imports
    /// the named corpus design (`rev2`).
    fn seed(&self, design_name: &str) -> Workspace {
        let init_inv = Invocation::new("rusteda", "0.1.0", "rusteda init", vec!["rusteda", "init"]);
        let initialized = Workspace::init(&self.ws_dir, &init_inv).unwrap();
        let ws = initialized.workspace;

        // rev1: import library
        let lib_text = library(Variant::Full);
        let lib_path = self.dir.join("cells.lib");
        std::fs::write(&lib_path, &lib_text).unwrap();
        let parsed_lib = liberty::read_text(&lib_text, "cells.lib").unwrap();
        let lib_hash = Hash64::of(lib_text.as_bytes());
        let cached = ws
            .cache()
            .put_library(
                &parsed_lib.name,
                lib_text.as_bytes(),
                rlib::KIND,
                rlib::VERSIONS,
                |_| rlib::encode_with_source(&parsed_lib, lib_hash),
            )
            .unwrap();

        let lib_inv = Invocation::new(
            "rusteda",
            "0.1.0",
            "rusteda import",
            vec!["rusteda", "import", "--lib"],
        );
        let mut txn1 = ws.begin(BaseRev::parse("rev0").unwrap(), &lib_inv).unwrap();
        txn1.add_library(cached.entry("default", lib_path.display().to_string()))
            .unwrap();
        txn1.commit(JsonMap::new()).unwrap();

        // rev2: import RTL
        let design = CORPUS.iter().find(|d| d.name == design_name).unwrap();
        let rtl_inv = Invocation::new(
            "rusteda",
            "0.1.0",
            "rusteda import",
            vec!["rusteda", "import", "--rtl"],
        );
        let mut txn2 = ws.begin(BaseRev::parse("rev1").unwrap(), &rtl_inv).unwrap();
        let rtl_stage = txn2.staging_dir().join("rtl");
        std::fs::create_dir_all(&rtl_stage).unwrap();
        let mut files = Vec::new();
        for (name, src) in design.files {
            let file_path = rtl_stage.join(name);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&file_path, src).unwrap();
            files.push(RtlFileEntry {
                path: (*name).to_string(),
                hash: Hash64::of(src.as_bytes()),
            });
        }
        let manifest = RtlManifest {
            top: design.top.to_string(),
            defines: BTreeMap::new(),
            params: BTreeMap::new(),
            files,
            predefined_macros: true,
            format: None,
        };
        std::fs::write(
            rtl_stage.join("rtl.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        txn2.add_dir("rtl", "rtl", Upstream::new()).unwrap();
        txn2.set_summary("rtl", "top", design.top).unwrap();
        txn2.set_summary("rtl", "files", design.files.len())
            .unwrap();
        txn2.commit(JsonMap::new()).unwrap();

        ws
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rustsys"));
        c.env_remove("NO_COLOR").current_dir(&self.dir);
        c.arg("-w").arg(&self.ws_dir).args(args).output().unwrap()
    }

    fn run_json(&self, args: &[&str]) -> (i32, Value, String) {
        let mut full_args = vec!["--format", "json"];
        full_args.extend_from_slice(args);
        let out = self.run(&full_args);
        let code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "invalid JSON on stdout (exit {code}): {e}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            )
        });
        assert_eq!(json["schema"], "rusteda.v1");
        assert!(
            json["command"]
                .as_str()
                .is_some_and(|c| c.starts_with("rustsys"))
        );
        (code, json, stderr)
    }
}

#[test]
fn full_workspace_synthesis_check_report_resize_and_opt_timing_flow() {
    let env = WsEnv::new("full-flow");
    let ws = env.seed("alu");

    // 1. rustsys -w ws synth --base-rev rev2 --format json -> rev3
    let (code, synth_env, stderr) = env.run_json(&["synth", "--base-rev", "rev2"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(synth_env["ok"], true);
    assert_eq!(synth_env["command"], "rustsys synth");
    assert_eq!(synth_env["base_rev"], "rev2");
    assert_eq!(synth_env["new_rev"], "rev3");
    assert_eq!(synth_env["scope"]["top"], "alu");
    assert_eq!(synth_env["scope"]["equivalence"], "unchecked");
    assert!(synth_env["metrics"]["cells"].as_u64().unwrap() > 0);
    assert!(synth_env["metrics"]["area_um2"].as_f64().unwrap() > 0.0);

    let rev3 = ws.revision(Rev::parse("rev3").unwrap()).unwrap();
    assert!(rev3.has_table("netlist"));
    assert!(rev3.has_table("synth"));
    assert!(!rev3.has_table("check"));

    // 2. rustsys -w ws report --rev rev3 --section cells --section summary --format json
    let (code, rep_env, _) = env.run_json(&[
        "report",
        "--rev",
        "rev3",
        "--section",
        "cells",
        "--section",
        "summary",
    ]);
    assert_eq!(code, 0);
    assert_eq!(rep_env["ok"], true);
    assert_eq!(rep_env["command"], "rustsys report");
    assert_eq!(rep_env["rev"], "rev3");
    assert!(rep_env["new_rev"].is_null());
    assert_eq!(rep_env["table"], "synth");
    let rows = rep_env["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
    assert_eq!(rows[0]["handle"], "cell1");

    // Also verify text mode output of `report` includes `next:` lines.
    let text_out = env.run(&["report", "--rev", "rev3", "--section", "cells"]);
    assert_eq!(text_out.status.code(), Some(0));
    let text_stdout = String::from_utf8_lossy(&text_out.stdout);
    assert!(text_stdout.contains("Cell"), "stdout: {text_stdout}");
    assert!(text_stdout.contains("next: "), "stdout: {text_stdout}");

    // 3. rustsys -w ws check --base-rev rev3 --format json -> rev4
    let (code, check_env, _) = env.run_json(&["check", "--base-rev", "rev3"]);
    assert_eq!(code, 0);
    assert_eq!(check_env["ok"], true);
    assert_eq!(check_env["command"], "rustsys check");
    assert_eq!(check_env["base_rev"], "rev3");
    assert_eq!(check_env["new_rev"], "rev4");
    assert_eq!(check_env["verdict"], "equivalent");
    assert_eq!(check_env["scope"]["equivalence"], "equivalent");

    let rev4 = ws.revision(Rev::parse("rev4").unwrap()).unwrap();
    assert!(rev4.has_table("netlist"));
    assert!(rev4.has_table("synth"));
    assert!(rev4.has_table("check"));
    assert_eq!(
        rev4.meta()
            .status
            .get("check")
            .and_then(|m| m.get("equivalence"))
            .and_then(Value::as_str),
        Some("equivalent")
    );

    // 4. rustsys -w ws resize --base-rev rev4 --target inst1 --drive-multiplier 2.0 -> rev5
    let (code, resize_env, _) = env.run_json(&[
        "resize",
        "--base-rev",
        "rev4",
        "--target",
        "inst1",
        "--drive-multiplier",
        "2.0",
    ]);
    assert_eq!(code, 0);
    assert_eq!(resize_env["ok"], true);
    assert_eq!(resize_env["command"], "rustsys resize");
    assert_eq!(resize_env["base_rev"], "rev4");
    assert_eq!(resize_env["new_rev"], "rev5");
    assert_eq!(resize_env["changes"].as_array().unwrap().len(), 1);
    assert_eq!(resize_env["changes"][0]["handle"], "inst1");

    // Carry-forward DAG must drop `check` in rev5 because `netlist` was recomputed!
    let rev5 = ws.revision(Rev::parse("rev5").unwrap()).unwrap();
    assert!(rev5.has_table("netlist"));
    assert!(rev5.has_table("synth"));
    assert!(
        !rev5.has_table("check"),
        "check table should be dropped when upstream netlist changes"
    );
    assert!(rev5.meta().dropped.iter().any(|d| d.table == "check"));

    // 5. rustsys -w ws opt-timing --base-rev rev5 --timing-period 0.05 --buffer-fanout 2 -> rev6
    let (code, opt_env, _) = env.run_json(&[
        "opt-timing",
        "--base-rev",
        "rev5",
        "--timing-period",
        "0.05",
        "--buffer-fanout",
        "2",
        "--target",
        "inst1",
    ]);
    assert_eq!(code, 0);
    assert_eq!(opt_env["ok"], true);
    assert_eq!(opt_env["command"], "rustsys opt-timing");
    assert_eq!(opt_env["base_rev"], "rev5");
    assert_eq!(opt_env["new_rev"], "rev6");

    // Verify CONTRACT.md §5 CA-GN d: any newly inserted buffer/net uses `rs_buf<k>` / `rs_net<k>`,
    // never `_rs_ebuf_*` or `_rs_enet_*`.
    let rev6 = ws.revision(Rev::parse("rev6").unwrap()).unwrap();
    let g6 = netlist::thaw_rev(&rev6).unwrap();
    let nl6 = rusteda_formats::verilog::netlist::from_gate_netlist(&g6).unwrap();
    for inst in &nl6.instances {
        assert!(
            !inst.name.starts_with("_rs_ebuf_"),
            "unexpected legacy buffer prefix: {}",
            inst.name
        );
    }
    for (wire, _) in &nl6.wires {
        assert!(
            !wire.starts_with("_rs_enet_"),
            "unexpected legacy net prefix: {wire}"
        );
    }
    if opt_env["metrics"]["buffers_added"].as_u64().unwrap_or(0) > 0 {
        assert!(
            nl6.instances
                .iter()
                .any(|inst| inst.name.starts_with("rs_buf")),
            "expected at least one rs_buf<k> instance when buffers_added > 0"
        );
        assert!(
            nl6.wires.iter().any(|(w, _)| w.starts_with("rs_net")),
            "expected at least one rs_net<k> net when buffers_added > 0"
        );
    }

    // 6. Re-verify equivalence on rev6 after resize + opt-timing -> rev7
    let (code, check2_env, _) = env.run_json(&["check", "--base-rev", "rev6"]);
    assert_eq!(code, 0);
    assert_eq!(check2_env["ok"], true);
    assert_eq!(check2_env["new_rev"], "rev7");
    assert_eq!(check2_env["verdict"], "equivalent");
}

#[test]
fn synth_with_inline_check_flag_commits_both_tables() {
    let env = WsEnv::new("synth-check");
    let ws = env.seed("counter");

    let (code, out, stderr) = env.run_json(&["synth", "--base-rev", "rev2", "--check"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(out["ok"], true);
    assert_eq!(out["new_rev"], "rev3");
    assert_eq!(out["scope"]["equivalence"], "equivalent");
    assert_eq!(out["check"]["verdict"], "equivalent");

    let rev3 = ws.revision(Rev::parse("rev3").unwrap()).unwrap();
    assert!(rev3.has_table("netlist"));
    assert!(rev3.has_table("synth"));
    assert!(rev3.has_table("check"));
}

#[test]
fn revision_conflict_exits_4_with_envelope_and_logs_rejection() {
    let env = WsEnv::new("rev-conflict");
    let ws = env.seed("counter");

    // Advance HEAD to rev3 first.
    let (code, _, _) = env.run_json(&["synth", "--base-rev", "rev2"]);
    assert_eq!(code, 0);

    // Attempt to run synth against stale base rev1 while HEAD is rev3.
    let (code, conflict, _) = env.run_json(&["synth", "--base-rev", "rev1"]);
    assert_eq!(code, 4);
    assert_eq!(conflict["ok"], false);
    assert_eq!(conflict["base_rev"], "rev1");
    assert!(conflict["new_rev"].is_null());
    assert_eq!(conflict["error"]["code"], "REVISION_CONFLICT");
    let next_actions = conflict["next_actions"].as_array().unwrap();
    assert!(!next_actions.is_empty());

    let history = ws.history().unwrap();
    let last = history.last().unwrap();
    assert!(!last.ok);
    assert_eq!(last.exit, 4);
}

#[test]
fn missing_table_and_unknown_handle_errors() {
    let env = WsEnv::new("errors");
    let _ws = env.seed("counter");

    // Running `check` on rev2 (before `synth` has produced a `netlist` table) -> MISSING_TABLE (exit 1)
    let (code, missing_env, _) = env.run_json(&["check", "--base-rev", "rev2"]);
    assert_eq!(code, 1);
    assert_eq!(missing_env["ok"], false);
    assert_eq!(missing_env["error"]["code"], "MISSING_TABLE");

    // Synthesize rev2 -> rev3
    let (code, _, _) = env.run_json(&["synth", "--base-rev", "rev2"]);
    assert_eq!(code, 0);

    // Resize an out-of-range handle `inst99999` -> UNKNOWN_HANDLE (exit 1)
    let (code, handle_env, _) =
        env.run_json(&["resize", "--base-rev", "rev3", "--target", "inst99999"]);
    assert_eq!(code, 1);
    assert_eq!(handle_env["ok"], false);
    assert_eq!(handle_env["error"]["code"], "UNKNOWN_HANDLE");

    // Usage error: synth without --base-rev in workspace mode -> USAGE (exit 1)
    let (code, usage_env, _) = env.run_json(&["synth"]);
    assert_eq!(code, 1);
    assert_eq!(usage_env["ok"], false);
    assert_eq!(usage_env["error"]["code"], "USAGE");
}

#[test]
fn selection_handle_resolution_in_resize() {
    let env = WsEnv::new("selection");
    let ws = env.seed("counter");

    let (code, _, _) = env.run_json(&["synth", "--base-rev", "rev2"]);
    assert_eq!(code, 0);

    // Write ui_state.json for rev3 with sel1 -> [inst1]
    let ui = UiState::new(
        Rev::parse("rev3").unwrap(),
        "sel1",
        vec!["inst1".to_string()],
        Vec::new(),
    );
    ws.write_ui_state(&ui).unwrap();

    let (code, resize_env, _) = env.run_json(&[
        "resize",
        "--base-rev",
        "rev3",
        "--target",
        "sel1",
        "--drive-multiplier",
        "2.0",
    ]);
    assert_eq!(code, 0);
    assert_eq!(resize_env["ok"], true);
    assert_eq!(resize_env["new_rev"], "rev4");
    assert_eq!(resize_env["changes"][0]["handle"], "inst1");
}

#[test]
fn negative_verdict_commits_revision_with_exit_3_and_mismatches_report() {
    let env = WsEnv::new("neg-verdict");
    let ws = env.seed("alu");

    let (code, _, _) = env.run_json(&["synth", "--base-rev", "rev2"]);
    assert_eq!(code, 0);

    // Corrupt a cell in rev3 -> rev4 so equivalence check finds a mismatch
    let rev3 = ws.revision(Rev::parse("rev3").unwrap()).unwrap();
    let (_, libs, _, _) = rustsys_cli::ws::load_libraries_from_rev(&ws, &rev3).expect("load libs");
    let g3 = netlist::thaw_rev(&rev3).unwrap();
    let mut nl = rusteda_formats::verilog::netlist::from_gate_netlist(&g3).unwrap();
    let pos = nl
        .instances
        .iter()
        .position(|i| i.cell.starts_with("wtf_nand2_"))
        .unwrap();
    nl.instances[pos].cell = "wtf_nor2_1".to_string();
    let g_bad = rustsys_cli::ws::nl_to_gate_netlist(&nl, &libs).unwrap();
    let frozen_bad = g_bad.freeze();
    let mut txn = ws
        .begin(
            BaseRev::parse("rev3").unwrap(),
            &Invocation::new(
                "rusteda",
                "0.1.0",
                "rusteda import",
                vec!["rusteda".to_string()],
            ),
        )
        .unwrap();
    let up = txn.upstream(&["rtl", "libraries"]).unwrap();
    netlist::add_to_txn(&mut txn, &frozen_bad, up).unwrap();
    txn.commit(JsonMap::new()).unwrap();

    // Now run `rustsys -w ws check --base-rev rev4`: should commit rev5 and exit 3!
    let (code, check_env, _) = env.run_json(&["check", "--base-rev", "rev4"]);
    assert_eq!(code, 3);
    assert_eq!(check_env["ok"], true);
    assert_eq!(check_env["new_rev"], "rev5");
    assert_eq!(check_env["verdict"], "not_equivalent");
    assert!(check_env["metrics"]["mismatches"].as_u64().unwrap() > 0);

    // Query mismatches table via `rustsys -w ws report --rev rev5 --table check --section mismatches`
    let (code, rep_env, _) = env.run_json(&[
        "report",
        "--rev",
        "rev5",
        "--table",
        "check",
        "--section",
        "mismatches",
    ]);
    assert_eq!(code, 0);
    assert_eq!(rep_env["table"], "check");
    let rows = rep_env["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
    assert_eq!(rows[0]["handle"], "mm1");
}
