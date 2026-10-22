// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Electrical refinement of the mapped cover and of its alternatives (SYNTH-REQ-0550,
//! SYNTH-REQ-0553): other covers of the same function are emitted, refined with the same options
//! and kept when they rank better.

mod individual;

use rustc_hash::FxHashSet;
use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::liberty::Library;
use rusteda_formats::verilog::write::{NlModule, NlRef};

use crate::map::{self, MapOptions, Mapped, TimingBoundary, library::CellLib};
use crate::{Effort, Goal, SynthError, SynthOptions, elab, electrical, emit, fail, opt};

/// What refining a cover needs, shared by all alternatives.
pub(crate) struct Covers<'a> {
    pub design: &'a elab::Design,
    pub lib: &'a CellLib,
    pub seq: &'a [map::SeqChoice],
    pub constant_states: &'a [opt::reset::ConstantState],
    pub merged_states: &'a [opt::reset::EquivalentState],
    pub libs: &'a [Library],
    pub options: &'a electrical::Options,
    pub corners: &'a [Library],
    pub state_aliases: &'a [NlRef],
    pub roots: &'a [crate::aig::Lit],
    pub free: &'a FxHashSet<u32>,
    pub mapping_options: MapOptions,
    pub boundary: Option<&'a TimingBoundary>,
    pub bound_first: bool,
    pub goal: Goal,
    pub delay_relax: f64,
    pub ctx: Ctx<'a>,
}

/// Rules first, then the area limit; over the limit the smaller area, else the larger estimated
/// slack (SYNTH-REQ-0550, SYNTH-REQ-0552); ties keep the first.
pub(crate) fn better(a: &electrical::Report, b: &electrical::Report) -> bool {
    let rank = |x: &electrical::Report| {
        (
            x.estimated_data_rules_met,
            x.area_limit_met,
            if x.area_limit_met {
                x.estimated_slack_ns
            } else {
                -x.area_after
            },
        )
    };
    let (a, b) = (rank(a), rank(b));
    (a.0, a.1) > (b.0, b.1) || ((a.0, a.1) == (b.0, b.1) && a.2 > b.2)
}

impl Covers<'_> {
    fn refine(&self, netlist: &mut NlModule) -> Result<electrical::Report, JobError<SynthError>> {
        electrical::refine_named(
            netlist,
            self.libs,
            self.lib,
            self.options,
            self.corners,
            Some(self.state_aliases),
            self.ctx,
        )
        .map_err(|e| match e {
            JobError::Cancelled => JobError::Cancelled,
            JobError::Failed(m) => fail(self.ctx, vec![format!("error[S0531]: {m}")], false),
        })
    }

    /// Maps, emits and refines another cover of the same function.
    fn alternative(
        &self,
        options: MapOptions,
        boundary: Option<&TimingBoundary>,
    ) -> Result<(Mapped, NlModule, electrical::Report), JobError<SynthError>> {
        self.ctx.checkpoint()?;
        let cover = map::map_with_boundary(
            &self.design.aig,
            self.lib,
            self.roots,
            self.free,
            options,
            boundary,
            self.ctx,
        )
        .map_err(|e| match e {
            JobError::Cancelled => JobError::Cancelled,
            JobError::Failed(m) => fail(self.ctx, vec![format!("error[S0530]: {m}")], false),
        })?;
        let (mut nl, _) = emit::emit_with_states(
            self.design,
            self.lib,
            &cover,
            self.seq,
            self.constant_states,
            self.merged_states,
        );
        let r = self.refine(&mut nl)?;
        Ok((cover, nl, r))
    }
}

/// Refines the emitted `netlist` of `mapped`, then tries the alternative covers and keeps the
/// best; `netlist` and `mapped` become the kept cover.
pub(crate) fn refine_with_alternatives(
    cx: &Covers<'_>,
    netlist: &mut NlModule,
    mapped: &mut Mapped,
) -> Result<electrical::Report, JobError<SynthError>> {
    let mut r = cx.refine(netlist)?;
    let mut keep = |(cover, nl, alt): (Mapped, NlModule, electrical::Report),
                    r: &mut electrical::Report| {
        if better(&alt, r) {
            *netlist = nl;
            *mapped = cover;
            *r = alt;
        }
    };
    // A cover without the timing boundary leaves the refinement more of a tight area budget, and
    // may also be faster after refinement (SYNTH-REQ-0550).
    if cx.options.area_limit.is_some()
        && (!r.estimated_data_rules_met || !r.area_limit_met || !r.estimated_timing_met)
    {
        let alt_boundary = if cx.bound_first { None } else { cx.boundary };
        let alt = cx.alternative(cx.mapping_options, alt_boundary)?;
        keep(alt, &mut r);
    }
    // Delay-oriented covers, with and without the timing boundary, when the estimated period is
    // missed (SYNTH-REQ-0553).
    for bounded in [true, false] {
        if cx.goal != Goal::Area || r.estimated_timing_met {
            break;
        }
        let mut options = cx.mapping_options;
        options.delay_relax = Some(cx.delay_relax);
        let alt = cx.alternative(options, if bounded { cx.boundary } else { None })?;
        keep(alt, &mut r);
    }
    Ok(r)
}

/// Carry structure of arithmetic under a timing period (SYNTH-REQ-0405, SYNTH-REQ-0554).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Carry {
    /// Sparse parallel prefix carries, with a ripple-carry alternative under a missed area limit.
    Prefix,
    /// Ripple carries only: the alternative itself.
    Ripple,
    Planned,
    Adaptive,
    AdaptiveFast,
    AdaptiveBalanced,
    AdaptiveCompact,
    AdaptiveSparse,
    AdaptiveSmall,
    Xor,
    XorFast,
    HighEffort,
}

impl Carry {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Prefix => "prefix carry",
            Self::Ripple => "ripple carry",
            Self::Planned => "planned carry blocks",
            Self::Adaptive => "adaptive carry 0.025",
            Self::AdaptiveFast => "adaptive carry 0",
            Self::AdaptiveBalanced => "adaptive carry 0.01",
            Self::AdaptiveCompact => "adaptive carry 0.08",
            Self::AdaptiveSparse => "adaptive carry 0.2",
            Self::AdaptiveSmall => "adaptive carry 0.5",
            Self::Xor => "XOR carry propagation",
            Self::XorFast => "arrival-driven XOR carry",
            Self::HighEffort => "high effort cover",
        }
    }
    pub(crate) fn area_weight(self) -> Option<f64> {
        match self {
            Self::Adaptive => Some(0.025),
            Self::XorFast => Some(0.0),
            Self::AdaptiveFast => Some(0.0),
            Self::AdaptiveBalanced => Some(0.01),
            Self::AdaptiveCompact => Some(0.08),
            Self::AdaptiveSparse => Some(0.2),
            Self::AdaptiveSmall => Some(0.5),
            _ => None,
        }
    }

    pub(crate) fn alternatives(effort: Effort, area_met: bool, arithmetic: bool) -> Vec<Self> {
        let mut choices = Vec::new();
        if effort == Effort::Medium {
            choices.push(Self::HighEffort);
        }
        if !arithmetic {
            return choices;
        }
        if !area_met {
            choices.extend([Self::Xor, Self::XorFast]);
            choices.push(Self::Planned);
        }
        if area_met {
            choices.extend([
                Self::AdaptiveFast,
                Self::AdaptiveBalanced,
                Self::Adaptive,
                Self::AdaptiveCompact,
            ]);
        } else {
            choices.extend([
                Self::AdaptiveCompact,
                Self::Adaptive,
                Self::AdaptiveBalanced,
                Self::AdaptiveFast,
            ]);
        }
        if area_met {
            choices.push(Self::Planned);
        }
        choices.extend([Self::AdaptiveSparse, Self::AdaptiveSmall]);
        if area_met {
            choices.extend([Self::Xor, Self::XorFast]);
        }
        choices.push(Self::Ripple);
        choices
    }
}

/// One bounded compact-cover alternative for unconstrained default quality.
#[allow(clippy::too_many_arguments)]
pub(crate) fn default_alternative(
    design: &elab::Design,
    libs: &[Library],
    dont_use: &[String],
    constants: &[opt::reset::ConstantState],
    merged: &[opt::reset::EquivalentState],
    mut options: MapOptions,
    netlist: &mut NlModule,
    report: &mut electrical::default_quality::QualityReport,
    ctx: Ctx<'_>,
) -> Result<(), JobError<SynthError>> {
    // One bounded alternative: at most64K Boolean nodes (~16MiB with graph/metadata),
    // shared library, <=4GiB existing cut cap. It may only spend the original cover's area.
    let Some(ceiling) = report.area_before else {
        return Ok(());
    };
    let mut d = design.clone();
    let limits = opt::parallel::WorkLimits {
        max_nodes: 65_536,
        max_plan_bytes: 256 * 1024 * 1024,
        max_cut_bytes: 128 * 1024 * 1024,
        ..Default::default()
    };
    let passes = [
        "merge",
        "rewrite_zero",
        "balance",
        "refactor",
        "resub",
        "rewrite",
        "refactor",
        "balance",
        "sweep",
    ];
    for pass in passes {
        ctx.checkpoint()?;
        match pass {
            "merge" => opt::merge::merge_local(&mut d),
            "rewrite" | "rewrite_zero" => {
                opt::rewrite::rewrite_with_limits(&mut d, pass == "rewrite_zero", limits);
            }
            "balance" => {
                opt::balance::balance_with_limits(&mut d, limits);
            }
            "refactor" => {
                opt::refactor::refactor_cached_with_limits(&mut d, &mut Default::default(), limits);
            }
            "resub" => {
                opt::resub::resub_with_limits(
                    &mut d,
                    opt::resub::ResubLimits {
                        max_nodes: 65_536,
                        max_proposal_bytes: 32 * 1024 * 1024,
                        ..Default::default()
                    },
                );
            }
            _ => {
                opt::sweep_with_limits(&mut d, limits);
            }
        }
        if d.aig.num_nodes() > 65_536 {
            return Ok(());
        }
    }
    let loaded = CellLib::build_profile(libs, dont_use, true, Some(0.1))
        .map_err(|m| fail(ctx, vec![format!("error[S0500]: {m}")], false))?;
    let lib = &loaded;
    let seq = map::select_seq(&d, lib)
        .map_err(|m| fail(ctx, vec![format!("error[S0520]: {m}")], false))?;
    let roots = map::roots(&d, &seq);
    let free = map::free_neg(&d, &seq);
    options.fanout_recovery = Some(8);
    options.cut_memory_budget = options.cut_memory_budget.min(256 * 1024 * 1024);
    options.primary_inputs = d.num_input_bits();
    let mapped = map::mapper::map_with_boundary(&d.aig, lib, &roots, &free, options, None, ctx)
        .map_err(|e| match e {
            JobError::Cancelled => JobError::Cancelled,
            JobError::Failed(m) => fail(ctx, vec![format!("error[S0530]: {m}")], false),
        })?;
    let (mut candidate, _) = emit::emit_with_states(&d, lib, &mapped, &seq, constants, merged);
    let mut aliases = emit::state_nets(&d);
    aliases.extend(merged.iter().map(|a| emit::state_name_ref(&d, &a.name)));
    let q = electrical::default_quality::run(
        &mut candidate,
        libs,
        lib,
        &aliases,
        Some(ceiling),
        report.limits,
        ctx,
    )
    .map_err(|e| match e {
        JobError::Cancelled => JobError::Cancelled,
        JobError::Failed(m) => fail(ctx, vec![format!("error[S0536]: {m}")], false),
    })?;
    report.alternative = Some(Box::new(q.clone()));
    if q.area_after.is_some_and(|a| a <= ceiling + 1e-8)
        && q.estimated_path_after_ns
            .zip(report.estimated_path_after_ns)
            .is_some_and(|(a, b)| a < b - 1e-8)
    {
        *netlist = candidate;
        report.status = "improved".into();
        report.area_after = q.area_after;
        report.estimated_path_after_ns = q.estimated_path_after_ns;
        report.logic_depth_after = q.logic_depth_after;
        report.changes = q.changes;
    }
    Ok(())
}

/// Mapping options per effort and goal.
pub(crate) fn map_options(opts: &SynthOptions) -> MapOptions {
    // Priority cuts and recovery passes per effort (SYNTH-REQ-0510).
    let (max_cuts, area_passes, exact_passes) = match opts.effort {
        Effort::Low => (12, 2, 2),
        Effort::Medium => (40, 4, 6),
        Effort::High => (100, 4, 8),
    };
    MapOptions {
        max_cuts,
        cut_memory_budget: MapOptions::default().cut_memory_budget,
        area_passes,
        exact_passes,
        exact_limits: Default::default(),
        delay_relax: match opts.goal {
            Goal::Delay => Some(opts.delay_relax),
            Goal::Area => None,
        },
        backend: opts.backend,
        fanout_recovery: opts.electrical.as_ref().map(|e| e.buffer_fanout as u32),
        primary_inputs: 0,
    }
}

/// Return only a jointly feasible per-addition XOR implementation.
pub(crate) fn individual_carries(
    widths: &[usize],
    evaluate: impl FnMut(&[usize]) -> Result<crate::SynthOutput, JobError<SynthError>>,
) -> Result<Option<crate::SynthOutput>, JobError<SynthError>> {
    individual::run(widths, evaluate, output_complete)
}

/// Joint acceptance for an electrically evaluated candidate.
pub(crate) fn output_complete(out: &crate::SynthOutput) -> bool {
    out.report
        .electrical
        .as_ref()
        .is_some_and(|r| r.area_limit_met && r.estimated_timing_met && r.estimated_data_rules_met)
}

/// A near-budget fast cover justifies bounded per-addition area recovery.
pub(crate) fn fast_area_miss(r: &electrical::Report, limit: Option<f64>) -> bool {
    !r.area_limit_met
        && r.estimated_timing_met
        && r.estimated_data_rules_met
        && limit.is_some_and(|l| r.area_after <= 1.05 * l)
}
