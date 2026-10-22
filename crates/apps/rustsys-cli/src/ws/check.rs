// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Workspace equivalence checker command (`rustsys -w <WS> check [--base-rev <REV>]`).
//!
//! Covers: CLI-REQ-0202.

use std::process::ExitCode;

use rusteda_core::job::Ctx;
use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    BaseRev, DiagCode, Diagnostic, Envelope, Error, Invocation, JsonObject, Rev, Severity,
    Workspace, netlist,
};
use rustsys::view::Report as SynthReport;
use rustsys_check::{CheckOptions, Input, Verdict};

use crate::Globals;
use crate::args::{CheckSynthArgs, FormatArg};
use crate::proto::job::{InitialValues, split_kv};
use crate::ws::synth::{parse_after_reset, verdict_exit_code, verdict_status_str};
use crate::ws::{
    DiagCollector, OutputOutcome, finish_error, finish_ok, load_libraries_from_rev,
    load_rtl_from_rev,
};

pub(crate) fn run_ws_check(
    ws: &Workspace,
    ws_arg: &str,
    args: &CheckSynthArgs,
    g: &Globals,
    inv: &Invocation,
    format: FormatArg,
    quiet: bool,
) -> ExitCode {
    let cmd_name = "rustsys check";
    let base = match args.base_rev.as_deref() {
        Some(spec) => {
            let parsed = Rev::parse(spec).ok();
            match ws.resolve_base(spec) {
                Ok(b) => b,
                Err(e) => {
                    let _ = ws.log_rejected(inv, parsed, &e);
                    return finish_error(
                        cmd_name,
                        Some(ws_arg),
                        parsed,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            }
        }
        None => {
            let head = match ws.head() {
                Ok(h) => h,
                Err(e) => {
                    return finish_error(
                        cmd_name,
                        Some(ws_arg),
                        None,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            };
            BaseRev::parse(&head.to_string()).expect("HEAD revision formats as rev<N>")
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
    let staged = match stage_ws_check(ws, &mut txn, args, g, &mut diags) {
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

    match finish_ws_check(ws_arg, txn, staged, &mut diags) {
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

struct StagedCheck {
    top: String,
    primary_corner: String,
    rep: rustsys_check::CheckReport,
    eq_status: &'static str,
    exit_u8: u8,
}

fn stage_ws_check(
    ws: &Workspace,
    txn: &mut rusteda_workspace::Txn,
    args: &CheckSynthArgs,
    g: &Globals,
    diags: &mut Vec<Diagnostic>,
) -> Result<StagedCheck, Error> {
    let (rtl_manifest, rtl_files, include_dirs) = load_rtl_from_rev(txn.base())?;
    let (primary_corner, libs, _extra_libs, unread_liberty) =
        load_libraries_from_rev(ws, txn.base())?;

    let gate_table = netlist::open_rev(txn.base())?;
    let gate_netlist = netlist::decode(&gate_table)?;

    let prev_synth: Option<SynthReport> = if txn.base().has_table("synth") {
        txn.base()
            .read_table("synth")
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    } else {
        None
    };

    let top = args
        .fe
        .top
        .clone()
        .or_else(|| prev_synth.as_ref().map(|s| s.top.clone()))
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

    let after_reset = if !args.fe.after_reset.is_empty() {
        parse_after_reset(&args.fe.after_reset)?
    } else {
        prev_synth
            .as_ref()
            .map(|s| s.after_reset.clone())
            .unwrap_or_default()
    };

    let uninitialized_dont_care = args.fe.uninitialized_dont_care
        || prev_synth
            .as_ref()
            .is_some_and(|s| s.uninitialized_dont_care);
    let assume_full_case =
        args.fe.assume_full_case || prev_synth.as_ref().is_some_and(|s| s.assume_full_case);
    let assume_parallel_case =
        args.fe.assume_parallel_case || prev_synth.as_ref().is_some_and(|s| s.assume_parallel_case);

    // The initial-value mode of the synthesis run unless given (CLI-REQ-0224).
    let initial_values = match args.fe.initial_values {
        Some(m) => m.job(),
        None if prev_synth
            .as_ref()
            .and_then(|s| s.initial_values_mode.as_deref())
            == Some("arbitrary") =>
        {
            InitialValues::Arbitrary
        }
        None => InitialValues::Reset,
    };
    // Include directories belong to SystemVerilog snapshots (SYNTH-REQ-0901).
    let include_dirs = if rtl_manifest.format.is_some() {
        Vec::new()
    } else {
        include_dirs
    };

    let collector = DiagCollector::default();
    let ctx = Ctx::new(None, Some(&collector));

    let opts = CheckOptions {
        rtl: Input::Files(rtl_files),
        initial_values: initial_values.checker(),
        top: Some(top.clone()),
        defines,
        include_dirs,
        params,
        no_predefined_macros,
        after_reset,
        uninitialized_dont_care,
        assume_full_case,
        assume_parallel_case,
        seed: args.seed,
        budget: args.budget,
        model_budget: args.model_budget,
        sweep_max_candidates: args.sweep_max_candidates,
        verbose: g.verbose,
        cycles: args.cycles,
        supply_ports: args.supply_ports.clone(),
        unread_liberty,
        ..CheckOptions::default()
    };

    let rep = rustsys_check::check_frozen_with(
        &opts,
        &libs,
        &gate_netlist,
        rustsys_check::netlist::frozen::Limits::default(),
        ctx,
    )
    .map_err(|_| Error::Cancelled)?;
    diags.extend(collector.into_vec());

    for u in &rep.undecided {
        diags.push(Diagnostic::new(
            Severity::Warning,
            DiagCode::of("C0005"),
            u.clone(),
        ));
    }

    if rep.verdict == Verdict::Error {
        let msg = rep
            .errors
            .first()
            .cloned()
            .unwrap_or_else(|| "equivalence check failed".to_string());
        let code = if rep.internal_error {
            DiagCode::of("A0002")
        } else {
            DiagCode::of("C0001")
        };
        diags.push(Diagnostic::new(Severity::Error, code, msg.clone()));
        if rep.internal_error {
            return Err(Error::internal(msg));
        }
        return Err(Error::input_rejected(msg));
    }

    let check_bytes = serde_json::to_vec_pretty(&rep)
        .map_err(|e| Error::internal(format!("serializing check.json: {e}")))?;
    let check_up = txn.upstream(&["rtl", "netlist", "libraries"])?;
    txn.add_bytes("check", "check.json", &check_bytes, check_up)?;

    let eq_status = verdict_status_str(rep.verdict);
    txn.set_status("check", "equivalence", eq_status)?;
    txn.set_summary("check", "mismatches", rep.mismatches.len())?;
    txn.set_summary("check", "outputs_checked", rep.outputs_checked)?;
    txn.set_summary("check", "state_pairs_checked", rep.state_pairs_checked)?;
    txn.set_summary("check", "sat_calls", rep.sat_calls)?;
    txn.set_summary("check", "runtime_s", rep.runtime_s)?;

    let (exit_i32, exit_u8) = verdict_exit_code(rep.verdict);
    txn.set_exit(exit_i32)?;

    Ok(StagedCheck {
        top,
        primary_corner,
        rep,
        eq_status,
        exit_u8,
    })
}

fn finish_ws_check(
    ws_arg: &str,
    txn: rusteda_workspace::Txn,
    staged: StagedCheck,
    diags: &mut Vec<Diagnostic>,
) -> Result<OutputOutcome, Error> {
    let StagedCheck {
        top,
        primary_corner,
        rep,
        eq_status,
        exit_u8,
    } = staged;
    let base_rev = txn.base().rev();
    let corners = txn.base().corners();
    let mut hist_metrics = rusteda_workspace::JsonMap::new();
    hist_metrics.insert(
        "mismatches".to_string(),
        serde_json::json!(rep.mismatches.len()),
    );
    hist_metrics.insert(
        "outputs_checked".to_string(),
        serde_json::json!(rep.outputs_checked),
    );
    hist_metrics.insert(
        "state_pairs_checked".to_string(),
        serde_json::json!(rep.state_pairs_checked),
    );
    let committed = txn.commit(hist_metrics)?;
    diags.extend(committed.warnings);

    let mut env = Envelope::new("rustsys check");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(base_rev);
    env.rev = Some(committed.rev);
    env.new_rev = Some(committed.rev);
    env.diagnostics = std::mem::take(diags);

    let mut scope = JsonObject::new();
    scope.insert("top", &top)?;
    scope.insert("primary_corner", &primary_corner)?;
    scope.insert("corners", &corners)?;
    scope.insert("equivalence", &eq_status)?;
    env.set_scope(scope)?;

    let mut metrics = JsonObject::new();
    metrics.insert("mismatches", &rep.mismatches.len())?;
    metrics.insert("outputs_checked", &rep.outputs_checked)?;
    metrics.insert("state_pairs_checked", &rep.state_pairs_checked)?;
    metrics.insert("rtl_state_bits", &rep.rtl_state_bits)?;
    metrics.insert("netlist_state_elements", &rep.netlist_state_elements)?;
    metrics.insert("sat_calls", &rep.sat_calls)?;
    metrics.insert("runtime_s", &rep.runtime_s)?;
    env.set_metrics(metrics)?;

    env.set("verdict", &eq_status)?;
    env.set("report", &rep)?;

    let new_rev_s = committed.rev.to_string();
    env.push_next_action(&[
        "rustsys", "-w", ws_arg, "report", "--rev", &new_rev_s, "--table", "check",
    ]);
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &new_rev_s]);

    let mut text_lines = Vec::new();
    for line in crate::proto::text::check_synth(&rep).lines() {
        text_lines.push(line.to_string());
    }
    text_lines.push(format!("committed {} (base {base_rev})", committed.rev));
    for next in &env.next_actions {
        text_lines.push(next_line(next));
    }

    Ok(OutputOutcome {
        envelope: env,
        text_lines,
        exit_code: exit_u8,
    })
}
