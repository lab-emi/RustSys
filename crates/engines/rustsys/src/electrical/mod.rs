// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Load-aware refinement of a Boolean cover (SYNTH-REQ-0530 to SYNTH-REQ-0535).

mod collapse;
mod decompose;
pub mod default_quality;
mod fanout;
mod incremental;
mod load_buffer;
mod model;
mod network;
mod parallel_timing;
mod pins;
mod polarity;
mod repack;
mod repair;
mod replicate;
mod resize;
mod rules;
mod state_phase;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_rules;
mod timing;

use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::liberty::Library;
use rusteda_formats::verilog::write::NlModule;
use serde::{Deserialize, Serialize};

use crate::map::library::CellLib;
pub use parallel_timing::ParallelTiming;
pub use rules::RuleReport;

/// Uniform ideal-clock constraints for electrical refinement; times are ns, loads are pF.
/// The estimator cuts sequential state and does not replace a complete STA check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub period_ns: f64,
    pub area_limit: Option<f64>,
    pub input_delay_ns: f64,
    pub output_delay_ns: f64,
    pub input_slew_ns: f64,
    pub clock_slew_ns: f64,
    pub output_load_pf: f64,
    pub margin_ns: f64,
    pub buffer_fanout: usize,
    /// Bounded CPU timing planning; exhausted budgets use the serial evaluator.
    pub parallel_timing: ParallelTiming,
    /// Incremental optimization of a netlist that is already mapped and refined, for
    /// post-mapping commands (SYNTH-REQ-0561); `None` runs the complete refinement of
    /// synthesis, which is unchanged by this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental: Option<Incremental>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            period_ns: 10.0,
            area_limit: None,
            input_delay_ns: 0.0,
            output_delay_ns: 0.0,
            input_slew_ns: 0.1,
            clock_slew_ns: 0.1,
            output_load_pf: 0.01,
            margin_ns: 0.05,
            buffer_fanout: 8,
            parallel_timing: ParallelTiming::default(),
            incremental: None,
        }
    }
}

/// The incremental mode (SYNTH-REQ-0561 to SYNTH-REQ-0564): start from the cells of the netlist
/// and change only the instances in `focus` and the nets they drive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incremental {
    /// Instance names that may be resized and whose output nets may be buffered; `None`: every
    /// instance. Buffers inserted by the mode belong to it.
    pub focus: Option<Vec<String>>,
    /// The most iterations (one change each), at least 1.
    pub max_iterations: usize,
    /// When to stop before `max_iterations`.
    pub stop: StopRule,
    /// How many candidate changes an iteration examines (SYNTH-REQ-0563).
    pub effort: SearchEffort,
}

/// The stop condition of the incremental mode, evaluated on the estimate of the starting netlist
/// and after every iteration (SYNTH-REQ-0562).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopRule {
    /// The estimated worst setup slack is at least the value (ns).
    WnsAtLeast(f64),
    /// The estimated total negative setup slack is at least the value (ns).
    TnsAtLeast(f64),
    /// Only when no change improves the estimate.
    NoImprovement,
}

/// The search breadth of one incremental iteration (SYNTH-REQ-0563).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchEffort {
    Low,
    Medium,
    High,
}

/// Why the incremental mode stopped (SYNTH-REQ-0562).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The stop condition holds.
    ConditionMet,
    /// `max_iterations` iterations ran.
    MaxIters,
    /// No examined change improves the estimate.
    NoImprovement,
    /// A change would improve the estimate, but every such change exceeds the area limit.
    AreaLimit,
}

impl StopReason {
    /// `condition_met`, `max_iters`, `no_improvement` or `area_limit`.
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::ConditionMet => "condition_met",
            StopReason::MaxIters => "max_iters",
            StopReason::NoImprovement => "no_improvement",
            StopReason::AreaLimit => "area_limit",
        }
    }
}

/// One iteration of the incremental mode: the change it made and the estimate after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IterationRecord {
    pub iteration: usize,
    /// `resize` or `buffer`.
    pub change: String,
    /// The resized instance, or the driver of the buffered net; `None` for a buffer that an
    /// earlier iteration inserted (it is named when the netlist is written).
    pub instance: Option<String>,
    /// Resize: the previous cell; buffer: the driver's cell.
    pub from_cell: String,
    /// Resize: the new cell; buffer: the buffer cell.
    pub to_cell: String,
    /// Buffer: the buffered net, when it has a name before the netlist is written.
    pub net: Option<String>,
    /// Buffer: the sinks moved behind the buffer.
    pub moved_sinks: usize,
    pub estimated_wns_ns: f64,
    pub estimated_tns_ns: f64,
    pub area: f64,
}

/// The result of the incremental mode (SYNTH-REQ-0562).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncrementalReport {
    pub iterations_run: usize,
    pub stop_reason: StopReason,
    /// Instances that the mode may change, before buffers are inserted.
    pub editable_instances: usize,
    pub wns_before_ns: f64,
    pub tns_before_ns: f64,
    pub wns_after_ns: f64,
    pub tns_after_ns: f64,
    pub history: Vec<IterationRecord>,
}

impl Options {
    pub fn validate(&self) -> Result<(), String> {
        if !self.period_ns.is_finite() || self.period_ns <= 0.0 {
            return Err("timing period must be finite and positive".into());
        }
        if [
            self.input_delay_ns,
            self.output_delay_ns,
            self.input_slew_ns,
            self.clock_slew_ns,
            self.output_load_pf,
            self.margin_ns,
        ]
        .iter()
        .any(|x| !x.is_finite() || *x < 0.0)
        {
            return Err(
                "timing delays, transitions, margin and load must be finite and nonnegative".into(),
            );
        }
        if self.area_limit.is_some_and(|a| !a.is_finite() || a <= 0.0) {
            return Err("area limit must be finite and positive".into());
        }
        if self.buffer_fanout < 2 || self.buffer_fanout > 128 {
            return Err("buffer fanout must be between 2 and 128".into());
        }
        if let Some(inc) = &self.incremental {
            if inc.max_iterations == 0 || inc.max_iterations > incremental::MAX_ITERATIONS {
                return Err(format!(
                    "incremental iterations must be between 1 and {}",
                    incremental::MAX_ITERATIONS
                ));
            }
            if let StopRule::WnsAtLeast(x) | StopRule::TnsAtLeast(x) = inc.stop {
                if !x.is_finite() {
                    return Err("an incremental stop threshold must be finite".into());
                }
            }
        }
        Ok(())
    }
}

/// Estimated electrical quality and the transformations performed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub logic_depth: u32,
    pub gates_collapsed: usize,
    pub two_cell_rewrites: usize,
    pub cells_compacted: usize,
    pub area_after_compaction: f64,
    pub inverters_absorbed: usize,
    pub state_inverters_absorbed: usize,
    /// Inserted tree cells, including replacements of earlier buffer or inverter trees.
    pub buffers_added: usize,
    /// Changed cells summed across sizing invocations; a cell can be counted more than once.
    pub resized_cells: usize,
    pub iterations: usize,
    pub area_before: f64,
    pub area_after: f64,
    pub estimated_slack_ns: f64,
    pub estimated_violating_endpoints: usize,
    pub area_limit_met: bool,
    pub estimated_timing_met: bool,
    pub electrical_rules: RuleReport,
    pub estimated_data_rules_met: bool,
    pub rule_buffers: usize,
    pub rule_resizes: usize,
    pub rule_clones: usize,
    pub pin_swaps: usize,
    pub load_buffers: usize,
    pub corner_rules: Vec<(String, RuleReport)>,
    /// The result of the incremental mode; absent after the complete refinement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental: Option<IncrementalReport>,
}

pub fn refine(
    netlist: &mut NlModule,
    libs: &[Library],
    eligible: &CellLib,
    opts: &Options,
    ctx: Ctx<'_>,
) -> Result<Report, JobError<String>> {
    refine_corners(netlist, libs, eligible, opts, &[], ctx)
}

/// Extra corners constrain electrical rules on the same cells and pin ordering.
/// Setup optimization still uses the primary timing library; external STA checks all corners.
pub fn refine_corners(
    netlist: &mut NlModule,
    libs: &[Library],
    eligible: &CellLib,
    opts: &Options,
    corners: &[Library],
    ctx: Ctx<'_>,
) -> Result<Report, JobError<String>> {
    refine_named(netlist, libs, eligible, opts, corners, None, ctx)
}

pub fn refine_named(
    netlist: &mut NlModule,
    libs: &[Library],
    eligible: &CellLib,
    opts: &Options,
    corners: &[Library],
    state_aliases: Option<&[rusteda_formats::verilog::write::NlRef]>,
    ctx: Ctx<'_>,
) -> Result<Report, JobError<String>> {
    opts.validate().map_err(JobError::Failed)?;
    if let Some(inc) = &opts.incremental {
        return incremental::run(netlist, libs, eligible, opts, inc, corners, ctx);
    }
    let model = model::Model::new(libs, eligible);
    let corner_models: Vec<_> = corners
        .iter()
        .map(|l| model::Model::corner(&model, l, eligible))
        .collect::<Result<_, _>>()
        .map_err(JobError::Failed)?;
    let mut net = network::Network::with_state_aliases(netlist, &model, state_aliases)
        .map_err(JobError::Failed)?;
    let mut report = Report {
        area_before: net.area(&model),
        ..Report::default()
    };
    report.state_inverters_absorbed = state_phase::fold(&mut net, &model);
    (report.gates_collapsed, report.two_cell_rewrites) = collapse::run(&mut net, &model, ctx)?;
    report.inverters_absorbed = polarity::fold(&mut net, &model);
    for g in net.order.clone() {
        let gate = &net.gates[g];
        let c = &model.cells[gate.cell];
        if net.nodes[gate.outputs[0].1].protected {
            continue;
        }
        let smallest = model.families[c.family][0];
        if model.cells[smallest].raw.area < c.raw.area - 1e-8 {
            net.change_cell(g, smallest, &model);
            report.cells_compacted += 1;
        }
    }
    report.area_after_compaction = net.area(&model);
    let compact = opts.area_limit.map(|_| (net.clone(), report.clone()));
    let repair_all = |net: &mut network::Network,
                      report: &mut Report,
                      opts: &Options|
     -> Result<(), JobError<String>> {
        // First make every corner's cap/slew constraints eligible for the budget, then
        // include fanout. A timing target never implicitly disables a library rule.
        for mode in [false, true] {
            for _ in 0..3 {
                let before = (report.rule_buffers, report.rule_resizes);
                for m in std::iter::once(&model).chain(&corner_models) {
                    repair::run_modes(net, m, opts, report, ctx, &[mode])?;
                }
                if before == (report.rule_buffers, report.rule_resizes) {
                    break;
                }
            }
        }
        Ok(())
    };
    let legal = |net: &network::Network| {
        std::iter::once(&model).chain(&corner_models).all(|m| {
            let r = rules::report(net, m, &timing::Timing::rules(net, m, opts));
            r.capacitance_nets == 0 && r.transition_nets == 0 && r.fanout_nets == 0
        })
    };
    // Data-net rules take precedence over the area limit (SYNTH-REQ-0552): violations the
    // budget cannot remove are repaired by the same cheapest-first passes without the limit.
    let unbounded = Options {
        area_limit: None,
        ..opts.clone()
    };
    let mandatory =
        |net: &mut network::Network, report: &mut Report| -> Result<(), JobError<String>> {
            if opts.area_limit.is_some() && !legal(net) {
                replicate::run(net, &model, &corner_models, &unbounded, report, ctx)?;
                repair_all(net, report, &unbounded)?;
            }
            Ok(())
        };
    replicate::run(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
    repair_all(&mut net, &mut report, opts)?;
    mandatory(&mut net, &mut report)?;
    let before_legal = legal(&net).then(|| (net.clone(), report.clone()));
    report.buffers_added += net.buffer(&model, opts);
    let mut sizing_options = opts.clone();
    if let Some(limit) = opts.area_limit {
        let area = net.area(&model);
        sizing_options.area_limit = Some(limit - (limit - area).max(0.0) * 0.10);
    }
    resize::run(
        &mut net,
        &model,
        &corner_models,
        &sizing_options,
        &mut report,
        ctx,
    )?;
    pins::run(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
    load_buffer::run(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
    repair_all(&mut net, &mut report, opts)?;
    mandatory(&mut net, &mut report)?;
    if let Some((saved, saved_report)) = before_legal {
        if !legal(&net) {
            net = saved;
            report = saved_report;
        }
    }
    let slack_after_sizing = timing::Timing::new(&net, &model, opts).worst_slack;
    let needs_refinement = slack_after_sizing >= -0.5
        && (opts.area_limit.is_some_and(|a| net.area(&model) > a + 1e-8)
            || slack_after_sizing < 0.0);
    if needs_refinement {
        // Buffer insertion can expose phase absorptions that were absent in the compact cover.
        let saved = (net.clone(), report.clone());
        let old_area = net.area(&model);
        let old_slack = timing::Timing::new(&net, &model, opts).worst_slack;
        let removed = polarity::fold(&mut net, &model);
        if removed > 0 {
            report.inverters_absorbed += removed;
            resize::run(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
            repair_all(&mut net, &mut report, opts)?;
            mandatory(&mut net, &mut report)?;
            let area = net.area(&model);
            let slack = timing::Timing::new(&net, &model, opts).worst_slack;
            let old_met = opts.area_limit.is_none_or(|limit| old_area <= limit + 1e-8);
            let new_met = opts.area_limit.is_none_or(|limit| area <= limit + 1e-8);
            let better = if !old_met {
                area < old_area - 1e-8 && slack >= old_slack.min(0.0) - 1e-8
            } else {
                new_met
                    && (slack > old_slack + 1e-8
                        || (slack >= old_slack - 1e-8 && area < old_area - 1e-8))
            };
            if !better || !legal(&net) {
                (net, report) = saved;
            }
        }
        for _ in 0..16 {
            let before = net.area(&model);
            let before_slack = timing::Timing::new(&net, &model, opts).worst_slack;
            fanout::run(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
            if opts.area_limit.is_some_and(|a| net.area(&model) > a + 1e-8) {
                fanout::run_phases(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
            }
            let slack = timing::Timing::new(&net, &model, opts).worst_slack;
            let area_met = opts.area_limit.is_none_or(|a| net.area(&model) <= a + 1e-8);
            if (area_met && slack >= 0.0)
                || (net.area(&model) >= before - 1e-8 && slack <= before_slack + 1e-8)
            {
                break;
            }
        }
        resize::run(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
        pins::run(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
        load_buffer::run(&mut net, &model, &corner_models, opts, &mut report, ctx)?;
        repair_all(&mut net, &mut report, opts)?;
        mandatory(&mut net, &mut report)?;
        if opts.area_limit.is_some_and(|a| net.area(&model) > a + 1e-8) {
            let saved = (net.clone(), report.clone());
            let mut best_area = net.area(&model);
            let old_slack = timing::Timing::new(&net, &model, opts).worst_slack;
            if let Some((mut cnet, mut creport)) = compact {
                if repack::recover(&mut cnet, &model, &mut creport, ctx)? > 0 {
                    creport.area_after_compaction = cnet.area(&model);
                    replicate::run(&mut cnet, &model, &corner_models, opts, &mut creport, ctx)?;
                    repair_all(&mut cnet, &mut creport, opts)?;
                    mandatory(&mut cnet, &mut creport)?;
                    creport.buffers_added += cnet.buffer(&model, opts);
                    let mut c_sizing = opts.clone();
                    if let Some(limit) = opts.area_limit {
                        let area = cnet.area(&model);
                        c_sizing.area_limit = Some(limit - (limit - area).max(0.0) * 0.10);
                    }
                    resize::run(
                        &mut cnet,
                        &model,
                        &corner_models,
                        &c_sizing,
                        &mut creport,
                        ctx,
                    )?;
                    pins::run(&mut cnet, &model, &corner_models, opts, &mut creport, ctx)?;
                    load_buffer::run(&mut cnet, &model, &corner_models, opts, &mut creport, ctx)?;
                    repair_all(&mut cnet, &mut creport, opts)?;
                    mandatory(&mut cnet, &mut creport)?;
                    if opts
                        .area_limit
                        .is_some_and(|a| cnet.area(&model) > a + 1e-8)
                    {
                        for _ in 0..8 {
                            let before = cnet.area(&model);
                            fanout::run(
                                &mut cnet,
                                &model,
                                &corner_models,
                                opts,
                                &mut creport,
                                ctx,
                            )?;
                            if opts
                                .area_limit
                                .is_some_and(|a| cnet.area(&model) > a + 1e-8)
                            {
                                fanout::run_phases(
                                    &mut cnet,
                                    &model,
                                    &corner_models,
                                    opts,
                                    &mut creport,
                                    ctx,
                                )?;
                            }
                            if opts
                                .area_limit
                                .is_none_or(|a| cnet.area(&model) <= a + 1e-8)
                                || cnet.area(&model) >= before - 1e-8
                            {
                                break;
                            }
                        }
                        resize::run(&mut cnet, &model, &corner_models, opts, &mut creport, ctx)?;
                        repair_all(&mut cnet, &mut creport, opts)?;
                        mandatory(&mut cnet, &mut creport)?;
                    }
                    repack::clone_oversized(&mut cnet, &model, &corner_models, opts, &mut creport)?;
                    let c_slack = timing::Timing::new(&cnet, &model, opts).worst_slack;
                    let c_area = cnet.area(&model);
                    if c_area < best_area - 1e-8
                        && c_slack >= old_slack.min(0.0) - 1e-8
                        && legal(&cnet)
                    {
                        best_area = c_area;
                        (net, report) = (cnet, creport);
                    }
                }
            }
            if opts.area_limit.is_some_and(|a| net.area(&model) > a + 1e-8) {
                let (mut bnet, mut breport) = saved;
                let changed = repack::recover(&mut bnet, &model, &mut breport, ctx)?;
                if changed > 0 {
                    replicate::run(&mut bnet, &model, &corner_models, opts, &mut breport, ctx)?;
                    repair_all(&mut bnet, &mut breport, opts)?;
                    mandatory(&mut bnet, &mut breport)?;
                    resize::run(&mut bnet, &model, &corner_models, opts, &mut breport, ctx)?;
                    repair_all(&mut bnet, &mut breport, opts)?;
                    mandatory(&mut bnet, &mut breport)?;
                }
                let cloned =
                    repack::clone_oversized(&mut bnet, &model, &corner_models, opts, &mut breport)?;
                if changed + cloned > 0 {
                    let b_slack = timing::Timing::new(&bnet, &model, opts).worst_slack;
                    let b_area = bnet.area(&model);
                    if b_area < best_area - 1e-8
                        && b_slack >= old_slack.min(0.0) - 1e-8
                        && legal(&bnet)
                    {
                        (net, report) = (bnet, breport);
                    }
                }
            }
        }
    }
    let timing = timing::Timing::new(&net, &model, opts);
    report.estimated_slack_ns = timing.worst_slack;
    report.estimated_timing_met = timing.worst_slack >= -1e-8;
    report.estimated_violating_endpoints = timing.endpoints;
    report.electrical_rules = rules::report(&net, &model, &timing);
    report.corner_rules = corners
        .iter()
        .zip(&corner_models)
        .map(|(raw, m)| {
            (
                raw.name.clone(),
                rules::report(&net, m, &timing::Timing::rules(&net, m, opts)),
            )
        })
        .collect();
    report.estimated_data_rules_met = report.electrical_rules.capacitance_nets == 0
        && report.electrical_rules.transition_nets == 0
        && report.electrical_rules.fanout_nets == 0;
    report.estimated_data_rules_met &= report
        .corner_rules
        .iter()
        .all(|(_, r)| r.capacitance_nets == 0 && r.transition_nets == 0 && r.fanout_nets == 0);
    let mut depth = vec![0u32; net.nodes.len()];
    for &g in &net.order {
        let gate = &net.gates[g];
        let d = 1 + gate.inputs.iter().map(|&n| depth[n]).max().unwrap_or(0);
        for &(_, n) in &gate.outputs {
            depth[n] = d;
        }
        report.logic_depth = report.logic_depth.max(d);
    }
    net.write(netlist, &model);
    report.area_after = net.area(&model);
    report.area_limit_met = opts
        .area_limit
        .is_none_or(|a| report.area_after <= a + 1e-8);
    Ok(report)
}
