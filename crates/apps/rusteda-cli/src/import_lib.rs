// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Liberty library import (`rusteda import --lib`; RCLI-REQ-0012, RCLI-REQ-0013).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use rusteda_core::diag as core_diag;
use rusteda_formats::liberty::{self, Library};
use rusteda_workspace::{DiagCode, Diagnostic, Error, JsonMap, Severity, Txn, Workspace, rlib};
use serde_json::json;

/// Converts a `rusteda_core::diag::Diag` from the Liberty reader into an envelope [`Diagnostic`].
fn convert_diag(d: core_diag::Diag, fallback_file: &str) -> Diagnostic {
    let sev = match d.severity {
        core_diag::Severity::Note => Severity::Note,
        core_diag::Severity::Warning => Severity::Warning,
        core_diag::Severity::Error => Severity::Error,
    };
    let code = DiagCode::parse(&d.code).unwrap_or(DiagCode::of("L0001"));
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

/// Validates and caches `--lib` inputs and adds them to `txn` (RCLI-REQ-0012, RCLI-REQ-0013).
pub(crate) fn import_libs(
    ws: &Workspace,
    txn: &mut Txn,
    libs: &[(PathBuf, String)],
    metrics: &mut JsonMap,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    if libs.is_empty() {
        return Ok(());
    }

    // Track (library_name, corner) pairs given in this invocation so duplicates are rejected.
    let mut seen_in_invocation: BTreeSet<(String, String)> = BTreeSet::new();
    // Track parsed libraries per corner to check cell uniqueness within each corner.
    let mut parsed_new: Vec<(PathBuf, String, String, Vec<u8>, Library)> =
        Vec::with_capacity(libs.len());

    for (path, corner) in libs {
        let source_str = path.display().to_string();
        let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
        if !meta.file_type().is_file() {
            let msg = format!("{source_str}: not a regular file");
            diags.push(
                Diagnostic::new(Severity::Error, DiagCode::of("L0001"), msg.clone())
                    .at(source_str, 1, 1),
            );
            return Err(Error::input_rejected(msg));
        }
        let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
        let text = std::str::from_utf8(&bytes).map_err(|e| {
            let msg = format!("{source_str}:1:1: invalid UTF-8 in Liberty file: {e}");
            diags.push(
                Diagnostic::new(Severity::Error, DiagCode::of("L0001"), msg.clone()).at(
                    source_str.clone(),
                    1,
                    1,
                ),
            );
            Error::input_rejected(msg)
        })?;

        let (lib, reader_diags) = match liberty::read_text_diags(text, &source_str) {
            Ok(pair) => pair,
            Err(liberty::Error::Syntax {
                file,
                line,
                col,
                msg,
            }) => {
                diags.push(
                    Diagnostic::new(Severity::Error, DiagCode::of("L0001"), msg.clone()).at(
                        file.clone(),
                        line.max(1),
                        col.max(1),
                    ),
                );
                return Err(Error::input_rejected(format!("{file}:{line}:{col}: {msg}")));
            }
            Err(liberty::Error::Model { file, msg }) => {
                diags.push(
                    Diagnostic::new(Severity::Error, DiagCode::of("L0002"), msg.clone()).at(
                        file.clone(),
                        1,
                        1,
                    ),
                );
                return Err(Error::input_rejected(format!("{file}:1:1: {msg}")));
            }
            Err(liberty::Error::Io { path: p, msg }) => {
                diags.push(
                    Diagnostic::new(Severity::Error, DiagCode::of("L0001"), msg.clone()).at(
                        p.clone(),
                        1,
                        1,
                    ),
                );
                return Err(Error::input_rejected(format!("{p}:1:1: {msg}")));
            }
        };

        let mut first_err: Option<String> = None;
        for rd in reader_diags {
            let converted = convert_diag(rd, &source_str);
            if converted.severity == Severity::Error && first_err.is_none() {
                first_err = Some(match &converted.location {
                    Some(loc) => format!(
                        "{}:{}:{}: {}",
                        loc.file, loc.line, loc.col, converted.message
                    ),
                    None => format!("{source_str}:1:1: {}", converted.message),
                });
            }
            diags.push(converted);
        }
        if let Some(err_msg) = first_err {
            return Err(Error::input_rejected(err_msg));
        }

        if !seen_in_invocation.insert((lib.name.clone(), corner.clone())) {
            let msg = format!(
                "{source_str}:1:1: library '{}' is given twice for corner '{corner}'",
                lib.name
            );
            diags.push(
                Diagnostic::new(
                    Severity::Error,
                    DiagCode::of("L0003"),
                    format!(
                        "library '{}' is given twice for corner '{corner}'",
                        lib.name
                    ),
                )
                .at(source_str, 1, 1),
            );
            return Err(Error::input_rejected(msg));
        }

        // Check duplicate cell definitions within this library itself (RCLI-REQ-0013).
        let mut local_cells = BTreeSet::new();
        for cell in &lib.cells {
            if !local_cells.insert(cell.name.as_str()) {
                let msg = format!(
                    "{source_str}:1:1: cell '{}' is defined twice in corner '{corner}' (library '{}')",
                    cell.name, lib.name
                );
                diags.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagCode::of("L0055"),
                        format!(
                            "cell '{}' is defined twice in corner '{corner}' (library '{}')",
                            cell.name, lib.name
                        ),
                    )
                    .at(source_str, 1, 1),
                );
                return Err(Error::input_rejected(msg));
            }
        }

        parsed_new.push((path.clone(), source_str, corner.clone(), bytes, lib));
    }

    // Check cell uniqueness across all libraries in each affected corner (including carried base
    // libraries that are not replaced by `(name, corner)`).
    let mut corner_cells: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for existing in txn.libraries() {
        if seen_in_invocation.contains(&(existing.name.clone(), existing.corner.clone())) {
            continue;
        }
        if let Some(cached) = ws.cache().lookup(&existing.name, existing.hash) {
            if let Ok(text) = fs::read_to_string(&cached.lib_path) {
                if let Ok(lib) = liberty::read_text(&text, &cached.lib) {
                    let map = corner_cells.entry(existing.corner.clone()).or_default();
                    for cell in lib.cells {
                        map.insert(cell.name, existing.name.clone());
                    }
                }
            }
        }
    }

    for (_, source_str, corner, _, lib) in &parsed_new {
        let map = corner_cells.entry(corner.clone()).or_default();
        for cell in &lib.cells {
            if let Some(prev_lib) = map.insert(cell.name.clone(), lib.name.clone()) {
                let msg = format!(
                    "{source_str}:1:1: cell '{}' in library '{}' is already defined in library '{prev_lib}' for corner '{corner}'",
                    cell.name, lib.name
                );
                diags.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagCode::of("L0055"),
                        format!(
                            "cell '{}' in library '{}' is already defined in library '{prev_lib}' for corner '{corner}'",
                            cell.name, lib.name
                        ),
                    )
                    .at(source_str.clone(), 1, 1),
                );
                return Err(Error::input_rejected(msg));
            }
        }
    }

    let mut total_cells = 0usize;
    for (_, source_str, corner, bytes, lib) in parsed_new {
        total_cells += lib.cells.len();
        let cached = rlib::put(ws, &lib, &bytes)?;
        txn.add_library(cached.entry(corner, source_str))?;
    }

    metrics.insert("libraries".to_string(), json!(libs.len()));
    metrics.insert("library_cells".to_string(), json!(total_cells));
    Ok(())
}
