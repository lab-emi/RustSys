// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The `rusteda` command-line program (`docs/specs/rusteda-cli/README.md`, RCLI-REQ-0001 to
//! RCLI-REQ-0018).

mod export;
mod import;
mod import_lib;
mod import_logic;
mod import_netlist;
mod import_rtl;
mod import_sdc;
mod import_spef;
mod init;
mod query;
mod revert;

pub use import_rtl::{RtlFileEntry, RtlManifest};
pub use query::{LibraryDiff, LibraryHashChange, LogEntry, TableDiffEntry};

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgMatches, Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use rusteda_workspace::text::render_diagnostic;
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, ErrorCode, Invocation, JsonStyle, Rev, Severity,
    Workspace,
};

use export::run_export;
use import::{ImportCommand, run_import};
use import_rtl::RtlImportArgs;
use init::run_init;
use query::{run_diff, run_log, run_show};
use revert::run_revert;

/// Application diagnostic codes for `rusteda` (WS-REQ-0097, RCLI-REQ-0005).
const CODE_THREAD_POOL: DiagCode = DiagCode::of("A0001");
const CODE_INTERNAL: DiagCode = DiagCode::of("A0002");
const CODE_USAGE: DiagCode = DiagCode::of("A0003");
const CODE_BACKEND: DiagCode = DiagCode::of("A0501");
const CODE_CANCELLED: DiagCode = DiagCode::of("A0130");

/// Successful command output in both JSON and text forms.
#[derive(Debug)]
pub(crate) struct OutputOutcome {
    pub envelope: Envelope,
    pub text_lines: Vec<String>,
}

/// Output format (`--format text|json`; RCLI-REQ-0002, RCLI-REQ-0004).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum FormatArg {
    #[default]
    Text,
    Json,
}

/// Colour mode (`--color auto|always|never`; RCLI-REQ-0002).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum ColorArg {
    #[default]
    Auto,
    Always,
    Never,
}

/// Progress mode (`--progress auto|always|never`; RCLI-REQ-0002).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum ProgressArg {
    #[default]
    Auto,
    Always,
    Never,
}

/// Top-level `rusteda` CLI definition (RCLI-REQ-0001, RCLI-REQ-0002).
#[derive(Parser, Debug)]
#[command(
    name = "rusteda",
    version = env!("CARGO_PKG_VERSION"),
    propagate_version = true,
    about = "RustEDA: workspace and design revision driver",
)]
pub struct Cli {
    /// Workspace directory (required for every command except completions).
    #[arg(short = 'w', long = "workspace", global = true, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
    /// Output format: human-readable text or a single rusteda.v1 JSON envelope on stdout.
    #[arg(long = "format", global = true, value_enum, default_value_t = FormatArg::Text)]
    pub format: FormatArg,
    /// Worker threads for parallel operations (0: default).
    #[arg(long = "threads", global = true, default_value_t = 0)]
    pub threads: usize,
    /// Suppress note diagnostics on stderr.
    #[arg(short = 'q', long = "quiet", global = true, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Verbose output.
    #[arg(short = 'v', long = "verbose", global = true)]
    pub verbose: bool,
    /// Colour on standard error.
    #[arg(long = "color", global = true, value_enum, default_value_t = ColorArg::Auto)]
    pub color: ColorArg,
    /// Progress reporting mode.
    #[arg(long = "progress", global = true, value_enum, default_value_t = ProgressArg::Auto)]
    pub progress: ProgressArg,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Initialize a new workspace at `rev0`.
    Init(InitArgs),
    /// Import libraries, RTL sources, structural netlists, or SDC constraints as a new revision.
    Import(Box<ImportArgs>),
    /// List committed revisions newest first.
    Log(LogArgs),
    /// Show metadata, tables, libraries, sources, summary, and status of a revision.
    Show(ShowArgs),
    /// Compare tables, libraries, and summary metrics between two revisions.
    Diff(DiffArgs),
    /// Move HEAD from `--base-rev` to an existing revision `--to`.
    Revert(RevertArgs),
    /// Export a committed table to a file.
    Export(ExportArgs),
    /// Print a shell completion script.
    Completions(CompletionsArgs),
}

#[derive(Args, Debug)]
pub struct InitArgs {}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// Detailed RC SPEF (repeatable); each needs an extraction corner, paired in order with
    /// `--spef-corner`.
    #[arg(long, requires = "spef_corner", value_name = "FILE")]
    pub spef: Vec<PathBuf>,
    /// The library corner of each `--spef`, in the same order (repeatable).
    #[arg(long, requires = "spef", value_name = "NAME")]
    pub spef_corner: Vec<String>,
    #[arg(long, default_value_t = 32, requires = "spef")]
    pub max_spef_mib: usize,
    #[arg(long, default_value_t = 100_000, requires = "spef")]
    pub max_rc_nets: usize,
    #[arg(long, default_value_t = 500_000, requires = "spef")]
    pub max_rc_nodes: usize,
    #[arg(long, default_value_t = 1_000_000, requires = "spef")]
    pub max_rc_elements: usize,
    #[arg(long, default_value_t = 16, requires = "spef")]
    pub max_rc_name_mib: usize,
    /// Base revision expected to be HEAD (e.g. `rev0`).
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: String,
    /// Optional commit message.
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
    /// Liberty library file to import (repeatable).
    #[arg(long = "lib", action = clap::ArgAction::Append, value_name = "FILE")]
    pub libs: Vec<PathBuf>,
    /// Corner name for the preceding `--lib` (default `"default"`).
    #[arg(long = "corner", action = clap::ArgAction::Append, value_name = "NAME")]
    pub corners: Vec<String>,
    /// RTL source files: Verilog/SystemVerilog, or BLIF (`.blif`) or AIGER (`.aag`, `.aig`).
    #[arg(long = "rtl", num_args = 1.., action = clap::ArgAction::Append, value_name = "FILE")]
    pub rtl: Vec<PathBuf>,
    /// Top module name for `--rtl`.
    #[arg(long = "top", value_name = "NAME")]
    pub top: Option<String>,
    /// Preprocessor macro definition `NAME` or `NAME=VAL` for `--rtl`.
    #[arg(short = 'D', action = clap::ArgAction::Append, value_name = "NAME[=VAL]")]
    pub defines: Vec<String>,
    /// Preprocessor include directory for `--rtl`.
    #[arg(short = 'I', action = clap::ArgAction::Append, value_name = "DIR")]
    pub include_dirs: Vec<PathBuf>,
    /// Top-level parameter override `NAME=VAL` for `--rtl`.
    #[arg(short = 'P', action = clap::ArgAction::Append, value_name = "NAME=VAL")]
    pub params: Vec<String>,
    /// Do not predefine `SYNTHESIS` when preprocessing `--rtl`.
    #[arg(long = "no-predefined-macros")]
    pub no_predefined_macros: bool,
    /// Structural gate-level Verilog netlist file.
    #[arg(long = "netlist", value_name = "FILE")]
    pub netlist: Option<PathBuf>,
    /// Largest `--netlist` file, in MiB, that the full Verilog reader reads when the streaming
    /// reader declines it (RCLI-REQ-0024).
    #[arg(long, default_value_t = 64, requires = "netlist", value_name = "MIB")]
    pub max_netlist_ast_mib: u64,
    /// Synopsys Design Constraints (SDC) files.
    #[arg(long = "sdc", num_args = 1.., action = clap::ArgAction::Append, value_name = "FILE")]
    pub sdc: Vec<PathBuf>,
    /// Environment variable binding `NAME=VALUE` for `--sdc`.
    #[arg(long = "sdc-env", action = clap::ArgAction::Append, value_name = "NAME=VALUE")]
    pub sdc_env: Vec<String>,
}

#[derive(Args, Debug)]
pub struct LogArgs {
    /// Maximum number of revisions to list.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// Revision to inspect (default `HEAD`).
    #[arg(long = "rev", default_value = "HEAD", value_name = "REV")]
    pub rev: String,
}

#[derive(Args, Debug)]
pub struct DiffArgs {
    /// Starting revision.
    #[arg(long = "from", value_name = "REV")]
    pub from: String,
    /// Ending revision.
    #[arg(long = "to", value_name = "REV")]
    pub to: String,
}

#[derive(Args, Debug)]
pub struct RevertArgs {
    /// Base revision expected to be HEAD.
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: String,
    /// Target revision to become HEAD.
    #[arg(long = "to", value_name = "REV")]
    pub to: String,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Revision to export from (default `HEAD`).
    #[arg(long = "rev", default_value = "HEAD", value_name = "REV")]
    pub rev: String,
    /// Destination structural Verilog file for the netlist table.
    #[arg(long = "netlist", value_name = "FILE")]
    pub netlist: PathBuf,
}

#[derive(Args, Debug)]
pub struct CompletionsArgs {
    /// Shell to generate completions for (`bash`, `elvish`, `fish`, `powershell`, `zsh`).
    #[arg(value_name = "SHELL")]
    pub shell: clap_complete::Shell,
}

/// Returns the clap [`clap::Command`] definition.
pub fn command() -> clap::Command {
    Cli::command()
}

/// Pre-scanned options extracted from raw argv before clap runs, so JSON mode and history logging
/// work even when clap rejects the command line (RCLI-REQ-0004, RCLI-REQ-0008).
#[derive(Debug, Default)]
struct Prescan {
    format: FormatArg,
    workspace: Option<String>,
    base_rev: Option<String>,
    subcommand: Option<String>,
    quiet: bool,
}

fn prescan(args: &[OsString]) -> Prescan {
    let mut out = Prescan::default();
    let strs: Vec<String> = args
        .iter()
        .skip(1)
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let known_subs = [
        "init",
        "import",
        "log",
        "show",
        "diff",
        "revert",
        "export",
        "completions",
    ];
    let mut i = 0;
    while i < strs.len() {
        let a = &strs[i];
        if a == "--" {
            break;
        }
        if let Some(v) = a.strip_prefix("--format=") {
            if v == "json" {
                out.format = FormatArg::Json;
            } else if v == "text" {
                out.format = FormatArg::Text;
            }
        } else if a == "--format" {
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

fn app_diag_code(code: ErrorCode) -> DiagCode {
    match code {
        ErrorCode::Usage => CODE_USAGE,
        ErrorCode::Internal => CODE_INTERNAL,
        ErrorCode::Cancelled => CODE_CANCELLED,
        ErrorCode::BackendUnavailable => CODE_BACKEND,
        _ => code.diag_code().unwrap_or(CODE_USAGE),
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

/// Emits the result or error in `--format text|json` and returns the process [`ExitCode`].
fn finish_error(
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

fn finish_ok(outcome: OutputOutcome, format: FormatArg, quiet: bool) -> ExitCode {
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
                body.push('\n');
                write_stdout(&body);
            }
        }
    }
    ExitCode::from(0)
}

/// Pairs `--lib` and `--corner` arguments using their command-line indices (RCLI-REQ-0012):
/// each `--corner` applies to the `--lib` immediately preceding it; a `--lib` without a
/// following `--corner` uses `"default"`.
fn pair_libs_and_corners(
    matches: &ArgMatches,
    libs: &[PathBuf],
    corners: &[String],
) -> Result<Vec<(PathBuf, String)>, Error> {
    if libs.is_empty() {
        if !corners.is_empty() {
            return Err(Error::usage("--corner requires a preceding --lib <FILE>"));
        }
        return Ok(Vec::new());
    }
    let lib_indices: Vec<usize> = matches
        .indices_of("libs")
        .map(|it| it.collect())
        .unwrap_or_default();
    let corner_indices: Vec<usize> = matches
        .indices_of("corners")
        .map(|it| it.collect())
        .unwrap_or_default();

    let mut assigned: Vec<Option<String>> = vec![None; libs.len()];
    for (c_pos, &c_idx) in corner_indices.iter().enumerate() {
        // Find the last --lib whose index is before c_idx.
        let Some(lib_pos) = lib_indices.iter().rposition(|&l_idx| l_idx < c_idx) else {
            return Err(Error::usage("--corner must follow a --lib <FILE> option"));
        };
        if assigned[lib_pos].is_some() {
            return Err(Error::usage(
                "at most one --corner may follow each --lib <FILE>",
            ));
        }
        assigned[lib_pos] = Some(corners[c_pos].clone());
    }

    Ok(libs
        .iter()
        .cloned()
        .zip(assigned)
        .map(|(path, c)| (path, c.unwrap_or_else(|| "default".to_string())))
        .collect())
}

/// Maybe logs a usage error to `history.jsonl` if the command was a workspace-changing subcommand
/// (`init`, `import`, `revert`) and `-w` points to an existing workspace (RCLI-REQ-0008).
fn maybe_log_usage_rejection(pre: &Prescan, argv_strings: &[String], err: &Error) {
    let Some(sub) = pre.subcommand.as_deref() else {
        return;
    };
    if !matches!(sub, "init" | "import" | "revert") {
        return;
    }
    let Some(ws_dir) = pre.workspace.as_deref() else {
        return;
    };
    if let Ok(ws) = Workspace::open(ws_dir) {
        let base_rev = pre.base_rev.as_deref().and_then(|s| Rev::parse(s).ok());
        let inv = Invocation::new(
            "rusteda",
            env!("CARGO_PKG_VERSION"),
            format!("rusteda {sub}"),
            argv_strings.to_vec(),
        );
        let _ = ws.log_rejected(&inv, base_rev, err);
    }
}

/// Runs `rusteda` with `args` (including `argv[0]`) and returns its [`ExitCode`].
pub fn run(args: Vec<OsString>) -> ExitCode {
    let pre = prescan(&args);
    let argv_strings: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cmd_name = pre
        .subcommand
        .as_deref()
        .map_or_else(|| "rusteda".to_string(), |s| format!("rusteda {s}"));

    let matches = match command().try_get_matches_from(&args) {
        Ok(m) => m,
        Err(e) => {
            use clap::error::ErrorKind;
            match e.kind() {
                ErrorKind::DisplayVersion => {
                    let ver = env!("CARGO_PKG_VERSION");
                    if pre.format == FormatArg::Json {
                        let mut env = Envelope::new(&cmd_name);
                        env.workspace = pre.workspace.clone();
                        let _ = env.set("version", ver);
                        return finish_ok(
                            OutputOutcome {
                                envelope: env,
                                text_lines: Vec::new(),
                            },
                            FormatArg::Json,
                            pre.quiet,
                        );
                    }
                    write_stdout(&format!("rusteda {ver}\n"));
                    return ExitCode::from(0);
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
                            },
                            FormatArg::Json,
                            pre.quiet,
                        );
                    }
                    write_stdout(&help_text);
                    return ExitCode::from(0);
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
                    maybe_log_usage_rejection(&pre, &argv_strings, &err);
                    let diag = Diagnostic::new(Severity::Error, CODE_USAGE, msg)
                        .with_help(format!("{cmd_name} --help lists every option"));
                    let base_rev = pre.base_rev.as_deref().and_then(|s| Rev::parse(s).ok());
                    return finish_error(
                        &cmd_name,
                        pre.workspace.as_deref(),
                        base_rev,
                        err,
                        vec![diag],
                        pre.format,
                        pre.quiet,
                    );
                }
            }
        }
    };

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => {
            let msg = e.to_string();
            let err = Error::usage(&msg);
            maybe_log_usage_rejection(&pre, &argv_strings, &err);
            let base_rev = pre.base_rev.as_deref().and_then(|s| Rev::parse(s).ok());
            return finish_error(
                &cmd_name,
                pre.workspace.as_deref(),
                base_rev,
                err,
                Vec::new(),
                pre.format,
                pre.quiet,
            );
        }
    };

    let format = cli.format;
    let quiet = cli.quiet;

    if cli.threads > 0 {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
            .map_err(|e| {
                Diagnostic::new(
                    Severity::Warning,
                    CODE_THREAD_POOL,
                    format!("thread pool already initialized: {e}"),
                )
            });
    }

    // Handle `completions` first since it does not require `-w/--workspace`.
    if let Cmd::Completions(comp) = &cli.cmd {
        let mut buf = Vec::new();
        let mut cmd_def = command();
        clap_complete::generate(comp.shell, &mut cmd_def, "rusteda", &mut buf);
        let script = String::from_utf8_lossy(&buf).into_owned();
        if format == FormatArg::Json {
            let mut env = Envelope::new("rusteda completions");
            let _ = env.set("shell", &comp.shell.to_string());
            let _ = env.set("script", &script);
            return finish_ok(
                OutputOutcome {
                    envelope: env,
                    text_lines: Vec::new(),
                },
                FormatArg::Json,
                quiet,
            );
        }
        write_stdout(&script);
        return ExitCode::from(0);
    }

    let sub_name = match &cli.cmd {
        Cmd::Init(_) => "rusteda init",
        Cmd::Import(_) => "rusteda import",
        Cmd::Log(_) => "rusteda log",
        Cmd::Show(_) => "rusteda show",
        Cmd::Diff(_) => "rusteda diff",
        Cmd::Revert(_) => "rusteda revert",
        Cmd::Export(_) => "rusteda export",
        Cmd::Completions(_) => "rusteda completions",
    };

    let Some(ws_path) = cli.workspace.as_ref() else {
        let err = Error::usage(format!("{sub_name} requires -w/--workspace <DIR>"));
        return finish_error(sub_name, None, None, err, Vec::new(), format, quiet);
    };
    let ws_arg = ws_path.display().to_string();
    let inv = Invocation::new(
        "rusteda",
        env!("CARGO_PKG_VERSION"),
        sub_name,
        argv_strings.clone(),
    );

    match &cli.cmd {
        Cmd::Init(_) => match run_init(ws_path, &ws_arg, &inv) {
            Ok(out) => finish_ok(out, format, quiet),
            Err(e) => finish_error(sub_name, Some(&ws_arg), None, e, Vec::new(), format, quiet),
        },
        Cmd::Import(imp) => {
            let sub_matches = matches
                .subcommand_matches("import")
                .expect("import subcommand matches");
            let base_rev_parsed = Rev::parse(&imp.base_rev).ok();

            let ws = match Workspace::open(ws_path) {
                Ok(w) => w,
                Err(e) => {
                    return finish_error(
                        sub_name,
                        Some(&ws_arg),
                        base_rev_parsed,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            };

            let libs = match pair_libs_and_corners(sub_matches, &imp.libs, &imp.corners) {
                Ok(l) => l,
                Err(e) => {
                    let _ = ws.log_rejected(&inv, base_rev_parsed, &e);
                    return finish_error(
                        sub_name,
                        Some(&ws_arg),
                        base_rev_parsed,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            };

            if imp.rtl.is_empty()
                && (imp.top.is_some()
                    || !imp.defines.is_empty()
                    || !imp.include_dirs.is_empty()
                    || !imp.params.is_empty()
                    || imp.no_predefined_macros)
            {
                let e = Error::usage(
                    "--top, -D, -I, -P, and --no-predefined-macros require --rtl <FILE>...",
                );
                let _ = ws.log_rejected(&inv, base_rev_parsed, &e);
                return finish_error(
                    sub_name,
                    Some(&ws_arg),
                    base_rev_parsed,
                    e,
                    Vec::new(),
                    format,
                    quiet,
                );
            }

            let rtl_args = if !imp.rtl.is_empty() {
                let Some(top) = imp.top.clone() else {
                    let e = Error::usage("--rtl requires --top <NAME>");
                    let _ = ws.log_rejected(&inv, base_rev_parsed, &e);
                    return finish_error(
                        sub_name,
                        Some(&ws_arg),
                        base_rev_parsed,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                };
                Some(RtlImportArgs {
                    files: imp.rtl.clone(),
                    top,
                    defines: imp.defines.clone(),
                    include_dirs: imp.include_dirs.clone(),
                    params: imp.params.clone(),
                    no_predefined_macros: imp.no_predefined_macros,
                })
            } else {
                None
            };

            if imp.spef.len() != imp.spef_corner.len() {
                let e = Error::usage(
                    "every --spef FILE needs its own --spef-corner NAME, paired in order",
                );
                let _ = ws.log_rejected(&inv, base_rev_parsed, &e);
                return finish_error(
                    sub_name,
                    Some(&ws_arg),
                    base_rev_parsed,
                    e,
                    Vec::new(),
                    format,
                    quiet,
                );
            }

            if imp.sdc.is_empty() && !imp.sdc_env.is_empty() {
                let e = Error::usage("--sdc-env requires --sdc <FILE>...");
                let _ = ws.log_rejected(&inv, base_rev_parsed, &e);
                return finish_error(
                    sub_name,
                    Some(&ws_arg),
                    base_rev_parsed,
                    e,
                    Vec::new(),
                    format,
                    quiet,
                );
            }

            let cmd = ImportCommand {
                base_rev: imp.base_rev.clone(),
                message: imp.message.clone(),
                libs,
                rtl: rtl_args,
                netlist: imp.netlist.clone(),
                max_netlist_ast_mib: imp.max_netlist_ast_mib,
                sdc_files: imp.sdc.clone(),
                sdc_env: imp.sdc_env.clone(),
                spefs: imp
                    .spef
                    .iter()
                    .cloned()
                    .zip(imp.spef_corner.iter().cloned())
                    .collect(),
                spef_limits: rusteda_formats::spef::Limits {
                    max_bytes: imp.max_spef_mib.saturating_mul(1 << 20),
                    max_nets: imp.max_rc_nets,
                    max_nodes: imp.max_rc_nodes,
                    max_elements: imp.max_rc_elements,
                    max_name_bytes: imp.max_rc_name_mib.saturating_mul(1 << 20),
                },
            };
            let mut diags = Vec::new();
            match run_import(&ws, &ws_arg, &cmd, &inv, &mut diags) {
                Ok(out) => finish_ok(out, format, quiet),
                Err(e) => finish_error(
                    sub_name,
                    Some(&ws_arg),
                    base_rev_parsed,
                    e,
                    diags,
                    format,
                    quiet,
                ),
            }
        }
        Cmd::Log(log) => {
            let ws = match Workspace::open(ws_path) {
                Ok(w) => w,
                Err(e) => {
                    return finish_error(
                        sub_name,
                        Some(&ws_arg),
                        None,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            };
            match run_log(&ws, &ws_arg, log.limit) {
                Ok(out) => finish_ok(out, format, quiet),
                Err(e) => finish_error(sub_name, Some(&ws_arg), None, e, Vec::new(), format, quiet),
            }
        }
        Cmd::Show(show) => {
            let ws = match Workspace::open(ws_path) {
                Ok(w) => w,
                Err(e) => {
                    return finish_error(
                        sub_name,
                        Some(&ws_arg),
                        None,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            };
            match run_show(&ws, &ws_arg, &show.rev) {
                Ok(out) => finish_ok(out, format, quiet),
                Err(e) => finish_error(sub_name, Some(&ws_arg), None, e, Vec::new(), format, quiet),
            }
        }
        Cmd::Diff(diff) => {
            let ws = match Workspace::open(ws_path) {
                Ok(w) => w,
                Err(e) => {
                    return finish_error(
                        sub_name,
                        Some(&ws_arg),
                        None,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            };
            match run_diff(&ws, &ws_arg, &diff.from, &diff.to) {
                Ok(out) => finish_ok(out, format, quiet),
                Err(e) => finish_error(sub_name, Some(&ws_arg), None, e, Vec::new(), format, quiet),
            }
        }
        Cmd::Revert(rev) => {
            let base_rev_parsed = Rev::parse(&rev.base_rev).ok();
            let ws = match Workspace::open(ws_path) {
                Ok(w) => w,
                Err(e) => {
                    return finish_error(
                        sub_name,
                        Some(&ws_arg),
                        base_rev_parsed,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            };
            match run_revert(&ws, &ws_arg, &rev.base_rev, &rev.to, &inv) {
                Ok(out) => finish_ok(out, format, quiet),
                Err(e) => finish_error(
                    sub_name,
                    Some(&ws_arg),
                    base_rev_parsed,
                    e,
                    Vec::new(),
                    format,
                    quiet,
                ),
            }
        }
        Cmd::Export(exp) => {
            let ws = match Workspace::open(ws_path) {
                Ok(w) => w,
                Err(e) => {
                    return finish_error(
                        sub_name,
                        Some(&ws_arg),
                        None,
                        e,
                        Vec::new(),
                        format,
                        quiet,
                    );
                }
            };
            match run_export(&ws, &ws_arg, &exp.rev, &exp.netlist) {
                Ok(out) => finish_ok(out, format, quiet),
                Err(e) => finish_error(sub_name, Some(&ws_arg), None, e, Vec::new(), format, quiet),
            }
        }
        Cmd::Completions(_) => unreachable!(),
    }
}
