// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Workspace synthesis command (`rustsys -w <WS> synth --base-rev <REV> [--check]`).
//!
//! Covers: CLI-REQ-0201.

use std::process::ExitCode;

use rusteda_core::job::{Ctx, Event, JobError, Observer};
use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, Invocation, JsonObject, Rev, Severity, Workspace,
    netlist,
};
use rustsys::view as synth_engine;
use rustsys_check::{CheckOptions, Input, Verdict};

use crate::Globals;
use crate::args::{EffortArg, FormatArg, GoalArg, SynthArgs};
use crate::out::{Err as ProgressOutput, ProgressMode};
use crate::proto::job::split_kv;
use crate::ws::{
    DiagCollector, OutputOutcome, finish_error, finish_ok, load_libraries_from_rev,
    load_rtl_from_rev, nl_to_frozen_netlist,
};

pub(crate) fn parse_after_reset(values: &[String]) -> Result<Vec<(String, bool)>, Error> {
    let mut out = Vec::with_capacity(values.len());
    for text in values {
        if let Some((name, value)) = text.split_once('=') {
            if !name.is_empty() && matches!(value, "0" | "1") {
                out.push((name.to_owned(), value == "1"));
                continue;
            }
        }
        return Err(Error::usage(format!(
            "invalid --after-reset {text:?}: expected NAME=0 or NAME=1"
        )));
    }
    Ok(out)
}

pub(crate) fn verdict_status_str(v: Verdict) -> &'static str {
    match v {
        Verdict::Equivalent => "equivalent",
        Verdict::InitialStateRefinement => "initial_state_refinement",
        Verdict::NotEquivalent => "not_equivalent",
        Verdict::Undecided => "undecided",
        Verdict::Error => "error",
    }
}

pub(crate) fn verdict_exit_code(v: Verdict) -> (i32, u8) {
    match v {
        Verdict::Equivalent | Verdict::InitialStateRefinement => (0, 0),
        Verdict::NotEquivalent => (3, 3),
        Verdict::Undecided => (5, 5),
        Verdict::Error => (1, 1),
    }
}

pub(crate) fn run_ws_synth(
    ws: &Workspace,
    ws_arg: &str,
    args: &SynthArgs,
    g: &Globals,
    inv: &Invocation,
    format: FormatArg,
    quiet: bool,
) -> ExitCode {
    let cmd_name = "rustsys synth";
    let Some(base_rev_str) = args.base_rev.as_deref() else {
        let e = Error::usage("rustsys synth in workspace mode requires --base-rev <REV>");
        let _ = ws.log_rejected(inv, None, &e);
        return finish_error(cmd_name, Some(ws_arg), None, e, Vec::new(), format, quiet);
    };
    let base_rev_parsed = Rev::parse(base_rev_str).ok();
    let base = match ws.resolve_base(base_rev_str) {
        Ok(b) => b,
        Err(e) => {
            let _ = ws.log_rejected(inv, base_rev_parsed, &e);
            return finish_error(
                cmd_name,
                Some(ws_arg),
                base_rev_parsed,
                e,
                Vec::new(),
                format,
                quiet,
            );
        }
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
    let staged = match stage_ws_synth(ws, &mut txn, args, g, &mut diags) {
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

    match finish_ws_synth(ws_arg, txn, staged, &mut diags) {
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

struct StagedSynth {
    primary_corner: String,
    report: synth_engine::Report,
    total_cells: usize,
    check_report_opt: Option<rustsys_check::CheckReport>,
    exit_code_u8: u8,
}

struct SynthEvents<'a> {
    diagnostics: &'a DiagCollector,
    progress: Option<&'a ProgressOutput>,
}

impl Observer for SynthEvents<'_> {
    fn event(&self, event: &Event) {
        self.diagnostics.event(event);
        // Diagnostics belong to the workspace response envelope. Progress is
        // streamed separately so long workspace runs honor --progress too.
        if !matches!(event, Event::Diagnostic(_))
            && let Some(progress) = self.progress
        {
            progress.event(event);
        }
    }
}

fn stage_ws_synth(
    ws: &Workspace,
    txn: &mut rusteda_workspace::Txn,
    args: &SynthArgs,
    g: &Globals,
    diags: &mut Vec<Diagnostic>,
) -> Result<StagedSynth, Error> {
    let (rtl_manifest, rtl_files, include_dirs) = load_rtl_from_rev(txn.base())?;
    let (primary_corner, libs, extra_libs, unread_liberty) =
        load_libraries_from_rev(ws, txn.base())?;
    let after_reset = parse_after_reset(&args.fe.after_reset)?;

    let Some(backend) = args.backend.0.map_backend() else {
        return Err(Error::BackendUnavailable(format!(
            "the {} runtime is not available in this build",
            args.backend.0.name()
        )));
    };

    let top = args
        .fe
        .top
        .clone()
        .unwrap_or_else(|| rtl_manifest.top.clone());

    let mut defines_map = rtl_manifest.defines.clone();
    for (k, v) in split_kv(&args.fe.define) {
        defines_map.insert(k, v);
    }
    let defines: Vec<(String, String)> = defines_map.into_iter().collect();

    let mut params_map = rtl_manifest.params.clone();
    for (k, v) in split_kv(&args.fe.param) {
        params_map.insert(k, v);
    }
    let params: Vec<(String, String)> = params_map.into_iter().collect();

    let no_predefined_macros = args.fe.no_predefined_macros || !rtl_manifest.predefined_macros;
    // Include directories belong to SystemVerilog snapshots (SYNTH-REQ-0901, CLI-REQ-0224).
    let include_dirs = if rtl_manifest.format.is_some() {
        Vec::new()
    } else {
        include_dirs
    };
    let initial_values = args.fe.initial_values.map(|m| m.job()).unwrap_or_default();

    let electrical = args
        .timing_period
        .map(|period_ns| synth_engine::ElectricalOptions {
            period_ns,
            area_limit: args.area_limit,
            input_delay_ns: args.input_delay,
            output_delay_ns: args.output_delay,
            input_slew_ns: args.input_slew,
            clock_slew_ns: args.clock_slew,
            output_load_pf: args.output_load,
            margin_ns: args.timing_margin,
            buffer_fanout: args.buffer_fanout as usize,
            parallel_timing: Default::default(),
            incremental: None,
        });

    let opts = synth_engine::SynthOptions {
        top: Some(top.clone()),
        defines: defines.clone(),
        include_dirs: include_dirs.clone(),
        params: params.clone(),
        no_predefined_macros,
        after_reset: after_reset.clone(),
        uninitialized_dont_care: args.fe.uninitialized_dont_care,
        assume_full_case: args.fe.assume_full_case,
        assume_parallel_case: args.fe.assume_parallel_case,
        dont_use: args.dont_use.clone(),
        effort: match args.effort {
            EffortArg::Low => synth_engine::Effort::Low,
            EffortArg::Medium => synth_engine::Effort::Medium,
            EffortArg::High => synth_engine::Effort::High,
        },
        goal: match args.goal {
            GoalArg::Delay => synth_engine::Goal::Delay,
            GoalArg::Area => synth_engine::Goal::Area,
        },
        delay_relax: args.delay_relax,
        electrical,
        default_quality: Some(Default::default()),
        electrical_corners: extra_libs,
        backend,
        gate_libs: Vec::new(),
        gate_netlist: false,
        initial_values: initial_values.engine(),
    };

    let collector = DiagCollector::default();
    let progress = (g.mode != ProgressMode::Off)
        .then(|| ProgressOutput::new(g.color, g.quiet, g.mode, "rustsys"));
    let events = SynthEvents {
        diagnostics: &collector,
        progress: progress.as_ref(),
    };
    let ctx = Ctx::new(None, Some(&events));
    let synth_res = synth_engine::synthesize_with(
        &synth_engine::Sources::Files(rtl_files.clone()),
        &libs,
        &opts,
        ctx,
    );
    diags.extend(collector.into_vec());

    let out_mod = match synth_res {
        Ok(m) => m,
        Err(JobError::Cancelled) => return Err(Error::Cancelled),
        Err(JobError::Failed(se)) => {
            let reason = se
                .messages
                .first()
                .cloned()
                .unwrap_or_else(|| "synthesis failed".to_string());
            if se.internal {
                return Err(Error::internal(reason));
            }
            return Err(Error::input_rejected(reason));
        }
    };

    let frozen = nl_to_frozen_netlist(&out_mod.netlist, &libs)?;
    let netlist_up = txn.upstream(&["rtl", "libraries"])?;
    netlist::add_to_txn(txn, &frozen, netlist_up)?;

    let synth_bytes = serde_json::to_vec_pretty(&out_mod.report)
        .map_err(|e| Error::internal(format!("serializing synth.json: {e}")))?;
    let synth_up = txn.upstream(&["rtl", "libraries"])?;
    txn.add_bytes("synth", "synth.json", &synth_bytes, synth_up)?;

    let r = &out_mod.report;
    let total_cells: usize = r.cells.values().sum();
    txn.set_summary("synth", "area", r.area)?;
    txn.set_summary("synth", "cells", total_cells)?;
    txn.set_summary("synth", "combinational_cells", r.combinational_cells)?;
    txn.set_summary("synth", "sequential_cells", r.sequential_cells)?;
    txn.set_summary("synth", "inverters", r.inverters)?;
    txn.set_summary("synth", "logic_depth", r.logic_depth)?;
    txn.set_summary("synth", "estimated_delay", r.estimated_delay)?;
    txn.set_summary("synth", "aig_nodes_before", r.aig_nodes_before)?;
    txn.set_summary("synth", "aig_nodes_after", r.aig_nodes_after)?;
    txn.set_summary("synth", "state_bits_rtl", r.state_bits_rtl)?;
    txn.set_summary("synth", "state_bits_retained", r.state_bits_retained)?;
    txn.set_summary("synth", "latches", r.latches)?;
    txn.set_summary("synth", "warnings", r.warnings)?;
    txn.set_summary("synth", "runtime_s", r.runtime_s)?;

    txn.set_summary("netlist", "area", r.area)?;
    txn.set_summary("netlist", "cells", total_cells)?;
    txn.set_summary("netlist", "instances", frozen.instances.len())?;
    txn.set_summary("netlist", "nets", frozen.nets.len())?;
    txn.set_summary("netlist", "ports", frozen.ports.len())?;

    let mut check_report_opt = None;
    let mut exit_code_u8 = 0u8;

    if args.check {
        let check_collector = DiagCollector::default();
        let check_ctx = Ctx::new(None, Some(&check_collector));
        let check_opts = CheckOptions {
            rtl: Input::Files(rtl_files),
            top: Some(top),
            defines,
            include_dirs,
            params,
            no_predefined_macros,
            after_reset,
            uninitialized_dont_care: args.fe.uninitialized_dont_care,
            assume_full_case: args.fe.assume_full_case,
            assume_parallel_case: args.fe.assume_parallel_case,
            initial_values: initial_values.checker(),
            netlist: Input::Texts(vec![("netlist.v".to_string(), out_mod.text.clone())]),
            verbose: g.verbose,
            unread_liberty,
            ..CheckOptions::default()
        };
        let check_rep = rustsys_check::check_with(&check_opts, &libs, check_ctx)
            .map_err(|_| Error::Cancelled)?;
        diags.extend(check_collector.into_vec());
        for u in &check_rep.undecided {
            diags.push(Diagnostic::new(
                Severity::Warning,
                DiagCode::of("C0005"),
                u.clone(),
            ));
        }
        if check_rep.verdict == Verdict::Error {
            let msg = check_rep
                .errors
                .first()
                .cloned()
                .unwrap_or_else(|| "equivalence check failed".to_string());
            let code = if check_rep.internal_error {
                DiagCode::of("A0002")
            } else {
                DiagCode::of("C0001")
            };
            diags.push(Diagnostic::new(Severity::Error, code, msg.clone()));
            if check_rep.internal_error {
                return Err(Error::internal(msg));
            }
            return Err(Error::input_rejected(msg));
        }

        let check_bytes = serde_json::to_vec_pretty(&check_rep)
            .map_err(|e| Error::internal(format!("serializing check.json: {e}")))?;
        let check_up = txn.upstream(&["rtl", "netlist", "libraries"])?;
        txn.add_bytes("check", "check.json", &check_bytes, check_up)?;
        let eq_status = verdict_status_str(check_rep.verdict);
        txn.set_status("check", "equivalence", eq_status)?;
        txn.set_summary("check", "mismatches", check_rep.mismatches.len())?;
        txn.set_summary("check", "outputs_checked", check_rep.outputs_checked)?;
        txn.set_summary(
            "check",
            "state_pairs_checked",
            check_rep.state_pairs_checked,
        )?;
        txn.set_summary("check", "sat_calls", check_rep.sat_calls)?;

        let (exit_i32, exit_u8) = verdict_exit_code(check_rep.verdict);
        txn.set_exit(exit_i32)?;
        exit_code_u8 = exit_u8;
        check_report_opt = Some(check_rep);
    }

    Ok(StagedSynth {
        primary_corner,
        report: out_mod.report,
        total_cells,
        check_report_opt,
        exit_code_u8,
    })
}

fn finish_ws_synth(
    ws_arg: &str,
    txn: rusteda_workspace::Txn,
    staged: StagedSynth,
    diags: &mut Vec<Diagnostic>,
) -> Result<OutputOutcome, Error> {
    let StagedSynth {
        primary_corner,
        report,
        total_cells,
        check_report_opt,
        exit_code_u8,
    } = staged;
    let r = &report;
    let base_rev = txn.base().rev();
    let corners = txn.base().corners();
    let mut hist_metrics = rusteda_workspace::JsonMap::new();
    hist_metrics.insert("area_um2".to_string(), serde_json::json!(r.area));
    hist_metrics.insert("cells".to_string(), serde_json::json!(total_cells));
    hist_metrics.insert("logic_depth".to_string(), serde_json::json!(r.logic_depth));
    let committed = txn.commit(hist_metrics)?;
    diags.extend(committed.warnings);

    let mut env = Envelope::new("rustsys synth");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(base_rev);
    env.rev = Some(committed.rev);
    env.new_rev = Some(committed.rev);
    env.diagnostics = std::mem::take(diags);

    let eq_str = check_report_opt
        .as_ref()
        .map_or("unchecked", |cr| verdict_status_str(cr.verdict));

    let mut scope = JsonObject::new();
    scope.insert("top", &r.top)?;
    scope.insert("primary_corner", &primary_corner)?;
    scope.insert("corners", &corners)?;
    scope.insert("equivalence", &eq_str)?;
    env.set_scope(scope)?;

    let mut metrics = JsonObject::new();
    metrics.insert("area_um2", &r.area)?;
    metrics.insert("cells", &total_cells)?;
    metrics.insert("combinational_cells", &r.combinational_cells)?;
    metrics.insert("sequential_cells", &r.sequential_cells)?;
    metrics.insert("inverters", &r.inverters)?;
    metrics.insert("logic_depth", &r.logic_depth)?;
    metrics.insert("estimated_delay", &r.estimated_delay)?;
    metrics.insert("aig_nodes_before", &r.aig_nodes_before)?;
    metrics.insert("aig_nodes_after", &r.aig_nodes_after)?;
    metrics.insert("state_bits_rtl", &r.state_bits_rtl)?;
    metrics.insert("state_bits_retained", &r.state_bits_retained)?;
    metrics.insert("runtime_s", &r.runtime_s)?;
    if let Some(cr) = &check_report_opt {
        metrics.insert("mismatches", &cr.mismatches.len())?;
        metrics.insert("outputs_checked", &cr.outputs_checked)?;
        metrics.insert("state_pairs_checked", &cr.state_pairs_checked)?;
    }
    env.set_metrics(metrics)?;

    env.set("top", &r.top)?;
    env.set("report", r)?;
    if let Some(cr) = &check_report_opt {
        env.set("check", cr)?;
    }

    let new_rev_s = committed.rev.to_string();
    env.push_next_action(&["rustsys", "-w", ws_arg, "report", "--rev", &new_rev_s]);
    if check_report_opt.is_none() {
        env.push_next_action(&["rustsys", "-w", ws_arg, "check", "--base-rev", &new_rev_s]);
    }
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &new_rev_s]);

    let mut text_lines = Vec::new();
    for line in crate::proto::text::synth_summary(r).lines() {
        text_lines.push(line.to_string());
    }
    if let Some(cr) = &check_report_opt {
        for line in crate::proto::text::check_synth(cr).lines() {
            text_lines.push(line.to_string());
        }
    }
    text_lines.push(format!("committed {} (base {base_rev})", committed.rev));
    for next in &env.next_actions {
        text_lines.push(next_line(next));
    }

    Ok(OutputOutcome {
        envelope: env,
        text_lines,
        exit_code: exit_code_u8,
    })
}
