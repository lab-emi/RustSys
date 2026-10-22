// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use super::model::Model;
use super::network::Network;
use super::timing::Timing;
use super::{Options, Report};
use rusteda_core::job::{Ctx, Event, JobError};

struct CornerTiming<'m, 'l> {
    lib: &'m Model<'l>,
    timing: Timing,
    limits: Vec<super::rules::Limits>,
    loads: Vec<f64>,
    fanouts: Vec<f64>,
}

fn input_changes(
    net: &Network,
    lib: &Model<'_>,
    g: usize,
    cell: usize,
) -> (usize, [(usize, f64, f64); 6]) {
    let old = &lib.cells[net.gates[g].cell];
    let new = &lib.cells[cell];
    let mut changes = [(0usize, 0.0_f64, 0.0_f64); 6];
    let mut len = 0usize;
    for (i, &n) in net.gates[g].inputs.iter().enumerate() {
        let dc = new.input_cap(new.inputs[i]) - old.input_cap(old.inputs[i]);
        let df = new.input_fanout(new.inputs[i]) - old.input_fanout(old.inputs[i]);
        if let Some(x) = changes[..len].iter_mut().find(|x| x.0 == n) {
            x.1 += dc;
            x.2 += df;
        } else {
            changes[len] = (n, dc, df);
            len += 1;
        }
    }
    (len, changes)
}

fn output_pin(lib: &Model<'_>, old: usize, new: usize, pin: usize) -> usize {
    if lib.cells[new].comb {
        lib.cells[new].outputs[0]
    } else {
        let name = &lib.cells[old].raw.pins[pin].name;
        lib.cells[new]
            .raw
            .pins
            .iter()
            .position(|p| &p.name == name)
            .expect("compatible state pin")
    }
}

fn legal_change(
    net: &Network,
    views: &[CornerTiming<'_, '_>],
    g: usize,
    cell: usize,
    opts: &Options,
) -> bool {
    views.iter().all(|v| {
        let new = &v.lib.cells[cell];
        let old = &v.lib.cells[net.gates[g].cell];
        let scale = |value: f64, limit: f64| {
            if limit.is_infinite() {
                1.0
            } else {
                (value / limit.max(1e-30)).max(1.0)
            }
        };
        for &(oldop, out) in &net.gates[g].outputs {
            let op = output_pin(v.lib, net.gates[g].cell, cell, oldop);
            if v.loads[out] > new.max_cap(op) * scale(v.loads[out], old.max_cap(oldop)) + 1e-12
                || v.fanouts[out]
                    > new.max_fanout(op) * scale(v.fanouts[out], old.max_fanout(oldop)) + 1e-8
            {
                return false;
            }
            let slew =
                v.timing
                    .slew_at_load(net, v.lib, (g, cell, op), v.loads[out], opts.clock_slew_ns);
            if slew[0].max(slew[1])
                > new.max_slew(op).min(v.limits[out].slew)
                    * scale(
                        v.timing.value[out].slew[0].max(v.timing.value[out].slew[1]),
                        v.limits[out].slew,
                    )
                    + 1e-10
            {
                return false;
            }
        }
        let (clen, changes) = input_changes(net, v.lib, g, cell);
        for &(n, dc, df) in &changes[..clen] {
            if net.nodes[n].protected {
                continue;
            }
            let Some((parent, pin)) = net.nodes[n].driver else {
                continue;
            };
            let pc = &v.lib.cells[net.gates[parent].cell];
            if !pc.comb && pc.clock.is_none() {
                continue;
            }
            if v.loads[n] + dc > pc.max_cap(pin).max(v.loads[n]) + 1e-12
                || v.fanouts[n] + df > pc.max_fanout(pin).max(v.fanouts[n]) + 1e-8
            {
                return false;
            }
            let slew = v.timing.slew_at_load(
                net,
                v.lib,
                (parent, net.gates[parent].cell, pin),
                v.loads[n] + dc,
                opts.clock_slew_ns,
            );
            let bound = net.gates[g]
                .inputs
                .iter()
                .enumerate()
                .filter(|(_, a)| **a == n)
                .fold(v.limits[n].slew, |a, (i, _)| {
                    a.min(new.max_slew(new.inputs[i]))
                });
            if slew[0].max(slew[1])
                > bound
                    * scale(
                        v.timing.value[n].slew[0].max(v.timing.value[n].slew[1]),
                        v.limits[n].slew,
                    )
                    + 1e-10
            {
                return false;
            }
        }
        true
    })
}

fn starting_slack(net: &Network, lib: &Model<'_>, t: &Timing, g: usize, opts: &Options) -> f64 {
    let gate = &net.gates[g];
    let c = &lib.cells[gate.cell];
    let mut slack = gate
        .outputs
        .iter()
        .map(|&(_, n)| t.slack(n))
        .fold(f64::INFINITY, f64::min);
    if !c.comb {
        for &d in &c.data {
            let i = c.inputs.iter().position(|p| *p == d).unwrap();
            let n = gate.inputs[i];
            let setup = c.setup(d, t.value[n].slew, opts.clock_slew_ns);
            for (p, value) in setup.iter().enumerate() {
                slack = slack.min(opts.period_ns - opts.margin_ns - value - t.value[n].arrival[p]);
            }
        }
    }
    slack
}

/// Local setup impact includes both launch clock-to-Q and capture setup for a state cell.
fn impact(
    net: &Network,
    lib: &Model<'_>,
    t: &Timing,
    g: usize,
    cell: usize,
    opts: &Options,
) -> (f64, f64, f64) {
    let gate = &net.gates[g];
    let old = &lib.cells[gate.cell];
    let new = &lib.cells[cell];
    if old.comb {
        let n = gate.outputs[0].1;
        let a = t.value[n];
        let b = t.candidate(net, lib, g, cell, true);
        return (
            t.slack(n),
            (b.arrival[0] - a.arrival[0]).max(b.arrival[1] - a.arrival[1]),
            (b.slew[0] - a.slew[0]).max(b.slew[1] - a.slew[1]),
        );
    }
    let mut before = f64::INFINITY;
    let mut after = f64::INFINITY;
    let mut slew = 0.0_f64;
    for &(pin, n) in &gate.outputs {
        let v = Timing::launch(
            lib,
            cell,
            output_pin(lib, gate.cell, cell, pin),
            t.load[n],
            opts.clock_slew_ns,
        );
        let delta =
            (v.arrival[0] - t.value[n].arrival[0]).max(v.arrival[1] - t.value[n].arrival[1]);
        before = before.min(t.slack(n));
        after = after.min(t.slack(n) - delta);
        if t.slack(n) < opts.period_ns * 0.15 {
            slew = slew.max((v.slew[0] - t.value[n].slew[0]).max(v.slew[1] - t.value[n].slew[1]));
        }
    }
    for (&od, &nd) in old.data.iter().zip(&new.data) {
        let i = old.inputs.iter().position(|p| *p == od).unwrap();
        let n = gate.inputs[i];
        let os = old.setup(od, t.value[n].slew, opts.clock_slew_ns);
        let ns = new.setup(nd, t.value[n].slew, opts.clock_slew_ns);
        let delay = t.slope[n] * (new.input_cap(nd) - old.input_cap(od));
        for p in 0..2 {
            let a = opts.period_ns - opts.margin_ns - os[p] - t.value[n].arrival[p];
            before = before.min(a);
            after = after.min(a - delay - (ns[p] - os[p]));
        }
    }
    (before, before - after, slew)
}

pub(super) fn run(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
) -> Result<(), JobError<String>> {
    let iterations_before = report.iterations;
    let start: Vec<_> = net.gates.iter().map(|g| g.cell).collect();
    let mut best = start.clone();
    let mut best_score = f64::INFINITY;
    let mut best_area = f64::INFINITY;
    let candidates: Vec<_> = net
        .order
        .iter()
        .copied()
        .chain(
            net.gates
                .iter()
                .enumerate()
                .filter(|(_, g)| {
                    g.active && !g.outputs.is_empty() && lib.cells[g.cell].clock.is_some()
                })
                .map(|(i, _)| i),
        )
        .collect();
    let mut seen: Vec<(Vec<usize>, f64)> = Vec::with_capacity(32);
    for round in 0..32 {
        ctx.checkpoint()?;
        let current: Vec<_> = net.gates.iter().map(|g| g.cell).collect();
        if let Some(r0) = seen.iter().position(|(s, _)| *s == current) {
            let period = round - r0;
            for k in round..32 {
                let slack = seen[r0 + (k - r0) % period].1;
                ctx.emit(|| Event::Metric {
                    name: "electrical_slack_ns",
                    value: slack,
                });
            }
            let final_state = &seen[r0 + (32 - r0) % period].0;
            for (g, &cell) in final_state.iter().enumerate() {
                if net.gates[g].active && net.gates[g].cell != cell {
                    net.change_cell(g, cell, lib);
                }
            }
            report.iterations = iterations_before + 32;
            break;
        }
        let mut views: Vec<_> = std::iter::once(lib)
            .chain(corners)
            .enumerate()
            .map(|(i, m)| {
                let timing = if i == 0 {
                    Timing::new(net, m, opts)
                } else {
                    Timing::rules(net, m, opts)
                };
                let limits: Vec<_> = (0..net.nodes.len())
                    .map(|n| super::rules::Limits::for_node(net, m, n))
                    .collect();
                CornerTiming {
                    lib: m,
                    loads: timing.load.clone(),
                    fanouts: limits.iter().map(|l| l.fanout_load).collect(),
                    timing,
                    limits,
                }
            })
            .collect();
        let t = &views[0].timing;
        let area = net.area(lib);
        let score = (-t.worst_slack).max(0.0) + t.violation * 1e-5;
        if score < best_score - 1e-8 || (score <= best_score + 1e-8 && area < best_area) {
            best_score = score;
            best_area = area;
            best.clone_from(&current);
        }
        ctx.emit(|| Event::Metric {
            name: "electrical_slack_ns",
            value: t.worst_slack,
        });
        seen.push((current, t.worst_slack));
        let mut proposals = Vec::new();
        for &g in &candidates {
            let gate = &net.gates[g];
            let c = &lib.cells[gate.cell];
            if gate.outputs.iter().any(|(_, n)| net.nodes[*n].protected) {
                continue;
            }
            let min_area = lib.cells[lib.families[c.family][0]].raw.area;
            if t.worst_slack >= 0.0 && c.raw.area <= min_area + 1e-8 {
                continue;
            }
            let slack = starting_slack(net, lib, t, g, opts);
            if slack >= opts.period_ns * 0.15 && c.raw.area <= min_area + 1e-8 {
                continue;
            }
            let mut selected = None;
            for &candidate in &lib.families[c.family] {
                if candidate == gate.cell {
                    continue;
                }
                let nc = &lib.cells[candidate];
                // A non-cheaper candidate can only win for a critical gate while setup
                // is unmet. The starting slack is independent of its replacement cell.
                if nc.raw.area - c.raw.area >= -1e-8
                    && (t.worst_slack >= 0.0 || slack >= opts.period_ns * 0.15)
                {
                    continue;
                }
                let (slack, delta, slew_delta) = impact(net, lib, t, g, candidate, opts);
                let area_delta = nc.raw.area - c.raw.area;
                let gain = -delta - slew_delta.max(0.0) * 0.5;
                let key =
                    if area_delta < -1e-8 && delta + slew_delta.max(0.0) < slack.max(0.0) * 0.08 {
                        1e6 + (-area_delta)
                    } else if t.worst_slack < 0.0 && gain > 1e-4 && slack < opts.period_ns * 0.15 {
                        let critical = (1.0 - slack / opts.period_ns).clamp(0.1, 10.0);
                        critical * gain / (area_delta.max(0.0) + 0.1 * c.raw.area.max(1e-3))
                    } else {
                        continue;
                    };
                if selected.is_some_and(|(k, _, _)| key <= k) {
                    continue;
                }
                if !legal_change(net, &views, g, candidate, opts) {
                    continue;
                }
                selected = Some((key, candidate, area_delta));
            }
            if let Some((score, cell, da)) = selected {
                proposals.push((score, g, cell, da));
            }
        }
        proposals.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut area = area;
        let mut changed = 0;
        for (_, g, cell, da) in proposals {
            if da > 0.0 && opts.area_limit.is_some_and(|lim| area + da > lim) {
                continue;
            }
            if !legal_change(net, &views, g, cell, opts) {
                continue;
            }
            for v in &mut views {
                let (clen, changes) = input_changes(net, v.lib, g, cell);
                for &(n, dc, df) in &changes[..clen] {
                    v.loads[n] += dc;
                    v.fanouts[n] += df;
                }
            }
            net.change_cell(g, cell, lib);
            area += da;
            changed += 1;
        }
        report.iterations = iterations_before + round + 1;
        if changed == 0 {
            break;
        }
    }
    let final_t = Timing::new(net, lib, opts);
    let score = (-final_t.worst_slack).max(0.0) + final_t.violation * 1e-5;
    if score > best_score + 1e-8 || (score >= best_score - 1e-8 && net.area(lib) > best_area) {
        for (g, cell) in best.into_iter().enumerate() {
            if net.gates[g].active && net.gates[g].cell != cell {
                net.change_cell(g, cell, lib);
            }
        }
    }
    recover_area(net, lib, corners, opts, ctx)?;
    let t = Timing::new(net, lib, opts);
    report.resized_cells += net
        .gates
        .iter()
        .zip(start)
        .filter(|(g, c)| g.active && g.cell != *c)
        .count();
    report.estimated_slack_ns = t.worst_slack;
    report.estimated_violating_endpoints = t.endpoints;
    report.estimated_timing_met = t.worst_slack >= -1e-8;
    Ok(())
}

/// Spend slack only after proving each replacement against fresh whole-network timing.
/// Batch sizing deliberately uses a small fraction of slack; this bounded cleanup avoids
/// accumulating stale local estimates while recovering a missed area budget.
fn recover_area(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    ctx: Ctx<'_>,
) -> Result<(), JobError<String>> {
    let Some(limit) = opts.area_limit else {
        return Ok(());
    };
    let mut rejected = std::collections::HashSet::new();
    for _ in 0..512 {
        if net.area(lib) <= limit + 1e-8 {
            break;
        }
        ctx.checkpoint()?;
        let views: Vec<_> = std::iter::once(lib)
            .chain(corners)
            .enumerate()
            .map(|(i, m)| {
                let timing = if i == 0 {
                    Timing::new(net, m, opts)
                } else {
                    Timing::rules(net, m, opts)
                };
                let limits: Vec<_> = (0..net.nodes.len())
                    .map(|n| super::rules::Limits::for_node(net, m, n))
                    .collect();
                CornerTiming {
                    lib: m,
                    loads: timing.load.clone(),
                    fanouts: limits.iter().map(|l| l.fanout_load).collect(),
                    timing,
                    limits,
                }
            })
            .collect();
        let timing = &views[0].timing;
        if timing.worst_slack < 0.0 {
            break;
        }
        let mut candidates = Vec::new();
        for (g, gate) in net.gates.iter().enumerate() {
            if !gate.active
                || gate.outputs.is_empty()
                || gate.outputs.iter().any(|(_, n)| net.nodes[*n].protected)
            {
                continue;
            }
            let c = &lib.cells[gate.cell];
            if c.family >= lib.families.len() || (!c.comb && c.clock.is_none()) {
                continue;
            }
            for &cell in &lib.families[c.family] {
                let saving = c.raw.area - lib.cells[cell].raw.area;
                if saving <= 1e-8 || rejected.contains(&(g, gate.cell, cell)) {
                    continue;
                }
                let (slack, delta, _) = impact(net, lib, timing, g, cell, opts);
                if delta > slack || !legal_change(net, &views, g, cell, opts) {
                    continue;
                }
                candidates.push((saving / delta.max(0.001), g, cell));
            }
        }
        candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let Some((_, g, cell)) = candidates.first().copied() else {
            break;
        };
        let old = net.gates[g].cell;
        net.change_cell(g, cell, lib);
        let t = Timing::new(net, lib, opts);
        let legal = std::iter::once(lib).chain(corners).all(|m| {
            let r = super::rules::report(net, m, &Timing::rules(net, m, opts));
            r.capacitance_nets == 0 && r.transition_nets == 0 && r.fanout_nets == 0
        });
        if t.worst_slack < 0.0 || !legal {
            net.change_cell(g, old, lib);
            rejected.insert((g, old, cell));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
