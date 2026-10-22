// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Rebuild complete buffer trees, keeping critical sinks close to the source.

use super::{
    Options, Report,
    model::Model,
    network::{Gate, Network, Node},
    rules,
    timing::Timing,
};
use rusteda_core::job::{Ctx, JobError};

/// Collect only transparent, unobserved buffers. Named state probes and ports remain roots.
fn leaves(net: &Network, lib: &Model<'_>, root: usize) -> (Vec<(usize, usize)>, Vec<usize>) {
    let mut pending = vec![root];
    let mut sinks = Vec::new();
    let mut buffers = Vec::new();
    while let Some(n) = pending.pop() {
        for &(g, p) in &net.nodes[n].sinks {
            let gate = &net.gates[g];
            let c = &lib.cells[gate.cell];
            let out = gate.outputs.first().map(|x| x.1);
            if c.comb
                && c.inputs.len() == 1
                && c.truth == Some(2)
                && gate.outputs.len() == 1
                && out.is_some_and(|n| {
                    !net.nodes[n].protected
                        && !net.nodes[n].state_alias
                        && net.nodes[n].outputs == 0
                })
            {
                buffers.push(g);
                pending.push(out.expect("transparent output"));
            } else {
                sinks.push((g, p));
            }
        }
    }
    (sinks, buffers)
}

/// Collect transparent edges with their polarity relative to the root.
fn phase_leaves(
    net: &Network,
    lib: &Model<'_>,
    root: usize,
) -> (Vec<(usize, usize)>, Vec<(usize, usize)>, Vec<usize>) {
    let mut pending = vec![(root, false)];
    let (mut positive, mut negative, mut removed) = (Vec::new(), Vec::new(), Vec::new());
    while let Some((n, phase)) = pending.pop() {
        for &(g, p) in &net.nodes[n].sinks {
            let gate = &net.gates[g];
            let c = &lib.cells[gate.cell];
            let out = gate.outputs.first().map(|x| x.1);
            if c.comb
                && c.inputs.len() == 1
                && matches!(c.truth, Some(1 | 2))
                && gate.outputs.len() == 1
                && out.is_some_and(|n| {
                    !net.nodes[n].protected
                        && !net.nodes[n].state_alias
                        && net.nodes[n].outputs == 0
                })
            {
                removed.push(g);
                pending.push((
                    out.expect("transparent output"),
                    phase ^ (c.truth == Some(1)),
                ));
            } else if phase {
                negative.push((g, p));
            } else {
                positive.push((g, p));
            }
        }
    }
    (positive, negative, removed)
}

fn collect(
    net: &Network,
    lib: &Model<'_>,
    root: usize,
    phases: bool,
) -> (Vec<(usize, usize)>, Vec<(usize, usize)>, Vec<usize>) {
    if phases {
        phase_leaves(net, lib, root)
    } else {
        let (s, r) = leaves(net, lib, root);
        (s, Vec::new(), r)
    }
}

/// Two inverter levels form a non-inverting branch. Sharing the first inverter
/// across leaf inverters can use less cell area than independent positive buffers.
fn paired_tree(
    net: &mut Network,
    root: usize,
    cell: usize,
    width: usize,
    lib: &Model<'_>,
) -> usize {
    let before = net.gates.len();
    let mut sinks = net.nodes[root].sinks.clone();
    while sinks.len() > width {
        let mut upper = Vec::new();
        for group in sinks.chunks(width * width) {
            let middle = net.nodes.len();
            net.nodes.push(Node::default());
            net.refs
                .push(rusteda_formats::verilog::write::NlRef::Net(String::new()));
            let first = net.gates.len();
            net.gates.push(Gate {
                active: true,
                cell,
                inputs: vec![root],
                outputs: vec![(lib.cells[cell].outputs[0], middle)],
            });
            for chunk in group.chunks(width) {
                let out = net.nodes.len();
                net.nodes.push(Node::default());
                net.refs
                    .push(rusteda_formats::verilog::write::NlRef::Net(String::new()));
                for &(g, p) in chunk {
                    net.gates[g].inputs[p] = out;
                }
                net.gates.push(Gate {
                    active: true,
                    cell,
                    inputs: vec![middle],
                    outputs: vec![(lib.cells[cell].outputs[0], out)],
                });
            }
            upper.push((first, 0));
        }
        sinks = upper;
    }
    net.gates.len() - before
}

/// A reusable flattened tree. Candidate exploration only changes its new nodes and sink pins.
struct TrialTree {
    net: Network,
    root: usize,
    sinks: Vec<(usize, usize)>,
    positive: Vec<(usize, usize)>,
    negative: Vec<(usize, usize)>,
    inverse: Option<usize>,
    gates: usize,
    nodes: usize,
    area: f64,
}

impl TrialTree {
    fn new(
        net: &Network,
        lib: &Model<'_>,
        root: usize,
        positive: Vec<(usize, usize)>,
        negative: Vec<(usize, usize)>,
        removed: &[usize],
    ) -> Result<Self, String> {
        let sinks: Vec<_> = positive.iter().chain(&negative).copied().collect();
        let inverse = lib
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.comb && c.inputs.len() == 1 && c.truth == Some(1))
            .min_by(|(i, a), (j, b)| a.raw.area.total_cmp(&b.raw.area).then(i.cmp(j)))
            .map(|(i, _)| i);
        let mut net = net.clone();
        for &g in removed {
            net.gates[g].active = false;
        }
        for &(g, p) in &sinks {
            net.gates[g].inputs[p] = root;
        }
        net.rebuild(lib)?;
        let (gates, nodes, area) = (net.gates.len(), net.nodes.len(), net.area(lib));
        Ok(Self {
            net,
            root,
            sinks,
            positive,
            negative,
            inverse,
            gates,
            nodes,
            area,
        })
    }

    fn build(
        &mut self,
        lib: &Model<'_>,
        cell: usize,
        width: usize,
        inverted: bool,
    ) -> (usize, Vec<usize>, f64) {
        self.net.gates.truncate(self.gates);
        self.net.nodes.truncate(self.nodes);
        self.net.refs.truncate(self.nodes);
        for &(g, p) in &self.sinks {
            self.net.gates[g].inputs[p] = self.root;
        }
        self.net.nodes[self.root].sinks = self.positive.clone();
        if !self.negative.is_empty() {
            let inverse = if inverted {
                cell
            } else {
                self.inverse.expect("phase tree needs an inverter")
            };
            let out = self.net.nodes.len();
            self.net.nodes.push(Node::default());
            self.net
                .refs
                .push(rusteda_formats::verilog::write::NlRef::Net(String::new()));
            let gate = self.net.gates.len();
            self.net.gates.push(Gate {
                active: true,
                cell: inverse,
                inputs: vec![self.root],
                outputs: vec![(lib.cells[inverse].outputs[0], out)],
            });
            self.net.nodes[self.root].sinks.push((gate, 0));
            self.net.nodes[out].sinks = self.negative.clone();
            for &(g, p) in &self.negative {
                self.net.gates[g].inputs[p] = out;
            }
            if inverted {
                paired_tree(&mut self.net, out, cell, width, lib);
            } else {
                self.net.split_fanout(out, cell, width, false, lib);
            }
        }
        if inverted {
            paired_tree(&mut self.net, self.root, cell, width, lib);
        } else {
            self.net.split_fanout(self.root, cell, width, false, lib);
        }
        let added = self.net.gates.len() - self.gates;
        for n in self.nodes..self.net.nodes.len() {
            self.net.nodes[n].sinks.clear();
        }
        self.net.nodes[self.root].sinks.clear();
        for g in self.gates..self.net.gates.len() {
            for &(pin, n) in &self.net.gates[g].outputs {
                self.net.nodes[n].driver = Some((g, pin));
            }
            let input = self.net.gates[g].inputs[0];
            self.net.nodes[input].sinks.push((g, 0));
        }
        for &(g, p) in &self.sinks {
            self.net.nodes[self.net.gates[g].inputs[p]]
                .sinks
                .push((g, p));
        }
        self.net.nodes[self.root].sinks.sort_unstable();
        for n in self.nodes..self.net.nodes.len() {
            self.net.nodes[n].sinks.sort_unstable();
        }
        let mut pending = vec![0u8; added];
        let mut order = Vec::with_capacity(added);
        for g in self.gates..self.net.gates.len() {
            if self.net.gates[g].inputs[0] == self.root {
                order.push(g);
            } else {
                pending[g - self.gates] = 1;
            }
        }
        let mut i = 0;
        while i < order.len() {
            let out = self.net.gates[order[i]].outputs[0].1;
            for &(g, _) in &self.net.nodes[out].sinks {
                if g >= self.gates {
                    pending[g - self.gates] -= 1;
                    if pending[g - self.gates] == 0 {
                        order.push(g);
                    }
                }
            }
            i += 1;
        }
        debug_assert_eq!(order.len(), added);
        let area = self.net.gates[self.gates..]
            .iter()
            .fold(self.area, |a, g| a + lib.cells[g.cell].raw.area);
        (added, order, area)
    }
}

/// Cheap ranking of a tree with boundary arrivals and sink requirements held fixed.
/// Acceptance still uses full-network timing and every corner's electrical rules.
fn local_slack(
    original: &Network,
    tree: &TrialTree,
    lib: &Model<'_>,
    before: &Timing,
    order: &[usize],
    opts: &Options,
) -> Option<f64> {
    let net = &tree.net;
    let load = |n: usize| {
        opts.output_load_pf * net.nodes[n].outputs as f64
            + net.nodes[n]
                .sinks
                .iter()
                .map(|&(g, p)| {
                    let c = &lib.cells[net.gates[g].cell];
                    c.input_cap(c.inputs[p])
                })
                .sum::<f64>()
    };
    let root_value = super::load_buffer::driver(net, lib, before, tree.root, load(tree.root), opts);
    let mut values = vec![super::timing::Value::default(); net.nodes.len() - tree.nodes];
    for &g in order {
        let gate = &net.gates[g];
        let input = gate.inputs[0];
        let value = if input == tree.root {
            root_value
        } else {
            values[input - tree.nodes]
        };
        let out = gate.outputs[0].1;
        values[out - tree.nodes] = super::load_buffer::through(lib, gate.cell, value, load(out));
    }
    let value = |n| {
        if n == tree.root {
            root_value
        } else {
            values[n - tree.nodes]
        }
    };
    for n in std::iter::once(tree.root).chain(tree.nodes..net.nodes.len()) {
        let limits = rules::Limits::for_node(net, lib, n);
        if load(n) > limits.cap + 1e-12
            || limits.fanout_load > limits.fanout + 1e-8
            || value(n).slew[0].max(value(n).slew[1]) > limits.slew + 1e-10
        {
            return None;
        }
    }
    Some(
        tree.sinks
            .iter()
            .map(|&(g, p)| {
                let req =
                    before.sink_required(original, lib, g, p, original.gates[g].inputs[p], opts);
                let v = value(net.gates[g].inputs[p]);
                req - v.arrival[0].max(v.arrival[1])
            })
            .fold(f64::INFINITY, f64::min),
    )
}

pub(super) fn run(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
) -> Result<(), JobError<String>> {
    run_mode(net, lib, corners, opts, report, ctx, false)
}
pub(super) fn run_phases(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
) -> Result<(), JobError<String>> {
    run_mode(net, lib, corners, opts, report, ctx, true)
}

fn run_mode(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    report: &mut Report,
    ctx: Ctx<'_>,
    phases: bool,
) -> Result<(), JobError<String>> {
    let initial = Timing::new(net, lib, opts);
    let over_area = opts.area_limit.is_some_and(|a| net.area(lib) > a + 1e-8);
    let cells: Vec<_> = lib
        .buffers
        .iter()
        .copied()
        .map(|c| (c, false))
        .chain(
            lib.cells
                .iter()
                .enumerate()
                .filter(|(_, c)| c.comb && c.inputs.len() == 1 && c.truth == Some(1))
                .map(|(c, _)| (c, true)),
        )
        .collect();
    let mut roots: Vec<_> = net
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(n, node)| {
            let (g, _) = node.driver?;
            let c = &lib.cells[net.gates[g].cell];
            if node.primary
                || node.clock
                || (c.comb
                    && c.inputs.len() == 1
                    && (c.truth == Some(2) || (phases && c.truth == Some(1)))
                    && node.outputs == 0
                    && !node.state_alias
                    && !node.protected)
            {
                return None;
            }
            let (sinks, negative, buffers) = collect(net, lib, n, phases);
            if phases && negative.is_empty() {
                return None;
            }
            (sinks.len() + negative.len() >= 5 && (!buffers.is_empty() || initial.slack(n) < 0.0))
                .then_some((
                    if over_area {
                        -(buffers.len() as f64)
                    } else {
                        initial.slack(n)
                    },
                    n,
                ))
        })
        .collect();
    roots.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut cached: Option<(Timing, f64, Vec<Timing>, Vec<rules::RuleReport>)> = None;
    let mut first_timing = Some(initial);
    // Bounded work: only the most constrained trees, independent of instance names.
    for (_, n) in roots.into_iter().take(16) {
        ctx.checkpoint()?;
        let (mut sinks, mut negative, buffers) = collect(net, lib, n, phases);
        if phases && negative.is_empty() {
            continue;
        }
        let (before, old_area, corner_timing, base_rules) = &*cached.get_or_insert_with(|| {
            let before = first_timing
                .take()
                .unwrap_or_else(|| Timing::new(net, lib, opts));
            let old_area = net.area(lib);
            let corner_timing: Vec<_> = corners
                .iter()
                .map(|m| Timing::rules(net, m, opts))
                .collect();
            let base_rules: Vec<_> = std::iter::once(rules::report(net, lib, &before))
                .chain(
                    corners
                        .iter()
                        .zip(&corner_timing)
                        .map(|(m, t)| rules::report(net, m, t)),
                )
                .collect();
            (before, old_area, corner_timing, base_rules)
        });
        let old_area = *old_area;
        negative.sort_by(|&(g, p), &(h, q)| {
            before
                .sink_required(net, lib, h, q, net.gates[h].inputs[q], opts)
                .total_cmp(&before.sink_required(net, lib, g, p, net.gates[g].inputs[p], opts))
                .then((g, p).cmp(&(h, q)))
        });
        sinks.sort_by(|&(g, p), &(h, q)| {
            before
                .sink_required(net, lib, h, q, net.gates[h].inputs[q], opts)
                .total_cmp(&before.sink_required(net, lib, g, p, net.gates[g].inputs[p], opts))
                .then((g, p).cmp(&(h, q)))
        });
        let recovering_area = opts.area_limit.is_some_and(|a| old_area > a + 1e-8);
        let mut best = None;
        let mut best_slack = before.worst_slack;
        let mut best_violation = before.violation;
        let mut best_area = old_area;
        let mut finalists = Vec::new();
        let mut bypass_seen = false;
        let mut tree =
            TrialTree::new(net, lib, n, sinks, negative, &buffers).map_err(JobError::Failed)?;
        for width in [2, 4, 8, 16, 32, 64] {
            for &(cell, inverted) in &cells {
                let (added, order, area) = tree.build(lib, cell, width, inverted);
                if added == 0 {
                    if buffers.is_empty() || bypass_seen {
                        continue;
                    }
                    bypass_seen = true;
                }
                let limit = opts.area_limit.unwrap_or(f64::INFINITY).max(old_area);
                if area > limit + 1e-8 {
                    continue;
                }
                if let Some(slack) = local_slack(net, &tree, lib, before, &order, opts) {
                    if recovering_area && slack < before.worst_slack.min(0.0) - 1e-8 {
                        continue;
                    }
                    if corners
                        .iter()
                        .zip(corner_timing)
                        .any(|(m, t)| local_slack(net, &tree, m, t, &order, opts).is_none())
                    {
                        continue;
                    }
                    let key = if recovering_area { area } else { -slack };
                    finalists.push((key, area, cell, width, inverted));
                }
            }
        }
        finalists.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        for (_, area, cell, width, inverted) in finalists.into_iter().take(4) {
            let (added, _, _) = tree.build(lib, cell, width, inverted);
            let mut trial = tree.net.clone();
            trial.rebuild(lib).map_err(JobError::Failed)?;
            // Once a feasible tree is found, a faster over-budget tree cannot replace it.
            if opts
                .area_limit
                .is_some_and(|limit| best_area <= limit + 1e-8 && area > limit + 1e-8)
            {
                continue;
            }
            let t = Timing::new(&trial, lib, opts);
            let gain = if opts.area_limit.is_some_and(|a| best_area > a + 1e-8) {
                area < best_area - 1e-8 && t.worst_slack >= before.worst_slack.min(0.0) - 1e-8
            } else {
                t.worst_slack > best_slack + 1e-5
                    || (t.worst_slack >= best_slack - 1e-8
                        && (t.violation < best_violation - 1e-8
                            || (t.violation <= best_violation + 1e-8 && area < best_area - 1e-8)))
            };
            if !gain {
                continue;
            }
            let legal = std::iter::once(lib)
                .chain(corners)
                .zip(base_rules)
                .all(|(m, b)| {
                    let r = rules::report(&trial, m, &Timing::rules(&trial, m, opts));
                    r.capacitance_nets <= b.capacitance_nets
                        && r.transition_nets <= b.transition_nets
                        && r.fanout_nets <= b.fanout_nets
                });
            if legal {
                best_slack = t.worst_slack;
                best_violation = t.violation;
                best_area = area;
                best = Some((trial, added));
                if recovering_area {
                    break;
                }
            }
        }
        if let Some((trial, added)) = best {
            *net = trial;
            cached = None;
            report.buffers_added += added;
            if opts.area_limit.is_none_or(|a| best_area <= a + 1e-8) && best_slack >= 0.0 {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
