// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Workspace-first CLI mode (`-w/--workspace`, `--format text|json`, `rusteda.v1` envelopes).
//!
//! Covers: CLI-REQ-0200, CLI-REQ-0201, CLI-REQ-0202, CLI-REQ-0203, CLI-REQ-0204, CLI-REQ-0205.

pub mod check;
mod constraints;
pub mod opt;
mod opt_timing;
pub mod report;
mod request;
pub mod synth;
mod targets;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

use rusteda_core::job::{Diag as CoreDiag, Event, Observer, Severity as CoreSev};
use rusteda_core::liberty::{Direction as LibDir, Library};
use rusteda_core::netlist::{FrozenNetlist, GateNetlist, PinDir};
use rusteda_formats::liberty;
use rusteda_formats::verilog::netlist::{FrozenBuildLimits, to_frozen_netlist, to_gate_netlist};
use rusteda_formats::verilog::write::NlModule;
use rusteda_workspace::cache::PutError;
use rusteda_workspace::text::render_diagnostic;
use rusteda_workspace::{
    Corruption, DiagCode, Diagnostic, Envelope, Error, ErrorCode, Hash64, Invocation, JsonStyle,
    Rev, Revision, Severity, Workspace, rlib,
};
use rustsys_check::liberty::{Unread, unread};
use serde::{Deserialize, Serialize};

use crate::Globals;
use crate::args::{CheckCmd, Cli, Cmd, FormatArg, ReportArgs};

pub(crate) const CODE_USAGE: DiagCode = DiagCode::of("A0003");
pub(crate) const CODE_INTERNAL: DiagCode = DiagCode::of("A0002");
pub(crate) const CODE_CANCELLED: DiagCode = DiagCode::of("A0130");
pub(crate) const CODE_BACKEND: DiagCode = DiagCode::of("A0501");

/// One file entry in `rtl/rtl.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtlFileEntry {
    pub path: String,
    pub hash: Hash64,
}

/// The `rtl/rtl.json` manifest written by `rusteda import --rtl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtlManifest {
    pub top: String,
    #[serde(default)]
    pub defines: BTreeMap<String, String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    pub files: Vec<RtlFileEntry>,
    #[serde(default = "default_true")]
    pub predefined_macros: bool,
    /// `blif` or `aiger` for a snapshot of those formats; absent for SystemVerilog
    /// (RCLI-REQ-0022).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Completed workspace command output in both JSON envelope and text forms.
#[derive(Debug)]
pub(crate) struct OutputOutcome {
    pub envelope: Envelope,
    pub text_lines: Vec<String>,
    pub exit_code: u8,
}

/// Pre-scanned workspace flags from raw argv before clap runs.
#[derive(Debug, Default, Clone)]
pub(crate) struct Prescan {
    pub format: FormatArg,
    pub has_format: bool,
    pub workspace: Option<String>,
    pub base_rev: Option<String>,
    pub subcommand: Option<String>,
    pub quiet: bool,
}

impl Prescan {
    pub fn is_workspace_mode(&self) -> bool {
        self.workspace.is_some()
            || self.has_format
            || matches!(
                self.subcommand.as_deref(),
                Some("report" | "opt-timing" | "resize")
            )
    }
}

pub(crate) fn prescan(args: &[OsString]) -> Prescan {
    let mut out = Prescan::default();
    let strs: Vec<String> = args
        .iter()
        .skip(1)
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let known_subs = [
        "synth",
        "check",
        "show",
        "report",
        "opt-timing",
        "resize",
        "completions",
    ];
    let mut i = 0;
    while i < strs.len() {
        let a = &strs[i];
        if a == "--" {
            break;
        }
        if let Some(v) = a.strip_prefix("--format=") {
            out.has_format = true;
            if v == "json" {
                out.format = FormatArg::Json;
            } else if v == "text" {
                out.format = FormatArg::Text;
            }
        } else if a == "--format" {
            out.has_format = true;
            if let Some(v) = strs.get(i + 1) {
                if v == "json" {
                    out.format = FormatArg::Json;
                } else if v == "text" {
                    out.format = FormatArg::Text;
                }
                i += 1;
            }
        } else if let Some(v) = a.strip_prefix("--workspace=") {
            out.workspace = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("-w=") {
            out.workspace = Some(v.to_string());
        } else if a == "-w" || a == "--workspace" {
            if let Some(v) = strs.get(i + 1) {
                out.workspace = Some(v.clone());
                i += 1;
            }
        } else if let Some(v) = a.strip_prefix("--base-rev=") {
            out.base_rev = Some(v.to_string());
        } else if a == "--base-rev" {
            if let Some(v) = strs.get(i + 1) {
                out.base_rev = Some(v.clone());
                i += 1;
            }
        } else if a == "-q" || a == "--quiet" {
            out.quiet = true;
        } else if out.subcommand.is_none()
            && !a.starts_with('-')
            && known_subs.contains(&a.as_str())
        {
            out.subcommand = Some(a.clone());
        }
        i += 1;
    }
    out
}

pub(crate) fn app_diag_code(code: ErrorCode) -> DiagCode {
    match code {
        ErrorCode::Usage => CODE_USAGE,
        ErrorCode::Internal => CODE_INTERNAL,
        ErrorCode::Cancelled => CODE_CANCELLED,
        ErrorCode::BackendUnavailable => CODE_BACKEND,
        _ => code.diag_code().unwrap_or(CODE_USAGE),
    }
}

pub(crate) fn convert_core_diag(d: &CoreDiag) -> Diagnostic {
    let sev = match d.severity {
        CoreSev::Note => Severity::Note,
        CoreSev::Warning => Severity::Warning,
        CoreSev::Error => Severity::Error,
    };
    let code = DiagCode::parse(&d.code).unwrap_or(DiagCode::of("S0900"));
    let loc = d
        .location
        .as_ref()
        .map(|l| (l.file.clone(), l.line.max(1), l.column.max(1)));
    Diagnostic::from_parts(sev, code, loc, d.message.clone(), d.help.clone())
}

/// Collects engine diagnostics into `rusteda_workspace::Diagnostic` items.
#[derive(Default)]
pub(crate) struct DiagCollector {
    diags: Mutex<Vec<Diagnostic>>,
}

impl DiagCollector {
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.diags.into_inner().unwrap_or_default()
    }
}

impl Observer for DiagCollector {
    fn event(&self, e: &Event) {
        if let Event::Diagnostic(d) = e {
            if let Ok(mut list) = self.diags.lock() {
                list.push(convert_core_diag(d));
            }
        }
    }
}

fn write_stdout(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn write_stderr_diags(diags: &[Diagnostic], quiet: bool) {
    let mut err = io::stderr().lock();
    for d in diags {
        if quiet && d.severity == Severity::Note {
            continue;
        }
        let line = render_diagnostic(d);
        let _ = writeln!(err, "{line}");
    }
    let _ = err.flush();
}

pub(crate) fn finish_error(
    cmd_name: &str,
    ws_arg: Option<&str>,
    base_rev: Option<Rev>,
    err: Error,
    mut diags: Vec<Diagnostic>,
    format: FormatArg,
    quiet: bool,
) -> ExitCode {
    let code = err.exit_code().clamp(1, 255) as u8;
    if !diags.iter().any(|d| d.severity == Severity::Error) {
        diags.push(err.to_diagnostic(app_diag_code(err.code())));
    }
    let mut env = Envelope::failure(cmd_name, &err);
    env.workspace = ws_arg.map(String::from);
    env.base_rev = base_rev;
    env.diagnostics = diags;
    if let (Error::RevisionConflict { base, head }, Some(w)) = (&err, ws_arg) {
        let base_s = base.to_string();
        let head_s = head.to_string();
        env.push_next_action(&["rusteda", "log", "-w", w]);
        env.push_next_action(&[
            "rusteda", "diff", "-w", w, "--from", &base_s, "--to", &head_s,
        ]);
        env.push_next_action(&["rusteda", "show", "-w", w, "--rev", &head_s]);
    }

    match format {
        FormatArg::Json => {
            let mut text = env.to_json(JsonStyle::Pretty);
            text.push('\n');
            write_stdout(&text);
        }
        FormatArg::Text => {
            write_stderr_diags(&env.diagnostics, quiet);
        }
    }
    ExitCode::from(code)
}

pub(crate) fn finish_ok(outcome: OutputOutcome, format: FormatArg, quiet: bool) -> ExitCode {
    match format {
        FormatArg::Json => {
            let mut text = outcome.envelope.to_json(JsonStyle::Pretty);
            text.push('\n');
            write_stdout(&text);
        }
        FormatArg::Text => {
            write_stderr_diags(&outcome.envelope.diagnostics, quiet);
            if !outcome.text_lines.is_empty() {
                let mut body = outcome.text_lines.join("\n");
                if !body.ends_with('\n') {
                    body.push('\n');
                }
                write_stdout(&body);
            }
        }
    }
    ExitCode::from(outcome.exit_code)
}

fn maybe_log_usage_rejection(pre: &Prescan, argv_strings: &[String], err: &Error) {
    let Some(sub) = pre.subcommand.as_deref() else {
        return;
    };
    if !matches!(sub, "synth" | "check" | "opt-timing" | "resize") {
        return;
    }
    let Some(ws_dir) = pre.workspace.as_deref() else {
        return;
    };
    if let Ok(ws) = Workspace::open(ws_dir) {
        let base_rev = pre.base_rev.as_deref().and_then(|s| Rev::parse(s).ok());
        let inv = Invocation::new(
            "rustsys",
            env!("CARGO_PKG_VERSION"),
            format!("rustsys {sub}"),
            argv_strings.to_vec(),
        );
        let _ = ws.log_rejected(&inv, base_rev, err);
    }
}

pub(crate) fn handle_clap_error(
    e: &clap::Error,
    pre: &Prescan,
    argv_strings: &[String],
) -> ExitCode {
    use clap::error::ErrorKind;
    let cmd_name = pre
        .subcommand
        .as_deref()
        .map_or_else(|| "rustsys".to_string(), |s| format!("rustsys {s}"));
    match e.kind() {
        ErrorKind::DisplayVersion => {
            let ver = crate::long_version();
            if pre.format == FormatArg::Json {
                let mut env = Envelope::new(&cmd_name);
                env.workspace = pre.workspace.clone();
                let _ = env.set("version", ver);
                return finish_ok(
                    OutputOutcome {
                        envelope: env,
                        text_lines: Vec::new(),
                        exit_code: 0,
                    },
                    FormatArg::Json,
                    pre.quiet,
                );
            }
            write_stdout(&format!("rustsys {ver}\n"));
            ExitCode::from(0)
        }
        ErrorKind::DisplayHelp => {
            let help_text = e.render().to_string();
            if pre.format == FormatArg::Json {
                let mut env = Envelope::new(&cmd_name);
                env.workspace = pre.workspace.clone();
                let _ = env.set("help", help_text.trim_end());
                return finish_ok(
                    OutputOutcome {
                        envelope: env,
                        text_lines: Vec::new(),
                        exit_code: 0,
                    },
                    FormatArg::Json,
                    pre.quiet,
                );
            }
            write_stdout(&help_text);
            ExitCode::from(0)
        }
        _ => {
            let rendered = e.render().to_string();
            let first = rendered
                .lines()
                .next()
                .unwrap_or("invalid command line")
                .trim();
            let msg = first.strip_prefix("error: ").unwrap_or(first).to_string();
            let err = Error::usage(&msg);
            maybe_log_usage_rejection(pre, argv_strings, &err);
            let diag = Diagnostic::new(Severity::Error, CODE_USAGE, msg)
                .with_help(format!("{cmd_name} --help lists every option"));
            let base_rev = pre.base_rev.as_deref().and_then(|s| Rev::parse(s).ok());
            finish_error(
                &cmd_name,
                pre.workspace.as_deref(),
                base_rev,
                err,
                vec![diag],
                pre.format,
                pre.quiet,
            )
        }
    }
}

/// Dispatches workspace-mode commands (`-w/--workspace` or workspace-only subcommands).
pub(crate) fn dispatch_workspace(cli: &Cli, g: &Globals, raw_args: &[OsString]) -> ExitCode {
    let format = cli.format;
    let quiet = cli.quiet;
    let mut argv_strings: Vec<String> = raw_args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(first) = argv_strings.first_mut() {
        *first = "rustsys".to_string();
    }

    if let Cmd::Completions(comp) = &cli.cmd {
        let Some(shell) = comp.shell.or_else(clap_complete::Shell::from_env) else {
            let err = Error::usage("cannot determine shell from environment; pass SHELL");
            return finish_error(
                "rustsys completions",
                None,
                None,
                err,
                Vec::new(),
                format,
                quiet,
            );
        };
        let mut buf = Vec::new();
        let mut cmd_def = crate::command();
        clap_complete::generate(shell, &mut cmd_def, "rustsys", &mut buf);
        let script = String::from_utf8_lossy(&buf).into_owned();
        if format == FormatArg::Json {
            let mut env = Envelope::new("rustsys completions");
            let _ = env.set("shell", &shell.to_string());
            let _ = env.set("script", &script);
            return finish_ok(
                OutputOutcome {
                    envelope: env,
                    text_lines: Vec::new(),
                    exit_code: 0,
                },
                FormatArg::Json,
                quiet,
            );
        }
        write_stdout(&script);
        return ExitCode::from(0);
    }

    let sub_name = match &cli.cmd {
        Cmd::Synth(_) => "rustsys synth",
        Cmd::Check(_) => "rustsys check",
        Cmd::Show(_) => "rustsys show",
        Cmd::Report(_) => "rustsys report",
        Cmd::OptTiming(_) => "rustsys opt-timing",
        Cmd::Resize(_) => "rustsys resize",
        Cmd::Completions(_) => "rustsys completions",
    };

    let Some(ws_path) = cli.workspace.as_ref() else {
        let err = Error::usage(format!("{sub_name} requires -w/--workspace <DIR>"));
        return finish_error(sub_name, None, None, err, Vec::new(), format, quiet);
    };
    let ws_arg = ws_path.display().to_string();
    let inv = Invocation::new("rustsys", env!("CARGO_PKG_VERSION"), sub_name, argv_strings);

    let ws = match Workspace::open(ws_path) {
        Ok(w) => w,
        Err(e) => {
            return finish_error(sub_name, Some(&ws_arg), None, e, Vec::new(), format, quiet);
        }
    };

    match &cli.cmd {
        Cmd::Synth(args) => synth::run_ws_synth(&ws, &ws_arg, args, g, &inv, format, quiet),
        Cmd::Check(check_args) => {
            let a = match &check_args.what {
                Some(CheckCmd::Synth(s)) => s,
                None => &check_args.direct,
            };
            check::run_ws_check(&ws, &ws_arg, a, g, &inv, format, quiet)
        }
        Cmd::Report(args) => report::run_ws_report(&ws, &ws_arg, args, sub_name, format, quiet),
        Cmd::Show(show_args) => {
            let rep_args = ReportArgs {
                rev: show_args
                    .rev
                    .clone()
                    .or_else(|| show_args.run.clone())
                    .unwrap_or_else(|| "HEAD".to_string()),
                table: None,
                sections: show_args.sections.clone(),
                limit: show_args.limit,
                sort: show_args.sort.clone(),
                reverse: show_args.reverse,
                filter: show_args.filter.clone(),
                exact: show_args.exact,
                sources: show_args.sources,
            };
            report::run_ws_report(&ws, &ws_arg, &rep_args, sub_name, format, quiet)
        }
        Cmd::OptTiming(args) => {
            opt_timing::run_ws_opt_timing(&ws, &ws_arg, args, g, &inv, format, quiet)
        }
        Cmd::Resize(args) => opt::run_ws_resize(&ws, &ws_arg, args, g, &inv, format, quiet),
        Cmd::Completions(_) => unreachable!(),
    }
}

/// Loads compiled `.rlib` libraries (and verbatim `.lib` unread constructs) for `rev`.
/// Returns `(primary_corner_name, primary_libs, extra_corner_libs, unread_constructs)`.
pub fn load_libraries_from_rev(
    ws: &Workspace,
    rev: &Revision,
) -> Result<(String, Vec<Library>, Vec<Library>, Vec<Unread>), Error> {
    let entries = &rev.meta().libraries;
    if entries.is_empty() {
        return Err(Error::MissingTable {
            rev: rev.rev(),
            table: "libraries".to_string(),
        });
    }
    let corners = rev.corners();
    let primary_corner = if corners.iter().any(|c| c == "default") {
        "default".to_string()
    } else {
        corners[0].clone()
    };

    let cache = ws.cache();
    let mut primary_libs = Vec::new();
    let mut extra_libs = Vec::new();
    let mut constructs = Vec::new();

    for entry in entries {
        let rbin = cache
            .open_rlib_or_recompile(&entry.name, entry.hash, rlib::KIND, rlib::VERSIONS, |raw| {
                let text = std::str::from_utf8(raw)
                    .map_err(|_| Error::corrupt(&entry.source, Corruption::NotUtf8))?;
                let lib = liberty::read_text(text, &entry.source).map_err(|e| {
                    Error::corrupt(
                        &entry.source,
                        Corruption::Json {
                            reason: e.to_string(),
                        },
                    )
                })?;
                rlib::encode_with_source(&lib, entry.hash)
            })
            .map_err(|e| match e {
                PutError::Compile(err) | PutError::Workspace(err) => err,
            })?;
        let lib = rlib::decode_rbin(&rbin)?;
        if entry.corner == primary_corner {
            if let Some(cached) = cache.lookup(&entry.name, entry.hash) {
                if let Ok(bytes) = std::fs::read(&cached.lib_path) {
                    let text = String::from_utf8_lossy(&bytes);
                    if let Ok(u) = unread(&text, &entry.source) {
                        constructs.extend(u);
                    }
                }
            }
            primary_libs.push(lib);
        } else {
            extra_libs.push(lib);
        }
    }

    Ok((primary_corner, primary_libs, extra_libs, constructs))
}

/// Loads `rtl/rtl.json` and resolves the hermetic RTL file paths and include directories inside
/// `rev`'s `rtl/` directory snapshot.
pub(crate) fn load_rtl_from_rev(
    rev: &Revision,
) -> Result<(RtlManifest, Vec<PathBuf>, Vec<PathBuf>), Error> {
    if !rev.has_table("rtl") {
        return Err(Error::MissingTable {
            rev: rev.rev(),
            table: "rtl".to_string(),
        });
    }
    let _verified = rev.table_files("rtl")?;
    let manifest_bytes = rev.read_table_file("rtl", "rtl.json")?;
    let manifest_path = rev.dir().join("rtl").join("rtl.json");
    let manifest: RtlManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        Error::corrupt(
            &manifest_path,
            Corruption::Json {
                reason: format!("invalid rtl.json: {e}"),
            },
        )
    })?;

    let rtl_root = rev.dir().join("rtl");
    let mut source_files = Vec::new();
    let mut include_dirs = Vec::new();
    let mut seen_dirs = BTreeSet::new();
    if seen_dirs.insert(rtl_root.clone()) {
        include_dirs.push(rtl_root.clone());
    }

    for entry in &manifest.files {
        let full = rtl_root.join(&entry.path);
        if let Some(parent) = full.parent() {
            if seen_dirs.insert(parent.to_path_buf()) {
                include_dirs.push(parent.to_path_buf());
            }
        }
        let ext = full
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if !matches!(ext, "svh" | "vh" | "h") {
            source_files.push(full);
        }
    }
    if source_files.is_empty() {
        for entry in &manifest.files {
            source_files.push(rtl_root.join(&entry.path));
        }
    }

    Ok((manifest, source_files, include_dirs))
}

/// Converts an [`NlModule`] to a [`GateNetlist`] using the cell pin definitions from `libs`.
pub fn nl_to_gate_netlist(m: &NlModule, libs: &[Library]) -> Result<GateNetlist, Error> {
    to_gate_netlist(m, &|name| library_cell_pins(libs, name))
        .map_err(|e| Error::internal(format!("to_gate_netlist: {e}")))
}

/// Read-only construction avoids a duplicated editable graph for ordinary
/// synthesis output. General writer semantics retain the existing converter.
pub fn nl_to_frozen_netlist(
    m: &NlModule,
    libs: &[Library],
) -> Result<FrozenNetlist<'static>, Error> {
    if let Some(frozen) = to_frozen_netlist(
        m,
        &|name| library_cell_pins(libs, name),
        FrozenBuildLimits::default(),
    )
    .map_err(|e| Error::internal(format!("to_frozen_netlist: {e}")))?
    {
        return Ok(frozen);
    }
    Ok(nl_to_gate_netlist(m, libs)?.freeze())
}

fn library_cell_pins(libs: &[Library], cell_name: &str) -> Option<Vec<(String, PinDir)>> {
    for lib in libs {
        if let Some(cell) = lib.cells.iter().find(|c| c.name == cell_name) {
            let mut pins = Vec::with_capacity(cell.pins.len());
            for p in &cell.pins {
                let dir = match p.direction {
                    LibDir::Input => PinDir::Input,
                    LibDir::Output => PinDir::Output,
                    LibDir::Inout => PinDir::Inout,
                    LibDir::Internal => continue,
                };
                pins.push((p.name.clone(), dir));
            }
            return Some(pins);
        }
    }
    None
}
