// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! SDC constraint import (`rusteda import --sdc`; RCLI-REQ-0011, SDC-REQ-0051 to SDC-REQ-0055).

use std::fs;
use std::path::PathBuf;

use rusteda_core::diag as core_diag;
use rusteda_formats::sdc::{self, Env, Limits, Source};
use rusteda_workspace::{
    DiagCode, Diagnostic, Error, Hash64, JsonMap, RbinWriter, Severity, Txn, Upstream, Workspace,
};
use serde_json::json;

/// Table name in `meta.json` (`SDC-REQ-0051`).
pub const TABLE_NAME: &str = "constraints";
/// File name of the `CONSTR v1` binary table inside a revision directory (`SDC-REQ-0051`).
pub const TABLE_PATH: &str = "constraints.rbin";
/// `rbin` header kind for `constraints.rbin` (`SDC-REQ-0040`, `SDC-REQ-0051`).
pub const KIND: &str = "CONSTR";
/// `rbin` format version for `CONSTR v1` (`SDC-REQ-0040`, `SDC-REQ-0051`).
pub const FORMAT_VERSION: u32 = 1;

/// Converts a `rusteda_core::diag::Diag` from `rusteda_formats::sdc` into an envelope [`Diagnostic`].
fn convert_diag(d: core_diag::Diag, fallback_file: &str) -> Diagnostic {
    let sev = match d.severity {
        core_diag::Severity::Note => Severity::Note,
        core_diag::Severity::Warning => Severity::Warning,
        core_diag::Severity::Error => Severity::Error,
    };
    let code = DiagCode::parse(&d.code).unwrap_or(DiagCode::of("D0001"));
    let loc = d.location.map(|l| {
        let file = if l.file.is_empty() {
            fallback_file.to_string()
        } else {
            l.file
        };
        (file, l.line.max(1), l.column.max(1))
    });
    Diagnostic::from_parts(sev, code, loc, d.message, d.help)
}

/// Parses `--sdc-env NAME=VALUE` command-line arguments (`SDC-REQ-0009`, `SDC-REQ-0055`).
fn parse_sdc_env_args(
    env_vars: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Result<Vec<(String, String)>, Error> {
    let mut pairs = Vec::with_capacity(env_vars.len());
    for raw in env_vars {
        let Some((name, val)) = raw.split_once('=') else {
            let msg = format!("invalid --sdc-env '{raw}': expected NAME=VALUE");
            diags.push(Diagnostic::new(
                Severity::Error,
                DiagCode::of("D0004"),
                msg.clone(),
            ));
            return Err(Error::input_rejected(msg));
        };
        if name.is_empty() || name.contains('\0') || val.contains('\0') {
            let msg = format!("invalid --sdc-env '{raw}': NAME must be non-empty without NUL");
            diags.push(Diagnostic::new(
                Severity::Error,
                DiagCode::of("D0004"),
                msg.clone(),
            ));
            return Err(Error::input_rejected(msg));
        }
        pairs.push((name.to_string(), val.to_string()));
    }
    Ok(pairs)
}

/// Imports SDC constraint files and `--sdc-env` bindings into `txn`
/// (`RCLI-REQ-0011`, `SDC-REQ-0051` to `SDC-REQ-0055`).
pub(crate) fn import_sdc(
    _ws: &Workspace,
    txn: &mut Txn,
    files: &[PathBuf],
    env_vars: &[String],
    metrics: &mut JsonMap,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    if files.is_empty() {
        return Ok(());
    }

    let env_pairs = parse_sdc_env_args(env_vars, diags)?;
    let sdc_env = Env::from_pairs(env_pairs.iter().cloned());

    let mut sources = Vec::with_capacity(files.len());
    let mut first_source_name = String::new();
    for (idx, file) in files.iter().enumerate() {
        let source_str = file.display().to_string();
        if idx == 0 {
            first_source_name = source_str.clone();
        }
        let meta = fs::symlink_metadata(file).map_err(|e| Error::io(file, e))?;
        if !meta.file_type().is_file() {
            let msg = format!("{source_str}: not a regular file");
            diags.push(
                Diagnostic::new(Severity::Error, DiagCode::of("D0001"), msg.clone()).at(
                    source_str.clone(),
                    1,
                    1,
                ),
            );
            return Err(Error::input_rejected(format!("{source_str}:1:1: {msg}")));
        }
        let bytes = fs::read(file).map_err(|e| Error::io(file, e))?;
        let source_hash = Hash64::of(&bytes);
        let text = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("invalid UTF-8 in SDC file: {e}");
                diags.push(
                    Diagnostic::new(Severity::Error, DiagCode::of("D0001"), msg.clone()).at(
                        source_str.clone(),
                        1,
                        1,
                    ),
                );
                return Err(Error::input_rejected(format!("{source_str}:1:1: {msg}")));
            }
        };
        sources.push(Source::with_hash(source_str, text, source_hash.0));
    }

    let (sdc_model, sdc_diags) = sdc::read(&sources, &sdc_env, &Limits::default());

    let mut first_err: Option<String> = None;
    for rd in sdc_diags {
        let converted = convert_diag(rd, &first_source_name);
        if converted.severity == Severity::Error && first_err.is_none() {
            first_err = Some(match &converted.location {
                Some(loc) => format!(
                    "{}:{}:{}: [{}] {}",
                    loc.file,
                    loc.line,
                    loc.col,
                    converted.code.as_str(),
                    converted.message
                ),
                None => format!(
                    "{first_source_name}:1:1: [{}] {}",
                    converted.code.as_str(),
                    converted.message
                ),
            });
        }
        diags.push(converted);
    }

    if let Some(err_msg) = first_err {
        return Err(Error::input_rejected(err_msg));
    }

    let sections = sdc_model.to_sections();
    let mut writer = RbinWriter::new(KIND, FORMAT_VERSION)?;
    for sec in &sections {
        writer.section_bytes(u32::from(sec.id), sec.elem_size, sec.bytes.as_slice())?;
    }
    txn.add_rbin(TABLE_NAME, TABLE_PATH, &writer, Upstream::new())?;

    for src in &sdc_model.sources {
        txn.add_source(TABLE_NAME, src.path.clone(), Hash64(src.xxh3))?;
    }
    for (name, val) in &env_pairs {
        txn.add_env_source(TABLE_NAME, name, val)?;
    }

    let n_cmds = sdc_model.cmds.len();
    let n_sources = sdc_model.sources.len();
    txn.set_summary(TABLE_NAME, "commands", n_cmds)?;
    txn.set_summary(TABLE_NAME, "sources", n_sources)?;
    if let Some(design) = &sdc_model.current_design {
        txn.set_summary(TABLE_NAME, "current_design", design.clone())?;
    }

    metrics.insert("constraints_commands".to_string(), json!(n_cmds));
    metrics.insert("constraints_sources".to_string(), json!(n_sources));
    Ok(())
}
