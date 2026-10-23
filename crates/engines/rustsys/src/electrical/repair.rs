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
                // Sequential sizing stays in this fallback: spending its budget
                // ahead of a feasible buffer tree can displace critical-path repairs.
                if sized.is_none() && branch.is_none() && (old.comb || old.clock.is_some()) {
                    let current = net.gates[g].cell;
                    let mut choices = lib.families[old.family]
                        .iter()
                        .copied()
                        .filter(|&cell| {
                            cell != current
                                && lib.cells[cell].raw.area - old.raw.area <= available + 1e-8
                        })
                        .peekable();
                    // No candidate can be selected, so do not walk state-output
                    // sink lists or evaluate the current driver's launch tables.
                    if choices.peek().is_none() {
                        continue;
                    }
                    let excess = |c: &super::model::CellModel<'_>, p: usize, slew: f64| {
                        (t.load[n] / c.max_cap(p).max(1e-20))
                            .max(slew / c.max_slew(p).min(lim.slew).max(1e-20))
                    };
                    // A sequential family has identical pin names, order and
                    // state equations (Model::new). Reuse the existing pin ID;
                    // the two normal Q/QN slew bounds need one sink scan each.
                    // Larger output sets retain the same per-output fallback.
                    let mut state_slew = [f64::INFINITY; 2];
                    if !old.comb {
                        for (limit, &(_, node)) in state_slew.iter_mut().zip(&net.gates[g].outputs)
                        {
                            *limit = Limits::for_node(net, lib, node).slew;
                        }
                    }
                    let state_excess = |cell| {
                        net.gates[g].outputs.iter().enumerate().fold(
                            0.0_f64,
                            |worst, (i, &(pin, node))| {
                                let value = Timing::launch(
                                    lib,
                                    cell,
                                    pin,
                                    t.load[node],
                                    opts.clock_slew_ns,
                                );
                                let c = &lib.cells[cell];
                                let slew_limit = if i < state_slew.len() {
                                    state_slew[i]
                                } else {
                                    Limits::for_node(net, lib, node).slew
                                };
                                worst.max((t.load[node] / c.max_cap(pin).max(1e-20)).max(
                                    value.slew[0].max(value.slew[1])
                                        / c.max_slew(pin).min(slew_limit).max(1e-20),
                                ))
                            },
                        )
                    };
                    let now = if old.comb {
                        excess(old, op, t.value[n].slew[0].max(t.value[n].slew[1]))
                    } else {
                        state_excess(net.gates[g].cell)
                    };
                    let mut best = (now * 0.9, None);
                    for cell in choices {
                        let c = &lib.cells[cell];
                        let cost = c.raw.area - old.raw.area;
                        let e = if old.comb {
                            let value =
                                t.candidate_inputs(net, lib, g, cell, true, &snapshot_inputs[g]);
                            excess(c, c.outputs[0], value.slew[0].max(value.slew[1]))
                        } else {
                            state_excess(cell)
                        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::electrical::tests::{inst, libs, port};
    use crate::map::library::CellLib;
    use rusteda_formats::liberty::Direction;
    use rusteda_formats::verilog::write::{NlDir, NlModule};

    /// One state, 32 sinks, two compatible variants: <1 MiB per test worker.
    /// No reset or initial-state substitution is permitted by this repair.
    #[test]
    fn sequential_drive_repair_preserves_state_pins_and_respects_area() {
        let mut libraries = libs();
        let mut weak = libraries[0]
            .cells
            .iter()
            .find(|c| c.name == "wt_dff_x1")
            .unwrap()
            .clone();
        weak.name = "weak_state".into();
        weak.area = 10.0;
        for pin in weak
            .pins
            .iter_mut()
            .filter(|p| p.direction == Direction::Output)
        {
            pin.max_capacitance = Some(0.001);
        }
        let mut strong = weak.clone();
        strong.name = "strong_state".into();
        strong.area = 10.5;
        for pin in strong
            .pins
            .iter_mut()
            .filter(|p| p.direction == Direction::Output)
        {
            pin.max_capacitance = Some(10.0);
        }
        libraries[0].cells.retain(|c| c.name == "wt_inv_x1");
        libraries[0].cells.extend([weak, strong]);
        let eligible = CellLib::build(&libraries, &[]).unwrap();
        let model = Model::new(&libraries, &eligible);
        let mut module = NlModule {
            name: "state_load".into(),
            ports: vec![port("clk", NlDir::Input), port("d", NlDir::Input)],
            instances: vec![inst(
                "state",
                "weak_state",
                &[("D", "d"), ("CK", "clk"), ("Q", "q"), ("QN", "qn")],
            )],
            ..Default::default()
        };
        for i in 0..32 {
            let output = format!("y{i}");
            module.ports.push(port(&output, NlDir::Output));
            module.instances.push(inst(
                &format!("load{i}"),
                "wt_inv_x1",
                &[("A", if i % 2 == 0 { "q" } else { "qn" }), ("Y", &output)],
            ));
        }
        let original_pins = module.instances[0].conns.clone();
        for (budget, expected) in [(0.0, "weak_state"), (0.5, "strong_state")] {
            let mut net = Network::new(&module, &model).unwrap();
            let initial_area = net.area(&model);
            let opts = Options {
                area_limit: Some(initial_area + budget),
                ..Default::default()
            };
            let mut report = Report::default();
            run_modes(&mut net, &model, &opts, &mut report, Ctx::NONE, &[false]).unwrap();
            let mut output = module.clone();
            net.write(&mut output, &model);
            let state = output.instances.iter().find(|g| g.name == "state").unwrap();
            assert_eq!(state.cell, expected);
            assert_eq!(state.conns, original_pins);
            assert!(net.area(&model) <= initial_area + budget + 1e-8);
            assert_eq!(report.rule_resizes, usize::from(budget > 0.0));
        }
    }
}
