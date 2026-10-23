// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! A separate FPGA side table: parameterized primitives do not fit the ASIC netlist table.

use super::{OutputOutcome, finish_error, finish_ok, load_rtl_from_rev};
use crate::args::{FormatArg, FpgaArgs};
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, Invocation, JsonObject, Rev, Severity, Workspace,
};
use std::process::ExitCode;

pub(super) fn run(
    ws: &Workspace,
    ws_arg: &str,
    args: &FpgaArgs,
    inv: &Invocation,
    format: FormatArg,
    quiet: bool,
) -> ExitCode {
    let command = "rustsys synth-fpga";
    let base = match ws.resolve_base(&args.base_rev) {
        Ok(base) => base,
        Err(e) => {
            let rev = Rev::parse(&args.base_rev).ok();
            let _ = ws.log_rejected(inv, rev, &e);
            return finish_error(command, Some(ws_arg), rev, e, Vec::new(), format, quiet);
        }
    };
    let mut txn = match ws.begin(base, inv) {
        Ok(txn) => txn,
        Err(e) => {
            return finish_error(
                command,
                Some(ws_arg),
                Some(base.rev()),
                e,
                Vec::new(),
                format,
                quiet,
            );
        }
    };
    let staged = (|| {
        let (manifest, files, include_dirs) = load_rtl_from_rev(txn.base())?;
        if manifest.format.is_some() {
            return Err(Error::input_rejected("FPGA input requires RTL"));
        }
        let front = rustsys::SynthOptions {
            top: Some(manifest.top),
            defines: manifest.defines.into_iter().collect(),
            params: manifest.params.into_iter().collect(),
            include_dirs,
            no_predefined_macros: !manifest.predefined_macros,
            ..Default::default()
        };
        let options = rustsys::fpga::Options {
            part: args.part.clone(),
            max_nodes: args.max_nodes,
            max_cuts_per_node: args.max_cuts_per_node,
            max_cut_merges: args.max_cut_merges,
            max_mapping_bytes: args.max_mapping_bytes,
            max_output_bytes: args.max_output_bytes,
        };
        let output = rustsys::fpga::synthesize(&rustsys::Sources::Files(files), &front, &options)
            .map_err(|e| Error::input_rejected(e.messages.join("\n")))?;
        let upstream = txn.upstream(&["rtl"])?;
        let report = serde_json::to_vec_pretty(&output.report)
            .map_err(|e| Error::internal(e.to_string()))?;
        let dir = txn.staging_path("fpga")?;
        std::fs::create_dir(&dir).map_err(|e| Error::io(&dir, e))?;
        for (name, bytes) in [
            ("mapped.v", output.text.as_bytes()),
            ("fpga.json", report.as_slice()),
            ("luts.bin", output.lut_table.as_slice()),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, bytes).map_err(|e| Error::io(&path, e))?;
        }
        txn.add_dir("fpga", "fpga", upstream)?;
        txn.set_summary("fpga", "lut_cells", output.report.lut_cells)?;
        txn.set_summary("fpga", "flip_flops", output.report.flip_flops)?;
        txn.set_summary("fpga", "latches", output.report.latches)?;
        txn.set_status("fpga", "equivalence", "unchecked")?;
        Ok::<_, Error>(output)
    })();
    let output = match staged {
        Ok(output) => output,
        Err(e) => {
            let _ = txn.reject(&e);
            return finish_error(
                command,
                Some(ws_arg),
                Some(base.rev()),
                e,
                Vec::new(),
                format,
                quiet,
            );
        }
    };
    let committed = match txn.commit(Default::default()) {
        Ok(c) => c,
        Err(e) => {
            return finish_error(
                command,
                Some(ws_arg),
                Some(base.rev()),
                e,
                Vec::new(),
                format,
                quiet,
            );
        }
    };
    let mut env = Envelope::new(command);
    env.workspace = Some(ws_arg.into());
    env.base_rev = Some(base.rev());
    env.rev = Some(committed.rev);
    env.new_rev = Some(committed.rev);
    env.diagnostics = committed.warnings;
    for message in &output.messages {
        let severity = if message.starts_with("warning") {
            Severity::Warning
        } else {
            Severity::Note
        };
        env.diagnostics.push(Diagnostic::new(
            severity,
            DiagCode::of("S0601"),
            message.clone(),
        ));
    }
    let dir = ws
        .root()
        .join("revisions")
        .join(committed.rev.dir_name())
        .join("fpga");
    let done = (|| {
        env.set("report", &output.report)?;
        env.set("messages", &output.messages)?;
        env.set("artifacts",&serde_json::json!({"netlist":dir.join("mapped.v"),"report":dir.join("fpga.json"),"lut_table":dir.join("luts.bin")}))?;
        let mut scope = JsonObject::new();
        scope.insert("corner", &output.report.corner)?;
        scope.insert("wire_model", &output.report.wire_model)?;
        scope.insert(
            "unconstrained_endpoints",
            &output.report.unconstrained_endpoints,
        )?;
        scope.insert("part", &output.report.part)?;
        scope.insert("equivalence", &"unchecked")?;
        env.set_scope(scope)?;
        Ok::<_, Error>(())
    })();
    if let Err(e) = done {
        return finish_error(
            command,
            Some(ws_arg),
            Some(base.rev()),
            e,
            Vec::new(),
            format,
            quiet,
        );
    }
    let text_lines = vec![
        format!(
            "{}: {} LUTs, {} FFs, {} latches; equivalence unchecked",
            output.report.part,
            output.report.lut_cells,
            output.report.flip_flops,
            output.report.latches
        ),
        format!("committed {} (base {})", committed.rev, base.rev()),
        format!("FPGA side table: {}", dir.display()),
    ];
    finish_ok(
        OutputOutcome {
            envelope: env,
            text_lines,
            exit_code: 0,
        },
        format,
        quiet,
    )
}
