// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Isolate a critical capacitive load even when its fanout is below the tree bound.

use super::{
    Options, Report,
    model::Model,
    network::Network,
    rules,
    timing::{ABSENT, Timing, Value},
};
use rusteda_core::job::{Ctx, JobError};

pub(super) fn driver(
    net: &Network,
    lib: &Model<'_>,
    timing: &Timing,
    n: usize,
    load: f64,
    opts: &Options,
) -> Value {
    let (g, op) = net.nodes[n].driver.unwrap();
    let gate = &net.gates[g];
    let c = &lib.cells[gate.cell];
    if !c.comb {
        return Timing::launch(lib, gate.cell, op, load, opts.clock_slew_ns);
    }
    let mut value = Value::default();
    for (&pin, &input) in c.inputs.iter().zip(&gate.inputs) {
        for (from, to, d, s) in c.arc(pin, op, timing.value[input].slew, load) {
            if d >= 0.0 && timing.value[input].arrival[from] > ABSENT / 2.0 {
                value.arrival[to] = value.arrival[to].max(timing.value[input].arrival[from] + d);
                value.slew[to] = value.slew[to].max(s);
            }
        }
    }
    value
}

pub(super) fn through(lib: &Model<'_>, buffer: usize, input: Value, load: f64) -> Value {
    let c = &lib.cells[buffer];
    let mut value = Value::default();
    for (from, to, d, s) in c.arc(c.inputs[0], c.outputs[0], input.slew, load) {
        if d >= 0.0 && input.arrival[from] > ABSENT / 2.0 {
            value.arrival[to] = value.arrival[to].max(input.arrival[from] + d);
            value.slew[to] = value.slew[to].max(s);
        }
    }
    value
}

pub(super) fn run(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
) -> Result<(), JobError<String>> {
    let Some(&cheapest) = lib.buffers.first() else {
        return Ok(());
    };
    for _ in 0..4 {
        ctx.checkpoint()?;
        let area = net.area(lib);
        if opts
            .area_limit
            .is_some_and(|a| area + lib.cells[cheapest].raw.area > a + 1e-8)
        {
            break;
        }
        let views: Vec<_> = std::iter::once(lib)
            .chain(corners)
            .enumerate()
            .map(|(i, m)| {
                let t = if i == 0 {
                    Timing::new(net, m, opts)
                } else {
                    Timing::rules(net, m, opts)
                };
                let rules = rules::report(net, m, &t);
                (m, t, rules)
            })
            .collect();
        let t = &views[0].1;
        if t.worst_slack >= 0.0 {
            break;
        }
        let mut proposals = Vec::new();
        for (n, node) in net.nodes.iter().enumerate() {
            if node.primary
                || node.protected
                || node.sinks.is_empty()
                || node.sinks.len() > opts.buffer_fanout
                || t.slack(n) > opts.period_ns * 0.15
            {
                continue;
            }
            let Some((g, op)) = node.driver else {
                continue;
            };
            let old = &lib.cells[net.gates[g].cell];
            if !old.comb && old.clock.is_none() {
                continue;
            }
            let ports = opts.output_load_pf * node.outputs as f64;
            let load = (t.load[n] - ports).max(0.0);
            let mut best = None;
            for &buffer in &lib.buffers {
                let b = &lib.cells[buffer];
                if opts
                    .area_limit
                    .is_some_and(|a| area + b.raw.area > a + 1e-8)
                {
                    continue;
                }
                let source = driver(net, lib, t, n, ports + b.input_cap(b.inputs[0]), opts);
                let value = through(lib, buffer, source, load);
                let slack = (0..2)
                    .map(|p| t.required[n][p] - value.arrival[p])
                    .fold(f64::INFINITY, f64::min);
                let slew = (value.slew[0] - t.value[n].slew[0])
                    .max(value.slew[1] - t.value[n].slew[1])
                    .max(0.0);
                let gain = slack - t.slack(n) - 0.5 * slew;
                if gain < 0.002 {
                    continue;
                }
                let score = gain / b.raw.area.max(1e-6);
                if best.is_some_and(|(v, _)| score <= v) {
                    continue;
                }
                let legal = views.iter().all(|(m, t, _)| {
                    let b = &m.cells[buffer];
                    let old = &m.cells[net.gates[g].cell];
                    let limit = rules::Limits::for_node(net, m, n);
                    let root_load = ports + b.input_cap(b.inputs[0]);
                    let load = (t.load[n] - ports).max(0.0);
                    if root_load > old.max_cap(op) + 1e-12
                        || b.input_fanout(b.inputs[0]) > old.max_fanout(op) + 1e-8
                        || load > b.max_cap(b.outputs[0]) + 1e-12
                        || limit.fanout_load > b.max_fanout(b.outputs[0]) + 1e-8
                    {
                        return false;
                    }
                    let source = driver(net, m, t, n, root_load, opts);
                    let value = through(m, buffer, source, load);
                    source.slew[0].max(source.slew[1])
                        <= old.max_slew(op).min(b.max_slew(b.inputs[0])) + 1e-10
                        && value.slew[0].max(value.slew[1])
                            <= b.max_slew(b.outputs[0]).min(limit.slew) + 1e-10
                });
                if legal {
                    best = Some((score, buffer));
                }
            }
            if let Some((score, buffer)) = best {
                proposals.push((score, n, buffer));
            }
        }
        if proposals.is_empty() {
            break;
        }
        proposals.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let saved = net.clone();
        let mut area = area;
        let mut added = 0;
        for (_, n, buffer) in proposals {
            let price = lib.cells[buffer].raw.area;
            if opts.area_limit.is_some_and(|a| area + price > a + 1e-8) {
                continue;
            }
            added += net.isolate_load(n, buffer, lib);
            area += price;
        }
        net.rebuild(lib).map_err(JobError::Failed)?;
        let after = Timing::new(net, lib, opts);
        let safe = views.iter().all(|(m, _, before)| {
            let after = rules::report(net, m, &Timing::rules(net, m, opts));
            after.capacitance_nets <= before.capacitance_nets
                && after.transition_nets <= before.transition_nets
                && after.fanout_nets <= before.fanout_nets
        });
        if added == 0 || !safe || after.worst_slack <= t.worst_slack + 1e-6 {
            *net = saved;
            break;
        }
        report.buffers_added += added;
        report.load_buffers += added;
    }
    Ok(())
}
