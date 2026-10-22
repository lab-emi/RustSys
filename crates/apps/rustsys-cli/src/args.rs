// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Command-line arguments (SYNTH-REQ-0701, CHECK-REQ-0001, CLI-REQ-0001) and their jobs.

use std::path::PathBuf;

use crate::proto::argv::Placement;
use crate::proto::job::{
    Backend, CheckSynthJob, Effort, FrontEnd, Goal, InitialValues, Job, SynthJob,
};
use crate::proto::options as o;
use clap::builder::PossibleValue;
use clap::{Args, Parser, Subcommand, ValueEnum};

const GETTING_STARTED: &str = "Getting started:
  rustsys --runs-root runs synth --lib cells.lib --record top.sv
  rustsys --runs-root runs check synth --from-run @synth --record
  rustsys --runs-root runs show @synth --section cells";

#[derive(Parser, Debug)]
#[command(
    name = "rustsys",
    version = crate::long_version(),
    propagate_version = true,
    about = "RustSys: logic synthesis and standard-cell mapping of SystemVerilog RTL",
    after_help = GETTING_STARTED,
)]
pub struct Cli {
    /// Workspace directory for workspace-first revision commands (`rusteda.v1`).
    #[arg(
        short = 'w',
        long = "workspace",
        global = true,
        value_name = "DIR",
        value_hint = clap::ValueHint::DirPath
    )]
    pub workspace: Option<PathBuf>,
    /// Output format in workspace mode: human-readable text or a single rusteda.v1 JSON envelope.
    #[arg(long = "format", global = true, value_enum, default_value_t = FormatArg::Text)]
    pub format: FormatArg,
    /// Worker threads of the parallel phases (0: one per available CPU).
    #[arg(long, global = true, default_value_t = 0)]
    pub threads: usize,
    /// Standard error keeps only errors and warnings.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Verbose notes; progress per phase unless --progress is given.
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Colour of the severity on standard error.
    #[arg(long, global = true, value_enum, default_value_t = ColorArg::Auto)]
    pub color: ColorArg,
    /// Progress on standard error: live line, phase/step logs, or none.
    #[arg(long, global = true, value_enum)]
    pub progress: Option<ProgressArg>,
    /// Where --record puts runs and @ references look (default: $RUSTSYS_RUNS_ROOT).
    #[arg(long, global = true, value_hint = clap::ValueHint::DirPath)]
    pub runs_root: Option<PathBuf>,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum FormatArg {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorArg {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ProgressArg {
    Auto,
    Live,
    Log,
    Off,
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Cmd {
    /// Synthesize SystemVerilog RTL into a standard-cell netlist.
    #[command(
        after_help = "Example:\n  rustsys synth --lib cells.lib --out top_mapped.v --report top.json top.sv"
    )]
    Synth(SynthArgs),
    /// The independent synthesis checker.
    Check(CheckArgs),
    /// Show sections of a run or report.
    #[command(
        after_help = "Examples:\n  rustsys show top.json --section cells --sort count --reverse --limit 0\n  rustsys --runs-root runs show @check-synth --section mismatches --print tsv"
    )]
    Show(ShowArgs),
    /// Render sections of a workspace revision report (`synth.json` or `check.json`).
    Report(ReportArgs),
    /// Run incremental post-mapping timing and load-rule optimization on a workspace revision.
    OptTiming(OptTimingArgs),
    /// Resize target cell instances in a workspace revision to another drive variant.
    Resize(ResizeArgs),
    /// Print a shell completion script.
    Completions(CompletionsArgs),
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    #[command(subcommand)]
    pub what: Option<CheckCmd>,
    #[command(flatten)]
    pub direct: CheckSynthArgs,
}

#[derive(Subcommand, Debug)]
pub enum CheckCmd {
    /// Check that a netlist implements the RTL.
    Synth(CheckSynthArgs),
}

/// Placement and standard output of a run (PROTO-REQ-0050, CLI-REQ-0003).
#[derive(Args, Debug, Clone, Default)]
pub struct PlacementArgs {
    #[arg(long, help = o::RUN_DIR.help, value_hint = clap::ValueHint::DirPath)]
    pub run_dir: Option<PathBuf>,
    #[arg(long, help = o::RECORD.help, conflicts_with = "run_dir")]
    pub record: bool,
    #[arg(long, help = o::LABEL.help)]
    pub label: Option<String>,
    #[arg(long, value_enum, default_value_t = PrintArg::Text, help = o::PRINT.help)]
    pub print: PrintArg,
}

impl PlacementArgs {
    pub fn placement(&self) -> Placement {
        Placement {
            run_dir: self.run_dir.clone(),
            record: self.record,
            label: self.label.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum PrintArg {
    #[default]
    Text,
    Json,
    None,
}

/// A backend; runtimes this build lacks are hidden from help but still parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendArg(pub Backend);

impl ValueEnum for BackendArg {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            BackendArg(Backend::Cpu),
            BackendArg(Backend::Gpu),
            BackendArg(Backend::Cuda),
        ]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        let compiled = match self.0 {
            Backend::Cpu => true,
            Backend::Gpu => crate::runtimes().len() > 1,
            Backend::Cuda => cfg!(feature = "cuda"),
            Backend::Wgpu => return None,
        };
        Some(PossibleValue::new(self.0.name()).hide(!compiled))
    }
}

fn delay_relax(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => Ok(v),
        _ => Err("a finite number not below 0".into()),
    }
}

fn positive(s: &str) -> Result<f64, String> {
    delay_relax(s).and_then(|v| {
        if v > 0.0 {
            Ok(v)
        } else {
            Err("a finite number above 0".into())
        }
    })
}

/// RTL front-end options.
#[derive(Args, Debug, Clone)]
pub struct FrontEndArgs {
    #[arg(long, help = o::TOP.help)]
    pub top: Option<String>,
    #[arg(short = 'D', value_name = "NAME[=VALUE]", help = o::DEFINE.help)]
    pub define: Vec<String>,
    #[arg(short = 'I', value_name = "DIR", help = o::INCLUDE.help, value_hint = clap::ValueHint::DirPath)]
    pub include: Vec<PathBuf>,
    #[arg(short = 'P', value_name = "NAME=EXPR", help = o::PARAM.help)]
    pub param: Vec<String>,
    #[arg(long, help = o::NO_PREDEFINED.help)]
    pub no_predefined_macros: bool,
    #[arg(long, value_name = "NAME=0|1", help = o::AFTER_RESET.help)]
    pub after_reset: Vec<String>,
    #[arg(long, help = o::UNINITIALIZED_DONT_CARE.help)]
    pub uninitialized_dont_care: bool,
    #[arg(long, help = o::ASSUME_FULL_CASE.help)]
    pub assume_full_case: bool,
    #[arg(long, help = o::ASSUME_PARALLEL_CASE.help)]
    pub assume_parallel_case: bool,
    #[arg(long = "lib", help = o::LIB.help, value_hint = clap::ValueHint::FilePath)]
    pub libs: Vec<PathBuf>,
    #[arg(long = "gate-netlist", value_name = "FILE", help = o::GATE_NETLIST.help, value_hint = clap::ValueHint::FilePath)]
    pub gate_netlist: Option<PathBuf>,
    #[arg(long = "gate-lib", value_name = "FILE", help = o::GATE_LIB.help, value_hint = clap::ValueHint::FilePath)]
    pub gate_libs: Vec<PathBuf>,
    #[arg(long = "initial-values", value_enum, help = o::INITIAL_VALUES.help)]
    pub initial_values: Option<InitialValuesArg>,
    #[arg(value_name = "SOURCE", help = o::SOURCES.help, value_hint = clap::ValueHint::FilePath)]
    pub sources: Vec<PathBuf>,
}

/// `--initial-values` (CLI-REQ-0222).
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum InitialValuesArg {
    Reset,
    Arbitrary,
}

impl InitialValuesArg {
    pub fn job(self) -> InitialValues {
        match self {
            InitialValuesArg::Reset => InitialValues::Reset,
            InitialValuesArg::Arbitrary => InitialValues::Arbitrary,
        }
    }
}

impl FrontEndArgs {
    fn job(&self) -> FrontEnd {
        FrontEnd {
            top: self.top.clone(),
            defines: self.define.clone(),
            include: self.include.clone(),
            params: self.param.clone(),
            no_predefined_macros: self.no_predefined_macros,
            after_reset: self.after_reset.clone(),
            uninitialized_dont_care: self.uninitialized_dont_care,
            assume_full_case: self.assume_full_case,
            assume_parallel_case: self.assume_parallel_case,
            libs: self.libs.clone(),
            sources: self.sources.clone(),
            gate_netlist: self.gate_netlist.clone(),
            gate_libs: self.gate_libs.clone(),
            initial_values: self.initial_values.map(InitialValuesArg::job),
        }
    }
}

#[derive(Args, Debug)]
pub struct SynthArgs {
    #[command(flatten)]
    pub fe: FrontEndArgs,
    /// Base revision expected to be HEAD in workspace mode (`-w`).
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: Option<String>,
    /// Optional commit message in workspace mode (`-w`).
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
    /// Run equivalence checking in the same revision in workspace mode (`-w`).
    #[arg(long = "check")]
    pub check: bool,
    #[arg(long = "dont-use", value_name = "GLOB", help = o::DONT_USE.help)]
    pub dont_use: Vec<String>,
    #[arg(long, value_enum, default_value = "medium", help = o::EFFORT.help)]
    pub effort: EffortArg,
    #[arg(long, value_enum, default_value = "delay", help = o::GOAL.help)]
    pub goal: GoalArg,
    #[arg(long, default_value = "0.1", value_parser = delay_relax, help = o::DELAY_RELAX.help)]
    pub delay_relax: f64,
    #[arg(long, value_parser = positive, help = o::TIMING_PERIOD.help)]
    pub timing_period: Option<f64>,
    #[arg(long = "electrical-corner", requires = "timing_period", help = o::ELECTRICAL_CORNER.help)]
    pub electrical_corners: Vec<PathBuf>,
    #[arg(long, requires = "timing_period", value_parser = positive, help = o::AREA_LIMIT.help)]
    pub area_limit: Option<f64>,
    #[arg(long, requires = "timing_period", default_value = "0", value_parser = delay_relax, help = o::INPUT_DELAY.help)]
    pub input_delay: f64,
    #[arg(long, requires = "timing_period", default_value = "0", value_parser = delay_relax, help = o::OUTPUT_DELAY.help)]
    pub output_delay: f64,
    #[arg(long, requires = "timing_period", default_value = "0.1", value_parser = delay_relax, help = o::INPUT_SLEW.help)]
    pub input_slew: f64,
    #[arg(long, requires = "timing_period", default_value = "0.1", value_parser = delay_relax, help = o::CLOCK_SLEW.help)]
    pub clock_slew: f64,
    #[arg(long, requires = "timing_period", default_value = "0.01", value_parser = delay_relax, help = o::OUTPUT_LOAD.help)]
    pub output_load: f64,
    #[arg(long, requires = "timing_period", default_value = "0.05", value_parser = delay_relax, help = o::TIMING_MARGIN.help)]
    pub timing_margin: f64,
    #[arg(long, requires = "timing_period", default_value_t = 8, value_parser = clap::value_parser!(u32).range(2..=128), help = o::BUFFER_FANOUT.help)]
    pub buffer_fanout: u32,
    #[arg(long, value_enum, default_value = "cpu", help = o::BACKEND.help)]
    pub backend: BackendArg,
    #[arg(long, help = o::OUT.help, value_hint = clap::ValueHint::FilePath)]
    pub out: Option<PathBuf>,
    #[arg(long, help = o::REPORT.help, value_hint = clap::ValueHint::FilePath)]
    pub report: Option<PathBuf>,
    #[command(flatten)]
    pub place: PlacementArgs,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum EffortArg {
    Low,
    Medium,
    High,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum GoalArg {
    Delay,
    Area,
}

impl SynthArgs {
    pub fn job(&self) -> Job {
        Job::Synth(SynthJob {
            fe: self.fe.job(),
            dont_use: self.dont_use.clone(),
            effort: match self.effort {
                EffortArg::Low => Effort::Low,
                EffortArg::Medium => Effort::Medium,
                EffortArg::High => Effort::High,
            },
            goal: match self.goal {
                GoalArg::Delay => Goal::Delay,
                GoalArg::Area => Goal::Area,
            },
            delay_relax: self.delay_relax,
            electrical_corners: self.electrical_corners.clone(),
            electrical: self
                .timing_period
                .map(|period_ns| rustsys::view::ElectricalOptions {
                    period_ns,
                    area_limit: self.area_limit,
                    input_delay_ns: self.input_delay,
                    output_delay_ns: self.output_delay,
                    input_slew_ns: self.input_slew,
                    clock_slew_ns: self.clock_slew,
                    output_load_pf: self.output_load,
                    margin_ns: self.timing_margin,
                    buffer_fanout: self.buffer_fanout as usize,
                    parallel_timing: Default::default(),
                    incremental: None,
                }),
            backend: self.backend.0,
            out: self.out.clone(),
            report: self.report.clone(),
        })
    }
}

#[derive(Args, Debug, Clone)]
pub struct CheckSynthArgs {
    #[command(flatten)]
    pub fe: FrontEndArgs,
    /// Base revision expected to be HEAD in workspace mode (`-w`).
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: Option<String>,
    /// Optional commit message in workspace mode (`-w`).
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
    #[arg(long = "netlist", help = o::NETLIST.help, value_hint = clap::ValueHint::FilePath)]
    pub netlists: Vec<PathBuf>,
    #[arg(long, help = o::REPORT.help, value_hint = clap::ValueHint::FilePath)]
    pub report: Option<PathBuf>,
    #[arg(long, default_value_t = 1, help = o::SEED.help)]
    pub seed: u64,
    #[arg(long, default_value_t = 0, help = o::CYCLES.help)]
    pub cycles: usize,
    #[arg(long, value_name = "CONFLICTS", default_value_t = rustsys_check::DEFAULT_BUDGET, help = o::BUDGET.help)]
    pub budget: u64,
    #[arg(long, value_name = "CONFLICTS", default_value_t = rustsys_check::DEFAULT_MODEL_BUDGET, help = o::MODEL_BUDGET.help)]
    pub model_budget: u64,
    #[arg(long, value_name = "COUNT", default_value_t = rustsys_check::DEFAULT_SWEEP_MAX_CANDIDATES, help = o::SWEEP_MAX_CANDIDATES.help)]
    pub sweep_max_candidates: usize,
    #[arg(long = "supply-port", value_name = "PORT", help = o::SUPPLY_PORT.help)]
    pub supply_ports: Vec<String>,
    #[arg(long, value_name = "RUN", help = o::FROM_RUN.help)]
    pub from_run: Option<String>,
    #[command(flatten)]
    pub place: PlacementArgs,
}

impl CheckSynthArgs {
    pub fn job(&self) -> Job {
        Job::CheckSynth(CheckSynthJob {
            fe: self.fe.job(),
            netlists: self.netlists.clone(),
            seed: self.seed,
            cycles: self.cycles,
            supply_ports: self.supply_ports.clone(),
            budget: self.budget,
            model_budget: self.model_budget,
            sweep_max_candidates: self.sweep_max_candidates,
            report: self.report.clone(),
        })
    }
}

#[derive(Args, Debug, Clone)]
pub struct ShowArgs {
    /// Run reference, run directory or report file (default: @, the latest run).
    #[arg(value_name = "RUN")]
    pub run: Option<String>,
    /// Revision to inspect in workspace mode (`-w`).
    #[arg(long = "rev", value_name = "REV")]
    pub rev: Option<String>,
    /// Section to print (repeatable).
    #[arg(long = "section", value_name = "NAME")]
    pub sections: Vec<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = ShowPrint::Text)]
    pub print: ShowPrint,
    /// Rows per table section (0: all).
    #[arg(long)]
    pub limit: Option<usize>,
    /// Sort table rows by this column key.
    #[arg(long, value_name = "KEY")]
    pub sort: Option<String>,
    /// Reverse the sort order.
    #[arg(long)]
    pub reverse: bool,
    /// Keep table rows that match this filter expression.
    #[arg(long, value_name = "EXPR")]
    pub filter: Option<String>,
    /// Numbers with every digit needed to read them back exactly.
    #[arg(long)]
    pub exact: bool,
    /// Show where each summary value comes from.
    #[arg(long)]
    pub sources: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ShowPrint {
    Text,
    Json,
    Tsv,
}

#[derive(Args, Debug, Clone)]
pub struct ReportArgs {
    /// Revision to inspect (default: HEAD).
    #[arg(long = "rev", default_value = "HEAD", value_name = "REV")]
    pub rev: String,
    /// Table to inspect (`synth` or `check`; default: `synth` if present, else `check`).
    #[arg(long = "table", value_name = "TABLE")]
    pub table: Option<String>,
    /// Section to print (repeatable).
    #[arg(long = "section", value_name = "NAME")]
    pub sections: Vec<String>,
    /// Rows per table section (0: all).
    #[arg(long)]
    pub limit: Option<usize>,
    /// Sort table rows by this column key.
    #[arg(long, value_name = "KEY")]
    pub sort: Option<String>,
    /// Reverse the sort order.
    #[arg(long)]
    pub reverse: bool,
    /// Keep table rows that match this filter expression.
    #[arg(long, value_name = "EXPR")]
    pub filter: Option<String>,
    /// Numbers with every digit needed to read them back exactly.
    #[arg(long)]
    pub exact: bool,
    /// Show where each summary value comes from.
    #[arg(long)]
    pub sources: bool,
}

#[derive(Args, Debug, Clone)]
pub struct OptTimingArgs {
    /// Base revision expected to be HEAD (e.g. `rev2`).
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: String,
    /// Optional commit message.
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
    /// Target handles (`path<N>`, `ep<N>` of the timing table; `inst<N>`, `net<N>`, `sel<N>`) or
    /// instance names; default: the instances on the ranked setup paths with negative slack of
    /// the timing table, or every instance without a timing table.
    #[arg(long = "target", action = clap::ArgAction::Append, value_name = "HANDLE")]
    pub targets: Vec<String>,
    /// Maximum optimization iterations, one change each (1 to 10000).
    #[arg(long = "max-iters", default_value_t = 16, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub max_iters: u32,
    /// Stop condition on the estimate: `wns_ns >= X`, `tns_ns >= X` or `no-improvement`
    /// (default `wns_ns >= 0`).
    #[arg(long = "stop-when", value_name = "COND")]
    pub stop_when: Option<String>,
    /// Search breadth per iteration: low (4 gates, next variants, no buffers), medium (16
    /// gates, all variants, smallest buffer), high (64 gates, every buffer, two splits).
    #[arg(long, value_enum, default_value = "medium")]
    pub effort: EffortArg,
    /// Area may grow by at most this percentage of the base revision's area.
    #[arg(long = "max-area-growth-pct", value_name = "P", value_parser = delay_relax)]
    pub max_area_growth_pct: Option<f64>,
    /// Maximum total cell area in library units.
    #[arg(long = "area-limit", value_parser = positive)]
    pub area_limit: Option<f64>,
    /// Idempotency key `[A-Za-z0-9_-]+`: a repeated request returns the revision it committed.
    #[arg(long = "request-id", value_name = "ID")]
    pub request_id: Option<String>,
    /// Ideal-clock period in ns (default: the smallest clock period of the constraints, else 10).
    #[arg(long = "timing-period", value_parser = positive)]
    pub timing_period: Option<f64>,
    /// Uniform input delay in ns (default: the largest `set_input_delay`, else 0).
    #[arg(long, value_parser = delay_relax)]
    pub input_delay: Option<f64>,
    /// Uniform output delay in ns (default: the largest `set_output_delay`, else 0).
    #[arg(long, value_parser = delay_relax)]
    pub output_delay: Option<f64>,
    /// Input transition in ns (default: the largest `set_input_transition`, else 0.1).
    #[arg(long, value_parser = delay_relax)]
    pub input_slew: Option<f64>,
    /// Clock transition in ns (default: the largest `set_clock_transition`, else 0.1).
    #[arg(long, value_parser = delay_relax)]
    pub clock_slew: Option<f64>,
    /// Load of each output port bit in pF (default: the largest `set_load` on ports, else 0.01).
    #[arg(long, value_parser = delay_relax)]
    pub output_load: Option<f64>,
    /// Setup margin in ns before the setup uncertainty of the constraints is added (default 0.05).
    #[arg(long, value_parser = delay_relax)]
    pub timing_margin: Option<f64>,
    /// The most sinks that one inserted buffer takes over.
    #[arg(long = "buffer-fanout", default_value_t = 8, value_parser = clap::value_parser!(u32).range(2..=128))]
    pub buffer_fanout: u32,
}

#[derive(Args, Debug, Clone)]
pub struct ResizeArgs {
    /// Base revision expected to be HEAD (e.g. `rev2`).
    #[arg(long = "base-rev", value_name = "REV")]
    pub base_rev: String,
    /// Optional commit message.
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
    /// Target handles (`inst<N>`, `net<N>`, `sel<N>`; `path<N>`, `ep<N>` of the timing table) or
    /// instance names.
    #[arg(long = "target", required = true, action = clap::ArgAction::Append, value_name = "HANDLE")]
    pub targets: Vec<String>,
    /// Area ratio K: the strictly larger (K >= 1) or smaller (K < 1) variant of the same
    /// function whose area is closest to K times the current area; none: the cell stays.
    #[arg(long = "drive-multiplier", default_value = "2.0", value_parser = positive, value_name = "K")]
    pub drive_multiplier: f64,
    /// Explicit library cell of the same function for every target instead.
    #[arg(long = "cell", value_name = "CELL")]
    pub cell: Option<String>,
    /// Idempotency key `[A-Za-z0-9_-]+`: a repeated request returns the revision it committed.
    #[arg(long = "request-id", value_name = "ID")]
    pub request_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct CompletionsArgs {
    /// Shell (default: taken from the environment).
    #[arg(value_enum)]
    pub shell: Option<clap_complete::Shell>,
}
