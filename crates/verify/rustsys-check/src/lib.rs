// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Independent synthesis checker (`docs/specs/check/synth.md`).

#![forbid(unsafe_code)]

mod aiger;
mod attempt;
mod blif;
pub mod circ;
mod corr;
mod discover;
mod gateref;
pub mod lfunc;
pub mod liberty;
pub mod netlist;
pub mod prove;
mod reset;
pub mod rtl;
pub mod sat;
pub mod seqsim;
pub mod sources;
pub mod sweep;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use rusteda_core::job::{Cancelled, Ctx, Diag, Event};
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::liberty::Library;
use rusteda_formats::verilog::{Diagnostic, PpOptions, SourceMap, read_sources, read_texts};
use serde::{Deserialize, Serialize};

use circ::Circuit;
use corr::Relation;
pub use sources::InitialValues;
use sources::Kind;

/// Source files or in-memory texts.
#[derive(Debug, Clone)]
pub enum Input {
    Files(Vec<PathBuf>),
    Texts(Vec<(String, String)>),
}

/// Bounded, combinational AIGER proof inputs for an external CEC executable.
/// Export is not a proof and always leaves the native check undecided.
#[derive(Debug, Clone)]
pub struct ObligationExport {
    pub directory: PathBuf,
    pub max_and_nodes: usize,
    pub max_outputs: usize,
}

impl ObligationExport {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            max_and_nodes: 64_000_000,
            max_outputs: 8_000_000,
        }
    }
}

/// Checker options (CHECK-REQ-0001).
#[derive(Debug, Clone)]
pub struct CheckOptions {
    pub rtl: Input,
    pub top: Option<String>,
    pub defines: Vec<(String, String)>,
    pub include_dirs: Vec<PathBuf>,
    pub params: Vec<(String, String)>,
    pub no_predefined_macros: bool,
    /// Simultaneous initial pulse on scalar inputs; later inputs are unrestricted.
    pub after_reset: Vec<(String, bool)>,
    /// Explicit permission to select otherwise unspecified unreset flop initial states.
    pub uninitialized_dont_care: bool,
    /// Complete-case assumption, as given to synthesis (CHECK-REQ-0012).
    pub assume_full_case: bool,
    /// Parallel-case assumption, as given to synthesis (CHECK-REQ-0013).
    pub assume_parallel_case: bool,
    pub netlist: Input,
    pub seed: u64,
    /// Random simulation rounds of 64 patterns (CHECK-REQ-0043).
    pub sim_rounds: usize,
    /// SAT conflict budget per proof; a proof that reaches it is undecided (CHECK-REQ-0002).
    pub budget: u64,
    /// SAT conflict budget of each proof that the RTL model needs; one that reaches it makes the
    /// check undecided (CHECK-REQ-0002).
    pub model_budget: u64,
    /// SAT conflict budget per candidate equivalence during sweeping (CHECK-REQ-0046).
    pub lemma_budget: u64,
    /// Random simulation rounds of 64 patterns that form the sweeping classes.
    pub sweep_rounds: usize,
    /// Maximum optional candidate equivalences attempted before structural-only sweeping.
    /// Zero disables candidate SAT calls; final proof obligations are never skipped.
    pub sweep_max_candidates: usize,
    /// Progress and phase times on standard error.
    pub verbose: bool,
    /// Clock cycles of sequential simulation; 0 disables it (CHECK-REQ-0045).
    pub cycles: usize,
    /// Netlist ports that carry power or ground (CHECK-REQ-0022).
    pub supply_ports: Vec<String>,
    /// What `liberty::unread` finds in the text of each library: an instance of such a cell is
    /// rejected (CHECK-REQ-0023). The library model does not keep these constructs, so a caller
    /// that reads the libraries itself gives them here; `rustsys check synth` does.
    pub unread_liberty: Vec<liberty::Unread>,
    /// `rtl` is a gate-level reference netlist of `gate_libs` cells (CHECK-REQ-0103).
    pub gate_netlist: bool,
    /// Libraries of a gate-level reference and of BLIF `.gate` and `.mlatch` cells
    /// (CHECK-REQ-0103, CHECK-REQ-0104).
    pub gate_libs: Vec<Library>,
    /// What `liberty::unread` finds in the texts of `gate_libs` (CHECK-REQ-0023).
    pub unread_gate_liberty: Vec<liberty::Unread>,
    /// What declared initial values of BLIF and AIGER sources become (CHECK-REQ-0105).
    pub initial_values: InitialValues,
    /// Optional AIGER export of every checked induction obligation. No external
    /// executable is launched by this crate and no export is counted as a pass.
    pub obligation_export: Option<ObligationExport>,
    /// Optional extra internal observations for external proof composition.
    /// Available only with obligation export; no observation is assumed true.
    pub observation_probes: Vec<rtl::ObservationProbe>,
}

impl Default for CheckOptions {
    fn default() -> CheckOptions {
        CheckOptions {
            rtl: Input::Texts(Vec::new()),
            top: None,
            defines: Vec::new(),
            include_dirs: Vec::new(),
            params: Vec::new(),
            no_predefined_macros: false,
            after_reset: Vec::new(),
            uninitialized_dont_care: false,
            assume_full_case: false,
            assume_parallel_case: false,
            netlist: Input::Texts(Vec::new()),
            seed: 1,
            sim_rounds: 16,
            budget: DEFAULT_BUDGET,
            model_budget: DEFAULT_MODEL_BUDGET,
            lemma_budget: 2_000,
            sweep_rounds: 256,
            sweep_max_candidates: DEFAULT_SWEEP_MAX_CANDIDATES,
            verbose: false,
            cycles: 0,
            supply_ports: Vec::new(),
            unread_liberty: Vec::new(),
            gate_netlist: false,
            gate_libs: Vec::new(),
            unread_gate_liberty: Vec::new(),
            initial_values: InitialValues::Reset,
            obligation_export: None,
            observation_probes: Vec::new(),
        }
    }
}

/// Verdict (CHECK-REQ-0001, CHECK-REQ-0002). JSON and text spell it as `as_str` does
/// (CHECK-REQ-0081).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Equivalent,
    /// Equivalent only for the explicitly reported choices of unspecified initial state. Reports
    /// of format version 1 spell it `refinement`.
    #[serde(alias = "refinement")]
    InitialStateRefinement,
    NotEquivalent,
    /// No difference was found, but a proof or search stopped at its budget before deciding; the
    /// report's `undecided` names each (CHECK-REQ-0002).
    Undecided,
    Error,
}

impl Verdict {
    /// Every verdict.
    pub const ALL: [Verdict; 5] = [
        Verdict::Equivalent,
        Verdict::InitialStateRefinement,
        Verdict::NotEquivalent,
        Verdict::Undecided,
        Verdict::Error,
    ];

    /// The spelling in the JSON report and on the verdict line (CHECK-REQ-0081).
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Equivalent => "equivalent",
            Verdict::InitialStateRefinement => "initial-state-refinement",
            Verdict::NotEquivalent => "not-equivalent",
            Verdict::Undecided => "undecided",
            Verdict::Error => "error",
        }
    }
}

/// A difference with its counterexample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mismatch {
    pub kind: String,
    pub name: String,
    pub counterexample: Vec<(String, bool)>,
}

/// Report (CHECK-REQ-0060, CHECK-REQ-0085).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckReport {
    /// `rustsys-check-synth-report`; empty in reports written before the field existed.
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub format_version: u32,
    pub verdict: Verdict,
    pub outputs_checked: usize,
    pub state_pairs_checked: usize,
    #[serde(default)]
    pub after_reset: Vec<(String, bool)>,
    /// Explicit permission to select otherwise unspecified unreset flop initial states.
    #[serde(default)]
    pub uninitialized_dont_care: bool,
    /// The complete-case assumption was enabled (CHECK-REQ-0012).
    #[serde(default)]
    pub assume_full_case: bool,
    /// The parallel-case assumption was enabled (CHECK-REQ-0013).
    #[serde(default)]
    pub assume_parallel_case: bool,
    /// Each entry has an independently checked base case and next-state obligation.
    #[serde(default)]
    pub constant_states_checked: Vec<(String, bool)>,
    /// Selected initial values, independently inferred from the emitted netlist.
    #[serde(default)]
    pub uninitialized_choices: Vec<(String, bool)>,
    /// Selected initial equality or inversion of two otherwise unspecified unreset states.
    #[serde(default)]
    pub uninitialized_state_relations: Vec<(String, String, bool)>,
    #[serde(default)]
    pub equivalent_states_checked: Vec<(String, String, bool)>,
    pub rtl_state_bits: usize,
    pub netlist_state_elements: usize,
    pub unmatched_rtl_state_bits: Vec<String>,
    /// Supply ports of the netlist (CHECK-REQ-0022).
    #[serde(default)]
    pub supply_ports: Vec<String>,
    /// Discovered correspondences: RTL state name and relation (CHECK-REQ-0032).
    #[serde(default)]
    pub discovered_states: Vec<(String, String)>,
    pub mismatches: Vec<Mismatch>,
    pub errors: Vec<String>,
    /// The `errors` are an internal failure of the checker, not an input error (CHECK-REQ-0001,
    /// CHECK-REQ-0044).
    #[serde(default)]
    pub internal_error: bool,
    /// What stopped before deciding, with the budget that ran out (CHECK-REQ-0002).
    #[serde(default)]
    pub undecided: Vec<String>,
    pub sat_calls: usize,
    /// Equivalence sweeping statistics (CHECK-REQ-0046).
    #[serde(default)]
    pub sweep: sweep::SweepStats,
    /// Sequential simulation (CHECK-REQ-0045).
    #[serde(default)]
    pub seq_sim: seqsim::SeqSimReport,
    pub seed: u64,
    pub runtime_s: f64,
    /// The format of a source other than SystemVerilog: `blif`, `aiger` or `gate-netlist`
    /// (CHECK-REQ-0105).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    /// `reset` or `arbitrary` for a BLIF or AIGER source (CHECK-REQ-0105).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_values_mode: Option<String>,
    /// The initial values that a BLIF or AIGER source declares, by state (CHECK-REQ-0105).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initial_values: Vec<(String, bool)>,
}

/// The report fields of a source other than SystemVerilog (CHECK-REQ-0105).
fn source_fields(kind: Option<Kind>, opts: &CheckOptions) -> (Option<String>, Option<String>) {
    let kind = kind.filter(|k| *k != Kind::Rtl);
    (
        kind.map(|k| k.name().to_string()),
        kind.filter(|k| *k != Kind::Gates)
            .map(|_| opts.initial_values.name().to_string()),
    )
}

impl CheckReport {
    /// The report of a check that stopped early (CHECK-REQ-0002): `error` with the reason, or the
    /// budget that ran out with the differences found before, `not-equivalent` when there are
    /// any and `undecided` otherwise.
    fn stopped(
        stop: Stop,
        mismatches: Vec<Mismatch>,
        opts: &CheckOptions,
        t0: Instant,
    ) -> CheckReport {
        let (verdict, errors, undecided, internal_error) = match stop {
            Stop::Error(m) => (Verdict::Error, vec![m], vec![], false),
            Stop::Internal(m) => (Verdict::Error, vec![m], vec![], true),
            Stop::Undecided(m) if mismatches.is_empty() => {
                (Verdict::Undecided, vec![], vec![m], false)
            }
            Stop::Undecided(m) => (Verdict::NotEquivalent, vec![], vec![m], false),
            Stop::Cancelled => unreachable!("a cancelled check has no report"),
        };
        let mismatches = if verdict == Verdict::Error {
            Vec::new()
        } else {
            mismatches
        };
        let (source_format, initial_values_mode) =
            source_fields(sources::kind(&opts.rtl, opts.gate_netlist).ok(), opts);
        CheckReport {
            source_format,
            initial_values_mode,
            initial_values: Vec::new(),
            format: REPORT_FORMAT.into(),
            format_version: REPORT_FORMAT_VERSION,
            verdict,
            outputs_checked: 0,
            state_pairs_checked: 0,
            after_reset: opts.after_reset.clone(),
            uninitialized_dont_care: opts.uninitialized_dont_care,
            assume_full_case: opts.assume_full_case,
            assume_parallel_case: opts.assume_parallel_case,
            constant_states_checked: Vec::new(),
            uninitialized_choices: Vec::new(),
            uninitialized_state_relations: Vec::new(),
            equivalent_states_checked: Vec::new(),
            rtl_state_bits: 0,
            netlist_state_elements: 0,
            unmatched_rtl_state_bits: vec![],
            supply_ports: vec![],
            discovered_states: vec![],
            mismatches,
            errors,
            internal_error,
            undecided,
            sat_calls: 0,
            sweep: sweep::SweepStats::default(),
            seq_sim: seqsim::SeqSimReport::default(),
            seed: opts.seed,
            runtime_s: t0.elapsed().as_secs_f64(),
        }
    }
}

fn read(
    sm: &mut SourceMap,
    input: &Input,
    pp: PpOptions,
) -> Result<Vec<rusteda_formats::verilog::ast::SourceUnit>, String> {
    let (units, diags) = match input {
        Input::Files(f) => read_sources(sm, f, pp),
        Input::Texts(t) => {
            let refs: Vec<(&str, &str)> = t.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            read_texts(sm, &refs, pp)
        }
    };
    if diags.iter().any(Diagnostic::is_error) {
        return Err(diags
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.render(sm))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    Ok(units)
}

/// Default of `CheckOptions::budget` (CHECK-REQ-0002).
pub const DEFAULT_BUDGET: u64 = 2_000_000;

/// Default work bound of optional equivalence sweeping (CHECK-REQ-0046).
pub const DEFAULT_SWEEP_MAX_CANDIDATES: usize = 32_768;

/// Default of `CheckOptions::model_budget` (CHECK-REQ-0002).
pub const DEFAULT_MODEL_BUDGET: u64 = rtl::PROOF_BUDGET;

/// Value of `CheckReport::format`.
pub const REPORT_FORMAT: &str = "rustsys-check-synth-report";

/// Value of `CheckReport::format_version` (CHECK-REQ-0085): 2 since the verdicts
/// `initial-state-refinement` and `undecided` and the field `undecided`.
pub const REPORT_FORMAT_VERSION: u32 = 2;

/// Proofs repeated after rejecting discovered relations (CHECK-REQ-0032).
const MAX_RETRIES: usize = 4;

/// Runs the checker. `libs` are the library models; the constructs of their texts that the
/// models do not keep come in `CheckOptions::unread_liberty` (CHECK-REQ-0023).
pub fn check(opts: &CheckOptions, libs: &[Library]) -> CheckReport {
    check_with(opts, libs, Ctx::NONE).unwrap_or_else(|_| unreachable!("no cancel token"))
}

/// `check` with a run context (CHECK-REQ-0080): phases `rtl`, `netlist`, `simulate`, `sweep` and
/// `prove`, sweep progress, verbose notes as `note[C0002]` messages, and checkpoints.
pub fn check_with(
    opts: &CheckOptions,
    libs: &[Library],
    ctx: Ctx<'_>,
) -> Result<CheckReport, Cancelled> {
    let t0 = Instant::now();
    ctx.emit(|| Event::PhaseStarted { phase: "rtl" });
    match run(opts, libs, t0, ctx, None) {
        Ok(r) => Ok(r),
        Err(Stop::Cancelled) => Err(Cancelled),
        Err(stop) => Ok(CheckReport::stopped(stop, Vec::new(), opts, t0)),
    }
}

/// Checks validated disk-table gate semantics without materializing a Verilog
/// string or mutable gate graph. `opts.netlist` is unused; all RTL, state,
/// initial-value and proof options retain exactly the semantics of `check`.
pub fn check_frozen(
    opts: &CheckOptions,
    libs: &[Library],
    netlist: &FrozenNetlist<'_>,
    limits: netlist::frozen::Limits,
) -> CheckReport {
    check_frozen_with(opts, libs, netlist, limits, Ctx::NONE)
        .unwrap_or_else(|_| unreachable!("no cancel token"))
}

/// `check_frozen` with progress/cancellation. Budget exhaustion is undecided.
pub fn check_frozen_with(
    opts: &CheckOptions,
    libs: &[Library],
    netlist: &FrozenNetlist<'_>,
    limits: netlist::frozen::Limits,
    ctx: Ctx<'_>,
) -> Result<CheckReport, Cancelled> {
    let t0 = Instant::now();
    ctx.emit(|| Event::PhaseStarted { phase: "rtl" });
    match run(opts, libs, t0, ctx, Some((netlist, limits))) {
        Ok(r) => Ok(r),
        Err(Stop::Cancelled) => Err(Cancelled),
        Err(stop) => Ok(CheckReport::stopped(stop, Vec::new(), opts, t0)),
    }
}

type FrozenInput<'a, 'data> = Option<(&'a FrozenNetlist<'data>, netlist::frozen::Limits)>;

#[allow(clippy::too_many_arguments)]
fn gate_model(
    c: &mut Circuit,
    opts: &CheckOptions,
    libs: &[Library],
    top: &str,
    inputs: Option<&HashMap<String, Vec<u32>>>,
    states: &[rtl::RtlState],
    frozen: FrozenInput<'_, '_>,
) -> Result<netlist::NetModel, Stop> {
    if let Some((tables, limits)) = frozen {
        let mut wanted = HashSet::new();
        wanted.extend(opts.observation_probes.iter().map(|p| p.name.as_str()));
        for state in states {
            wanted.insert(state.name.as_str());
            if let Some((base, index)) = state
                .name
                .strip_suffix(']')
                .and_then(|s| s.rsplit_once('['))
            {
                if index.parse::<usize>().is_ok() {
                    wanted.insert(base);
                }
            }
        }
        return netlist::frozen::build(
            c,
            tables,
            libs,
            top,
            inputs,
            &opts.supply_ports,
            &opts.unread_liberty,
            &wanted,
            limits,
        )
        .map_err(|message| {
            if message.starts_with("warning[C0005]") {
                Stop::Undecided(message)
            } else {
                Stop::Error(message)
            }
        });
    }
    let mut sm = SourceMap::new();
    let units = netlist::read(&mut sm, &opts.netlist)?;
    netlist::build(
        c,
        &sm,
        &units,
        libs,
        top,
        inputs,
        &opts.supply_ports,
        &opts.unread_liberty,
    )
    .map_err(|message| {
        if message.starts_with("warning[C0005]") {
            Stop::Undecided(message)
        } else {
            Stop::Error(message)
        }
    })
}

/// Why a check stopped early.
pub(crate) enum Stop {
    /// Erroneous input or a construct outside the model.
    Error(String),
    /// A failure of the checker itself, such as a counterexample that does not replay
    /// (CHECK-REQ-0044); exit status 2, not 1.
    Internal(String),
    /// A budget ran out before the check could decide (CHECK-REQ-0002).
    Undecided(String),
    Cancelled,
}

impl From<rtl::BuildError> for Stop {
    fn from(e: rtl::BuildError) -> Stop {
        match e {
            rtl::BuildError::Input(m) => Stop::Error(m),
            rtl::BuildError::Undecided(m) => Stop::Undecided(m),
        }
    }
}

impl From<String> for Stop {
    fn from(m: String) -> Stop {
        Stop::Error(m)
    }
}

impl From<&str> for Stop {
    fn from(m: &str) -> Stop {
        Stop::Error(m.to_string())
    }
}

impl From<Cancelled> for Stop {
    fn from(_: Cancelled) -> Stop {
        Stop::Cancelled
    }
}

#[allow(clippy::too_many_lines)]
fn run(
    opts: &CheckOptions,
    libs: &[Library],
    t0: Instant,
    ctx: Ctx<'_>,
    frozen: FrozenInput<'_, '_>,
) -> Result<CheckReport, Stop> {
    let mut t_phase = Instant::now();
    // Ends phase `done`, starts `next` and checks for cancellation (CHECK-REQ-0080).
    let mut mark = |done: &'static str, next: Option<&'static str>| -> Result<(), Cancelled> {
        let seconds = t_phase.elapsed().as_secs_f64();
        ctx.emit(|| Event::PhaseFinished {
            phase: done,
            seconds,
        });
        if let Some(phase) = next {
            ctx.emit(|| Event::PhaseStarted { phase });
        }
        t_phase = Instant::now();
        ctx.checkpoint()
    };
    // Sources other than RTL take the checker's own readers (CHECK-REQ-0100).
    let kind = sources::kind(&opts.rtl, opts.gate_netlist)?;
    if !opts.observation_probes.is_empty() && opts.obligation_export.is_none() {
        return Err(Stop::Error(
            "internal observations require external obligation export".into(),
        ));
    }
    if opts.obligation_export.is_some() && kind != Kind::Rtl {
        return Err(Stop::Error(
            "event obligation export currently accepts RTL references only".into(),
        ));
    }
    let mut c = Circuit::new();
    let mut declared = Vec::new();
    let model = if kind != Kind::Rtl {
        if !opts.defines.is_empty()
            || !opts.params.is_empty()
            || opts.assume_full_case
            || opts.assume_parallel_case
        {
            return Err(Stop::Error(format!(
                "error[{}]: -D, -P and the case assumptions apply to SystemVerilog only",
                sources::SOURCE
            )));
        }
        let top = opts.top.as_deref();
        let built = match kind {
            Kind::Blif => blif::build(
                &mut c,
                &opts.rtl,
                &opts.gate_libs,
                &opts.unread_gate_liberty,
                top,
                opts.initial_values,
            )?,
            Kind::Aiger => aiger::build(&mut c, &opts.rtl, top, opts.initial_values)?,
            _ => gateref::build(
                &mut c,
                &opts.rtl,
                &opts.gate_libs,
                &opts.unread_gate_liberty,
                top,
                &opts.supply_ports,
            )?,
        };
        for w in &built.warnings {
            if let Some(d) = Diag::parse(w) {
                ctx.diag(|| d);
            }
        }
        declared = built.initial_values;
        built.model
    } else {
        let mut sm = SourceMap::new();
        let mut defines = Vec::new();
        if !opts.no_predefined_macros {
            defines.push(("SYNTHESIS".to_string(), String::new()));
        }
        defines.extend(opts.defines.iter().cloned());
        let units = read(
            &mut sm,
            &opts.rtl,
            PpOptions {
                include_dirs: opts.include_dirs.clone(),
                defines,
            },
        )?;
        let mut params = Vec::new();
        for (n, t) in &opts.params {
            let e = rusteda_formats::verilog::parse_expression(&mut sm, n, t)
                .map_err(|d| d.render(&sm))?;
            params.push((n.clone(), e));
        }
        let built = rtl::build_with(
            &sm,
            &units,
            &mut c,
            &rtl::RtlOptions {
                top: opts.top.clone(),
                params,
                assume_full_case: opts.assume_full_case,
                assume_parallel_case: opts.assume_parallel_case,
                observations: opts.observation_probes.clone(),
            },
            opts.model_budget,
        );
        match built {
            Ok(m) => m,
            // An erroneous netlist is rejected before the check is called undecided: more budget
            // would not change that verdict (CHECK-REQ-0002).
            Err(rtl::BuildError::Undecided(m)) => {
                let top = rtl::top_name(&units, opts.top.as_ref())?;
                gate_model(&mut Circuit::new(), opts, libs, &top, None, &[], frozen)?;
                return Err(Stop::Undecided(m));
            }
            Err(e) => return Err(e.into()),
        }
    };
    let phase = |what: &str| {
        if opts.verbose {
            ctx.diag(|| {
                Diag::note(
                    "C0002",
                    format!("{:>8.2} s  {what}", t0.elapsed().as_secs_f64()),
                )
            });
        }
    };
    phase("RTL model built");
    mark("rtl", Some("netlist"))?;
    let input_vars: HashMap<String, Vec<u32>> = model
        .inputs
        .iter()
        .map(|(n, v, _)| (n.clone(), v.clone()))
        .collect();
    let nm = gate_model(
        &mut c,
        opts,
        libs,
        &model.top,
        Some(&input_vars),
        &model.states,
        frozen,
    )?;
    phase("netlist model built");
    mark("netlist", Some("simulate"))?;
    let mut mismatches = Vec::new();
    // Ports (CHECK-REQ-0030): name, direction and width; the order is not compared.
    if let Some(m) = port_difference(&model, &nm) {
        mismatches.push(m);
    }
    if opts.obligation_export.is_some() && !mismatches.is_empty() {
        return Err(Stop::Error(
            "cannot export CEC obligations for mismatched RTL/netlist interfaces".into(),
        ));
    }
    let pulse = reset::pulse(&model, &opts.after_reset)?;
    let control_roots: Vec<_> = model
        .states
        .iter()
        .flat_map(|s| std::iter::once(s.clk).chain(s.arst.map(|(r, _)| r)))
        .collect();
    let control_vars: HashSet<_> = c
        .order(&control_roots)
        .map_err(|_| "cycle in RTL sequential controls")?
        .into_iter()
        .collect();
    let setting = corr::Setting {
        c: &c,
        model: &model,
        nm: &nm,
        pulse: &pulse,
        control_vars: &control_vars,
        uninitialized_dont_care: opts.uninitialized_dont_care,
    };
    // State correspondence: names first (CHECK-REQ-0031), then discovery (CHECK-REQ-0032) when
    // an element has no named counterpart or a cut point refutes an obligation.
    let named = if opts.obligation_export.is_some() {
        corr::by_name_for_export(&setting)?
    } else {
        corr::by_name(&setting)?
    };
    if opts.obligation_export.is_some() && !named.unused.is_empty() {
        let first = &nm.seq[named.unused[0]];
        return Err(Stop::Undecided(format!(
            "obligation export: {} implementation states lack a unique named RTL counterpart; first {} ({})",
            named.unused.len(),
            first.inst,
            first.cell
        )));
    }
    let mut discovered: Vec<(usize, Relation)> = Vec::new();
    let mut searched = false;
    let mut excluded: HashSet<(usize, Relation)> = HashSet::new();
    let search = |excluded: &HashSet<(usize, Relation)>| -> Result<Vec<(usize, Relation)>, Stop> {
        let rels = discover::discover(&setting, &named, opts.seed, excluded, ctx)?;
        phase(&format!("discovered {} state relations", rels.len()));
        Ok(rels)
    };
    if !named.unused.is_empty() {
        discovered = search(&excluded)?;
        searched = true;
    }
    let models = attempt::Models {
        model: &model,
        nm: &nm,
        pulse: &pulse,
    };
    let mut retries = 0;
    // The repetition limit of discovery ends the check undecided, unless a port difference
    // already decides it (CHECK-REQ-0002, CHECK-REQ-0032).
    let at_limit = |msg: String, mismatches: Vec<Mismatch>| {
        let mut r = CheckReport::stopped(Stop::Undecided(msg), mismatches, opts, t0);
        r.rtl_state_bits = model.states.len();
        r.netlist_state_elements = nm.seq.len();
        r.supply_ports.clone_from(&nm.supply_ports);
        r
    };
    let no_counterpart = |ci: usize| {
        Stop::from(format!(
            "netlist state element {} ({}) has no RTL counterpart (CHECK-REQ-0031)",
            nm.seq[ci].inst, nm.seq[ci].cell
        ))
    };
    let (corr, proved) = loop {
        let mut corr = named.clone();
        corr::apply(&setting, &mut corr, &discovered);
        // An element still without a counterpart is an error, unless the proof may first reject
        // a discovered relation that simulation could not refute; the element is then cut free.
        let unused = corr.unused.first().copied();
        if let Some(ci) = unused {
            if corr.discovered.is_empty() {
                return Err(no_counterpart(ci));
            }
            // The previous proof still rejected relations: the repetition limit, not the
            // netlist, ends the search (CHECK-REQ-0002).
            if retries == MAX_RETRIES {
                let msg = format!(
                    "state correspondence discovery reached its limit of {MAX_RETRIES} \
                     repetitions with netlist state element {} ({}) still without an RTL \
                     counterpart (CHECK-REQ-0032)",
                    nm.seq[ci].inst, nm.seq[ci].cell
                );
                return Ok(at_limit(msg, mismatches));
            }
        }
        let mut work = c.clone();
        let stop = !searched && !corr.unmatched.is_empty();
        match attempt::prove(
            &mut work, &models, &corr, opts, stop, &mut mark, &phase, ctx,
        )? {
            attempt::Outcome::CutPoints => {
                discovered = search(&excluded)?;
                searched = true;
            }
            attempt::Outcome::Proved(p) => {
                // A discovered relation that the proof rejects is excluded, and discovery and
                // proof run again (CHECK-REQ-0032).
                let failed: HashSet<&str> = p.mismatches.iter().map(|m| m.name.as_str()).collect();
                let mut rejected: Vec<(usize, Relation)> = corr
                    .discovered
                    .iter()
                    .copied()
                    .filter(|(si, _)| failed.contains(model.states[*si].name.as_str()))
                    .collect();
                // Only the first kind present in the order constant, shared, cell goes: a wrong
                // relation that simulation could not refute takes the correct ones that read it
                // along in the proof.
                let kind = |r: &Relation| match r {
                    Relation::Constant(_) => 0,
                    Relation::Shares(..) => 1,
                    Relation::Cell(..) => 2,
                };
                if let Some(first) = rejected.iter().map(|(_, r)| kind(r)).min() {
                    rejected.retain(|(_, r)| kind(r) == first);
                }
                if rejected.is_empty() {
                    if let Some(ci) = unused {
                        return Err(no_counterpart(ci));
                    }
                    break (corr, p);
                }
                // Discovered relations still fail at the repetition limit; their mismatches are
                // no evidence of a difference (CHECK-REQ-0002).
                if retries == MAX_RETRIES {
                    let mut names: Vec<&str> = rejected
                        .iter()
                        .map(|(si, _)| model.states[*si].name.as_str())
                        .collect();
                    let more = names.len().saturating_sub(8);
                    names.truncate(8);
                    let msg = format!(
                        "state correspondence discovery reached its limit of {MAX_RETRIES} \
                         repetitions while the proof still rejects the discovered relations of \
                         {}{} (CHECK-REQ-0032)",
                        names.join(", "),
                        if more > 0 {
                            format!(" and {more} more")
                        } else {
                            String::new()
                        }
                    );
                    return Ok(at_limit(msg, mismatches));
                }
                excluded.extend(rejected);
                discovered = search(&excluded)?;
                retries += 1;
                mark("prove", Some("simulate"))?;
            }
        }
    };
    mismatches.extend(proved.mismatches);
    // A replayed counterexample decides even when other proofs ran out of budget
    // (CHECK-REQ-0002).
    let verdict = if !mismatches.is_empty() {
        Verdict::NotEquivalent
    } else if !proved.undecided.is_empty() {
        Verdict::Undecided
    } else if !corr.initial_choices.is_empty()
        || corr
            .equivalents
            .iter()
            .any(|&(i, j, _)| model.states[i].arst.is_none() && model.states[j].arst.is_none())
    {
        Verdict::InitialStateRefinement
    } else {
        Verdict::Equivalent
    };
    mark("prove", None)?;
    let (source_format, initial_values_mode) = source_fields(Some(kind), opts);
    Ok(CheckReport {
        source_format,
        initial_values_mode,
        initial_values: declared,
        format: REPORT_FORMAT.into(),
        format_version: REPORT_FORMAT_VERSION,
        verdict,
        outputs_checked: proved.outputs_checked,
        state_pairs_checked: proved.pairs,
        equivalent_states_checked: corr
            .equivalents
            .iter()
            .map(|&(i, j, inv)| {
                (
                    model.states[i].name.clone(),
                    model.states[j].name.clone(),
                    inv,
                )
            })
            .collect(),
        after_reset: opts.after_reset.clone(),
        uninitialized_dont_care: opts.uninitialized_dont_care,
        assume_full_case: opts.assume_full_case,
        assume_parallel_case: opts.assume_parallel_case,
        uninitialized_choices: model
            .states
            .iter()
            .filter_map(|s| corr.initial_choices.get(&s.q).map(|v| (s.name.clone(), *v)))
            .collect(),
        uninitialized_state_relations: corr
            .equivalents
            .iter()
            .filter(|&&(i, j, _)| model.states[i].arst.is_none() && model.states[j].arst.is_none())
            .map(|&(i, j, inv)| {
                (
                    model.states[i].name.clone(),
                    model.states[j].name.clone(),
                    inv,
                )
            })
            .collect(),
        constant_states_checked: model
            .states
            .iter()
            .zip(&corr.constants)
            .filter_map(|(s, v)| v.map(|v| (s.name.clone(), v)))
            .collect(),
        rtl_state_bits: model.states.len(),
        netlist_state_elements: nm.seq.len(),
        unmatched_rtl_state_bits: corr
            .unmatched
            .iter()
            .map(|si| model.states[*si].name.clone())
            .collect(),
        supply_ports: nm.supply_ports.clone(),
        discovered_states: {
            let mut d = corr.discovered.clone();
            d.sort_by_key(|x| x.0);
            d
        }
        .iter()
        .map(|&(si, rel)| {
            (
                model.states[si].name.clone(),
                corr::describe(&model, &nm, rel),
            )
        })
        .collect(),
        mismatches,
        errors: Vec::new(),
        internal_error: false,
        undecided: proved.undecided,
        sat_calls: proved.sweep.solver_calls,
        sweep: proved.sweep,
        seq_sim: proved.seq_sim,
        seed: opts.seed,
        runtime_s: t0.elapsed().as_secs_f64(),
    })
}

/// Ports that differ in presence, direction or width (CHECK-REQ-0030).
fn port_difference(model: &rtl::RtlModel, nm: &netlist::NetModel) -> Option<Mismatch> {
    let width = |n: usize, scalar: bool| if scalar { None } else { Some(n as u32) };
    let rtl: Vec<(String, bool, Option<u32>)> = model
        .port_order
        .iter()
        .map(|&(input, i)| {
            if input {
                let (n, v, s) = &model.inputs[i];
                (n.clone(), true, width(v.len(), *s))
            } else {
                let (n, v, s) = &model.outputs[i];
                (n.clone(), false, width(v.len(), *s))
            }
        })
        .collect();
    let only_rtl: Vec<_> = rtl.iter().filter(|p| !nm.ports.contains(p)).collect();
    let only_net: Vec<_> = nm.ports.iter().filter(|p| !rtl.contains(p)).collect();
    if only_rtl.is_empty() && only_net.is_empty() {
        return None;
    }
    Some(Mismatch {
        kind: "ports".into(),
        name: format!("RTL {only_rtl:?} netlist {only_net:?}"),
        counterexample: vec![],
    })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::{CheckOptions, CheckReport, Mismatch, Stop, Verdict};

    // CHECK-REQ-0001, CHECK-REQ-0002: a check that stops early reports an input error, an
    // internal failure (exit status 2, not 1) or the budget that ran out; a difference found
    // before the budget ran out still decides.
    #[test]
    fn stopped_reports() {
        let opts = CheckOptions {
            after_reset: vec![("rn".into(), false)],
            ..CheckOptions::default()
        };
        let t0 = Instant::now();
        let port = Mismatch {
            kind: "ports".into(),
            name: "z".into(),
            counterexample: vec![],
        };
        let r = CheckReport::stopped(Stop::Error("bad".into()), vec![], &opts, t0);
        assert_eq!((r.verdict, r.internal_error), (Verdict::Error, false));
        assert_eq!(r.errors, ["bad"]);
        let r = CheckReport::stopped(Stop::Internal("internal: x".into()), vec![], &opts, t0);
        assert_eq!((r.verdict, r.internal_error), (Verdict::Error, true));
        let r = CheckReport::stopped(Stop::Undecided("limit".into()), vec![], &opts, t0);
        assert_eq!(r.verdict, Verdict::Undecided);
        assert!(r.errors.is_empty() && r.undecided == ["limit"]);
        let r = CheckReport::stopped(Stop::Undecided("limit".into()), vec![port], &opts, t0);
        assert_eq!(r.verdict, Verdict::NotEquivalent);
        assert!(r.mismatches.len() == 1 && r.undecided == ["limit"]);
        assert_eq!(r.after_reset, opts.after_reset);
        let json = serde_json::to_value(&r).expect("json");
        assert_eq!(json["internal_error"], false);
    }

    // CHECK-REQ-0002, CHECK-REQ-0081: the JSON spelling of every verdict is `as_str`, and the
    // spelling `refinement` of format version 1 still reads.
    #[test]
    fn verdict_spelling() {
        for v in Verdict::ALL {
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str());
            assert_eq!(
                serde_json::from_value::<Verdict>(v.as_str().into()).unwrap(),
                v
            );
        }
        assert_eq!(
            serde_json::from_str::<Verdict>("\"refinement\"").unwrap(),
            Verdict::InitialStateRefinement
        );
    }
}
