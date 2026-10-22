// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `rustsys -w <WS> opt-timing`: one bounded step of timing-driven post-mapping optimization,
//! committed as a revision (plan Example 2).
//!
//! The targets come from the timing table of the base revision (CLI-REQ-0206, CLI-REQ-0207),
//! the timing target from its constraints table (CLI-REQ-0209); the engine's incremental mode
//! changes only the targets and the nets they drive, starting from the cells of the netlist,
//! for at most `--max-iters` iterations or until `--stop-when` holds (CLI-REQ-0205,
//! CLI-REQ-0208, CLI-REQ-0210). Every metric is computed from the committed netlist with the
//! estimate's scope stated (CLI-REQ-0213).
//!
//! Covers: CLI-REQ-0205, CLI-REQ-0208, CLI-REQ-0209, CLI-REQ-0210, CLI-REQ-0213.

use std::process::ExitCode;

use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::verilog::netlist::from_gate_netlist;
use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    Diagnostic, Envelope, Error, Invocation, JsonObject, Rev, Workspace, netlist,
};
use rustsys::view::{
    CellLib, ElectricalOptions, ElectricalReport, Incremental, SearchEffort, StopRule,
    refine_corners,
};

use crate::Globals;
use crate::args::{EffortArg, FormatArg, OptTimingArgs};
use crate::ws::constraints::{self, Derived};
use crate::ws::opt::{
    NetlistMetrics, netlist_area, netlist_changes, prepare, stage_netlist_and_synth,
};
use crate::ws::targets::{self, Resolved, Source};
use crate::ws::{DiagCollector, OutputOutcome, finish_error, finish_ok, load_libraries_from_rev};

/// The stop condition without `--stop-when` (CLI-REQ-0210).
pub(crate) const DEFAULT_STOP: &str = "wns_ns >= 0";

/// Reads a stop condition (CLI-REQ-0210): `wns_ns >= X`, `tns_ns >= X` (spaces optional, `X` a
/// finite number; `wns` and `tns` are aliases) or `no-improvement`. Returns the rule and its
/// normalized text; anything else is `USAGE`.
pub(crate) fn parse_stop(text: &str) -> Result<(StopRule, String), Error> {
    let t = text.trim();
    if t == "no-improvement" {
        return Ok((StopRule::NoImprovement, t.to_string()));
    }
    let bad = || {
        Error::usage(format!(
            "invalid --stop-when '{text}'; expected 'wns_ns >= X', 'tns_ns >= X' (X a finite \
             number in ns) or 'no-improvement'"
        ))
    };
    let (lhs, rhs) = t.split_once(">=").ok_or_else(bad)?;
    let value: f64 = rhs.trim().parse().map_err(|_| bad())?;
    if !value.is_finite() {
        return Err(bad());
    }
    let value = if value == 0.0 { 0.0 } else { value };
    match lhs.trim() {
        "wns_ns" | "wns" => Ok((StopRule::WnsAtLeast(value), format!("wns_ns >= {value}"))),
        "tns_ns" | "tns" => Ok((StopRule::TnsAtLeast(value), format!("tns_ns >= {value}"))),
        _ => Err(bad()),
    }
}

fn effort(e: EffortArg) -> SearchEffort {
    match e {
        EffortArg::Low => SearchEffort::Low,
        EffortArg::Medium => SearchEffort::Medium,
        EffortArg::High => SearchEffort::High,
    }
}

fn effort_name(e: EffortArg) -> &'static str {
    match e {
        EffortArg::Low => "low",
        EffortArg::Medium => "medium",
        EffortArg::High => "high",
    }
}

pub(crate) fn run_ws_opt_timing(
    ws: &Workspace,
    ws_arg: &str,
    args: &OptTimingArgs,
    _g: &Globals,
    inv: &Invocation,
    format: FormatArg,
    quiet: bool,
) -> ExitCode {
    let cmd_name = "rustsys opt-timing";
    let stop = match args.stop_when.as_deref().map(parse_stop).transpose() {
        Ok(s) => s,
        Err(e) => {
            let parsed = Rev::parse(&args.base_rev).ok();
            let _ = ws.log_rejected(inv, parsed, &e);
            return finish_error(cmd_name, Some(ws_arg), parsed, e, Vec::new(), format, quiet);
        }
    };
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
    let (rule, stop_text) = stop.unwrap_or((StopRule::WnsAtLeast(0.0), DEFAULT_STOP.to_string()));
    let mut diags = Vec::new();
    let staged = match stage(ws, &mut txn, args, rule, &mut diags) {
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
    match finish(ws_arg, txn, staged, args, &stop_text, &mut diags) {
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

struct Staged {
    primary_corner: String,
    area_before: f64,
    area_limit: Option<f64>,
    derived: Derived,
    targets: Resolved,
    report: ElectricalReport,
    metrics: NetlistMetrics,
    changes: Vec<serde_json::Value>,
}

fn stage(
    ws: &Workspace,
    txn: &mut rusteda_workspace::Txn,
    args: &OptTimingArgs,
    stop: StopRule,
    diags: &mut Vec<Diagnostic>,
) -> Result<Staged, Error> {
    let (primary_corner, libs, extra_libs, _unread) = load_libraries_from_rev(ws, txn.base())?;
    let lib = libs
        .first()
        .ok_or_else(|| Error::input_rejected("the primary corner has no library"))?;
    let eligible = CellLib::build(&libs, &[]).map_err(Error::input_rejected)?;
    let g_before = netlist::thaw_rev(txn.base())?;
    let mut nl = from_gate_netlist(&g_before).map_err(|e| {
        Error::input_rejected(format!("cannot convert netlist for opt-timing: {e}"))
    })?;
    let before = nl.clone();
    let area_before = netlist_area(&nl, &libs);
    let resolved = targets::resolve(ws, txn.base(), &g_before, &nl, &args.targets, true, diags)?;
    let derived = constraints::derive(txn.base(), args, lib, diags)?;
    let growth = args
        .max_area_growth_pct
        .map(|p| area_before * (1.0 + p / 100.0))
        .filter(|a| *a > 0.0);
    let area_limit = match (args.area_limit, growth) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let focus = match resolved.source {
        Source::AllInstances => None,
        Source::Cli | Source::TimingTable => Some(
            resolved
                .instances
                .iter()
                .map(|&i| nl.instances[i].name.clone())
                .collect(),
        ),
    };
    let t = &derived.target;
    let opts = ElectricalOptions {
        period_ns: t.period_ns,
        area_limit,
        input_delay_ns: t.input_delay_ns,
        output_delay_ns: t.output_delay_ns,
        input_slew_ns: t.input_slew_ns,
        clock_slew_ns: t.clock_slew_ns,
        output_load_pf: t.output_load_pf,
        margin_ns: t.margin_ns,
        buffer_fanout: args.buffer_fanout as usize,
        parallel_timing: Default::default(),
        incremental: Some(Incremental {
            focus,
            max_iterations: args.max_iters as usize,
            stop,
            effort: effort(args.effort),
        }),
    };
    let collector = DiagCollector::default();
    let ctx = Ctx::new(None, Some(&collector));
    let report = match refine_corners(&mut nl, &libs, &eligible, &opts, &extra_libs, ctx) {
        Ok(rep) => rep,
        Err(JobError::Cancelled) => return Err(Error::Cancelled),
        Err(JobError::Failed(msg)) => return Err(Error::input_rejected(msg)),
    };
    diags.extend(collector.into_vec());
    let (_g_after, metrics) = stage_netlist_and_synth(txn, &nl, &libs, &eligible, diags)?;
    let inc = report
        .incremental
        .as_ref()
        .ok_or_else(|| Error::internal("the incremental mode reports its iterations"))?;
    txn.set_summary("netlist", "buffers_added", report.buffers_added)?;
    txn.set_summary("netlist", "resized_cells", report.resized_cells)?;
    txn.set_summary("netlist", "estimated_slack_ns", inc.wns_after_ns)?;
    txn.set_summary("netlist", "estimated_tns_ns", inc.tns_after_ns)?;
    txn.set_summary("netlist", "iterations_run", inc.iterations_run)?;
    Ok(Staged {
        primary_corner,
        area_before,
        area_limit,
        derived,
        targets: resolved,
        changes: netlist_changes(&before, &nl),
        report,
        metrics,
    })
}

fn finish(
    ws_arg: &str,
    txn: rusteda_workspace::Txn,
    staged: Staged,
    args: &OptTimingArgs,
    stop_text: &str,
    diags: &mut Vec<Diagnostic>,
) -> Result<OutputOutcome, Error> {
    let Staged {
        primary_corner,
        area_before,
        area_limit,
        derived,
        targets,
        report,
        metrics,
        changes,
    } = staged;
    let inc = report
        .incremental
        .ok_or_else(|| Error::internal("the incremental mode reports its iterations"))?;
    let base_rev = txn.base().rev();
    let corners = txn.base().corners();
    let mut hist = rusteda_workspace::JsonMap::new();
    hist.insert("area_um2".into(), serde_json::json!(metrics.area));
    hist.insert("cells".into(), serde_json::json!(metrics.cells));
    hist.insert(
        "buffers_added".into(),
        serde_json::json!(report.buffers_added),
    );
    hist.insert(
        "resized_cells".into(),
        serde_json::json!(report.resized_cells),
    );
    hist.insert(
        "iterations_run".into(),
        serde_json::json!(inc.iterations_run),
    );
    hist.insert(
        "estimated_slack_ns".into(),
        serde_json::json!(inc.wns_after_ns),
    );
    let committed = txn.commit(hist)?;
    diags.extend(committed.warnings);

    let mut env = Envelope::new("rustsys opt-timing");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(base_rev);
    env.rev = Some(committed.rev);
    env.new_rev = Some(committed.rev);
    env.diagnostics = std::mem::take(diags);

    let mut scope = JsonObject::new();
    scope.insert("primary_corner", &primary_corner)?;
    scope.insert("corners", &corners)?;
    scope.insert("timing_source", "estimate")?;
    scope.insert("clock_model", "ideal")?;
    scope.insert("wire_model", "none")?;
    scope.insert("period_ns", &derived.target.period_ns)?;
    scope.insert("constraints_source", derived.source)?;
    scope.insert("constraints", &derived.target)?;
    scope.insert("constraints_clocks", &derived.clocks)?;
    scope.insert("cli_overrides", &derived.overrides)?;
    scope.insert("targets", &args.targets)?;
    scope.insert("target_source", targets.source.as_str())?;
    scope.insert("target_instances", &targets.instances.len())?;
    scope.insert("target_paths", &targets.paths)?;
    scope.insert(
        "timing_table",
        &targets.table_used.then(|| base_rev.to_string()),
    )?;
    scope.insert("effort", effort_name(args.effort))?;
    scope.insert("max_iters", &args.max_iters)?;
    scope.insert("stop_when", stop_text)?;
    scope.insert("area_limit_um2", &area_limit)?;
    scope.insert("max_area_growth_pct", &args.max_area_growth_pct)?;
    env.set_scope(scope)?;

    let mut completeness = JsonObject::new();
    completeness.insert("editable_instances", &inc.editable_instances)?;
    completeness.insert(
        "estimated_violating_endpoints",
        &report.estimated_violating_endpoints,
    )?;
    completeness.insert("data_rules_met", &report.estimated_data_rules_met)?;
    env.set_completeness(completeness)?;

    let growth_pct = if area_before > 0.0 {
        (metrics.area / area_before - 1.0) * 100.0
    } else {
        0.0
    };
    let mut m = JsonObject::new();
    m.insert("area_before_um2", &area_before)?;
    m.insert("area_um2", &metrics.area)?;
    m.insert("area_growth_pct", &growth_pct)?;
    m.insert("cells", &metrics.cells)?;
    m.insert("buffers_added", &report.buffers_added)?;
    m.insert("resized_cells", &report.resized_cells)?;
    m.insert("logic_depth", &metrics.estimate.logic_depth)?;
    m.insert("estimated_delay", &metrics.estimate.estimated_delay)?;
    m.insert("estimated_slack_ns", &inc.wns_after_ns)?;
    m.insert("estimated_tns_ns", &inc.tns_after_ns)?;
    m.insert("estimated_slack_before_ns", &inc.wns_before_ns)?;
    m.insert("estimated_tns_before_ns", &inc.tns_before_ns)?;
    m.insert("iterations_run", &inc.iterations_run)?;
    env.set_metrics(m)?;

    env.set("iterations_run", &inc.iterations_run)?;
    env.set("stop_reason", inc.stop_reason.as_str())?;
    env.set("replayed", &false)?;
    env.set("request_id", &args.request_id)?;
    env.set("changes", &changes)?;
    env.set("iterations", &inc.history)?;

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
            "opt-timing: estimated WNS {:.4} -> {:.4} ns, TNS {:.4} -> {:.4} ns (ideal clock, \
             no wires, period {} ns from {})",
            inc.wns_before_ns,
            inc.wns_after_ns,
            inc.tns_before_ns,
            inc.tns_after_ns,
            derived.target.period_ns,
            derived.source
        ),
        format!(
            "targets: {} instance(s) ({}); {} iteration(s), stop: {}; resized {}, buffers {}; \
             area {:.4} -> {:.4}",
            targets.instances.len(),
            targets.source.as_str(),
            inc.iterations_run,
            inc.stop_reason.as_str(),
            report.resized_cells,
            report.buffers_added,
            area_before,
            metrics.area
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

#[cfg(test)]
mod tests {
    use super::*;

    // CLI-REQ-0210: the grammar of --stop-when, with spaces optional and the short aliases.
    #[test]
    fn stop_conditions() {
        let ok = |t: &str| parse_stop(t).unwrap();
        assert_eq!(
            ok("wns_ns >= 0.0"),
            (StopRule::WnsAtLeast(0.0), "wns_ns >= 0".into())
        );
        assert_eq!(ok("wns_ns>=-0.25").0, StopRule::WnsAtLeast(-0.25));
        assert_eq!(ok(" tns_ns >= -1e-3 ").0, StopRule::TnsAtLeast(-1e-3));
        assert_eq!(ok("wns>=0").0, StopRule::WnsAtLeast(0.0));
        assert_eq!(ok("tns >= -0").1, "tns_ns >= 0");
        assert_eq!(ok("no-improvement").0, StopRule::NoImprovement);
        for bad in [
            "",
            "wns_ns > 0",
            "wns_ns >= inf",
            "wns_ns >= NaN",
            "slack >= 0",
            "wns_ns >= 1ns",
            "no_improvement",
            "wns_ns >=",
        ] {
            let e = parse_stop(bad).unwrap_err();
            assert_eq!(e.code(), rusteda_workspace::ErrorCode::Usage, "{bad:?}");
        }
    }
}
