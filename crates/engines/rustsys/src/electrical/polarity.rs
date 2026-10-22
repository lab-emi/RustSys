// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Exact truth-table substitutions that absorb otherwise explicit inverters.

use super::model::Model;
use super::network::Network;

fn mask(n: usize) -> u64 {
    if n == 6 {
        u64::MAX
    } else {
        (1u64 << (1 << n)) - 1
    }
}
fn flip(tt: u64, n: usize, phase: usize) -> u64 {
    (0..(1 << n)).fold(0, |out, b| out | (((tt >> (b ^ phase)) & 1) << b))
}
fn inverter(net: &Network, lib: &Model<'_>, g: usize) -> bool {
    let c = &lib.cells[net.gates[g].cell];
    net.gates[g].active && c.truth == Some(1) && c.inputs.len() == 1
}
fn state_alias(net: &Network, lib: &Model<'_>, g: usize) -> bool {
    if net.known_state_aliases {
        return net.nodes[net.gates[g].outputs[0].1].state_alias;
    }
    net.nodes[net.gates[g].inputs[0]]
        .driver
        .is_some_and(|(d, _)| !lib.cells[net.gates[d].cell].comb)
}

/// A shared inverter is removable only after all of its consumers change phase.
/// Price that substitution jointly: each consumer alone may grow, although the
/// total replacement is smaller once the shared inverter disappears.
fn shared_inputs(net: &mut Network, lib: &Model<'_>) -> usize {
    let mut removed = 0;
    for inv in net.order.clone() {
        if !inverter(net, lib, inv) || state_alias(net, lib, inv) {
            continue;
        }
        let out = net.gates[inv].outputs[0].1;
        let node = &net.nodes[out];
        if node.protected || node.state_alias || node.outputs != 0 || node.sinks.len() < 2 {
            continue;
        }
        let mut consumers: Vec<_> = node.sinks.iter().map(|&(g, _)| g).collect();
        consumers.sort_unstable();
        consumers.dedup();
        let mut gain = lib.cells[net.gates[inv].cell].raw.area;
        let mut changes = Vec::new();
        for g in consumers {
            let gate = &net.gates[g];
            let c = &lib.cells[gate.cell];
            if !gate.active || !c.comb || gate.outputs.iter().any(|(_, n)| net.nodes[*n].protected)
            {
                changes.clear();
                break;
            }
            let n = c.inputs.len();
            let phase = gate
                .inputs
                .iter()
                .enumerate()
                .fold(0, |p, (i, &v)| p | if v == out { 1 << i } else { 0 });
            let tt = flip(c.truth.expect("comb"), n, phase);
            let Some(&(cell, perm)) = lib.matching.get(&(n, tt)).and_then(|v| v.first()) else {
                changes.clear();
                break;
            };
            gain += c.raw.area - lib.cells[cell].raw.area;
            changes.push((g, cell, perm));
        }
        if changes.is_empty() || gain <= 1e-8 {
            continue;
        }
        let source = net.gates[inv].inputs[0];
        for (g, cell, perm) in changes {
            let inputs = net.gates[g].inputs.clone();
            for &n in &inputs {
                net.nodes[n].sinks.retain(|&(sink, _)| sink != g);
            }
            net.gates[g].inputs = perm[..inputs.len()]
                .iter()
                .map(|&i| {
                    let v = inputs[i as usize];
                    if v == out { source } else { v }
                })
                .collect();
            net.change_cell(g, cell, lib);
            for (pin, &n) in net.gates[g].inputs.iter().enumerate() {
                net.nodes[n].sinks.push((g, pin));
            }
        }
        net.nodes[source].sinks.retain(|&(sink, _)| sink != inv);
        net.gates[inv].active = false;
        net.nodes[out].driver = None;
        removed += 1;
    }
    if removed > 0 {
        net.rebuild(lib)
            .expect("shared inversion absorption preserves DAG");
    }
    removed
}

/// Change the physical phase of a shared driver together with all consumers of
/// its old phase, eliminating the explicit inverter that produces the other phase.
fn shared_outputs(net: &mut Network, lib: &Model<'_>) -> usize {
    let mut count = 0;
    for g in net.order.clone() {
        if !net.gates[g].active {
            continue;
        }
        let c = &lib.cells[net.gates[g].cell];
        let n = c.inputs.len();
        let out = net.gates[g].outputs[0].1;
        let node = &net.nodes[out];
        if node.protected || node.state_alias || node.outputs != 0 {
            continue;
        }
        let Some(&(inv, _)) = node
            .sinks
            .iter()
            .find(|&&(sink, _)| inverter(net, lib, sink))
        else {
            continue;
        };
        let dest = net.gates[inv].outputs[0].1;
        if net.nodes[dest].protected {
            continue;
        }
        let Some(&(root_cell, root_perm)) = lib
            .matching
            .get(&(n, (!c.truth.expect("comb")) & mask(n)))
            .and_then(|v| v.first())
        else {
            continue;
        };
        let mut gain =
            c.raw.area + lib.cells[net.gates[inv].cell].raw.area - lib.cells[root_cell].raw.area;
        let mut consumers: Vec<_> = node
            .sinks
            .iter()
            .map(|&(g, _)| g)
            .filter(|&g| g != inv)
            .collect();
        consumers.sort_unstable();
        consumers.dedup();
        let mut changes = Vec::new();
        let mut valid = true;
        for sink in consumers {
            let gate = &net.gates[sink];
            let c = &lib.cells[gate.cell];
            if !gate.active || !c.comb || gate.outputs.iter().any(|(_, n)| net.nodes[*n].protected)
            {
                valid = false;
                break;
            }
            let n = c.inputs.len();
            let phase = gate
                .inputs
                .iter()
                .enumerate()
                .fold(0, |p, (i, &v)| p | if v == out { 1 << i } else { 0 });
            let Some(&(cell, perm)) = lib
                .matching
                .get(&(n, flip(c.truth.expect("comb"), n, phase)))
                .and_then(|v| v.first())
            else {
                valid = false;
                break;
            };
            gain += c.raw.area - lib.cells[cell].raw.area;
            changes.push((sink, cell, perm));
        }
        if !valid || gain <= 1e-8 {
            continue;
        }
        for (sink, cell, perm) in changes {
            let inputs = net.gates[sink].inputs.clone();
            for &input in &inputs {
                net.nodes[input].sinks.retain(|&(s, _)| s != sink);
            }
            net.gates[sink].inputs = perm[..inputs.len()]
                .iter()
                .map(|&i| {
                    if inputs[i as usize] == out {
                        dest
                    } else {
                        inputs[i as usize]
                    }
                })
                .collect();
            net.change_cell(sink, cell, lib);
            for (pin, &input) in net.gates[sink].inputs.iter().enumerate() {
                net.nodes[input].sinks.push((sink, pin));
            }
        }
        let inputs = net.gates[g].inputs.clone();
        for &input in &inputs {
            net.nodes[input].sinks.retain(|&(s, _)| s != g);
        }
        net.gates[g].inputs = root_perm[..n].iter().map(|&i| inputs[i as usize]).collect();
        net.gates[g].outputs[0].1 = dest;
        net.gates[inv].active = false;
        net.nodes[out].driver = None;
        net.nodes[out].sinks.clear();
        net.change_cell(g, root_cell, lib);
        for (pin, &input) in net.gates[g].inputs.iter().enumerate() {
            net.nodes[input].sinks.push((g, pin));
        }
        count += 1;
    }
    if count > 0 {
        net.rebuild(lib)
            .expect("shared output phase substitution preserves DAG");
    }
    count
}

pub(super) fn fold(net: &mut Network, lib: &Model<'_>) -> usize {
    let mut removed = 0;
    for _ in 0..4 {
        let mut count = 0;
        let mut uses: Vec<usize> = net.nodes.iter().map(|n| n.sinks.len()).collect();
        for g in net.order.clone() {
            if !net.gates[g].active {
                continue;
            }
            let cell = net.gates[g].cell;
            let c = &lib.cells[cell];
            let n = c.inputs.len();
            let out = net.gates[g].outputs[0].1;
            if net.nodes[out].protected || inverter(net, lib, g) {
                continue;
            }
            let mut inv = vec![None; n];
            for (i, &v) in net.gates[g].inputs.iter().enumerate() {
                if let Some((d, _)) = net.nodes[v].driver {
                    if inverter(net, lib, d) && !net.nodes[v].protected && !state_alias(net, lib, d)
                    {
                        inv[i] = Some(d);
                    }
                }
            }
            let phases = (0..n).fold(0, |a, i| a | if inv[i].is_some() { 1 << i } else { 0 });
            let mut best = None;
            for ph in 1..(1usize << n) {
                if ph & !phases != 0 {
                    continue;
                }
                let tt = flip(c.truth.expect("comb"), n, ph);
                let Some(&(nc, perm)) = lib.matching.get(&(n, tt)).and_then(|x| x.first()) else {
                    continue;
                };
                let saved: f64 = (0..n)
                    .filter(|&i| ph & (1 << i) != 0)
                    .filter_map(|i| inv[i].map(|d| (d, net.gates[g].inputs[i])))
                    .filter(|&(_, v)| uses[v] == 1 && net.nodes[v].outputs == 0)
                    .map(|(d, _)| lib.cells[net.gates[d].cell].raw.area)
                    .sum();
                let gain = c.raw.area + saved - lib.cells[nc].raw.area;
                if gain > 1e-8 && best.as_ref().is_none_or(|(v, _, _, _)| gain > *v + 1e-8) {
                    best = Some((gain, ph, nc, perm));
                }
            }
            if let Some((_, ph, nc, perm)) = best {
                let mut ins = net.gates[g].inputs.clone();
                for i in 0..n {
                    if ph & (1 << i) == 0 {
                        continue;
                    }
                    let d = inv[i].expect("selected");
                    let v = ins[i];
                    let source = net.gates[d].inputs[0];
                    uses[v] -= 1;
                    uses[source] += 1;
                    ins[i] = source;
                    if uses[v] == 0 && net.nodes[v].outputs == 0 && !net.nodes[v].state_alias {
                        net.gates[d].active = false;
                        net.nodes[v].driver = None;
                        uses[source] -= 1;
                        count += 1;
                    }
                }
                net.gates[g].inputs = perm[..n].iter().map(|&i| ins[i as usize]).collect();
                net.change_cell(g, nc, lib);
            }
        }
        net.rebuild(lib)
            .expect("input inversion absorption preserves DAG");
        for g in net.order.clone() {
            if !net.gates[g].active {
                continue;
            }
            let c = &lib.cells[net.gates[g].cell];
            let n = c.inputs.len();
            let out = net.gates[g].outputs[0].1;
            if inverter(net, lib, g)
                || net.nodes[out].protected
                || net.nodes[out].state_alias
                || net.nodes[out].outputs != 0
                || net.nodes[out].sinks.len() != 1
            {
                continue;
            }
            let (inv, _) = net.nodes[out].sinks[0];
            if !inverter(net, lib, inv) {
                continue;
            }
            let dest = net.gates[inv].outputs[0].1;
            if net.nodes[dest].protected {
                continue;
            }
            let tt = (!c.truth.expect("comb")) & mask(n);
            let Some(&(nc, perm)) = lib.matching.get(&(n, tt)).and_then(|x| x.first()) else {
                continue;
            };
            if lib.cells[nc].raw.area >= c.raw.area + lib.cells[net.gates[inv].cell].raw.area - 1e-8
            {
                continue;
            }
            let ins = net.gates[g].inputs.clone();
            net.gates[g].inputs = perm[..n].iter().map(|&i| ins[i as usize]).collect();
            net.gates[g].outputs[0].1 = dest;
            net.change_cell(g, nc, lib);
            net.gates[inv].active = false;
            count += 1;
        }
        net.rebuild(lib)
            .expect("output inversion absorption preserves DAG");
        count += shared_inputs(net, lib);
        count += shared_outputs(net, lib);
        removed += count;
        if count == 0 {
            break;
        }
    }
    removed
}
