// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! End-to-end workspace scenarios of the post-mapping commands guided by a timing table and by
//! compiled constraints: synthesis, a timing table built with the shared builder whose paths
//! name real instances of the netlist, `opt-timing` and `resize` on `path<N>`, `ep<N>`,
//! `sel<N>` and default targets, the timing target from the constraints table, the iteration
//! bounds and stop conditions, request-id replays, and an independent equivalence proof of every
//! optimized revision.
//!
//! Covers: CLI-REQ-0204 to CLI-REQ-0213, SYNTH-REQ-0561, SYNTH-REQ-0564.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusteda_formats::liberty::{self, Direction, Library};
use rusteda_formats::verilog::netlist::from_gate_netlist;
use rusteda_formats::verilog::write::{NlDir, NlModule, NlRef};
use rusteda_workspace::sta::{
    CheckKind, EndpointKind, NodeRef, PathSpec, PointKind, StaTableBuilder, StoreMode, Transition,
};
use rusteda_workspace::{
    BaseRev, Hash64, Invocation, JsonMap, RbinWriter, Rev, UiState, Upstream, Workspace, netlist,
    rlib,
};
use rustsys_cli::ws::{RtlFileEntry, RtlManifest};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;
use serde_json::Value;

/// The constraints of the scenarios: a clock of 0.3 ns on the `pipeline` design.
const SDC: &str = "create_clock -name clk -period 0.3 [get_ports clk]
set_input_delay -clock [get_clocks clk] 0.05 [all_inputs]
set_output_delay -clock [get_clocks clk] 0.05 [all_outputs]
";

struct WsEnv {
    dir: PathBuf,
    ws_dir: PathBuf,
}

fn inv(command: &str) -> Invocation {
    Invocation::new("rusteda", "0.1.0", command, vec!["rusteda".to_string()])
}

impl WsEnv {
    fn new(tag: &str) -> WsEnv {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("ws-opt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ws_dir = dir.join("ws");
        WsEnv { dir, ws_dir }
    }

    /// rev1 library, rev2 RTL of `design`, rev3 constraints from `sdc` (when given).
    fn seed(&self, design: &str, sdc: Option<&str>) -> Workspace {
        let ws = Workspace::init(&self.ws_dir, &inv("rusteda init"))
            .unwrap()
            .workspace;
        let lib_text = library(Variant::Full);
        let lib_path = self.dir.join("cells.lib");
        std::fs::write(&lib_path, &lib_text).unwrap();
        let parsed = liberty::read_text(&lib_text, "cells.lib").unwrap();
        let hash = Hash64::of(lib_text.as_bytes());
        let cached = ws
            .cache()
            .put_library(
                &parsed.name,
                lib_text.as_bytes(),
                rlib::KIND,
                rlib::VERSIONS,
                |_| rlib::encode_with_source(&parsed, hash),
            )
            .unwrap();
        let mut txn = ws
            .begin(BaseRev::parse("rev0").unwrap(), &inv("rusteda import"))
            .unwrap();
        txn.add_library(cached.entry("default", lib_path.display().to_string()))
            .unwrap();
        txn.commit(JsonMap::new()).unwrap();

        let d = CORPUS.iter().find(|d| d.name == design).unwrap();
        let mut txn = ws
            .begin(BaseRev::parse("rev1").unwrap(), &inv("rusteda import"))
            .unwrap();
        let stage = txn.staging_dir().join("rtl");
        std::fs::create_dir_all(&stage).unwrap();
        let mut files = Vec::new();
        for (name, src) in d.files {
            std::fs::write(stage.join(name), src).unwrap();
            files.push(RtlFileEntry {
                path: (*name).to_string(),
                hash: Hash64::of(src.as_bytes()),
            });
        }
        let manifest = RtlManifest {
            top: d.top.to_string(),
            defines: BTreeMap::new(),
            params: BTreeMap::new(),
            files,
            predefined_macros: true,
            format: None,
        };
        std::fs::write(
            stage.join("rtl.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        txn.add_dir("rtl", "rtl", Upstream::new()).unwrap();
        txn.commit(JsonMap::new()).unwrap();

        if let Some(text) = sdc {
            let (model, diags) = rusteda_formats::sdc::read_str(text);
            assert!(
                diags
                    .iter()
                    .all(|d| d.severity != rusteda_formats::sdc::Severity::Error),
                "{diags:?}"
            );
            let sections = model.to_sections();
            let mut w = RbinWriter::new("CONSTR", 1).unwrap();
            for s in &sections {
                w.section_bytes(u32::from(s.id), s.elem_size, s.bytes.as_slice())
                    .unwrap();
            }
            let head = ws.head().unwrap().to_string();
            let mut txn = ws
                .begin(BaseRev::parse(&head).unwrap(), &inv("rusteda import"))
                .unwrap();
            txn.add_rbin("constraints", "constraints.rbin", &w, Upstream::new())
                .unwrap();
            txn.commit(JsonMap::new()).unwrap();
        }
        ws
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rustsys"))
            .env_remove("NO_COLOR")
            .current_dir(&self.dir)
            .arg("-w")
            .arg(&self.ws_dir)
            .args(args)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut full = vec!["--format", "json"];
        full.extend_from_slice(args);
        let out = self.run(&full);
        let code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "no envelope (exit {code}): {e}\n{stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert_eq!(v["schema"], "rusteda.v1");
        (code, v)
    }

    /// Runs the independent checker on `rev` and asserts the verdict `equivalent`.
    fn assert_equivalent(&self, rev: &str) -> String {
        let (code, v) = self.json(&["check", "--base-rev", rev]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["verdict"], "equivalent", "{v}");
        v["new_rev"].as_str().unwrap().to_string()
    }
}

/// The netlist and libraries of `rev`.
fn design_of(ws: &Workspace, rev: &str) -> (NlModule, Library) {
    let r = ws.revision(Rev::parse(rev).unwrap()).unwrap();
    let (_, libs, _, _) = rustsys_cli::ws::load_libraries_from_rev(ws, &r).unwrap();
    let g = netlist::thaw_rev(&r).unwrap();
    (from_gate_netlist(&g).unwrap(), libs[0].clone())
}

/// The direction of pin `pin` of `cell`.
fn direction(lib: &Library, cell: &str, pin: &str) -> Option<Direction> {
    let c = lib.cells.iter().find(|c| c.name == cell)?;
    c.pins.iter().find(|p| p.name == pin).map(|p| p.direction)
}

fn sequential(lib: &Library, cell: &str) -> bool {
    lib.cells
        .iter()
        .find(|c| c.name == cell)
        .is_some_and(|c| !c.sequentials.is_empty())
}

/// A step of a path: instance, input pin, output pin.
type Step = (String, String, String);

/// The deepest combinational path of `nl` into a flip-flop data pin, from a flip-flop output
/// or a primary input: the startpoint (a flip-flop and its clock and output pins, or a port),
/// the combinational steps, and the capturing flip-flop with its data pin.
fn deepest_path(nl: &NlModule, lib: &Library) -> (Option<Step>, Vec<Step>, (String, String)) {
    let mut driver: HashMap<&NlRef, (usize, &str)> = HashMap::new();
    for (i, inst) in nl.instances.iter().enumerate() {
        for (pin, r) in &inst.conns {
            if direction(lib, &inst.cell, pin) == Some(Direction::Output) {
                driver.insert(r, (i, pin.as_str()));
            }
        }
    }
    let inputs = |i: usize| -> Vec<(&str, &NlRef)> {
        let inst = &nl.instances[i];
        inst.conns
            .iter()
            .filter(|(p, _)| direction(lib, &inst.cell, p) == Some(Direction::Input))
            .map(|(p, r)| (p.as_str(), r))
            .collect()
    };
    // Depth of a net: combinational cells between it and a startpoint.
    let mut memo: HashMap<NlRef, usize> = HashMap::new();
    let mut net_depth = |r: &NlRef| -> usize {
        let mut stack = vec![r.clone()];
        while let Some(top) = stack.last().cloned() {
            if memo.contains_key(&top) {
                stack.pop();
                continue;
            }
            match driver.get(&top) {
                Some(&(i, _)) if !sequential(lib, &nl.instances[i].cell) => {
                    let ins = inputs(i);
                    let pending: Vec<NlRef> = ins
                        .iter()
                        .filter(|(_, n)| !memo.contains_key(*n))
                        .map(|(_, n)| (*n).clone())
                        .collect();
                    if pending.is_empty() {
                        let d = 1 + ins.iter().map(|(_, n)| memo[*n]).max().unwrap_or(0);
                        memo.insert(top, d);
                        stack.pop();
                    } else {
                        stack.extend(pending);
                    }
                }
                _ => {
                    memo.insert(top, 0);
                    stack.pop();
                }
            }
        }
        memo[r]
    };
    let mut best: Option<(usize, usize, &str)> = None;
    for (i, inst) in nl.instances.iter().enumerate() {
        if !sequential(lib, &inst.cell) {
            continue;
        }
        if let Some((_, r)) = inst.conns.iter().find(|(p, _)| p == "D") {
            let d = net_depth(r);
            if best.is_none_or(|(bd, _, _)| d > bd) {
                best = Some((d, i, "D"));
            }
        }
    }
    let (_, end_inst, end_pin) = best.expect("a flip-flop with a data pin");
    let end = (nl.instances[end_inst].name.clone(), end_pin.to_string());
    let d_net = nl.instances[end_inst]
        .conns
        .iter()
        .find(|(p, _)| p == end_pin)
        .unwrap()
        .1
        .clone();
    let mut steps = Vec::new();
    let mut net = d_net;
    let start = loop {
        match driver.get(&net) {
            Some(&(i, out)) if !sequential(lib, &nl.instances[i].cell) => {
                let ins = inputs(i);
                let (pin, next) = ins
                    .iter()
                    .max_by_key(|(_, n)| memo.get(*n).copied().unwrap_or(0))
                    .copied()
                    .expect("a gate with inputs");
                steps.push((
                    nl.instances[i].name.clone(),
                    pin.to_string(),
                    out.to_string(),
                ));
                net = next.clone();
            }
            Some(&(i, out)) => {
                break Some((
                    nl.instances[i].name.clone(),
                    "CLK".to_string(),
                    out.to_string(),
                ));
            }
            None => break None,
        }
    };
    steps.reverse();
    (start, steps, end)
}

/// Adds the deepest path of `nl` to `b` as a setup path of slack `slack` in corner 0; returns
/// the instances on it.
fn add_path(b: &mut StaTableBuilder, nl: &NlModule, lib: &Library, slack: f64) -> Vec<String> {
    let (start, steps, (end_inst, end_pin)) = deepest_path(nl, lib);
    let mut points: Vec<(NodeRef, Transition, PointKind, f64)> = Vec::new();
    let mut instances = Vec::new();
    let mut t = 0.0;
    match &start {
        Some((inst, clk, q)) => {
            points.push((
                b.pin(inst, clk),
                Transition::Rise,
                PointKind::ClockLaunch,
                0.0,
            ));
            t += 0.2;
            points.push((b.pin(inst, q), Transition::Rise, PointKind::Cell, t));
            instances.push(inst.clone());
        }
        None => {
            let port = nl
                .ports
                .iter()
                .find(|p| p.dir == NlDir::Input)
                .unwrap()
                .name
                .clone();
            points.push((b.port(&port), Transition::Rise, PointKind::Port, 0.0));
        }
    }
    for (inst, input, output) in &steps {
        points.push((b.pin(inst, input), Transition::Rise, PointKind::Net, t));
        t += 0.1;
        points.push((b.pin(inst, output), Transition::Fall, PointKind::Cell, t));
        instances.push(inst.clone());
    }
    let end = b.pin(&end_inst, &end_pin);
    points.push((end, Transition::Fall, PointKind::Net, t));
    instances.push(end_inst.clone());
    b.endpoint(end, EndpointKind::FlipFlop)
        .endpoint_slack(end, 0, Some(slack), Some(0.01));
    b.path(PathSpec {
        corner: 0,
        kind: CheckKind::Setup,
        slack_ns: slack,
        required_ns: t + slack,
        points,
    });
    instances
}

/// Commits a timing table on HEAD of `ws`, computed from its netlist, and returns the new HEAD
/// and the instances of `path1`.
fn commit_table(
    ws: &Workspace,
    build: impl FnOnce(&mut StaTableBuilder, &NlModule, &Library) -> Vec<String>,
) -> (String, Vec<String>) {
    let head = ws.head().unwrap().to_string();
    let (nl, lib) = design_of(ws, &head);
    let mut b = StaTableBuilder::new(StoreMode::Summary);
    b.corner("default");
    b.clock("clk", 0.3, 0.0);
    let instances = build(&mut b, &nl, &lib);
    let mut txn = ws
        .begin(BaseRev::parse(&head).unwrap(), &inv("timing run"))
        .unwrap();
    let up = txn.upstream(&["netlist"]).unwrap();
    b.stage(&mut txn, up).unwrap();
    let rev = txn.commit(JsonMap::new()).unwrap().rev.to_string();
    (rev, instances)
}

/// The instances that `changes` resized, and the drivers of the nets that inserted buffers
/// read, which must all be in `allowed`.
fn assert_changes_within(v: &Value, ws: &Workspace, allowed: &[String]) -> usize {
    let changes = v["changes"].as_array().unwrap();
    let new_rev = v["new_rev"].as_str().unwrap();
    let (nl, lib) = design_of(ws, new_rev);
    let inserted: BTreeSet<&str> = changes
        .iter()
        .filter(|c| c["change"] == "insert")
        .map(|c| c["instance"].as_str().unwrap())
        .collect();
    let mut driver: HashMap<&NlRef, &str> = HashMap::new();
    for inst in &nl.instances {
        for (pin, r) in &inst.conns {
            if direction(&lib, &inst.cell, pin) == Some(Direction::Output) {
                driver.insert(r, &inst.name);
            }
        }
    }
    for c in changes {
        let name = c["instance"].as_str().unwrap();
        match c["change"].as_str().unwrap() {
            "resize" => assert!(
                allowed.iter().any(|a| a == name),
                "{name} is not a target: {c}"
            ),
            "insert" => {
                assert!(name.starts_with("rs_buf"), "{c}");
                let inst = nl.instances.iter().find(|i| i.name == name).unwrap();
                let (_, input) = inst.conns.iter().find(|(p, _)| p == "A").unwrap();
                let d = driver[input];
                assert!(
                    allowed.iter().any(|a| a == d) || inserted.contains(d),
                    "{name} reads a net of {d}"
                );
            }
            other => panic!("change {other}"),
        }
    }
    changes.len()
}

fn ws_head(ws: &Workspace) -> String {
    ws.head().unwrap().to_string()
}

fn revert(ws: &Workspace, to: &str) {
    let head = ws_head(ws);
    ws.revert(
        BaseRev::parse(&head).unwrap(),
        Rev::parse(to).unwrap(),
        &inv("rusteda revert"),
    )
    .unwrap();
}

// CLI-REQ-0205, CLI-REQ-0206, CLI-REQ-0207, CLI-REQ-0209, CLI-REQ-0211, CLI-REQ-0213,
// SYNTH-REQ-0561, SYNTH-REQ-0564: opt-timing --target path1 changes only instances of that path
// of the timing table, under the period of the constraints, with honest metrics; the result is
// proven equivalent.
#[test]
fn opt_timing_on_a_timing_table_path() {
    let env = WsEnv::new("path");
    let ws = env.seed("pipeline", Some(SDC));
    let (code, synth) = env.json(&["synth", "--base-rev", "rev3"]);
    assert_eq!(code, 0, "{synth}");
    let (table_rev, path1) = commit_table(&ws, |b, nl, lib| add_path(b, nl, lib, -0.25));
    assert_eq!(table_rev, "rev5");
    let (code, v) = env.json(&[
        "opt-timing",
        "--base-rev",
        "rev5",
        "--target",
        "path1",
        "--stop-when",
        "no-improvement",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["new_rev"], "rev6");
    let scope = &v["scope"];
    assert_eq!(scope["timing_source"], "estimate");
    assert_eq!(scope["clock_model"], "ideal");
    assert_eq!(scope["wire_model"], "none");
    assert_eq!(scope["period_ns"], 0.3);
    assert_eq!(scope["constraints_source"], "sdc");
    assert_eq!(scope["constraints"]["period_ns"], 0.3);
    assert_eq!(scope["constraints"]["input_delay_ns"], 0.05);
    assert_eq!(scope["constraints"]["output_delay_ns"], 0.05);
    assert_eq!(scope["cli_overrides"], serde_json::json!([]));
    assert_eq!(scope["target_source"], "cli");
    assert_eq!(scope["target_instances"], path1.len());
    assert_eq!(scope["target_paths"][0]["handle"], "path1");
    assert_eq!(scope["target_paths"][0]["slack_ns"], -0.25);
    assert_eq!(scope["timing_table"], "rev5");
    let changed = assert_changes_within(&v, &ws, &path1);
    assert!(changed > 0, "{v}");
    let m = &v["metrics"];
    let (nl6, lib) = design_of(&ws, "rev6");
    let area: f64 = nl6
        .instances
        .iter()
        .map(|i| lib.cells.iter().find(|c| c.name == i.cell).unwrap().area)
        .sum();
    assert!((m["area_um2"].as_f64().unwrap() - area).abs() < 1e-6, "{m}");
    assert_eq!(m["cells"], nl6.instances.len());
    let rev6 = ws.revision(Rev::parse("rev6").unwrap()).unwrap();
    let synth6 = &rev6.meta().summary["synth"];
    assert_eq!(synth6["logic_depth"], m["logic_depth"]);
    assert_eq!(synth6["estimated_delay"], m["estimated_delay"]);
    assert!(m["estimated_slack_ns"].as_f64() >= m["estimated_slack_before_ns"].as_f64());
    assert_eq!(
        v["iterations_run"],
        v["iterations"].as_array().unwrap().len()
    );
    assert_eq!(v["replayed"], false);
    let next: Vec<&str> = v["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(next.len(), 2, "{next:?}");
    assert!(next[0].contains("check --base-rev rev6"), "{next:?}");
    assert!(next[1].starts_with("rusteda diff") && next[1].contains("--from rev5 --to rev6"));
    assert!(
        !rev6.has_table("sta"),
        "the timing table of the old netlist is dropped"
    );
    assert!(rev6.has_table("constraints"));
    env.assert_equivalent("rev6");

    // resize on the same path of the timing table (CLI-REQ-0204, CLI-REQ-0207).
    revert(&ws, "rev5");
    let (code, v) = env.json(&[
        "resize",
        "--base-rev",
        "rev5",
        "--target",
        "path1",
        "--drive-multiplier",
        "2",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["scope"]["target_source"], "cli");
    assert_eq!(v["scope"]["target_instances"], path1.len());
    assert_eq!(v["scope"]["drive_multiplier"], 2.0);
    let n = assert_changes_within(&v, &ws, &path1);
    assert!(
        n > 0
            && v["changes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["change"] == "resize")
    );
    assert_eq!(v["resized_cells"], n);
    let new_rev = v["new_rev"].as_str().unwrap().to_string();
    env.assert_equivalent(&new_rev);
}

// CLI-REQ-0207: path<N> and ep<N> need the timing table of the base revision: without it the
// command fails with MISSING_TABLE and a help line, and out-of-range rows are UNKNOWN_HANDLE.
#[test]
fn timing_table_handles_need_the_table() {
    let env = WsEnv::new("missing");
    let ws = env.seed("pipeline", Some(SDC));
    let (code, _) = env.json(&["synth", "--base-rev", "rev3"]);
    assert_eq!(code, 0);
    for (cmd, handle) in [
        ("opt-timing", "path1"),
        ("opt-timing", "ep1"),
        ("resize", "path1"),
    ] {
        let (code, v) = env.json(&[cmd, "--base-rev", "rev4", "--target", handle]);
        assert_eq!(code, 1, "{v}");
        assert_eq!(v["error"]["code"], "MISSING_TABLE", "{v}");
        let help = v["diagnostics"][0]["help"].as_str().unwrap_or_default();
        assert!(help.contains("static timing analysis tool"), "{v}");
        let text = env.run(&[cmd, "--base-rev", "rev4", "--target", handle]);
        let stderr = String::from_utf8_lossy(&text.stderr);
        assert!(
            stderr.contains("help: run the static timing analysis tool"),
            "{stderr}"
        );
    }
    assert_eq!(ws_head(&ws), "rev4");
    let (table_rev, _) = commit_table(&ws, |b, nl, lib| add_path(b, nl, lib, -0.1));
    for handle in ["path2", "ep2", "path0x"] {
        let (code, v) = env.json(&["opt-timing", "--base-rev", &table_rev, "--target", handle]);
        assert_eq!(code, 1, "{v}");
        assert_eq!(v["error"]["code"], "UNKNOWN_HANDLE", "{handle}: {v}");
    }
    // ep1 is the worst endpoint: its worst setup path is path1.
    let (code, v) = env.json(&["opt-timing", "--base-rev", &table_rev, "--target", "ep1"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["scope"]["target_paths"][0]["handle"], "path1");
    env.assert_equivalent(v["new_rev"].as_str().unwrap());
}

// CLI-REQ-0208: without --target the targets are the instances on the ranked setup paths with
// negative slack; a table without one names no target; without a table every instance.
#[test]
fn default_targets_follow_negative_setup_paths() {
    let env = WsEnv::new("default");
    let ws = env.seed("pipeline", Some(SDC));
    let (code, _) = env.json(&["synth", "--base-rev", "rev3"]);
    assert_eq!(code, 0);
    let (table_rev, path1) = commit_table(&ws, |b, nl, lib| add_path(b, nl, lib, -0.2));
    let (code, v) = env.json(&[
        "opt-timing",
        "--base-rev",
        &table_rev,
        "--stop-when",
        "no-improvement",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["scope"]["target_source"], "timing_table");
    assert_eq!(v["scope"]["target_instances"], path1.len());
    assert_changes_within(&v, &ws, &path1);
    env.assert_equivalent(v["new_rev"].as_str().unwrap());

    revert(&ws, "rev4");
    let (met_rev, _) = commit_table(&ws, |b, nl, lib| add_path(b, nl, lib, 0.05));
    let (code, v) = env.json(&["opt-timing", "--base-rev", &met_rev]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["scope"]["target_source"], "timing_table");
    assert_eq!(v["scope"]["target_instances"], 0);
    assert_eq!(v["changes"], serde_json::json!([]));
    env.assert_equivalent(v["new_rev"].as_str().unwrap());
    assert!(
        v["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "S0537"),
        "{v}"
    );

    revert(&ws, "rev4");
    let (code, v) = env.json(&["opt-timing", "--base-rev", "rev4", "--max-iters", "2"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["scope"]["target_source"], "all_instances");
    assert_eq!(v["scope"]["timing_table"], Value::Null);
    env.assert_equivalent(v["new_rev"].as_str().unwrap());
}

// CLI-REQ-0206: sel<N> expands its paths through the timing table and its instances.
#[test]
fn selections_expand_their_paths() {
    let env = WsEnv::new("selection");
    let ws = env.seed("pipeline", Some(SDC));
    let (code, _) = env.json(&["synth", "--base-rev", "rev3"]);
    assert_eq!(code, 0);
    let (table_rev, path1) = commit_table(&ws, |b, nl, lib| add_path(b, nl, lib, -0.2));
    let ui = UiState::new(
        Rev::parse(&table_rev).unwrap(),
        "sel3",
        vec!["inst1".to_string()],
        vec!["path1".to_string()],
    );
    ws.write_ui_state(&ui).unwrap();
    let (code, v) = env.json(&[
        "resize",
        "--base-rev",
        &table_rev,
        "--target",
        "sel3",
        "--drive-multiplier",
        "1.5",
    ]);
    assert_eq!(code, 0, "{v}");
    let (nl, _) = design_of(&ws, &table_rev);
    let mut allowed = path1.clone();
    allowed.push(nl.instances[0].name.clone());
    let unique: BTreeSet<&String> = allowed.iter().collect();
    assert_eq!(v["scope"]["target_instances"], unique.len());
    assert_eq!(v["scope"]["target_paths"][0]["handle"], "path1");
    assert_changes_within(&v, &ws, &allowed);
    env.assert_equivalent(v["new_rev"].as_str().unwrap());
}

// CLI-REQ-0209: the target derived from constraints, with units, overrides and warnings for
// what the estimate does not model; without constraints the target is not derived.
#[test]
fn constraints_give_the_timing_target() {
    let sdc = "set_units -time ps -capacitance fF
create_clock -name clk -period 500 [get_ports clk]
create_clock -name slow -period 1000
set_input_delay -clock [get_clocks clk] -max 200 [all_inputs]
set_input_delay -clock [get_clocks clk] -min 10 [all_inputs]
set_output_delay -clock [get_clocks clk] 300 [all_outputs]
set_load 20 [all_outputs]
set_input_transition 150 [all_inputs]
set_clock_transition 70 [get_clocks clk]
set_clock_uncertainty -setup 40 [get_clocks clk]
set_clock_uncertainty -hold 90 [get_clocks clk]
set_false_path -from [get_ports rst_n]
set_clock_latency 100 [get_clocks clk]
";
    let env = WsEnv::new("constraints");
    let ws = env.seed("pipeline", Some(sdc));
    let (code, _) = env.json(&["synth", "--base-rev", "rev3"]);
    assert_eq!(code, 0);
    let (code, v) = env.json(&["opt-timing", "--base-rev", "rev4", "--max-iters", "1"]);
    assert_eq!(code, 0, "{v}");
    let c = &v["scope"]["constraints"];
    let near = |x: &Value, want: f64| (x.as_f64().unwrap() - want).abs() < 1e-9;
    assert!(near(&v["scope"]["period_ns"], 0.5), "{v}");
    assert!(near(&c["input_delay_ns"], 0.2), "{c}");
    assert!(near(&c["output_delay_ns"], 0.3), "{c}");
    assert!(near(&c["output_load_pf"], 0.02), "{c}");
    assert!(near(&c["input_slew_ns"], 0.15), "{c}");
    assert!(near(&c["clock_slew_ns"], 0.07), "{c}");
    assert!(near(&c["setup_uncertainty_ns"], 0.04), "{c}");
    assert!(near(&c["margin_ns"], 0.09), "{c}");
    assert_eq!(v["scope"]["constraints_clocks"], 2);
    let warnings: Vec<String> = v["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "S0535")
        .map(|d| d["message"].as_str().unwrap().to_string())
        .collect();
    for want in ["2 clocks", "set_false_path", "set_clock_latency"] {
        assert!(
            warnings.iter().any(|w| w.contains(want)),
            "{want}: {warnings:?}"
        );
    }
    let located = v["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["message"].as_str().unwrap().contains("set_false_path"))
        .unwrap();
    assert_eq!(located["location"]["line"], 12, "{located}");
    env.assert_equivalent(v["new_rev"].as_str().unwrap());

    revert(&ws, "rev4");
    let (code, v) = env.json(&[
        "opt-timing",
        "--base-rev",
        "rev4",
        "--max-iters",
        "1",
        "--timing-period",
        "0.7",
        "--timing-margin",
        "0",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["scope"]["period_ns"], 0.7);
    assert_eq!(v["scope"]["constraints_source"], "sdc");
    assert_eq!(
        v["scope"]["cli_overrides"],
        serde_json::json!(["period_ns", "margin_ns"])
    );
    assert!(near(&v["scope"]["constraints"]["margin_ns"], 0.04));
    env.assert_equivalent(v["new_rev"].as_str().unwrap());

    let env = WsEnv::new("no-constraints");
    let _ws = env.seed("pipeline", None);
    let (code, _) = env.json(&["synth", "--base-rev", "rev2"]);
    assert_eq!(code, 0);
    let (code, v) = env.json(&["opt-timing", "--base-rev", "rev3", "--max-iters", "1"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["scope"]["constraints_source"], "defaults");
    assert_eq!(v["scope"]["period_ns"], 10.0);
    assert!(
        v["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "S0536"),
        "{v}"
    );
    env.assert_equivalent(v["new_rev"].as_str().unwrap());
}

// CLI-REQ-0210: the iteration bound, the stop conditions and their grammar, and the area growth
// limit.
#[test]
fn iteration_bounds_and_stop_conditions() {
    let env = WsEnv::new("bounds");
    let ws = env.seed("pipeline", Some(SDC));
    let (code, _) = env.json(&["synth", "--base-rev", "rev3"]);
    assert_eq!(code, 0);
    let (code, v) = env.json(&[
        "opt-timing",
        "--base-rev",
        "rev4",
        "--stop-when",
        "wns_ns >= -1000",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        (v["stop_reason"].as_str(), v["iterations_run"].as_u64()),
        (Some("condition_met"), Some(0))
    );
    assert_eq!(v["scope"]["stop_when"], "wns_ns >= -1000");
    assert_eq!(v["changes"], serde_json::json!([]));
    env.assert_equivalent(v["new_rev"].as_str().unwrap());

    revert(&ws, "rev4");
    let (code, v) = env.json(&[
        "opt-timing",
        "--base-rev",
        "rev4",
        "--max-iters",
        "1",
        "--stop-when",
        "wns>=0",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["scope"]["stop_when"], "wns_ns >= 0");
    assert_eq!(v["iterations_run"], 1, "{v}");
    assert_eq!(v["stop_reason"], "max_iters");
    env.assert_equivalent(v["new_rev"].as_str().unwrap());

    revert(&ws, "rev4");
    let (code, v) = env.json(&[
        "opt-timing",
        "--base-rev",
        "rev4",
        "--max-area-growth-pct",
        "0",
    ]);
    assert_eq!(code, 0, "{v}");
    let before = v["metrics"]["area_before_um2"].as_f64().unwrap();
    assert_eq!(v["scope"]["area_limit_um2"].as_f64(), Some(before));
    assert!(v["metrics"]["area_um2"].as_f64().unwrap() <= before + 1e-9);
    assert!(
        matches!(
            v["stop_reason"].as_str(),
            Some("area_limit" | "no_improvement" | "condition_met")
        ),
        "{v}"
    );
    env.assert_equivalent(v["new_rev"].as_str().unwrap());

    for bad in ["slack > 0", "wns_ns >= inf", "no_improvement"] {
        let (code, v) = env.json(&["opt-timing", "--base-rev", "rev4", "--stop-when", bad]);
        assert_eq!(code, 1, "{v}");
        assert_eq!(v["error"]["code"], "USAGE", "{bad}");
    }
    let (code, v) = env.json(&["opt-timing", "--base-rev", "rev4", "--max-iters", "0"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "USAGE");
}

// CLI-REQ-0212: a repeated request id returns the committed revision without a new one; the
// id must be shell-safe and belong to the same base.
#[test]
fn request_ids_replay_their_revision() {
    let env = WsEnv::new("request");
    let ws = env.seed("pipeline", Some(SDC));
    let (code, _) = env.json(&["synth", "--base-rev", "rev3"]);
    assert_eq!(code, 0);
    let args = [
        "opt-timing",
        "--base-rev",
        "rev4",
        "--max-iters",
        "1",
        "--request-id",
        "req-1",
    ];
    let (code, first) = env.json(&args);
    assert_eq!(code, 0, "{first}");
    assert_eq!(first["new_rev"], "rev5");
    assert_eq!(first["replayed"], false);
    assert_eq!(first["request_id"], "req-1");
    let lines = ws.history().unwrap().len();
    let (code, again) = env.json(&args);
    assert_eq!(code, 0, "{again}");
    assert_eq!(again["replayed"], true);
    assert_eq!(again["new_rev"], "rev5");
    assert_eq!(again["base_rev"], "rev4");
    assert_eq!(again["metrics"]["area_um2"], first["metrics"]["area_um2"]);
    assert_eq!(ws_head(&ws), "rev5", "no new revision");
    assert_eq!(ws.history().unwrap().len(), lines, "no history line");
    let text = env.run(&args);
    assert_eq!(text.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&text.stdout).contains("replayed: request id req-1"));
    let checked = env.assert_equivalent("rev5");

    let (code, v) = env.json(&[
        "opt-timing",
        "--base-rev",
        &checked,
        "--max-iters",
        "1",
        "--request-id",
        "req-1",
    ]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "INPUT_REJECTED");
    let (code, v) = env.json(&["opt-timing", "--base-rev", &checked, "--request-id", "a b"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "USAGE");

    let resize = [
        "resize",
        "--base-rev",
        &checked,
        "--target",
        "inst2",
        "--request-id",
        "r2",
    ];
    let (code, v) = env.json(&resize);
    assert_eq!(code, 0, "{v}");
    let new_rev = v["new_rev"].as_str().unwrap().to_string();
    let (code, v) = env.json(&resize);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        (v["replayed"].as_bool(), v["new_rev"].as_str()),
        (Some(true), Some(new_rev.as_str()))
    );
    let recorded = ws.history().unwrap();
    let last = recorded
        .iter()
        .rev()
        .find(|h| h.command == "rustsys resize")
        .unwrap();
    assert!(
        last.argv
            .windows(2)
            .any(|w| w[0] == "--request-id" && w[1] == "r2")
    );
    env.assert_equivalent(&new_rev);
}
