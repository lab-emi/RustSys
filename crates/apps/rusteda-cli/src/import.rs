// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! `rusteda import` orchestration (RCLI-REQ-0011 to RCLI-REQ-0014).

use std::path::PathBuf;

use rusteda_workspace::text::{mutation_header, next_line};
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, Invocation, JsonMap, JsonObject, Severity, Workspace,
};

use crate::OutputOutcome;
use crate::import_lib::import_libs;
use crate::import_netlist::import_netlist;
use crate::import_rtl::{RtlImportArgs, import_rtl};
use crate::import_sdc::import_sdc;

/// Parsed options for `rusteda import` (RCLI-REQ-0011).
#[derive(Debug, Clone)]
pub(crate) struct ImportCommand {
    pub base_rev: String,
    pub message: Option<String>,
    pub libs: Vec<(PathBuf, String)>,
    pub rtl: Option<RtlImportArgs>,
    pub netlist: Option<PathBuf>,
    /// Budget of the full netlist reader in MiB (RCLI-REQ-0024).
    pub max_netlist_ast_mib: u64,
    pub sdc_files: Vec<PathBuf>,
    pub sdc_env: Vec<String>,
    /// `(--spef, --spef-corner)` pairs in command-line order (RCLI-REQ-0075).
    pub spefs: Vec<(PathBuf, String)>,
    pub spef_limits: rusteda_formats::spef::Limits,
}

/// Runs `rusteda import` (RCLI-REQ-0011).
pub(crate) fn run_import(
    ws: &Workspace,
    ws_arg: &str,
    cmd: &ImportCommand,
    inv: &Invocation,
    diags: &mut Vec<Diagnostic>,
) -> Result<OutputOutcome, Error> {
    if cmd.libs.is_empty()
        && cmd.rtl.is_none()
        && cmd.netlist.is_none()
        && cmd.sdc_files.is_empty()
        && cmd.spefs.is_empty()
    {
        let err = Error::usage(
            "import requires at least one of --lib, --rtl, --netlist, --sdc, or --spef",
        );
        let _ = ws.log_rejected(inv, None, &err);
        return Err(err);
    }

    let base = match ws.resolve_base(&cmd.base_rev) {
        Ok(b) => b,
        Err(e) => {
            let _ = ws.log_rejected(inv, None, &e);
            return Err(e);
        }
    };

    // WS-REQ-0060, WS-REQ-0069: `begin` logs its own failure (including REVISION_CONFLICT).
    let mut txn = ws.begin(base, inv)?;
    if let Some(msg) = &cmd.message {
        txn.set_message(msg.clone());
    }

    let mut metrics = JsonMap::new();
    let step_res = (|| -> Result<(), Error> {
        if !cmd.libs.is_empty() {
            import_libs(ws, &mut txn, &cmd.libs, &mut metrics, diags)?;
        }
        if let Some(rtl_args) = &cmd.rtl {
            import_rtl(&mut txn, rtl_args, &mut metrics, diags)?;
        }
        if let Some(netlist_path) = &cmd.netlist {
            import_netlist(
                ws,
                &mut txn,
                netlist_path,
                cmd.max_netlist_ast_mib,
                &mut metrics,
                diags,
            )?;
        }
        if !cmd.sdc_files.is_empty() {
            import_sdc(
                ws,
                &mut txn,
                &cmd.sdc_files,
                &cmd.sdc_env,
                &mut metrics,
                diags,
            )?;
        }
        if !cmd.spefs.is_empty() {
            crate::import_spef::import_spefs(
                &mut txn,
                &cmd.spefs,
                cmd.spef_limits,
                &mut metrics,
                diags,
            )?;
        }
        Ok(())
    })();

    if let Err(err) = step_res {
        let _ = txn.reject(&err);
        return Err(err);
    }

    let committed = txn.commit(metrics.clone())?;
    diags.extend(committed.warnings);
    // A dropped `parasitics` table is never silent: the next timing run of the revision has no
    // extracted parasitics (RCLI-REQ-0076).
    if let Some(dropped) = committed
        .meta
        .dropped
        .iter()
        .find(|d| d.table == crate::import_spef::TABLE)
    {
        let corners: Vec<String> = ws
            .revision(committed.base)
            .ok()
            .and_then(|base| crate::import_spef::read_corners(&base).ok())
            .map(|c| c.into_iter().map(|e| e.source.corner).collect())
            .unwrap_or_default();
        let names = if corners.is_empty() {
            String::new()
        } else {
            format!(" of corner(s) {}", corners.join(", "))
        };
        diags.push(Diagnostic::new(
            Severity::Warning,
            DiagCode::of("T0140"),
            format!(
                "the extracted parasitics{names} were dropped ({}): timing of {} uses no SPEF \
                 until it is imported again; `--require-wire-model spef_rc` makes such a run \
                 an error",
                dropped.reason, committed.rev
            ),
        ));
    }

    let mut env = Envelope::new("rusteda import");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(committed.base);
    env.new_rev = Some(committed.rev);

    let mut metrics_obj = JsonObject::new();
    for (k, v) in &metrics {
        metrics_obj.insert(k, v)?;
    }
    env.set_metrics(metrics_obj)?;

    env.set("tables", &committed.meta.tables)?;
    env.set("libraries", &committed.meta.libraries)?;
    env.set("sources", &committed.meta.sources)?;
    env.set("dropped", &committed.meta.dropped)?;
    env.diagnostics = diags.clone();

    let new_rev_s = committed.rev.to_string();
    let base_rev_s = committed.base.to_string();
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &new_rev_s]);
    env.push_next_action(&[
        "rusteda",
        "diff",
        "-w",
        ws_arg,
        "--from",
        &base_rev_s,
        "--to",
        &new_rev_s,
    ]);

    let mut lines = Vec::new();
    lines.push(mutation_header(
        committed.rev,
        "rusteda import",
        committed.base,
    ));
    if !committed.meta.tables.is_empty() {
        let names: Vec<&str> = committed.meta.tables.keys().map(String::as_str).collect();
        lines.push(format!("  tables: {}", names.join(", ")));
    }
    if !committed.meta.libraries.is_empty() {
        let libs_desc: Vec<String> = committed
            .meta
            .libraries
            .iter()
            .map(|l| format!("{} ({})", l.name, l.corner))
            .collect();
        lines.push(format!("  libraries: {}", libs_desc.join(", ")));
    }
    if !committed.meta.dropped.is_empty() {
        let dropped_desc: Vec<&str> = committed
            .meta
            .dropped
            .iter()
            .map(|d| d.table.as_str())
            .collect();
        lines.push(format!("  dropped: {}", dropped_desc.join(", ")));
    }
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok(OutputOutcome {
        envelope: env,
        text_lines: lines,
    })
}
