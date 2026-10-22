// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The `rustsys` command (`docs/specs/cli/README.md`, SYNTH-REQ-0701, CHECK-REQ-0001): a library
//! behind a thin binary.

pub mod args;
mod out;
pub mod proto;
mod show;
pub mod ws;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches};
use proto::exec::{ExecOptions, complete_from_run, execute};
use proto::job::Job;
use proto::record::{RunLocation, read_record, resolve};
use proto::{Diag, ExitClass};

use args::{CheckCmd, Cli, Cmd, ColorArg, PrintArg, ProgressArg};
use out::{Err, ProgressMode};

/// GPU runtimes of this build, CPU first.
pub fn runtimes() -> Vec<&'static str> {
    let mut v = vec!["cpu"];
    if cfg!(feature = "cuda") {
        v.push("cuda");
    }
    v
}

/// `0.1.0 (runtimes: cpu, cuda)`.
pub fn long_version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        format!(
            "{} (runtimes: {})",
            env!("CARGO_PKG_VERSION"),
            runtimes().join(", ")
        )
    })
}

/// The command definition, for completions and tests.
pub fn command() -> clap::Command {
    Cli::command()
}

/// Settings shared by every subcommand.
#[derive(Debug, Clone)]
pub(crate) struct Globals {
    pub color: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub mode: ProgressMode,
    pub threads: usize,
    pub runs_root: Option<PathBuf>,
    pub arguments: Vec<String>,
}

impl Globals {
    pub fn err(&self, product: &'static str) -> Err {
        Err::new(self.color, self.quiet, self.mode, product)
    }
}

fn color_on(choice: ColorArg) -> bool {
    match choice {
        ColorArg::Always => true,
        ColorArg::Never => false,
        ColorArg::Auto => {
            std::io::stderr().is_terminal()
                && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
                && std::env::var("TERM").map_or(true, |t| t != "dumb")
        }
    }
}

/// `--color` before parsing, so that usage errors are coloured as asked.
fn prescan_color(args: &[OsString]) -> ColorArg {
    let mut it = args.iter().map(|a| a.to_string_lossy());
    while let Some(a) = it.next() {
        let v = if let Some(v) = a.strip_prefix("--color=") {
            Some(v.to_string())
        } else if a == "--color" {
            it.next().map(|s| s.to_string())
        } else {
            None
        };
        match v.as_deref() {
            Some("always") => return ColorArg::Always,
            Some("never") => return ColorArg::Never,
            _ => {}
        }
    }
    ColorArg::Auto
}

/// The command reached by the words of `args` that name subcommands.
fn command_path(args: &[OsString]) -> (String, clap::Command) {
    let mut cmd = command();
    let mut path = vec!["rustsys".to_string()];
    for a in args.iter().skip(1) {
        let a = a.to_string_lossy();
        let next = cmd.find_subcommand(a.as_ref()).cloned();
        if let Some(c) = next {
            path.push(c.get_name().to_string());
            cmd = c;
        }
    }
    (path.join(" "), cmd)
}

/// Prints a command-line error as a coded message with help lines (CLI-REQ-0005): what is
/// missing or wrong, the valid values, and the command that lists every option.
fn usage_error(e: &clap::Error, args: &[OsString], color: bool) {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    let get = |k: ContextKind| {
        e.get(k).map(|v| match v {
            ContextValue::String(s) => s.clone(),
            ContextValue::Strings(v) => v.join(", "),
            ContextValue::StyledStr(s) => s.to_string(),
            ContextValue::StyledStrs(v) => v
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            ContextValue::Number(n) => n.to_string(),
            _ => String::new(),
        })
    };
    let (path, cmd) = command_path(args);
    let text = e.render().to_string();
    let first = text.lines().next().unwrap_or("invalid command line");
    let first = first.strip_prefix("error: ").unwrap_or(first).to_string();
    let mut code = "A0003";
    let msg = match e.kind() {
        ErrorKind::MissingRequiredArgument => format!(
            "missing required arguments: {}",
            get(ContextKind::InvalidArg).unwrap_or_default()
        ),
        ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand | ErrorKind::MissingSubcommand => {
            let subs: Vec<String> = cmd
                .get_subcommands()
                .filter(|c| !c.is_hide_set() && c.get_name() != "help")
                .map(|c| c.get_name().to_string())
                .collect();
            if subs.is_empty() {
                format!("{path} needs arguments")
            } else {
                format!("{path} needs a command: {}", subs.join(", "))
            }
        }
        ErrorKind::InvalidValue => {
            // `show` reports its own option errors as A0022 (CLI-REQ-0100).
            if path.ends_with(" show") {
                code = "A0022";
            }
            let arg = get(ContextKind::InvalidArg).unwrap_or_default();
            let val = get(ContextKind::InvalidValue).unwrap_or_default();
            match get(ContextKind::ValidValue) {
                Some(v) if !v.is_empty() => {
                    format!("invalid value '{val}' for {arg}; valid values: {v}")
                }
                _ => first,
            }
        }
        _ => first,
    };
    let err = Err::new(color, false, ProgressMode::Off, "rustsys");
    let mut d = Diag::error(code, msg);
    let mut help: Vec<String> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("tip: ").map(String::from))
        .collect();
    help.push(format!("{path} --help lists every option"));
    d.help = Some(help.join("; "));
    err.diag(&d);
}

/// Runs the command line `args` (including the program name).
pub fn run(args: Vec<OsString>) -> ExitCode {
    // Panics are reported as coded messages with their location, nothing else (CLI-REQ-0012).
    proto::exec::install_quiet_panic_hook();
    let pre = ws::prescan(&args);
    let mut argv_strings: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(first) = argv_strings.first_mut() {
        *first = "rustsys".to_string();
    }
    let color = color_on(prescan_color(&args));
    let cmd = command().color(if color {
        clap::ColorChoice::Always
    } else {
        clap::ColorChoice::Never
    });
    let parsed = cmd
        .try_get_matches_from(&args)
        .and_then(|m| Cli::from_arg_matches(&m));
    let cli = match parsed {
        Ok(c) => c,
        Err(e) if pre.is_workspace_mode() => {
            return ws::handle_clap_error(&e, &pre, &argv_strings);
        }
        Err(e) if e.use_stderr() => {
            usage_error(&e, &args, color);
            return ExitCode::from(1);
        }
        Err(e) => {
            let _ = out::stdout(e.render().to_string().as_bytes());
            return ExitCode::SUCCESS;
        }
    };
    let color = color_on(cli.color);
    let mode = match cli.progress {
        Some(ProgressArg::Live) => ProgressMode::Live,
        Some(ProgressArg::Log) => ProgressMode::Log,
        Some(ProgressArg::Off) => ProgressMode::Off,
        None if cli.verbose => ProgressMode::Log,
        None | Some(ProgressArg::Auto) => {
            if cli.quiet {
                ProgressMode::Off
            } else if std::io::stderr().is_terminal()
                && std::env::var("TERM").map_or(true, |t| t != "dumb")
            {
                ProgressMode::Live
            } else {
                ProgressMode::Off
            }
        }
    };
    let g = Globals {
        color,
        quiet: cli.quiet,
        verbose: cli.verbose,
        mode: if cli.quiet { ProgressMode::Off } else { mode },
        threads: cli.threads,
        runs_root: cli.runs_root.clone().or_else(|| {
            std::env::var_os("RUSTSYS_RUNS_ROOT")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        }),
        arguments: args
            .iter()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
    };
    let is_ws = cli.workspace.is_some()
        || pre.has_format
        || matches!(
            &cli.cmd,
            Cmd::Report(_) | Cmd::OptTiming(_) | Cmd::Resize(_)
        );
    if cli.threads > 0 {
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
        {
            if !is_ws {
                g.err("rustsys").diag(&Diag::error(
                    "A0001",
                    format!("cannot start {} worker threads: {e}", cli.threads),
                ));
                return ExitCode::from(2);
            }
        }
    }
    // A panic anywhere is an internal failure (CLI-REQ-0012).
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if is_ws {
            ws::dispatch_workspace(&cli, &g, &args)
        } else {
            dispatch(&cli, &g)
        }
    }));
    match result {
        Ok(code) => code,
        Err(p) => {
            let msg = proto::exec::panic_message(&*p);
            if is_ws {
                let err = rusteda_workspace::Error::internal(format!("internal failure: {msg}"));
                let sub = pre
                    .subcommand
                    .as_deref()
                    .map_or_else(|| "rustsys".to_string(), |s| format!("rustsys {s}"));
                return ws::finish_error(
                    &sub,
                    pre.workspace.as_deref(),
                    None,
                    err,
                    Vec::new(),
                    cli.format,
                    cli.quiet,
                );
            }
            g.err("rustsys")
                .diag(&Diag::error("A0002", format!("internal failure: {msg}")));
            ExitCode::from(2)
        }
    }
}

fn dispatch(cli: &Cli, g: &Globals) -> ExitCode {
    match &cli.cmd {
        Cmd::Synth(a) => product(g, a.job(), &a.place, None),
        Cmd::Check(c) => match &c.what {
            Some(CheckCmd::Synth(a)) => product(g, a.job(), &a.place, a.from_run.as_deref()),
            None => {
                g.err("rustsys").diag(
                    &Diag::error("A0003", "rustsys check needs a command: synth")
                        .with_help("rustsys check --help lists every option"),
                );
                ExitCode::from(1)
            }
        },
        Cmd::Show(a) => show::show(g, a),
        Cmd::Report(_) | Cmd::OptTiming(_) | Cmd::Resize(_) => ws::dispatch_workspace(cli, g, &[]),
        Cmd::Completions(a) => completions(g, a),
    }
}

fn code(class: ExitClass) -> ExitCode {
    ExitCode::from(class.status())
}

/// Resolves a run reference to a run directory and its record.
pub(crate) fn run_dir(
    g: &Globals,
    reference: &str,
) -> Result<(PathBuf, proto::record::RunRecord), Diag> {
    match resolve(reference, g.runs_root.as_deref())? {
        RunLocation::Dir(d) => {
            let r = read_record(&d)?;
            Ok((d, r))
        }
        RunLocation::Report(p) => Err(Diag::error(
            "A0009",
            format!("{} is a report file, not a run directory", p.display()),
        )),
    }
}

/// `synth` and `check synth`.
fn product(
    g: &Globals,
    mut job: Job,
    place: &args::PlacementArgs,
    from_run: Option<&str>,
) -> ExitCode {
    let product = job.product();
    let err = g.err(product.name());
    if let Some(r) = from_run {
        let done = run_dir(g, r).and_then(|(d, rec)| complete_from_run(&mut job, &rec, &d));
        if let Err(d) = done {
            err.diag(&d);
            return ExitCode::from(1);
        }
    }
    let problems = job.problems();
    let sources = job.source_problems();
    if !problems.is_empty() || !sources.is_empty() {
        let cmd = product.command();
        for (code, p) in problems.into_iter().map(|p| ("A0003", p)).chain(sources) {
            err.diag(
                &Diag::error(code, p).with_help(format!("rustsys {cmd} --help lists every option")),
            );
        }
        return ExitCode::from(1);
    }
    let o = ExecOptions {
        threads: g.threads,
        runs_root: g.runs_root.clone(),
        arguments: g.arguments.clone(),
        verbose: g.verbose,
        runtimes: runtimes().into_iter().map(String::from).collect(),
    };
    let placement = place.placement();
    let outcome = out::with_ticker(&err, || execute(&job, &placement, &o, None, Some(&err)));
    // Results of finished runs, verdicts `not-equivalent` and `undecided` included (CLI-REQ-0009).
    if outcome.class.has_result() {
        let printed = match place.print {
            PrintArg::Text => out::stdout(outcome.text.as_bytes()),
            PrintArg::Json => {
                let bytes = match &outcome.report_path {
                    Some(p) => std::fs::read(p).unwrap_or_default(),
                    None => outcome
                        .report
                        .as_ref()
                        .map(|r| r.to_json_pretty())
                        .unwrap_or_default()
                        .into_bytes(),
                };
                out::stdout(&bytes)
            }
            PrintArg::None => Ok(()),
        };
        if let Err(d) = printed {
            err.diag(&d);
            return ExitCode::from(1);
        }
    }
    code(outcome.class)
}

/// `completions [SHELL]` (CLI-REQ-0150).
fn completions(g: &Globals, a: &args::CompletionsArgs) -> ExitCode {
    let Some(shell) = a.shell.or_else(clap_complete::Shell::from_env) else {
        use clap::ValueEnum;
        let shells: Vec<String> = clap_complete::Shell::value_variants()
            .iter()
            .filter_map(|s| s.to_possible_value().map(|v| v.get_name().to_string()))
            .collect();
        g.err("rustsys").diag(&Diag::error(
            "A0030",
            format!(
                "cannot tell the shell from the environment; name one of: {}",
                shells.join(", ")
            ),
        ));
        return ExitCode::from(1);
    };
    let mut cmd = command();
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut cmd, "rustsys", &mut buf);
    match out::stdout(&buf) {
        Ok(()) => ExitCode::SUCCESS,
        Err(d) => {
            g.err("rustsys").diag(&d);
            ExitCode::from(1)
        }
    }
}
