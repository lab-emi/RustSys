// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! RustSys: logic synthesis and standard-cell mapping (`docs/specs/synth/`).

#![forbid(unsafe_code)]

pub mod aig;
pub mod bits;
mod covers;
mod diagnostics;
pub mod elab;
pub mod electrical;
pub mod emit;
pub mod estimate;
pub mod ingest;
pub mod map;
pub mod opt;
pub mod sat;
pub mod view;

use covers::{Carry, map_options};
use diagnostics::{fail, render, stream};
use std::path::PathBuf;
use std::time::Instant;

use rusteda_core::job::{Ctx, Event, JobError};
use rusteda_formats::liberty::Library;
use rusteda_formats::verilog::Diagnostic;
use rusteda_formats::verilog::{PpOptions, SourceMap, read_sources, read_texts, write};
use serde::{Deserialize, Serialize};

pub use ingest::{InitialValues, SourceFormat};

/// Optimization effort (SYNTH-REQ-0412).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effort {
    Low,
    #[default]
    Medium,
    High,
}

/// Mapping goal (SYNTH-REQ-0512).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Goal {
    #[default]
    Delay,
    Area,
}

/// RTL input: files on disk or in-memory texts (SYNTH-REQ-0101).
#[derive(Debug, Clone)]
pub enum Sources {
    Files(Vec<PathBuf>),
    Texts(Vec<(String, String)>),
}

/// Synthesis options (SYNTH-REQ-0701).
#[derive(Debug, Clone)]
pub struct SynthOptions {
    pub top: Option<String>,
    pub defines: Vec<(String, String)>,
    pub include_dirs: Vec<PathBuf>,
    /// Top-level parameter overrides as expression texts.
    pub params: Vec<(String, String)>,
    pub no_predefined_macros: bool,
    /// Scalar inputs asserted together before observation; empty preserves arbitrary init.
    pub after_reset: Vec<(String, bool)>,
    /// Allow a proved constant choice for otherwise unspecified unreset flop state.
    pub uninitialized_dont_care: bool,
    /// Case statements marked complete take their last item as the default (SYNTH-REQ-0224).
    pub assume_full_case: bool,
    /// Case statements marked parallel merge their items without priority (SYNTH-REQ-0225).
    pub assume_parallel_case: bool,
    pub dont_use: Vec<String>,
    pub effort: Effort,
    pub goal: Goal,
    pub delay_relax: f64,
    /// Where the mapper's cut passes run (SYNTH-REQ-0516).
    pub backend: map::MapBackend,
    /// Optional load-aware buffering and sizing after compact Boolean covering.
    pub electrical: Option<electrical::Options>,
    /// Optional area-capped cleanup when no timing constraints were supplied.
    pub default_quality: Option<electrical::default_quality::Limits>,
    pub electrical_corners: Vec<Library>,
    /// Libraries of the cells of a gate-level netlist source and of BLIF `.gate` and `.mlatch`
    /// instances (SYNTH-REQ-0909, SYNTH-REQ-0910).
    pub gate_libs: Vec<Library>,
    /// The one source is a gate-level netlist of `gate_libs` cells (SYNTH-REQ-0900).
    pub gate_netlist: bool,
    /// What declared initial values of BLIF and AIGER state become (SYNTH-REQ-0906).
    pub initial_values: InitialValues,
}

impl Default for SynthOptions {
    fn default() -> SynthOptions {
        SynthOptions {
            top: None,
            defines: Vec::new(),
            include_dirs: Vec::new(),
            params: Vec::new(),
            no_predefined_macros: false,
            after_reset: Vec::new(),
            uninitialized_dont_care: false,
            assume_full_case: false,
            assume_parallel_case: false,
            dont_use: Vec::new(),
            effort: Effort::Medium,
            goal: Goal::Delay,
            delay_relax: 0.1,
            backend: map::MapBackend::Cpu,
            electrical: None,
            default_quality: Some(Default::default()),
            electrical_corners: Vec::new(),
            gate_libs: Vec::new(),
            gate_netlist: false,
            initial_values: InitialValues::Reset,
        }
    }
}

/// JSON report (SYNTH-REQ-0703, SYNTH-REQ-0707).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Report {
    /// `rustsys-synth-report`; empty in reports written before the field existed.
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub format_version: u32,
    pub top: String,
    pub cells: std::collections::BTreeMap<String, usize>,
    pub area: f64,
    pub sequential_cells: usize,
    pub combinational_cells: usize,
    pub inverters: usize,
    pub logic_depth: u32,
    pub estimated_delay: f64,
    pub aig_nodes_before: usize,
    pub aig_nodes_after: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_requested_cuts: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_effective_cuts: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_cut_memory_budget_bytes: Option<u64>,
    pub state_bits_rtl: usize,
    pub state_bits_retained: usize,
    #[serde(default)]
    pub after_reset: Vec<(String, bool)>,
    /// Allow a proved constant choice for otherwise unspecified unreset flop state.
    #[serde(default)]
    pub uninitialized_dont_care: bool,
    /// The complete-case assumption was enabled (SYNTH-REQ-0224).
    #[serde(default)]
    pub assume_full_case: bool,
    /// The parallel-case assumption was enabled (SYNTH-REQ-0225).
    #[serde(default)]
    pub assume_parallel_case: bool,
    #[serde(default)]
    pub constant_states: Vec<opt::reset::ConstantState>,
    #[serde(default)]
    pub merged_states: Vec<opt::reset::EquivalentState>,
    pub latches: usize,
    pub warnings: usize,
    /// Library cells that synthesis cannot use, with the reason (SYNTH-REQ-0703).
    pub unused_cells: std::collections::BTreeMap<String, String>,
    pub runtime_s: f64,
    /// Seconds per phase, in execution order (SYNTH-REQ-0703).
    pub phase_s: Vec<(String, f64)>,
    /// Where the mapper's cut passes ran: `cpu` or `cuda`.
    pub map_backend: String,
    /// Seconds per step of the map phase, in execution order (SYNTH-REQ-0703).
    #[serde(default)]
    pub map_phase_s: Vec<(String, f64)>,
    #[serde(default)]
    pub opt_phase_s: Vec<(String, f64)>,
    /// Bounded optimizer work and ordered speculative replay counts.
    #[serde(default)]
    pub opt_work: Vec<OptimizationWork>,
    /// Area of each used cell as mapped (SYNTH-REQ-0707).
    #[serde(default)]
    pub cell_area: std::collections::BTreeMap<String, f64>,
    /// Time unit of the first library in ns.
    #[serde(default)]
    pub time_unit_ns: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electrical: Option<Box<electrical::Report>>,
    /// Bounded area-neutral cleanup of the unconstrained cover, with its own timing scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_quality: Option<Box<electrical::default_quality::QualityReport>>,
    /// The format of a source other than SystemVerilog: `blif`, `aiger` or `gate-netlist`
    /// (SYNTH-REQ-0913).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    /// `reset` or `arbitrary` for a source other than SystemVerilog (SYNTH-REQ-0906).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_values_mode: Option<String>,
    /// The declared initial values of the source's states, in state order (SYNTH-REQ-0906).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initial_values: Vec<(String, bool)>,
    /// The generated global-clock port (SYNTH-REQ-0904).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_port: Option<String>,
    /// The generated initial-value port (SYNTH-REQ-0906).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_port: Option<String>,
}

/// Actual bounded planning work. Retry counts can vary with worker count;
/// functional results and the emitted netlist remain deterministic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationWork {
    pub pass: String,
    pub windows: u64,
    pub bounded_windows: u64,
    pub replayed_windows: u64,
    pub pass_budget_reached: bool,
}

/// Value of `Report::format`.
pub const REPORT_FORMAT: &str = "rustsys-synth-report";

/// Result of a successful run.
#[derive(Debug, Clone)]
pub struct SynthOutput {
    pub netlist: write::NlModule,
    pub text: String,
    pub report: Report,
    /// Rendered warnings and notes.
    pub messages: Vec<String>,
}

/// Failure with rendered diagnostics.
#[derive(Debug, Clone)]
pub struct SynthError {
    pub messages: Vec<String>,
    pub internal: bool,
}

/// Runs RustSys. With a GPU mapping backend, the backend is prepared on a second thread while
/// the design is read and optimized (SYNTH-REQ-0518, COMPUTE-REQ-0003).
pub fn synthesize(
    src: &Sources,
    libs: &[Library],
    opts: &SynthOptions,
) -> Result<SynthOutput, SynthError> {
    synthesize_with(src, libs, opts, Ctx::NONE).map_err(|e| match e {
        JobError::Failed(e) => e,
        JobError::Cancelled => SynthError {
            messages: vec!["error[S0901]: internal: cancelled without a cancel token".into()],
            internal: true,
        },
    })
}

/// `synthesize` with a run context (SYNTH-REQ-0710): events for phases, optimization and mapping
/// steps, level progress, metrics and messages, and checkpoints for cancellation.
pub fn synthesize_with(
    src: &Sources,
    libs: &[Library],
    opts: &SynthOptions,
    ctx: Ctx<'_>,
) -> Result<SynthOutput, JobError<SynthError>> {
    if opts.backend == map::MapBackend::Cpu {
        return run(src, libs, opts, None, ctx, Carry::Prefix, None);
    }
    std::thread::scope(|s| {
        // The preparation runs its parallel steps in a pool of its own, so that it never waits
        // for the threads of the caller's pool; without a thread it is skipped.
        let warm = std::thread::Builder::new()
            .name("rustsys-gpu-prepare".into())
            .spawn_scoped(s, || {
                if let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(1).build() {
                    pool.install(|| map::prewarm(libs, &opts.dont_use, map_options(opts)));
                }
            })
            .ok();
        run(src, libs, opts, warm, ctx, Carry::Prefix, None)
    })
}

/// The optimization passes of each effort, in order (SYNTH-REQ-0412, SYNTH-REQ-0710).
pub fn optimization_steps(effort: Effort) -> &'static [&'static str] {
    const LOW: &[&str] = &["sweep"];
    const MEDIUM: &[&str] = &[
        "sweep",
        "mux_decode",
        "balance",
        "rewrite",
        "refactor",
        "resub",
        "rewrite",
        "balance",
        "mux_merge",
        "sweep",
    ];
    const HIGH: &[&str] = &[
        "sweep",
        "mux_decode",
        "balance",
        "rewrite",
        "refactor",
        "resub",
        "rewrite",
        "balance",
        "mux_merge",
        "sweep",
        "merge",
        "rewrite_zero",
        "balance",
        "refactor",
        "resub",
        "rewrite",
        "refactor",
        "balance",
    ];
    match effort {
        Effort::Low => LOW,
        Effort::Medium => MEDIUM,
        Effort::High => HIGH,
    }
}

/// Messages of a run: kept for the output and streamed as they arise (SYNTH-REQ-0708).
struct Msgs<'c> {
    list: Vec<String>,
    ctx: Ctx<'c>,
}

impl Msgs<'_> {
    fn push(&mut self, text: String) {
        stream(self.ctx, &text);
        self.list.push(text);
    }

    fn extend(&mut self, texts: Vec<String>) {
        for t in texts {
            self.push(t);
        }
    }
}

fn run(
    src: &Sources,
    libs: &[Library],
    opts: &SynthOptions,
    warm: Option<std::thread::ScopedJoinHandle<'_, ()>>,
    ctx: Ctx<'_>,
    carry: Carry,
    plan: Option<&[usize]>,
) -> Result<SynthOutput, JobError<SynthError>> {
    let t0 = Instant::now();
    if opts.electrical.is_none() && !opts.electrical_corners.is_empty() {
        return Err(fail(
            ctx,
            vec!["error[S0531]: electrical corners require a timing period".into()],
            false,
        ));
    }
    if let Some(e) = &opts.electrical {
        e.validate()
            .map_err(|m| fail(ctx, vec![format!("error[S0531]: {m}")], false))?;
        // SYNTH-REQ-0561: the incremental mode belongs to post-mapping commands; synthesis
        // always runs the complete refinement.
        if e.incremental.is_some() {
            return Err(fail(
                ctx,
                vec!["error[S0531]: the incremental electrical mode applies to a mapped netlist, not to synthesis".into()],
                false,
            ));
        }
    }
    let mut phase_s: Vec<(String, f64)> = Vec::new();
    let mut t_phase = Instant::now();
    // Records a phase and announces the next one (SYNTH-REQ-0710).
    let mut lap =
        |name: &'static str, next: Option<&'static str>, phase_s: &mut Vec<(String, f64)>| {
            let seconds = t_phase.elapsed().as_secs_f64();
            phase_s.push((name.to_string(), seconds));
            ctx.emit(|| Event::PhaseFinished {
                phase: name,
                seconds,
            });
            if let Some(phase) = next {
                ctx.emit(|| Event::PhaseStarted { phase });
            }
            t_phase = Instant::now();
        };
    ctx.emit(|| Event::PhaseStarted { phase: "read" });
    // Sources other than SystemVerilog (SYNTH-REQ-0900).
    let format =
        ingest::format_of(src, opts.gate_netlist).map_err(|m| fail(ctx, vec![m], false))?;
    let mut parsed = None;
    let mut messages = Msgs {
        list: Vec::new(),
        ctx,
    };
    let mut read_warnings = 0;
    if format != SourceFormat::SystemVerilog {
        ingest::check_options(opts, format).map_err(|m| fail(ctx, vec![m], false))?;
        let (p, warnings) = ingest::read(src, format, opts).map_err(|m| fail(ctx, m, false))?;
        read_warnings = warnings.len();
        messages.extend(warnings);
        parsed = Some(p);
    }
    let mut sm = SourceMap::new();
    let mut defines = Vec::new();
    if !opts.no_predefined_macros {
        defines.push(("SYNTHESIS".to_string(), String::new()));
    }
    defines.extend(opts.defines.iter().cloned());
    let pp = PpOptions {
        include_dirs: opts.include_dirs.clone(),
        defines,
    };
    let (units, diags) = match src {
        _ if parsed.is_some() => (Vec::new(), Vec::new()),
        Sources::Files(f) => read_sources(&mut sm, f, pp),
        Sources::Texts(t) => {
            let refs: Vec<(&str, &str)> = t.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            read_texts(&mut sm, &refs, pp)
        }
    };
    if diags.iter().any(Diagnostic::is_error) {
        return Err(fail(ctx, render(&sm, &diags), false));
    }
    messages.extend(render(&sm, &diags));
    let mut params = Vec::new();
    for (n, text) in &opts.params {
        match rusteda_formats::verilog::parse_expression(&mut sm, &format!("<-P {n}>"), text) {
            Ok(e) => params.push((n.clone(), e)),
            Err(d) => return Err(fail(ctx, render(&sm, &[d]), false)),
        }
    }
    let eopts = elab::ElabOptions {
        parallel_carry: opts.electrical.is_some() && carry != Carry::Ripple,
        carry_cost_weight: carry.area_weight(),
        xor_carry: matches!(carry, Carry::Xor | Carry::XorFast),
        carry_plan: plan.unwrap_or_default().to_vec(),
        assume_full_case: opts.assume_full_case,
        assume_parallel_case: opts.assume_parallel_case,
        top: opts.top.clone(),
        params,
    };
    lap("read", Some("elaborate"), &mut phase_s);
    ctx.checkpoint()?;
    let mut info = None;
    let (mut design, mut warnings) = if let Some(p) = &parsed {
        // Lowering of the other formats (SYNTH-REQ-0903 to SYNTH-REQ-0912).
        let low = ingest::lower(p, opts).map_err(|m| {
            let mut all = std::mem::take(&mut messages.list);
            for x in &m {
                stream(ctx, x);
            }
            all.extend(m);
            JobError::Failed(SynthError {
                messages: all,
                internal: false,
            })
        })?;
        messages.extend(low.messages);
        info = Some(low.info);
        (low.design, low.warnings + read_warnings)
    } else {
        let (design, ediags) = elab::elaborate(&sm, &units, &eopts);
        let Some(design) = design else {
            let mut all = std::mem::take(&mut messages.list);
            let rendered = render(&sm, &ediags);
            for m in &rendered {
                stream(ctx, m);
            }
            all.extend(rendered);
            return Err(JobError::Failed(SynthError {
                messages: all,
                internal: false,
            }));
        };
        messages.extend(render(&sm, &ediags));
        let w = ediags
            .iter()
            .filter(|d| d.severity == rusteda_formats::verilog::Severity::Warning)
            .count();
        (design, w)
    };
    // Elaboration ends after its messages, and only when it succeeded.
    lap("elaborate", Some("optimize"), &mut phase_s);
    let has_parallel_arithmetic = design.aig.has_parallel_arithmetic;
    let carry_plan = if carry == Carry::Prefix {
        bits::carry_plan(&design)
    } else {
        Vec::new()
    };
    design.aig.add_sites.clear();
    let state_bits_rtl = design.states.len();
    let aig_nodes_before = design.aig.num_ands();
    let constant_states = opt::reset::constants_with_initial(
        &mut design,
        &opts.after_reset,
        opts.uninitialized_dont_care,
    )
    .map_err(|m| fail(ctx, vec![format!("error[S0309]: {m}")], false))?;
    let ands = |d: &elab::Design| Event::Metric {
        name: "aig_ands",
        value: d.aig.num_ands() as f64,
    };
    ctx.emit(|| ands(&design));
    ctx.checkpoint()?;
    // Optimization script (SYNTH-REQ-0412); the order of the calls is that of
    // `optimization_steps`, one checkpoint and one event per pass.
    let mut script = optimization_steps(opts.effort).to_vec();
    if opts.electrical.is_some() && opts.effort == Effort::Medium {
        script.extend([
            "merge_local",
            "rewrite_zero",
            "balance",
            "refactor",
            "resub",
            "rewrite",
            "refactor",
            "balance",
            "sweep",
        ]);
    }
    if !opts.after_reset.is_empty() {
        script.push("merge_reset");
    }
    let mut step = 0usize;
    let mut opt_phase_s = Vec::new();
    let mut opt_work = Vec::new();
    let mut forms = opt::refactor::FormCache::default();
    let mut run_pass = |name: &'static str,
                        d: &mut elab::Design,
                        pass: fn(&mut elab::Design)|
     -> Result<(), JobError<SynthError>> {
        debug_assert_eq!(script.get(step), Some(&name));
        ctx.checkpoint()?;
        ctx.emit(|| Event::Step {
            phase: "optimize",
            step: name,
            ordinal: step as u32 + 1,
            count: script.len() as u32,
        });
        let started = Instant::now();
        if name == "sweep" {
            let stats = opt::sweep_with_limits(d, opt::parallel::WorkLimits::default());
            opt_work.push(OptimizationWork {
                pass: name.into(),
                windows: stats.windows,
                bounded_windows: 0,
                replayed_windows: 0,
                pass_budget_reached: stats.node_budget_reached || stats.storage_budget_reached,
            });
            ctx.emit(|| Event::Metric {
                name: "sweep_budget_reached",
                value: f64::from(stats.node_budget_reached || stats.storage_budget_reached),
            });
        } else if name == "balance" {
            let stats = opt::balance::balance_with_limits(d, opt::parallel::WorkLimits::default());
            opt_work.push(OptimizationWork {
                pass: name.into(),
                windows: stats.windows,
                bounded_windows: stats.bounded_windows,
                replayed_windows: 0,
                pass_budget_reached: stats.node_budget_reached || stats.storage_budget_reached,
            });
        } else if name == "refactor" {
            let stats = opt::refactor::refactor_cached_with_limits(
                d,
                &mut forms,
                opt::parallel::WorkLimits::default(),
            );
            opt_work.push(OptimizationWork {
                pass: name.into(),
                windows: stats.windows,
                bounded_windows: stats.bounded_windows,
                replayed_windows: 0,
                pass_budget_reached: stats.node_budget_reached || stats.storage_budget_reached,
            });
            ctx.emit(|| Event::Metric {
                name: "refactor_bounded_windows",
                value: stats.bounded_windows as f64,
            });
            ctx.emit(|| Event::Metric {
                name: "refactor_budget_reached",
                value: f64::from(stats.node_budget_reached || stats.storage_budget_reached),
            });
        } else if matches!(name, "rewrite" | "rewrite_zero") {
            let stats = opt::rewrite::rewrite_with_limits(
                d,
                name == "rewrite_zero",
                opt::parallel::WorkLimits::default(),
            );
            opt_work.push(OptimizationWork {
                pass: name.into(),
                windows: stats.windows,
                bounded_windows: stats.bounded_windows,
                replayed_windows: 0,
                pass_budget_reached: stats.node_budget_reached || stats.storage_budget_reached,
            });
            ctx.emit(|| Event::Metric {
                name: "rewrite_bounded_windows",
                value: stats.bounded_windows as f64,
            });
            ctx.emit(|| Event::Metric {
                name: "rewrite_budget_reached",
                value: f64::from(stats.node_budget_reached || stats.storage_budget_reached),
            });
        } else if name == "mux_decode" {
            let status =
                opt::muxdecode::mux_decode_with_limits(d, opt::muxdecode::DecodeLimits::default());
            ctx.emit(|| Event::Metric {
                name: "mux_decode_budget_reached",
                value: f64::from(status == opt::muxdecode::DecodeStatus::BudgetReached),
            });
        } else if name == "resub" {
            let stats = opt::resub::resub_with_limits(d, opt::resub::ResubLimits::default());
            opt_work.push(OptimizationWork {
                pass: name.into(),
                windows: stats.windows,
                bounded_windows: stats.bounded_windows,
                replayed_windows: stats.speculative_replays,
                pass_budget_reached: stats.node_budget_reached,
            });
            ctx.emit(|| Event::Metric {
                name: "resub_fanout_visits",
                value: stats.fanout_visits as f64,
            });
            ctx.emit(|| Event::Metric {
                name: "resub_bounded_windows",
                value: stats.bounded_windows as f64,
            });
            ctx.emit(|| Event::Metric {
                name: "resub_speculative_replays",
                value: stats.speculative_replays as f64,
            });
        } else {
            pass(d);
        }
        opt_phase_s.push((name.to_string(), started.elapsed().as_secs_f64()));
        ctx.emit(|| ands(d));
        step += 1;
        Ok(())
    };
    run_pass("sweep", &mut design, opt::sweep)?;
    match opts.effort {
        Effort::Low => {}
        Effort::Medium | Effort::High => {
            run_pass("mux_decode", &mut design, opt::muxdecode::mux_decode)?;
            run_pass("balance", &mut design, opt::balance::balance)?;
            run_pass("rewrite", &mut design, opt::rewrite::rewrite)?;
            run_pass("refactor", &mut design, opt::refactor::refactor)?;
            run_pass("resub", &mut design, opt::resub::resub)?;
            run_pass("rewrite", &mut design, opt::rewrite::rewrite)?;
            run_pass("balance", &mut design, opt::balance::balance)?;
            run_pass("mux_merge", &mut design, opt::muxmerge::mux_merge)?;
            run_pass("sweep", &mut design, opt::sweep)?;
            if opts.effort == Effort::High {
                run_pass("merge", &mut design, opt::merge::merge)?;
                run_pass("rewrite_zero", &mut design, opt::rewrite::rewrite_zero)?;
                run_pass("balance", &mut design, opt::balance::balance)?;
                run_pass("refactor", &mut design, opt::refactor::refactor)?;
                run_pass("resub", &mut design, opt::resub::resub)?;
                run_pass("rewrite", &mut design, opt::rewrite::rewrite)?;
                run_pass("refactor", &mut design, opt::refactor::refactor)?;
                run_pass("balance", &mut design, opt::balance::balance)?;
            }
        }
    }
    if opts.electrical.is_some() && opts.effort == Effort::Medium {
        run_pass("merge_local", &mut design, opt::merge::merge_local)?;
        run_pass("rewrite_zero", &mut design, opt::rewrite::rewrite_zero)?;
        run_pass("balance", &mut design, opt::balance::balance)?;
        run_pass("refactor", &mut design, opt::refactor::refactor)?;
        run_pass("resub", &mut design, opt::resub::resub)?;
        run_pass("rewrite", &mut design, opt::rewrite::rewrite)?;
        run_pass("refactor", &mut design, opt::refactor::refactor)?;
        run_pass("balance", &mut design, opt::balance::balance)?;
        run_pass("sweep", &mut design, opt::sweep)?;
    }
    if !opts.after_reset.is_empty() {
        ctx.checkpoint()?;
        ctx.emit(|| Event::Step {
            phase: "optimize",
            step: "merge_reset",
            ordinal: step as u32 + 1,
            count: script.len() as u32,
        });
    }
    let started = Instant::now();
    let merged_states = opt::reset::merge(&mut design, &opts.after_reset)
        .map_err(|m| fail(ctx, vec![format!("error[S0309]: {m}")], false))?;
    if !opts.after_reset.is_empty() {
        opt_phase_s.push(("merge_reset".into(), started.elapsed().as_secs_f64()));
    }
    drop(forms);
    let aig_nodes_after = design.aig.num_ands();
    lap("optimize", Some("map"), &mut phase_s);
    ctx.checkpoint()?;
    // Mapping.
    let lib = map::library::CellLib::build_profile(
        libs,
        &opts.dont_use,
        opts.electrical.is_some(),
        opts.electrical.as_ref().map(|e| e.input_slew_ns),
    )
    .map_err(|m| fail(ctx, vec![format!("error[S0500]: {m}")], false))?;
    for w in &lib.warnings {
        messages.push(format!("warning[S0501]: {w}"));
    }
    let seq = map::select_seq(&design, &lib)
        .map_err(|m| fail(ctx, vec![format!("error[S0520]: {m}")], false))?;
    let roots = map::roots(&design, &seq);
    let free = map::free_neg(&design, &seq);
    ctx.checkpoint()?;
    let mut wait = None;
    if let Some(w) = warm {
        ctx.emit(|| Event::Step {
            phase: "map",
            step: "gpu_wait",
            ordinal: 1,
            count: 1,
        });
        let t = Instant::now();
        // A panic of the preparation is reported by the mapping itself.
        let _ = w.join();
        wait = Some(("gpu_wait".to_string(), t.elapsed().as_secs_f64()));
    }
    let mut mapping_options = map_options(opts);
    let effective_cuts = map::mapper::effective_cut_limit(&design.aig, mapping_options)
        .map_err(|m| fail(ctx, vec![format!("error[S0530]: {m}")], false))?;
    mapping_options.primary_inputs = design.num_input_bits();
    if let Some(limit) = mapping_options.fanout_recovery {
        let l = libs
            .iter()
            .filter_map(|l| l.default_max_fanout)
            .fold(limit as f64, f64::min);
        mapping_options.fanout_recovery = Some((l.floor() as u32).max(2));
    }
    let boundary = opts
        .electrical
        .as_ref()
        .map(|e| map::timing_boundary(&design, &seq, &lib, libs, e));
    let bound_first = opts
        .electrical
        .as_ref()
        .is_none_or(|e| e.area_limit.is_none())
        || design.states.is_empty()
        || design.aig.num_ands() <= design.states.len() * 32;
    let mut mapped = map::map_with_boundary(
        &design.aig,
        &lib,
        &roots,
        &free,
        mapping_options,
        if bound_first { boundary.as_ref() } else { None },
        ctx,
    )
    .map_err(|e| match e {
        JobError::Cancelled => JobError::Cancelled,
        JobError::Failed(m) => fail(ctx, vec![format!("error[S0530]: {m}")], false),
    })?;
    ctx.emit(|| Event::Metric {
        name: "map_gates",
        value: mapped.gates.len() as f64,
    });
    ctx.checkpoint()?;
    lap("map", Some("emit"), &mut phase_s);
    let (mut netlist, mut stats) = emit::emit_with_states(
        &design,
        &lib,
        &mapped,
        &seq,
        &constant_states,
        &merged_states,
    );
    let electrical = if let Some(options) = &opts.electrical {
        lap("emit", Some("electrical"), &mut phase_s);
        let mut state_aliases = emit::state_nets(&design);
        state_aliases.extend(
            merged_states
                .iter()
                .map(|a| emit::state_name_ref(&design, &a.name)),
        );
        let r = covers::refine_with_alternatives(
            &covers::Covers {
                design: &design,
                lib: &lib,
                seq: &seq,
                constant_states: &constant_states,
                merged_states: &merged_states,
                libs,
                options,
                corners: &opts.electrical_corners,
                state_aliases: &state_aliases,
                roots: &roots,
                free: &free,
                mapping_options,
                boundary: boundary.as_ref(),
                bound_first,
                goal: opts.goal,
                delay_relax: opts.delay_relax,
                ctx,
            },
            &mut netlist,
            &mut mapped,
        )?;
        // Independently selected carry blocks can shorten only the additions that need it;
        // the ripple candidate may recover area. Rank completed, electrically checked covers.
        if carry == Carry::Prefix && (!r.area_limit_met || !r.estimated_timing_met) {
            let mut best = None;
            let mut fast_area_miss = covers::fast_area_miss(&r, options.area_limit);
            let alternatives =
                Carry::alternatives(opts.effort, r.area_limit_met, has_parallel_arithmetic);
            let count = alternatives.len() as u32;
            for (i, alternative) in alternatives.into_iter().enumerate() {
                if alternative == Carry::Planned && !carry_plan.iter().any(|x| *x != 0) {
                    continue;
                }
                ctx.checkpoint()?;
                ctx.emit(|| Event::Step {
                    phase: "electrical",
                    step: alternative.label(),
                    ordinal: i as u32 + 1,
                    count,
                });
                let mut refined_options = opts.clone();
                if alternative == Carry::HighEffort {
                    refined_options.effort = Effort::High;
                }
                match run(
                    src,
                    libs,
                    &refined_options,
                    None,
                    ctx.unobserved(),
                    alternative,
                    (alternative == Carry::Planned).then_some(carry_plan.as_slice()),
                ) {
                    Ok(alt) => {
                        fast_area_miss |= alt
                            .report
                            .electrical
                            .as_ref()
                            .is_some_and(|r| covers::fast_area_miss(r, options.area_limit));
                        let incumbent = best
                            .as_ref()
                            .and_then(|o: &SynthOutput| o.report.electrical.as_deref())
                            .unwrap_or(&r);
                        if alt
                            .report
                            .electrical
                            .as_deref()
                            .is_some_and(|a| covers::better(a, incumbent))
                        {
                            best = Some(alt);
                        }
                    }
                    Err(JobError::Cancelled) => return Err(JobError::Cancelled),
                    Err(_) => {}
                }
                if best.as_ref().is_some_and(covers::output_complete) {
                    break;
                }
            }
            if fast_area_miss && !best.as_ref().is_some_and(covers::output_complete) {
                let mut xo = eopts.clone();
                xo.xor_carry = true;
                xo.carry_cost_weight = None;
                xo.carry_plan.clear();
                if let Some(xd) = elab::elaborate(&sm, &units, &xo).0 {
                    let widths: Vec<_> = xd
                        .aig
                        .add_sites
                        .iter()
                        .map(|s| s.as_ref().map_or(0, |s| s.a.len()))
                        .collect();
                    let mut trial = 0;
                    if let Some(out) = covers::individual_carries(&widths, |plan| {
                        trial += 1;
                        ctx.emit(|| Event::Step {
                            phase: "electrical",
                            step: "individual XOR carry",
                            ordinal: trial,
                            count: 12,
                        });
                        run(
                            src,
                            libs,
                            opts,
                            None,
                            ctx.unobserved(),
                            Carry::Xor,
                            Some(plan),
                        )
                    })? {
                        best = Some(out);
                    }
                }
            }
            if let Some(mut out) = best {
                lap("electrical", Some("write"), &mut phase_s);
                lap("write", None, &mut phase_s);
                for m in &out.messages {
                    if !messages.list.contains(m) {
                        stream(ctx, m);
                    }
                }
                out.report.phase_s = phase_s;
                out.report.runtime_s = t0.elapsed().as_secs_f64();
                return Ok(out);
            }
        }
        stats = emit::recount(&netlist, &lib);
        if !r.area_limit_met {
            messages.push(format!(
                "warning[S0532]: final cell area {:.6} exceeds the requested limit {:.6}",
                r.area_after,
                options.area_limit.expect("bounded area")
            ));
            warnings += 1;
        }
        if !r.estimated_timing_met {
            messages.push(format!("warning[S0533]: estimated setup slack is {:.6} ns; the requested period is not met by the electrical estimate",r.estimated_slack_ns));
            warnings += 1;
        }
        if !r.estimated_data_rules_met {
            messages.push("warning[S0534]: data-net electrical constraints remain unmet; inspect per-corner capacitance, transition and weighted fanout in the report".into());
            warnings += 1;
        }
        lap("electrical", Some("write"), &mut phase_s);
        Some(Box::new(r))
    } else {
        None
    };
    let default_quality = if let Some(limits) =
        opts.default_quality.filter(|_| electrical.is_none())
    {
        let mut aliases = Vec::new();
        if netlist.instances.len() <= limits.max_gates {
            aliases = emit::state_nets(&design);
            aliases.extend(
                merged_states
                    .iter()
                    .map(|a| emit::state_name_ref(&design, &a.name)),
            );
        }
        let mut quality =
            electrical::default_quality::run(&mut netlist, libs, &lib, &aliases, None, limits, ctx)
                .map_err(|e| match e {
                    JobError::Cancelled => JobError::Cancelled,
                    JobError::Failed(m) => fail(ctx, vec![format!("error[S0536]: {m}")], false),
                })?;
        if design.aig.num_nodes() <= limits.max_remap_nodes.min(65_536)
            && opts.effort != Effort::Low
            && quality
                .estimated_path_before_ns
                .zip(quality.estimated_path_after_ns)
                .is_some_and(|(a, b)| b > a * 0.30)
            && quality.changes.as_ref().is_some_and(|r| {
                r.electrical_rules.worst_capacitance_ratio > 4.0
                    || r.electrical_rules.worst_transition_ratio > 4.0
            })
        {
            covers::default_alternative(
                &design,
                libs,
                &opts.dont_use,
                &constant_states,
                &merged_states,
                mapping_options,
                &mut netlist,
                &mut quality,
                ctx,
            )?;
        }
        if quality.status == "improved" {
            stats = emit::recount(&netlist, &lib);
        }
        Some(Box::new(quality))
    } else {
        None
    };
    let text = write::write_netlist(&netlist);
    lap(
        if electrical.is_some() {
            "write"
        } else {
            "emit"
        },
        None,
        &mut phase_s,
    );
    let latches = design
        .states
        .iter()
        .filter(|s| s.kind == elab::StateKind::Latch)
        .count();
    let report = Report {
        format: REPORT_FORMAT.into(),
        format_version: 1,
        top: design.top.clone(),
        cells: stats.cells.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        area: stats.area,
        sequential_cells: stats.sequential,
        combinational_cells: stats.combinational,
        inverters: stats.inverters,
        logic_depth: electrical.as_ref().map_or_else(
            || {
                default_quality
                    .as_ref()
                    .filter(|q| q.status == "improved")
                    .and_then(|q| q.logic_depth_after)
                    .unwrap_or(mapped.depth)
            },
            |e| e.logic_depth,
        ),
        estimated_delay: mapped.max_arrival,
        aig_nodes_before,
        aig_nodes_after,
        map_requested_cuts: Some(mapping_options.max_cuts),
        map_effective_cuts: Some(effective_cuts),
        map_cut_memory_budget_bytes: Some(mapping_options.cut_memory_budget),
        state_bits_rtl,
        state_bits_retained: design.states.len(),
        after_reset: opts.after_reset.clone(),
        uninitialized_dont_care: opts.uninitialized_dont_care,
        assume_full_case: opts.assume_full_case,
        assume_parallel_case: opts.assume_parallel_case,
        constant_states,
        merged_states,
        latches,
        warnings,
        unused_cells: lib.rejected.iter().cloned().collect(),
        runtime_s: t0.elapsed().as_secs_f64(),
        phase_s,
        map_backend: mapped.backend.to_string(),
        map_phase_s: wait
            .into_iter()
            .chain(mapped.phase_s.iter().map(|(k, v)| (k.to_string(), *v)))
            .collect(),
        opt_phase_s,
        opt_work,
        cell_area: stats
            .cell_area
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
        time_unit_ns: (lib.time_unit_ns > 0.0).then_some(lib.time_unit_ns),
        electrical,
        default_quality,
        source_format: info.as_ref().map(|i| i.format.to_string()),
        initial_values_mode: info
            .as_ref()
            .filter(|i| i.format != "gate-netlist")
            .map(|_| opts.initial_values.name().to_string()),
        initial_values: info
            .as_ref()
            .map(|i| i.initial_values.clone())
            .unwrap_or_default(),
        clock_port: info.as_ref().and_then(|i| i.clock_port.clone()),
        init_port: info.as_ref().and_then(|i| i.init_port.clone()),
    };
    // An observer may cancel in the final phase-finished notification too.
    ctx.checkpoint()?;
    Ok(SynthOutput {
        netlist,
        text,
        report,
        messages: messages.list,
    })
}
