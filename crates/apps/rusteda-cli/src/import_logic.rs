// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! BLIF and AIGER snapshots of `rusteda import --rtl` (RCLI-REQ-0021, RCLI-REQ-0022): the files
//! are validated with the shared readers and stored as given, with their format in `rtl.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusteda_core::diag::{Diag, Severity as DSev};
use rusteda_workspace::{DiagCode, Diagnostic, Error, Hash64, JsonMap, Severity, Txn, Upstream};
use serde_json::json;

use crate::import_rtl::{RtlFileEntry, RtlImportArgs, RtlManifest};

/// A format of logic-network sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogicFormat {
    Blif,
    Aiger,
}

impl LogicFormat {
    fn name(self) -> &'static str {
        match self {
            LogicFormat::Blif => "blif",
            LogicFormat::Aiger => "aiger",
        }
    }
}

/// The format of one file by its extension, ignoring letter case: `None` for SystemVerilog.
fn of(p: &Path) -> Option<LogicFormat> {
    match p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("blif") => Some(LogicFormat::Blif),
        Some("aag" | "aig") => Some(LogicFormat::Aiger),
        _ => None,
    }
}

/// The format of the `--rtl` files: `None` for SystemVerilog; a mix of formats, or several AIGER
/// files, is a `USAGE` error (RCLI-REQ-0021).
pub(crate) fn format_of(files: &[PathBuf]) -> Result<Option<LogicFormat>, Error> {
    let Some(first) = files.first().map(|f| of(f)) else {
        return Ok(None);
    };
    if files.iter().any(|f| of(f) != first) {
        return Err(Error::usage(
            "--rtl files of several formats; one import takes SystemVerilog, BLIF or AIGER files",
        ));
    }
    if first == Some(LogicFormat::Aiger) && files.len() > 1 {
        return Err(Error::usage("--rtl takes one AIGER file"));
    }
    Ok(first)
}

/// A reader message as a workspace diagnostic.
fn convert(d: &Diag) -> Diagnostic {
    let sev = match d.severity {
        DSev::Note => Severity::Note,
        DSev::Warning => Severity::Warning,
        DSev::Error => Severity::Error,
    };
    let code = DiagCode::parse(&d.code).unwrap_or(DiagCode::of("W0002"));
    let loc = d
        .location
        .as_ref()
        .map(|l| (l.file.clone(), l.line.max(1), l.column.max(1)));
    Diagnostic::from_parts(sev, code, loc, d.message.clone(), d.help.clone())
}

/// Validates and snapshots BLIF or AIGER `--rtl` files into `txn` (RCLI-REQ-0021,
/// RCLI-REQ-0022).
pub(crate) fn import_logic(
    txn: &mut Txn,
    args: &RtlImportArgs,
    format: LogicFormat,
    metrics: &mut JsonMap,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    if !args.defines.is_empty()
        || !args.include_dirs.is_empty()
        || !args.params.is_empty()
        || args.no_predefined_macros
    {
        return Err(Error::usage(
            "-D, -I, -P and --no-predefined-macros apply to SystemVerilog files only",
        ));
    }
    let top = args.top.trim();
    if top.is_empty() {
        return Err(Error::usage("--top name must not be empty"));
    }
    // Validation with the shared readers.
    let checked = match format {
        LogicFormat::Blif => {
            let o = rusteda_formats::blif::Options {
                top: Some(top.to_string()),
                ..Default::default()
            };
            rusteda_formats::blif::read_files(&args.files, &o).map(|r| r.warnings)
        }
        LogicFormat::Aiger => {
            rusteda_formats::aiger::read_file(&args.files[0], &Default::default()).map(|_| vec![])
        }
    };
    match checked {
        Ok(warnings) => diags.extend(warnings.iter().map(convert)),
        Err(errors) => {
            diags.extend(errors.iter().map(convert));
            let first = errors
                .first()
                .map_or_else(|| "invalid source".to_string(), Diag::render_first_line);
            return Err(Error::input_rejected(first));
        }
    }
    // The snapshot, stored as given.
    let rtl_dir = txn.staging_path("rtl")?;
    fs::create_dir_all(&rtl_dir).map_err(|e| Error::io(&rtl_dir, e))?;
    let mut files = BTreeMap::new();
    for (idx, file) in args.files.iter().enumerate() {
        let bytes = fs::read(file).map_err(|e| Error::io(file, e))?;
        let hash = Hash64::of(&bytes);
        txn.add_source("rtl", file.display().to_string(), hash)?;
        let fname = file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("source");
        let rel = format!("src_{idx}_{fname}");
        let dst = rtl_dir.join(&rel);
        fs::write(&dst, &bytes).map_err(|e| Error::io(&dst, e))?;
        files.insert(rel, hash);
    }
    let manifest = RtlManifest {
        top: top.to_string(),
        defines: BTreeMap::new(),
        params: BTreeMap::new(),
        files: files
            .into_iter()
            .map(|(path, hash)| RtlFileEntry { path, hash })
            .collect(),
        predefined_macros: true,
        format: Some(format.name().to_string()),
    };
    let mut text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| Error::internal(format!("serializing rtl.json: {e}")))?;
    text.push('\n');
    let path = rtl_dir.join("rtl.json");
    fs::write(&path, text.as_bytes()).map_err(|e| Error::io(&path, e))?;
    txn.add_dir("rtl", "rtl", Upstream::new())?;
    let n = manifest.files.len();
    txn.set_summary("rtl", "rtl_files", n)?;
    metrics.insert("rtl_files".to_string(), json!(n));
    metrics.insert("rtl_format".to_string(), json!(format.name()));
    Ok(())
}
