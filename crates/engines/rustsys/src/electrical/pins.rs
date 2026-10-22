// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Area-neutral assignment of late signals to faster functionally symmetric pins.

use super::{Options, Report, model::Model, network::Network, rules::Limits, timing::Timing};
use rusteda_core::job::{Ctx, JobError};

fn symmetric(tt: u64, n: usize, a: usize, b: usize) -> bool {
    (0..1usize << n).all(|p| {
        let q = if ((p >> a) ^ (p >> b)) & 1 != 0 {
            p ^ (1 << a) ^ (1 << b)
        } else {
            p
        };
        (tt >> p) & 1 == (tt >> q) & 1
    })
}

/// Pair symmetry partitions pins into interchangeable groups. Exchanging entire equal-size
/// groups can preserve a compound gate even when no individual cross-group swap does.
fn choices(tt: u64, n: usize) -> Vec<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for a in 0..n {
        for b in a + 1..n {
            if symmetric(tt, n, a, b) {
                out.push(vec![(a, b)]);
            }
        }
        if let Some(group) = groups.iter_mut().find(|g| symmetric(tt, n, a, g[0])) {
            group.push(a);
        } else {
            groups.push(vec![a]);
        }
    }
    for a in 0..groups.len() {
        for b in a + 1..groups.len() {
            if groups[a].len() < 2 || groups[a].len() != groups[b].len() {
                continue;
            }
            let swaps: Vec<_> = groups[a]
                .iter()
                .copied()
                .zip(groups[b].iter().copied())
                .collect();
            let same = (0..1usize << n).all(|p| {
                let q = swaps.iter().fold(p, |q, &(a, b)| {
                    if ((q >> a) ^ (q >> b)) & 1 != 0 {
                        q ^ (1 << a) ^ (1 << b)
                    } else {
                        q
                    }
                });
                (tt >> p) & 1 == (tt >> q) & 1
            });
            if same {
                out.push(swaps);
            }
        }
    }
    out
}

fn load_changes(
    c: &super::model::CellModel<'_>,
    old: &[usize],
    new: &[usize],
) -> (usize, [(usize, f64, f64, f64); 12]) {
    let mut changes = [(0usize, 0.0_f64, 0.0_f64, f64::INFINITY); 12];
    let mut len = 0usize;
    for (i, (&before, &after)) in old.iter().zip(new).enumerate() {
        if before == after {
            continue;
        }
        let pin = c.inputs[i];
        for (n, sign) in [(before, -1.0), (after, 1.0)] {
            let slew = if sign > 0.0 {
                c.max_slew(pin)
            } else {
                f64::INFINITY
            };
            if let Some(x) = changes[..len].iter_mut().find(|x| x.0 == n) {
                x.1 += sign * c.input_cap(pin);
                x.2 += sign * c.input_fanout(pin);
                x.3 = x.3.min(slew);
            } else {
                changes[len] = (n, sign * c.input_cap(pin), sign * c.input_fanout(pin), slew);
                len += 1;
            }
        }
    }
    (len, changes)
}

pub(super) fn run(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
) -> Result<(), JobError<String>> {
    let permutations: Vec<_> = lib
        .cells
        .iter()
        .map(|c| {
            c.truth
                .map_or_else(Vec::new, |tt| choices(tt, c.inputs.len()))
        })
        .collect();
    let original: Vec<_> = net.gates.iter().map(|g| g.inputs.clone()).collect();
    let initial = Timing::new(net, lib, opts);
    let score = |t: &Timing| (-t.worst_slack).max(0.0) + t.violation * 1e-5;
    let initial_score = score(&initial);
    let initial_swaps = report.pin_swaps;
    for _ in 0..2 {
        ctx.checkpoint()?;
        let mut views: Vec<_> = std::iter::once(lib)
            .chain(corners)
            .enumerate()
            .map(|(i, m)| {
                let t = if i == 0 {
                    Timing::new(net, m, opts)
                } else {
                    Timing::rules(net, m, opts)
                };
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
        let mut changed = 0;
        for &g in &net.order.clone() {
            let cell = net.gates[g].cell;
            if permutations[cell].is_empty() {
                continue;
            }
            let c = &lib.cells[cell];
            let out = net.gates[g].outputs[0].1;
            let t = &views[0].4;
            if net.nodes[out].protected || t.slack(out) > opts.period_ns * 0.15 {
                continue;
            }
            let before = (0..2)
                .map(|p| t.value[out].arrival[p] - t.required[out][p])
                .fold(f64::NEG_INFINITY, f64::max);
            let mut best = None;
            for swaps in &permutations[cell] {
                let mut inputs = net.gates[g].inputs.clone();
                for &(a, b) in swaps {
                    inputs.swap(a, b);
                }
                if inputs == net.gates[g].inputs {
                    continue;
                }
                let v = t.candidate_inputs(net, lib, g, cell, false, &inputs);
                let (clen, changes_buf) = load_changes(c, &net.gates[g].inputs, &inputs);
                let changes = &changes_buf[..clen];
                let extra = changes
                    .iter()
                    .map(|&(n, dc, _, _)| t.slope[n] * dc)
                    .fold(0.0_f64, f64::max);
                let after = (0..2)
                    .map(|p| v.arrival[p] - t.required[out][p])
                    .fold(f64::NEG_INFINITY, f64::max)
                    + extra;
                let gain = before - after;
                if gain <= 1e-5 || best.as_ref().is_some_and(|(old, _)| gain <= *old + 1e-8) {
                    continue;
                }
                if changes.iter().any(|&(n, dc, _, _)| {
                    dc > 0.0 && t.slope[n] * dc > t.slack(n).max(0.0) * 0.05 + 1e-8
                }) {
                    continue;
                }
                let legal = views.iter().all(|(m, loads, fanouts, limits, t)| {
                    let c = &m.cells[cell];
                    let op = c.outputs[0];
                    let slew =
                        t.slew_at_load_inputs(m, cell, op, loads[out], opts.clock_slew_ns, &inputs);
                    if slew[0].max(slew[1])
                        > limits[out]
                            .slew
                            .max(t.value[out].slew[0].max(t.value[out].slew[1]))
                            + 1e-10
                    {
                        return false;
                    }
                    let (clen, changes_buf) = load_changes(c, &net.gates[g].inputs, &inputs);
                    changes_buf[..clen]
                        .iter()
                        .copied()
                        .all(|(n, dc, df, pin_slew)| {
                            if net.nodes[n].protected {
                                return dc <= 0.0 && df <= 0.0;
                            }
                            let Some((parent, op)) = net.nodes[n].driver else {
                                return true;
                            };
                            let load = loads[n] + dc;
                            if load > limits[n].cap.max(loads[n]) + 1e-12
                                || fanouts[n] + df > limits[n].fanout.max(fanouts[n]) + 1e-8
                            {
                                return false;
                            }
                            let slew = t.slew_at_load(
                                net,
                                m,
                                (parent, net.gates[parent].cell, op),
                                load,
                                opts.clock_slew_ns,
                            );
                            slew[0].max(slew[1])
                                <= limits[n]
                                    .slew
                                    .min(pin_slew)
                                    .max(t.value[n].slew[0].max(t.value[n].slew[1]))
                                    + 1e-10
                        })
                });
                if legal {
                    best = Some((gain, inputs));
                }
            }
            if let Some((_, inputs)) = best {
                for (m, loads, fanouts, _, _) in &mut views {
                    let (clen, changes_buf) =
                        load_changes(&m.cells[cell], &net.gates[g].inputs, &inputs);
                    for &(n, dc, df, _) in &changes_buf[..clen] {
                        loads[n] += dc;
                        fanouts[n] += df;
                    }
                }
                net.gates[g].inputs = inputs;
                changed += 1;
            }
        }
        net.rebuild(lib).map_err(JobError::Failed)?;
        report.pin_swaps += changed;
        if changed == 0 {
            break;
        }
    }
    if score(&Timing::new(net, lib, opts)) > initial_score + 1e-8 {
        for (g, inputs) in net.gates.iter_mut().zip(original) {
            g.inputs = inputs;
        }
        net.rebuild(lib).map_err(JobError::Failed)?;
        report.pin_swaps = initial_swaps;
    }
    Ok(())
}
