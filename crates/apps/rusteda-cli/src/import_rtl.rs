// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! RTL snapshot import (`rusteda import --rtl`; RCLI-REQ-0014).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use rusteda_formats::verilog::{
    self, Diagnostic as VDiag, PpOptions, Preprocessor, Severity as VSev, SourceMap, Span,
};
use rusteda_workspace::{DiagCode, Diagnostic, Error, Hash64, JsonMap, Severity, Txn, Upstream};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// One file in `rtl/rtl.json` (RCLI-REQ-0014).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtlFileEntry {
    /// Relative path inside the `rtl/` directory snapshot.
    pub path: String,
    /// File content hash (`xxh3:<16 hex>`).
    pub hash: Hash64,
}

/// The `rtl/rtl.json` manifest consumed by synthesis and verification (RCLI-REQ-0014).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtlManifest {
    /// Top module name (`--top`).
    pub top: String,
    /// Preprocessor macro definitions (`-D NAME[=VAL]`), sorted by name.
    pub defines: BTreeMap<String, String>,
    /// Top-level parameter overrides (`-P NAME=VAL`), sorted by name.
    pub params: BTreeMap<String, String>,
    /// Files in the `rtl/` directory snapshot in deterministic order.
    pub files: Vec<RtlFileEntry>,
    /// Whether predefined macros (`SYNTHESIS`) are enabled (`!--no-predefined-macros`).
    pub predefined_macros: bool,
    /// `blif` or `aiger` for a snapshot of those formats; absent for SystemVerilog
    /// (RCLI-REQ-0022).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

/// Options for `rusteda import --rtl` (RCLI-REQ-0014).
#[derive(Debug, Clone)]
pub(crate) struct RtlImportArgs {
    pub files: Vec<PathBuf>,
    pub top: String,
    pub defines: Vec<String>,
    pub include_dirs: Vec<PathBuf>,
    pub params: Vec<String>,
    pub no_predefined_macros: bool,
}

fn convert_vdiag(sm: &SourceMap, d: &VDiag, fallback_file: &str) -> Diagnostic {
    let sev = match d.severity {
        VSev::Note => Severity::Note,
        VSev::Warning => Severity::Warning,
        VSev::Error => Severity::Error,
    };
    let code = DiagCode::parse(d.code).unwrap_or(DiagCode::of("V0200"));
    let loc = match sm.get(d.span.file) {
        Some(f) => {
            let (line, col) = f.line_col(d.span.start);
            Some((f.path.display().to_string(), line.max(1), col.max(1)))
        }
        None if !fallback_file.is_empty() => Some((fallback_file.to_string(), 1, 1)),
        None => None,
    };
    Diagnostic::from_parts(sev, code, loc, d.message.clone(), None)
}

/// Normalizes a relative path without `.` or `..`; returns `None` if it is absolute, empty, or
/// escapes via `..`.
fn clean_rel_path(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(part) => {
                let s = part.to_str()?;
                if s.is_empty() || s.starts_with('.') {
                    return None;
                }
                out.push(s);
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Resolves an `` `include `` target hermetically: only the including file's directory and `-I`
/// directories are searched; no CWD fallback and no absolute/parent-escaping paths outside them
/// (RCLI-REQ-0014).
fn resolve_hermetic_include(
    target: &str,
    including_file: &Path,
    include_dirs: &[PathBuf],
) -> Result<(PathBuf, PathBuf), String> {
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        return Err(format!(
            "include file '{target}' uses an absolute path; only relative paths in the including file's directory or -I directories are allowed"
        ));
    }
    let Some(clean_rel) = clean_rel_path(target_path) else {
        return Err(format!(
            "include file '{target}' escapes the include directory"
        ));
    };

    let including_dir = match including_file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let candidate = including_dir.join(&clean_rel);
    if fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_file()) {
        return Ok((candidate, clean_rel));
    }

    for dir in include_dirs {
        let candidate = dir.join(&clean_rel);
        if fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_file()) {
            return Ok((candidate, clean_rel));
        }
    }

    Err(format!("include file '{target}' not found"))
}

/// Scans `text` for `` `include `` directives outside comments and string literals and returns
/// `(1-based line, 1-based col, raw target)` for each directive.
fn scan_include_directives(text: &str) -> Vec<(u32, u32, String)> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut line_start = 0usize;
    let mut out = Vec::new();

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            i += 1;
            line += 1;
            line_start = i;
            continue;
        }
        // Line comment
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    line += 1;
                    line_start = i + 1;
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        // String literal
        if b == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'\n' {
                    line += 1;
                    line_start = i + 1;
                }
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            continue;
        }
        // Directive
        if b == b'`' {
            let tick_col = (i - line_start) as u32 + 1;
            let tick_line = line;
            i += 1;
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            if word == "include" {
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'"') {
                    i += 1;
                    let s = i;
                    while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                        i += 1;
                    }
                    let target = text[s..i].to_string();
                    if bytes.get(i) == Some(&b'"') {
                        i += 1;
                    }
                    out.push((tick_line, tick_col, target));
                } else if bytes.get(i) == Some(&b'<') {
                    i += 1;
                    let s = i;
                    while i < bytes.len() && bytes[i] != b'>' && bytes[i] != b'\n' {
                        i += 1;
                    }
                    let target = text[s..i].to_string();
                    if bytes.get(i) == Some(&b'>') {
                        i += 1;
                    }
                    out.push((tick_line, tick_col, target));
                }
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Validates and snapshots `--rtl` files and their hermetic `` `include `` closure into `txn`
/// (RCLI-REQ-0014).
pub(crate) fn import_rtl(
    txn: &mut Txn,
    args: &RtlImportArgs,
    metrics: &mut JsonMap,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    // BLIF and AIGER files (RCLI-REQ-0021).
    if let Some(format) = crate::import_logic::format_of(&args.files)? {
        return crate::import_logic::import_logic(txn, args, format, metrics, diags);
    }
    let top = args.top.trim();
    if top.is_empty() {
        return Err(Error::usage("--top module name must not be empty"));
    }

    let mut defines_map = BTreeMap::new();
    let mut pp_defines = Vec::new();
    if !args.no_predefined_macros {
        pp_defines.push(("SYNTHESIS".to_string(), String::new()));
    }
    for def in &args.defines {
        let (name, val) = match def.split_once('=') {
            Some((n, v)) => (n.trim(), v),
            None => (def.trim(), ""),
        };
        if name.is_empty() {
            return Err(Error::usage(format!("invalid -D definition '{def}'")));
        }
        defines_map.insert(name.to_string(), val.to_string());
        pp_defines.push((name.to_string(), val.to_string()));
    }

    let mut sm = SourceMap::new();
    let mut params_map = BTreeMap::new();
    for param in &args.params {
        let Some((name, val)) = param.split_once('=') else {
            return Err(Error::usage(format!(
                "invalid -P parameter '{param}'; expected NAME=EXPR"
            )));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::usage(format!(
                "invalid -P parameter '{param}'; parameter name must not be empty"
            )));
        }
        let label = format!("<-P {name}>");
        if let Err(d) = verilog::parse_expression(&mut sm, &label, val) {
            let conv = convert_vdiag(&sm, &d, &label);
            let msg = format!("{label}: {}", conv.message);
            diags.push(conv);
            return Err(Error::input_rejected(msg));
        }
        params_map.insert(name.to_string(), val.to_string());
    }

    // Collect given files and their hermetic include closure.
    // Snapshot entries: (snapshot_rel_path, source_disk_path, bytes, hash)
    let mut snapshot_files: BTreeMap<String, (PathBuf, Vec<u8>, Hash64)> = BTreeMap::new();
    let mut recorded_sources: BTreeSet<String> = BTreeSet::new();
    let mut visited_disk: BTreeSet<PathBuf> = BTreeSet::new();
    let mut work_queue: Vec<(PathBuf, PathBuf)> = Vec::new();

    for (idx, file) in args.files.iter().enumerate() {
        let file_str = file.display().to_string();
        let meta = fs::symlink_metadata(file).map_err(|e| Error::io(file, e))?;
        if !meta.file_type().is_file() {
            let msg = format!("{file_str}: not a regular file");
            diags.push(
                Diagnostic::new(Severity::Error, DiagCode::of("V0002"), msg.clone())
                    .at(file_str, 1, 1),
            );
            return Err(Error::input_rejected(msg));
        }
        let snap_rel = clean_rel_path(file).unwrap_or_else(|| {
            let fname = file
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("source.v");
            PathBuf::from(format!("src_{idx}_{fname}"))
        });
        work_queue.push((file.clone(), snap_rel));
    }

    while let Some((disk_path, snap_rel)) = work_queue.pop() {
        let canonical = fs::canonicalize(&disk_path).unwrap_or_else(|_| disk_path.clone());
        let bytes = fs::read(&disk_path).map_err(|e| Error::io(&disk_path, e))?;
        let text = std::str::from_utf8(&bytes).map_err(|e| {
            let p_str = disk_path.display().to_string();
            let msg = format!("{p_str}:1:1: invalid UTF-8 in Verilog source: {e}");
            diags.push(
                Diagnostic::new(Severity::Error, DiagCode::of("V0001"), msg.clone())
                    .at(p_str, 1, 1),
            );
            Error::input_rejected(msg)
        })?;
        let hash = Hash64::of(&bytes);
        let src_str = disk_path.display().to_string();
        let safe_src = if src_str.starts_with("env:") {
            format!("./{src_str}")
        } else {
            src_str.clone()
        };
        if recorded_sources.insert(safe_src.clone()) {
            txn.add_source("rtl", safe_src, hash)?;
        }

        let snap_key = snap_rel.to_string_lossy().replace('\\', "/");
        snapshot_files
            .entry(snap_key)
            .or_insert_with(|| (disk_path.clone(), bytes.clone(), hash));

        if !visited_disk.insert(canonical) {
            continue;
        }

        for (line, col, inc_target) in scan_include_directives(text) {
            match resolve_hermetic_include(&inc_target, &disk_path, &args.include_dirs) {
                Ok((inc_disk, inc_rel)) => {
                    // Store at both the relative include path and relative to the including file's
                    // snapshot directory when applicable so relative includes inside the snapshot
                    // resolve hermetically.
                    if let Some(parent) = snap_rel.parent() {
                        if !parent.as_os_str().is_empty() {
                            if let Some(joined) = clean_rel_path(&parent.join(&inc_rel)) {
                                let inc_bytes =
                                    fs::read(&inc_disk).map_err(|e| Error::io(&inc_disk, e))?;
                                let inc_hash = Hash64::of(&inc_bytes);
                                let key = joined.to_string_lossy().replace('\\', "/");
                                snapshot_files
                                    .entry(key)
                                    .or_insert_with(|| (inc_disk.clone(), inc_bytes, inc_hash));
                            }
                        }
                    }
                    work_queue.push((inc_disk, inc_rel));
                }
                Err(reason) => {
                    let msg = format!("{src_str}:{line}:{col}: {reason}");
                    diags.push(
                        Diagnostic::new(Severity::Error, DiagCode::of("V0200"), reason)
                            .at(src_str, line, col),
                    );
                    return Err(Error::input_rejected(msg));
                }
            }
        }
    }

    // Run `rusteda_formats::verilog::Preprocessor` across the given files to validate directives,
    // macros, and conditional includes.
    let pp_opts = PpOptions {
        include_dirs: args.include_dirs.clone(),
        defines: pp_defines,
    };
    let mut pp = Preprocessor::new(pp_opts);
    for file in &args.files {
        let file_str = file.display().to_string();
        let id = sm.load(file).map_err(|e| Error::io(file, e))?;
        let _tokens = pp.run(&mut sm, id);
        let mut first_err: Option<String> = None;
        for vd in pp.diags.drain(..) {
            let conv = convert_vdiag(&sm, &vd, &file_str);
            if conv.severity == Severity::Error && first_err.is_none() {
                first_err = Some(match &conv.location {
                    Some(loc) => format!("{}:{}:{}: {}", loc.file, loc.line, loc.col, conv.message),
                    None => format!("{file_str}:1:1: {}", conv.message),
                });
            }
            diags.push(conv);
        }
        if let Some(err_msg) = first_err {
            return Err(Error::input_rejected(err_msg));
        }
    }

    // Check if `pp.run` loaded any additional files via macro-expanded includes, and verify they
    // also satisfy hermetic include resolution.
    let mut idx = 0u32;
    while let Some(sf) = sm.get(verilog::FileId(idx)) {
        idx += 1;
        let sf_path = sf.path.clone();
        if sf_path.to_string_lossy().starts_with("<-P ") {
            continue;
        }
        let is_given = args.files.iter().any(|f| f == &sf_path);
        if !is_given {
            let fname = sf_path.to_string_lossy().to_string();
            let allowed = args.files.iter().any(|f| {
                let dir = match f.parent() {
                    Some(p) if !p.as_os_str().is_empty() => p,
                    _ => Path::new("."),
                };
                sf_path.starts_with(dir) && dir != Path::new("")
            }) || args.include_dirs.iter().any(|d| sf_path.starts_with(d));
            if !allowed {
                let d = VDiag::error(
                    "V0200",
                    Span::new(verilog::FileId(idx - 1), 0, 0),
                    format!("include file '{fname}' is outside the include directories"),
                );
                let conv = convert_vdiag(&sm, &d, &fname);
                diags.push(conv);
                return Err(Error::input_rejected(format!(
                    "{fname}:1:1: include file '{fname}' is outside the include directories"
                )));
            }
            if let Some(clean) = clean_rel_path(&sf_path) {
                let bytes = sf.text.as_bytes().to_vec();
                let hash = Hash64::of(&bytes);
                let key = clean.to_string_lossy().replace('\\', "/");
                snapshot_files
                    .entry(key)
                    .or_insert_with(|| (sf_path.clone(), bytes, hash));
            }
        }
    }

    // Write the snapshot directory into staging `rtl/`.
    let rtl_dir = txn.staging_path("rtl")?;
    fs::create_dir_all(&rtl_dir).map_err(|e| Error::io(&rtl_dir, e))?;

    let mut manifest_files = Vec::with_capacity(snapshot_files.len());
    for (rel_path, (_, bytes, hash)) in &snapshot_files {
        if rel_path == "rtl.json" {
            return Err(Error::input_rejected(
                "RTL source file may not be named 'rtl.json'",
            ));
        }
        let dst = rtl_dir.join(rel_path);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        fs::write(&dst, bytes).map_err(|e| Error::io(&dst, e))?;
        manifest_files.push(RtlFileEntry {
            path: rel_path.clone(),
            hash: *hash,
        });
    }

    let rtl_manifest = RtlManifest {
        top: top.to_string(),
        defines: defines_map,
        params: params_map,
        files: manifest_files,
        predefined_macros: !args.no_predefined_macros,
        format: None,
    };
    let mut manifest_json = serde_json::to_string_pretty(&rtl_manifest)
        .map_err(|e| Error::internal(format!("serializing rtl.json: {e}")))?;
    manifest_json.push('\n');
    let manifest_path = rtl_dir.join("rtl.json");
    fs::write(&manifest_path, manifest_json.as_bytes())
        .map_err(|e| Error::io(&manifest_path, e))?;

    txn.add_dir("rtl", "rtl", Upstream::new())?;
    let file_count = rtl_manifest.files.len();
    txn.set_summary("rtl", "rtl_files", file_count)?;
    metrics.insert("rtl_files".to_string(), json!(file_count));
    Ok(())
}
