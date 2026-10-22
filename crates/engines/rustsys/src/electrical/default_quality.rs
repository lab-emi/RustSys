// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Area-capped load-aware cleanup for unconstrained mapping. The input cover is the hard area
//! ceiling; constraints are estimates for ranking only, never an implicit timing specification.
//! A bounded attempt either improves the cover or leaves the original netlist untouched.

use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::{
    liberty::Library,
    verilog::write::{NlModule, NlRef},
};
use serde::{Deserialize, Serialize};

use super::{
    Options, Report, collapse, model::Model, network::Network, pins, polarity, repair, resize,
    state_phase, timing::Timing,
};
use crate::map::library::CellLib;

/// Work/storage budgets are independent of design names and thread count. Network, two timing
/// views and their pin/state tables need <256 MiB at these limits, plus a shared PDK model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_gates: usize,
    pub max_pins: usize,
    pub max_name_bytes: usize,
    pub max_remap_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_gates: 65_536,
            max_pins: 524_288,
            max_name_bytes: 16 * 1024 * 1024,
            max_remap_nodes: 65_536,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityReport {
    pub status: String,
    pub limits: Limits,
    pub corner: Vec<String>,
    pub wire_model: String,
    pub unconstrained_endpoints: String,
    pub area_before: Option<f64>,
    pub area_after: Option<f64>,
    pub estimated_path_before_ns: Option<f64>,
    pub estimated_path_after_ns: Option<f64>,
    pub logic_depth_after: Option<u32>,
    pub changes: Option<Report>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternative: Option<Box<QualityReport>>,
}

pub fn run(
    netlist: &mut NlModule,
    libs: &[Library],
    eligible: &CellLib,
    aliases: &[NlRef],
    area_ceiling: Option<f64>,
    limits: Limits,
    ctx: Ctx<'_>,
) -> Result<QualityReport, JobError<String>> {
    let mut report = QualityReport {
        status: "budget_reached".into(),
        limits,
        corner: libs.iter().map(|l| l.name.clone()).collect(),
        wire_model: "none".into(),
        unconstrained_endpoints:
            "all; no timing constraints supplied; automatic load-aware ranking only".into(),
        area_before: None,
        area_after: None,
        estimated_path_before_ns: None,
        estimated_path_after_ns: None,
        logic_depth_after: None,
        changes: None,
        alternative: None,
    };
    if netlist.instances.len() > limits.max_gates {
        return Ok(report);
    }
    let mut pins = netlist.assigns.len().saturating_mul(2);
    let mut names = 0usize;
    let name_bytes = |r: &NlRef| match r {
        NlRef::Net(n) | NlRef::Bit(n, _) => n.len(),
        NlRef::Const(_) => 0,
    };
    for port in &netlist.ports {
        let width = port.width.unwrap_or(1) as usize;
        pins = pins.saturating_add(width);
        names = names.saturating_add(width.saturating_mul(port.name.len() + 16));
    }
    for (a, b) in &netlist.assigns {
        names = names
            .saturating_add(name_bytes(a))
            .saturating_add(name_bytes(b));
    }
    for g in &netlist.instances {
        pins = pins.saturating_add(g.conns.len());
        names = names.saturating_add(g.cell.len() + g.name.len());
        for (pin, r) in &g.conns {
            names = names
                .saturating_add(pin.len())
                .saturating_add(name_bytes(r));
        }
    }
    if netlist.instances.len() > limits.max_gates
        || pins > limits.max_pins
        || names > limits.max_name_bytes
    {
        return Ok(report);
    }
    ctx.checkpoint()?;
    let model = Model::new(libs, eligible);
    let mut net =
        Network::with_state_aliases(netlist, &model, Some(aliases)).map_err(JobError::Failed)?;
    report.logic_depth_after = Some(depth(&net));
    let area = net.area(&model);
    // An alternative cover may spend the savings relative to the original cover, while the
    // caller still enforces that original ceiling when choosing the final implementation.
    let ceiling = area_ceiling.unwrap_or(area);
    let mut opts = Options {
        period_ns: 1000.0,
        margin_ns: 0.0,
        area_limit: Some(ceiling),
        ..Options::default()
    };
    let initial = Timing::new(&net, &model, &opts);
    let delay = opts.period_ns - initial.worst_slack;
    report.area_before = Some(area);
    report.area_after = Some(area);
    if !delay.is_finite() || delay <= 0.0 {
        report.status = "no_timed_paths".into();
        return Ok(report);
    }
    report.estimated_path_before_ns = Some(delay);
    report.estimated_path_after_ns = Some(delay);
    // Set an internal search target from the actual baseline load; it is not a user constraint.
    opts.period_ns = (delay * 0.25).max(0.001);
    drop(initial);
    let mut changes = Report {
        area_before: area,
        ..Report::default()
    };
    changes.state_inverters_absorbed = state_phase::fold(&mut net, &model);
    (changes.gates_collapsed, changes.two_cell_rewrites) = collapse::run(&mut net, &model, ctx)?;
    changes.inverters_absorbed = polarity::fold(&mut net, &model);
    changes.area_after_compaction = net.area(&model);
    repair::run_modes(&mut net, &model, &opts, &mut changes, ctx, &[false])?;
    resize::run(&mut net, &model, &[], &opts, &mut changes, ctx)?;
    pins::run(&mut net, &model, &[], &opts, &mut changes, ctx)?;
    let timing = Timing::new(&net, &model, &opts);
    let after = opts.period_ns - timing.worst_slack;
    changes.area_after = net.area(&model);
    changes.logic_depth = depth(&net);
    changes.area_limit_met = changes.area_after <= ceiling + 1e-8;
    changes.electrical_rules = super::rules::report(&net, &model, &timing);
    changes.estimated_slack_ns = timing.worst_slack;
    changes.estimated_timing_met = timing.worst_slack >= 0.0;
    changes.estimated_violating_endpoints = timing.endpoints;
    changes.estimated_data_rules_met = changes.electrical_rules.capacitance_nets == 0
        && changes.electrical_rules.transition_nets == 0
        && changes.electrical_rules.fanout_nets == 0;
    if changes.area_limit_met && after.is_finite() && after < delay - 1e-8 {
        net.write(netlist, &model);
        report.status = "improved".into();
        report.area_after = Some(changes.area_after);
        report.estimated_path_after_ns = Some(after);
        report.logic_depth_after = Some(changes.logic_depth);
    } else {
        report.status = "unchanged".into();
    }
    report.changes = Some(changes);
    Ok(report)
}

/// Linear topological DP; no path enumeration. One u32 per bounded network node.
fn depth(net: &Network) -> u32 {
    let mut levels = vec![0u32; net.nodes.len()];
    let mut maximum = 0;
    for &g in &net.order {
        let gate = &net.gates[g];
        let level = 1 + gate.inputs.iter().map(|&n| levels[n]).max().unwrap_or(0);
        for &(_, n) in &gate.outputs {
            levels[n] = level;
        }
        maximum = maximum.max(level);
    }
    maximum
}
