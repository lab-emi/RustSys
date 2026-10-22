// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Post-mapping workspace commands: the staging of a changed netlist shared by `opt-timing` and
//! `resize`, request-id replays, and `rustsys -w <WS> resize`.
//!
//! Covers: CLI-REQ-0204, CLI-REQ-0206, CLI-REQ-0211, CLI-REQ-0212, CLI-REQ-0213.

use std::collections::{BTreeMap, HashMap};
use std::process::ExitCode;

use rusteda_core::liberty::{Cell, Direction as LibDir, Library};
use rusteda_core::netlist::GateNetlist;
use rusteda_formats::verilog::netlist::from_gate_netlist;
use rusteda_formats::verilog::write::NlModule;
use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    Corruption, DiagCode, Diagnostic, Envelope, Error, HistoryEntry, Invocation, JsonObject,
    RawJson, Rev, Severity, Txn, Workspace, netlist,
};
use rustsys::view::{CellLib, NetlistEstimate, Report as SynthReport, netlist_estimate};

use crate::Globals;
use crate::args::{FormatArg, ResizeArgs};
use crate::ws::{
    OutputOutcome, finish_error, finish_ok, load_libraries_from_rev, nl_to_gate_netlist, request,
    targets,
};

/// The note of a request-id replay (CLI-REQ-0212).
const REPLAYED: DiagCode = DiagCode::of("A0111");

fn is_usable_cell(c: &Cell) -> bool {
    !c.dont_use
        && !c.dont_touch
        && !c.is_macro_cell
        && !c.pad_cell
        && !c.is_isolation_cell
        && !c.is_level_shifter
        && c.level_shifter_type.is_none()
        && !c.always_on
        && c.switch_cell_type.is_none()
        && c.retention_cell.is_none()
        && c.antenna_diode_type.is_none()
        && !c.is_clock_gating
        && c.clock_gating_integrated_cell.is_none()
        && !c.has_statetable
        && !c.has_bus
        && c.problems.is_empty()
        && !c.pins.iter().any(|p| {
            p.isolation_cell_enable_pin
                || p.isolation_cell_data_pin
                || p.level_shifter_enable_pin
                || p.level_shifter_data_pin
                || p.always_on
        })
}

/// Checks whether two library cells belong to the same functional family (matching non-internal
/// pin names, directions, output Boolean functions, and sequential definitions).
fn same_functional_family(a: &Cell, b: &Cell) -> bool {
    if !is_usable_cell(b) {
        return false;
    }
    let a_pins: Vec<_> = a
        .pins
        .iter()
        .filter(|p| p.direction != LibDir::Internal)
        .collect();
    let b_pins: Vec<_> = b
        .pins
        .iter()
        .filter(|p| p.direction != LibDir::Internal)
        .collect();
    if a_pins.len() != b_pins.len() || a.sequentials.len() != b.sequentials.len() {
        return false;
    }
    for pa in &a_pins {
        let Some(pb) = b_pins.iter().find(|p| p.name == pa.name) else {
            return false;
        };
        if pa.direction != pb.direction || pa.function != pb.function {
            return false;
        }
    }
    for (sa, sb) in a.sequentials.iter().zip(&b.sequentials) {
        if sa.kind != sb.kind
            || sa.data != sb.data
            || sa.clock != sb.clock
            || sa.clear != sb.clear
            || sa.preset != sb.preset
        {
            return false;
        }
    }
    true
}

pub(crate) fn find_cell<'a>(libs: &'a [Library], name: &str) -> Option<&'a Cell> {
    libs.iter()
        .find_map(|lib| lib.cells.iter().find(|c| c.name == name))
}

/// The cells of the family of `ref_cell`, smallest area first, then by name.
fn family_variants<'a>(libs: &'a [Library], ref_cell: &Cell) -> Vec<&'a Cell> {
    let mut members: Vec<&'a Cell> = libs
        .iter()
        .flat_map(|l| l.cells.iter())
        .filter(|c| same_functional_family(ref_cell, c))
        .collect();
    members.sort_by(|a, b| a.area.total_cmp(&b.area).then_with(|| a.name.cmp(&b.name)));
    members.dedup_by(|a, b| a.name == b.name);
    members
}

fn compute_netlist_stats(
    nl: &NlModule,
    libs: &[Library],
) -> (f64, BTreeMap<String, usize>, BTreeMap<String, f64>) {
    let mut total_area = 0.0;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut cell_area: BTreeMap<String, f64> = BTreeMap::new();
    for inst in &nl.instances {
        *counts.entry(inst.cell.clone()).or_insert(0) += 1;
        if let Some(c) = find_cell(libs, &inst.cell) {
            total_area += c.area;
            cell_area.entry(inst.cell.clone()).or_insert(c.area);
        }
    }
    (total_area, counts, cell_area)
}

/// The area of `nl` in library area units.
pub(crate) fn netlist_area(nl: &NlModule, libs: &[Library]) -> f64 {
    compute_netlist_stats(nl, libs).0
}

/// The metrics of a staged netlist, all computed from it (CLI-REQ-0213).
#[derive(Debug, Clone)]
pub(crate) struct NetlistMetrics {
    pub area: f64,
    pub cells: usize,
    pub estimate: NetlistEstimate,
}

/// Stages `nl` as the new `netlist` table and republishes the `synth` report of the base with
/// every value that describes the netlist recomputed from it (CLI-REQ-0213): area, cells, cell
/// areas, cell classes, logic depth and the mapping delay estimate (SYNTH-REQ-0565); the
/// electrical report of synthesis, which no longer describes the netlist, is removed.
pub(crate) fn stage_netlist_and_synth(
    txn: &mut Txn,
    nl: &NlModule,
    libs: &[Library],
    eligible: &CellLib,
    diags: &mut Vec<Diagnostic>,
) -> Result<(GateNetlist, NetlistMetrics), Error> {
    let gate_netlist = nl_to_gate_netlist(nl, libs)?;
    let frozen = gate_netlist.freeze();
    let up_names: &[&str] = if txn.base().has_table("rtl") {
        &["rtl", "libraries"]
    } else {
        &["libraries"]
    };
    let netlist_up = txn.upstream(up_names)?;
    netlist::add_to_txn(txn, &frozen, netlist_up)?;

    let (area, cell_counts, cell_area) = compute_netlist_stats(nl, libs);
    let total_cells: usize = cell_counts.values().sum();
    let estimate = netlist_estimate(nl, eligible);
    if estimate.unmodelled_cells + estimate.cyclic_cells > 0 {
        diags.push(Diagnostic::new(
            Severity::Warning,
            DiagCode::of("S0538"),
            format!(
                "the logic depth and delay estimate leave out {} instances of cells outside the \
                 classified library and {} instances on combinational cycles",
                estimate.unmodelled_cells, estimate.cyclic_cells
            ),
        ));
    }

    txn.set_summary("netlist", "area", area)?;
    txn.set_summary("netlist", "cells", total_cells)?;
    txn.set_summary("netlist", "instances", gate_netlist.instances().count())?;
    txn.set_summary("netlist", "nets", gate_netlist.nets().count())?;
    txn.set_summary("netlist", "ports", gate_netlist.ports().len())?;

    if txn.base().has_table("synth") {
        let bytes = txn.base().read_table("synth")?;
        let path = txn.base().table_path("synth")?;
        let mut sr: SynthReport = serde_json::from_slice(&bytes).map_err(|e| {
            Error::corrupt(
                path,
                Corruption::Json {
                    reason: format!("synth.json: {e}"),
                },
            )
        })?;
        sr.area = area;
        sr.cells = cell_counts;
        sr.cell_area = cell_area;
        sr.sequential_cells = estimate.sequential_cells;
        sr.combinational_cells = estimate.combinational_cells;
        sr.inverters = estimate.inverters;
        sr.logic_depth = estimate.logic_depth;
        sr.estimated_delay = estimate.estimated_delay;
        sr.electrical = None;
        let synth_bytes = serde_json::to_vec_pretty(&sr)
            .map_err(|e| Error::internal(format!("serializing synth.json: {e}")))?;
        let synth_up = txn.upstream(up_names)?;
        txn.add_bytes("synth", "synth.json", &synth_bytes, synth_up)?;
        txn.set_summary("synth", "area", sr.area)?;
        txn.set_summary("synth", "cells", total_cells)?;
        txn.set_summary("synth", "logic_depth", sr.logic_depth)?;
        txn.set_summary("synth", "estimated_delay", sr.estimated_delay)?;
    }

    Ok((
        gate_netlist,
        NetlistMetrics {
            area,
            cells: total_cells,
            estimate,
        },
    ))
}

/// The instances whose cell changed and the instances that are new, against `before`, in the
/// instance order of `after`, with their `inst<N>` handles in the new revision (CLI-REQ-0211).
pub(crate) fn netlist_changes(before: &NlModule, after: &NlModule) -> Vec<serde_json::Value> {
    let old: HashMap<&str, &str> = before
        .instances
        .iter()
        .map(|i| (i.name.as_str(), i.cell.as_str()))
        .collect();
    after
        .instances
        .iter()
        .enumerate()
        .filter_map(|(row, i)| {
            let handle = format!("inst{}", row + 1);
            match old.get(i.name.as_str()) {
                Some(&cell) if cell != i.cell => Some(serde_json::json!({
                    "change": "resize",
                    "handle": handle,
                    "instance": i.name,
                    "from_cell": cell,
                    "to_cell": i.cell,
                })),
                Some(_) => None,
                None => Some(serde_json::json!({
                    "change": "insert",
                    "handle": handle,
                    "instance": i.name,
                    "from_cell": null,
                    "to_cell": i.cell,
                })),
            }
        })
        .collect()
}

/// The answer to a request whose id the history records as committed (CLI-REQ-0212): that
/// revision, and nothing run or committed.
pub(crate) fn replay_outcome(
    ws_arg: &str,
    command: &str,
    id: &str,
    entry: &HistoryEntry,
) -> Result<OutputOutcome, Error> {
    let new_rev = entry
        .new_rev
        .ok_or_else(|| Error::internal("a replayed history entry has a new revision"))?;
    let mut env = Envelope::new(command);
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = entry.base_rev;
    env.rev = Some(new_rev);
    env.new_rev = Some(new_rev);
    let mut scope = JsonObject::new();
    scope.insert("request_id", id)?;
    scope.insert("source", "history.jsonl")?;
    env.set_scope(scope)?;
    env.set_metrics(RawJson::new(&entry.metrics)?)?;
    env.set("replayed", &true)?;
    env.set("request_id", id)?;
    env.diagnostics.push(Diagnostic::new(
        Severity::Note,
        REPLAYED,
        format!(
            "request id '{id}' already committed {new_rev}; nothing was run and nothing committed"
        ),
    ));
    let new_s = new_rev.to_string();
    env.push_next_action(&["rustsys", "-w", ws_arg, "check", "--base-rev", &new_s]);
    if let Some(base) = entry.base_rev {
        env.push_next_action(&[
            "rusteda",
            "diff",
            "-w",
            ws_arg,
            "--from",
            &base.to_string(),
            "--to",
            &new_s,
        ]);
    }
    let mut text_lines = vec![format!(
        "replayed: request id {id} committed {new_rev}{}; nothing was run",
        entry
            .base_rev
            .map_or_else(String::new, |b| format!(" (base {b})"))
    )];
    for next in &env.next_actions {
        text_lines.push(next_line(next));
    }
    Ok(OutputOutcome {
        envelope: env,
        text_lines,
        exit_code: 0,
    })
}

/// The steps before a post-mapping transaction: the request id (checked, and replayed when the
/// history has its commit) and the base revision. `Err` holds the exit code of a finished
/// command (CLI-REQ-0212).
pub(crate) fn prepare(
    ws: &Workspace,
    ws_arg: &str,
    command: &str,
    base_rev: &str,
    request_id: Option<&str>,
    inv: &Invocation,
    format: FormatArg,
    quiet: bool,
) -> Result<rusteda_workspace::BaseRev, ExitCode> {
    let parsed = Rev::parse(base_rev).ok();
    let fail = |e: Error| {
        let _ = ws.log_rejected(inv, parsed, &e);
        finish_error(command, Some(ws_arg), parsed, e, Vec::new(), format, quiet)
    };
    if let Some(id) = request_id {
        request::check(id).map_err(fail)?;
    }
    let base = ws.resolve_base(base_rev).map_err(fail)?;
    if let Some(id) = request_id {
        match request::replay(ws, command, id, Some(base.rev())) {
            Ok(Some(entry)) => {
                return Err(match replay_outcome(ws_arg, command, id, &entry) {
                    Ok(outcome) => finish_ok(outcome, format, quiet),
                    Err(e) => {
                        finish_error(command, Some(ws_arg), parsed, e, Vec::new(), format, quiet)
                    }
                });
            }
            Ok(None) => {}
            Err(e) => return Err(fail(e)),
        }
    }
    Ok(base)
}

pub(crate) fn run_ws_resize(
    ws: &Workspace,
    ws_arg: &str,
    args: &ResizeArgs,
    _g: &Globals,
    inv: &Invocation,
    format: FormatArg,
    quiet: bool,
) -> ExitCode {
    let cmd_name = "rustsys resize";
    let base = match prepare(
        ws,
        ws_arg,
        cmd_name,
        &args.base_rev,
        args.request_id.as_deref(),
        inv,
        format,
        quiet,
    ) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let mut txn = match ws.begin(base, inv) {
        Ok(t) => t,
        Err(e) => {
            return finish_error(
                cmd_name,
                Some(ws_arg),
                Some(base.rev()),
                e,
                Vec::new(),
                format,
                quiet,
            );
        }
    };
    if let Some(msg) = &args.message {
        txn.set_message(msg.clone());
    }
    let mut diags = Vec::new();
    let staged = match stage_ws_resize(ws, &mut txn, args, &mut diags) {
        Ok(s) => s,
        Err(e) => {
            let _ = txn.reject(&e);
            return finish_error(
                cmd_name,
                Some(ws_arg),
                Some(base.rev()),
                e,
                diags,
                format,
                quiet,
            );
        }
    };
    match finish_ws_resize(ws_arg, txn, staged, args, &mut diags) {
        Ok(outcome) => finish_ok(outcome, format, quiet),
        Err(e) => finish_error(
            cmd_name,
            Some(ws_arg),
            Some(base.rev()),
            e,
            diags,
            format,
            quiet,
        ),
    }
}

struct StagedResize {
    primary_corner: String,
    area_before: f64,
    metrics: NetlistMetrics,
    targets: targets::Resolved,
    resized_count: usize,
    changes: Vec<serde_json::Value>,
}

/// The cell for `old` under `--drive-multiplier k` (CLI-REQ-0204): with `k >= 1` the strictly
/// larger variant whose area is closest to `k` times the current area, with `k < 1` the strictly
/// smaller one; ties to the smaller area, then the name; none: the cell stays.
fn multiplied<'a>(family: &[&'a Cell], old: &'a Cell, k: f64) -> &'a Cell {
    let desired = old.area * k;
    family
        .iter()
        .copied()
        .filter(|c| {
            if k >= 1.0 {
                c.area > old.area + 1e-9
            } else {
                c.area < old.area - 1e-9
            }
        })
        .min_by(|a, b| {
            (a.area - desired)
                .abs()
                .total_cmp(&(b.area - desired).abs())
                .then(a.area.total_cmp(&b.area))
                .then_with(|| a.name.cmp(&b.name))
        })
        .unwrap_or(old)
}

fn stage_ws_resize(
    ws: &Workspace,
    txn: &mut Txn,
    args: &ResizeArgs,
    diags: &mut Vec<Diagnostic>,
) -> Result<StagedResize, Error> {
    let (primary_corner, libs, _extra_libs, _unread) = load_libraries_from_rev(ws, txn.base())?;
    let eligible = CellLib::build(&libs, &[]).map_err(Error::input_rejected)?;
    let g_before = netlist::thaw_rev(txn.base())?;
    let mut nl = from_gate_netlist(&g_before)
        .map_err(|e| Error::input_rejected(format!("cannot convert netlist for resize: {e}")))?;
    let before = nl.clone();
    let area_before = netlist_area(&nl, &libs);
    let resolved = targets::resolve(ws, txn.base(), &g_before, &nl, &args.targets, false, diags)?;
    let explicit = match &args.cell {
        Some(name) => Some(find_cell(&libs, name).ok_or_else(|| {
            Error::input_rejected(format!("target cell '{name}' not found in libraries"))
        })?),
        None => None,
    };
    let mut resized_count = 0usize;
    for &idx in &resolved.instances {
        let inst_name = nl.instances[idx].name.clone();
        let old_cell_name = nl.instances[idx].cell.clone();
        let Some(old_cell) = find_cell(&libs, &old_cell_name) else {
            return Err(Error::input_rejected(format!(
                "instance '{inst_name}' uses unknown library cell '{old_cell_name}'"
            )));
        };
        let new_cell = match explicit {
            Some(cand) => {
                if !same_functional_family(old_cell, cand) {
                    return Err(Error::input_rejected(format!(
                        "cell '{}' is not functionally compatible with '{old_cell_name}'",
                        cand.name
                    )));
                }
                cand
            }
            None => multiplied(
                &family_variants(&libs, old_cell),
                old_cell,
                args.drive_multiplier,
            ),
        };
        if new_cell.name != old_cell_name {
            nl.instances[idx].cell = new_cell.name.clone();
            resized_count += 1;
        }
    }
    let (_g_after, metrics) = stage_netlist_and_synth(txn, &nl, &libs, &eligible, diags)?;
    txn.set_summary("netlist", "resized_cells", resized_count)?;
    Ok(StagedResize {
        primary_corner,
        area_before,
        metrics,
        targets: resolved,
        resized_count,
        changes: netlist_changes(&before, &nl),
    })
}

fn finish_ws_resize(
    ws_arg: &str,
    txn: Txn,
    staged: StagedResize,
    args: &ResizeArgs,
    diags: &mut Vec<Diagnostic>,
) -> Result<OutputOutcome, Error> {
    let StagedResize {
        primary_corner,
        area_before,
        metrics,
        targets,
        resized_count,
        changes,
    } = staged;
    let base_rev = txn.base().rev();
    let corners = txn.base().corners();
    let mut hist_metrics = rusteda_workspace::JsonMap::new();
    hist_metrics.insert("area_um2".to_string(), serde_json::json!(metrics.area));
    hist_metrics.insert("cells".to_string(), serde_json::json!(metrics.cells));
    hist_metrics.insert(
        "resized_cells".to_string(),
        serde_json::json!(resized_count),
    );
    let committed = txn.commit(hist_metrics)?;
    diags.extend(committed.warnings);

    let mut env = Envelope::new("rustsys resize");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(base_rev);
    env.rev = Some(committed.rev);
    env.new_rev = Some(committed.rev);
    env.diagnostics = std::mem::take(diags);

    let mut scope = JsonObject::new();
    scope.insert("primary_corner", &primary_corner)?;
    scope.insert("corners", &corners)?;
    scope.insert("targets", &args.targets)?;
    scope.insert("target_source", targets.source.as_str())?;
    scope.insert("target_instances", &targets.instances.len())?;
    scope.insert("target_paths", &targets.paths)?;
    match &args.cell {
        Some(cell) => scope.insert("cell", cell)?,
        None => scope.insert("drive_multiplier", &args.drive_multiplier)?,
    };
    env.set_scope(scope)?;

    let mut m = JsonObject::new();
    m.insert("area_before_um2", &area_before)?;
    m.insert("area_um2", &metrics.area)?;
    m.insert("cells", &metrics.cells)?;
    m.insert("resized_cells", &resized_count)?;
    m.insert("logic_depth", &metrics.estimate.logic_depth)?;
    m.insert("estimated_delay", &metrics.estimate.estimated_delay)?;
    env.set_metrics(m)?;

    env.set("replayed", &false)?;
    env.set("request_id", &args.request_id)?;
    env.set("resized_cells", &resized_count)?;
    env.set("changes", &changes)?;

    let new_rev_s = committed.rev.to_string();
    env.push_next_action(&["rustsys", "-w", ws_arg, "check", "--base-rev", &new_rev_s]);
    env.push_next_action(&[
        "rusteda",
        "diff",
        "-w",
        ws_arg,
        "--from",
        &base_rev.to_string(),
        "--to",
        &new_rev_s,
    ]);

    let mut text_lines = vec![
        format!(
            "resized {} of {} target instance(s): area {:.4} -> {:.4} (cells {})",
            resized_count,
            targets.instances.len(),
            area_before,
            metrics.area,
            metrics.cells
        ),
        format!("committed {} (base {base_rev})", committed.rev),
    ];
    for next in &env.next_actions {
        text_lines.push(next_line(next));
    }
    Ok(OutputOutcome {
        envelope: env,
        text_lines,
        exit_code: 0,
    })
}
