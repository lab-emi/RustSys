// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Replicate inexpensive combinational drivers when that costs less than buffering.

use super::{
    Options, Report,
    model::Model,
    network::{Gate, Network, Node, tree_size},
    rules::Limits,
    timing::Timing,
};
use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::verilog::write::NlRef;

pub(super) fn run(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
) -> Result<(), JobError<String>> {
    let Some(&buffer) = lib.buffers.first() else {
        return Ok(());
    };
    let price = lib.cells[buffer].raw.area;
    let mut views: Vec<_> = std::iter::once(lib)
        .chain(corners)
        .map(|m| {
            let t = Timing::rules(net, m, opts);
            let limits: Vec<_> = (0..net.nodes.len())
                .map(|n| Limits::for_node(net, m, n))
                .collect();
            (
                m,
                t.load.clone(),
                limits.iter().map(|l| l.fanout_load).collect::<Vec<_>>(),
                limits,
                t,
            )
        })
        .collect();
    let snapshot_inputs: Vec<_> = net.gates.iter().map(|g| g.inputs.clone()).collect();
    let mut area = net.area(lib);
    for (iteration, g) in net.order.clone().into_iter().enumerate() {
        if iteration % 256 == 0 {
            ctx.checkpoint()?;
        }
        let gate = &net.gates[g];
        let cell = gate.cell;
        let c = &lib.cells[cell];
        let (op, n) = gate.outputs[0];
        let node = &net.nodes[n];
        let count = node.sinks.len();
        if !gate.active
            || !c.comb
            || c.inputs.len() > 2
            || c.raw.area > price + 1e-8
            || node.protected
            || node.outputs > 0
            || count < 3
        {
            continue;
        }
        if views
            .iter()
            .all(|(_, _, _, limits, t)| !limits[n].violations(t, n).iter().any(|v| *v))
        {
            continue;
        }
        let original_inputs = gate.inputs.clone();
        // A preceding replication can create a new input; defer that gate until
        // fresh timing exists instead of indexing the original timing snapshot.
        if original_inputs.iter().any(|&n| n >= views[0].1.len()) {
            continue;
        }
        let mut selected = None;
        for width in (2..count.min(opts.buffer_fanout + 1)).rev() {
            let copies = count.div_ceil(width) - 1;
            let cost = copies as f64 * c.raw.area;
            let buffering = tree_size(count, width, true) as f64 * price;
            if cost > buffering + 1e-8 || opts.area_limit.is_some_and(|a| area + cost > a + 1e-8) {
                continue;
            }
            let legal = views.iter().all(|(m, loads, fanouts, limits, t)| {
                let c = &m.cells[cell];
                let max_load = node
                    .sinks
                    .iter()
                    .map(|&(s, i)| {
                        let cell = &m.cells[net.gates[s].cell];
                        cell.input_cap(cell.inputs[i])
                    })
                    .fold(0.0_f64, f64::max)
                    * width as f64;
                let max_fo = node
                    .sinks
                    .iter()
                    .map(|&(s, i)| {
                        let cell = &m.cells[net.gates[s].cell];
                        cell.input_fanout(cell.inputs[i])
                    })
                    .fold(0.0_f64, f64::max)
                    * width as f64;
                if max_load > limits[n].cap + 1e-12 || max_fo > limits[n].fanout + 1e-8 {
                    return false;
                }
                let slew = t.slew_at_load_inputs(
                    m,
                    cell,
                    op,
                    max_load,
                    opts.clock_slew_ns,
                    &snapshot_inputs[g],
                );
                if slew[0].max(slew[1]) > limits[n].slew + 1e-10 {
                    return false;
                }
                let mut changes: Vec<(usize, f64, f64)> = Vec::new();
                for (&input, &pin) in original_inputs.iter().zip(&c.inputs) {
                    let dc = copies as f64 * c.input_cap(pin);
                    let df = copies as f64 * c.input_fanout(pin);
                    if let Some(x) = changes.iter_mut().find(|x| x.0 == input) {
                        x.1 += dc;
                        x.2 += df;
                    } else {
                        changes.push((input, dc, df));
                    }
                }
                changes.into_iter().all(|(input, dc, df)| {
                    if net.nodes[input].primary {
                        return true;
                    }
                    let Some((p, pin)) = net.nodes[input].driver else {
                        return false;
                    };
                    if net.nodes[input].protected {
                        return false;
                    }
                    let load = loads[input] + dc;
                    if load > limits[input].cap + 1e-12
                        || fanouts[input] + df > limits[input].fanout + 1e-8
                    {
                        return false;
                    }
                    let slew = t.slew_at_load_inputs(
                        m,
                        net.gates[p].cell,
                        pin,
                        load,
                        opts.clock_slew_ns,
                        &snapshot_inputs[p],
                    );
                    slew[0].max(slew[1]) <= limits[input].slew + 1e-10
                })
            });
            if legal {
                selected = Some((width, copies, cost));
                break;
            }
        }
        let Some((width, copies, cost)) = selected else {
            continue;
        };
        let sinks = net.nodes[n].sinks.clone();
        for chunk in sinks.chunks(width).take(copies) {
            let out = net.nodes.len();
            net.nodes.push(Node::default());
            net.refs.push(NlRef::Net(String::new()));
            for &(s, pin) in chunk {
                net.gates[s].inputs[pin] = out;
            }
            net.gates.push(Gate {
                active: true,
                cell,
                inputs: original_inputs.clone(),
                outputs: vec![(op, out)],
            });
        }
        for (m, loads, fanouts, _, _) in &mut views {
            let c = &m.cells[cell];
            for (&input, &pin) in original_inputs.iter().zip(&c.inputs) {
                loads[input] += copies as f64 * c.input_cap(pin);
                fanouts[input] += copies as f64 * c.input_fanout(pin);
            }
        }
        area += cost;
        report.rule_clones += copies;
    }
    net.rebuild(lib).map_err(JobError::Failed)
}
