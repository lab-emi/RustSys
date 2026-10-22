// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Repair electrical violations without requiring an endpoint to be setup-critical.

use super::{Options, Report, model::Model, network::Network, rules::Limits, timing::Timing};
use rusteda_core::job::{Ctx, JobError};

use super::network::tree_size;

#[cfg(test)]
pub(super) fn run(
    net: &mut Network,
    lib: &Model<'_>,
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
) -> Result<(), JobError<String>> {
    run_modes(net, lib, opts, report, ctx, &[false, true])
}

pub(super) fn run_modes(
    net: &mut Network,
    lib: &Model<'_>,
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
    modes: &[bool],
) -> Result<(), JobError<String>> {
    // Real capacitance/transition violations precede the dimensionless fanout guideline.
    for &include_fanout in modes {
        for _ in 0..4 {
            ctx.checkpoint()?;
            let t = Timing::rules(net, lib, opts);
            let mut area = net.area(lib);
            let snapshot_inputs: Vec<_> = net.gates.iter().map(|g| g.inputs.clone()).collect();
            let mut candidates = Vec::new();
            for (n, node) in net.nodes.iter().enumerate() {
                // Clock nets are left to clock-tree construction (SYNTH-REQ-0552).
                if node.primary || node.clock {
                    continue;
                }
                let Some((g, _)) = node.driver else {
                    continue;
                };
                let c = &lib.cells[net.gates[g].cell];
                if !c.comb && c.clock.is_none() {
                    continue;
                }
                let l = Limits::for_node(net, lib, n);
                let bad = l.violations(&t, n);
                if bad[0] || bad[1] || (include_fanout && bad[2]) {
                    let ratio = |v: f64, l: f64| (v / l.max(1e-20) - 1.0).max(0.0);
                    let score = 3.0 * ratio(t.load[n], l.cap)
                        + 3.0 * ratio(t.value[n].slew[0].max(t.value[n].slew[1]), l.slew)
                        + if include_fanout {
                            ratio(l.fanout_load, l.fanout)
                        } else {
                            0.0
                        };
                    candidates.push((score, n));
                }
            }
            candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            let mut changed = 0;
            for (_, n) in candidates {
                let (g, op) = net.nodes[n].driver.expect("driver");
                let old = &lib.cells[net.gates[g].cell];
                let lim = Limits::for_node(net, lib, n);
                let available = opts
                    .area_limit
                    .map_or(f64::INFINITY, |a| (a - area).max(0.0));
                let mut sized = None;
                if old.comb {
                    for &cell in &lib.families[old.family] {
                        if cell == net.gates[g].cell {
                            continue;
                        }
                        let c = &lib.cells[cell];
                        let cost = c.raw.area - old.raw.area;
                        if cost > available + 1e-8 {
                            continue;
                        }
                        let p = c.outputs[0];
                        let v = t.candidate_inputs(net, lib, g, cell, true, &snapshot_inputs[g]);
                        if t.load[n] <= c.max_cap(p) + 1e-12
                            && v.slew[0].max(v.slew[1]) <= c.max_slew(p).min(lim.slew) + 1e-10
                            && (!include_fanout || lim.fanout_load <= c.max_fanout(p) + 1e-8)
                            && sized.is_none_or(|(price, _)| cost < price)
                        {
                            sized = Some((cost, cell));
                        }
                    }
                }
                let mut branch = None;
                for &buffer in &lib.buffers {
                    let b = &lib.cells[buffer];
                    let bp = b.outputs[0];
                    let sink_weight = net.nodes[n]
                        .sinks
                        .iter()
                        .map(|&(g, i)| {
                            let c = &lib.cells[net.gates[g].cell];
                            c.input_fanout(c.inputs[i])
                        })
                        .fold(b.input_fanout(b.inputs[0]), f64::max);
                    let max_cap = net.nodes[n]
                        .sinks
                        .iter()
                        .map(|&(g, i)| {
                            let c = &lib.cells[net.gates[g].cell];
                            c.input_cap(c.inputs[i])
                        })
                        .fold(b.input_cap(b.inputs[0]), f64::max);
                    let bound = (b.max_fanout(bp) / sink_weight.max(1e-20))
                        .floor()
                        .min(b.max_cap(bp) / max_cap.max(1e-20))
                        .min(opts.buffer_fanout as f64);
                    let mut width = bound as usize;
                    if width < 2 {
                        continue;
                    }
                    width = width.min(128);
                    // Respect the original driver's fanout bound too.
                    if old.max_fanout(op).is_finite() {
                        width = width
                            .min((old.max_fanout(op) / sink_weight.max(1e-20)).floor() as usize);
                    }
                    if width < 2 {
                        continue;
                    }
                    let count = tree_size(net.nodes[n].sinks.len(), width, true);
                    let cost = count as f64 * b.raw.area;
                    if count > 0
                        && cost <= available + 1e-8
                        && branch.is_none_or(|(price, _, _, _)| cost < price)
                    {
                        branch = Some((cost, buffer, width, count));
                    }
                }
                // No affordable change removes the violation: the affordable drive variant that
                // reduces it most, if by at least a tenth (SYNTH-REQ-0551).
                if sized.is_none() && branch.is_none() && old.comb {
                    let excess = |c: &super::model::CellModel<'_>, slew: f64| {
                        let p = c.outputs[0];
                        (t.load[n] / c.max_cap(p).max(1e-20))
                            .max(slew / c.max_slew(p).min(lim.slew).max(1e-20))
                    };
                    let now = excess(old, t.value[n].slew[0].max(t.value[n].slew[1]));
                    let mut best = (now * 0.9, None);
                    for &cell in &lib.families[old.family] {
                        let c = &lib.cells[cell];
                        let cost = c.raw.area - old.raw.area;
                        if cell == net.gates[g].cell || cost > available + 1e-8 {
                            continue;
                        }
                        let v = t.candidate_inputs(net, lib, g, cell, true, &snapshot_inputs[g]);
                        let e = excess(c, v.slew[0].max(v.slew[1]));
                        if e < best.0 {
                            best = (e, Some((cost, cell)));
                        }
                    }
                    sized = best.1;
                }
                if let Some((cost, cell)) =
                    sized.filter(|(cost, _)| branch.is_none_or(|(bc, _, _, _)| *cost <= bc))
                {
                    net.change_cell(g, cell, lib);
                    area += cost;
                    report.rule_resizes += 1;
                    changed += 1;
                } else if let Some((cost, buffer, width, count)) = branch {
                    let added = net.split_fanout(n, buffer, width, true, lib);
                    debug_assert_eq!(added, count);
                    area += cost;
                    report.buffers_added += added;
                    report.rule_buffers += added;
                    changed += 1;
                }
            }
            net.rebuild(lib).map_err(JobError::Failed)?;
            if changed == 0 {
                break;
            }
        }
    }
    Ok(())
}
