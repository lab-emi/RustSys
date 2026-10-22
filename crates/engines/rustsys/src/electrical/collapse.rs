// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded exact cell substitution across boundaries of the first Boolean cover.

use super::model::Model;
use super::network::{Gate, Network, Node};
use crate::map::cuts::{Cut, var_mask};
use rustc_hash::{FxHashMap, FxHashSet};
use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::verilog::write::NlRef;

#[derive(Clone)]
struct Cone {
    leaves: Vec<usize>,
    gates: Vec<usize>,
}

fn truth(net: &Network, lib: &Model<'_>, n: usize, leaves: &[usize]) -> u64 {
    if let Some(i) = leaves.iter().position(|&v| v == n) {
        return var_mask(i);
    }
    let (g, _) = net.nodes[n].driver.expect("inside a combinational cone");
    let gate = &net.gates[g];
    let k = gate.inputs.len();
    let mut ins = [0u64; 6];
    let mut neg = [0u64; 6];
    for (i, &inp) in gate.inputs.iter().enumerate() {
        let v = truth(net, lib, inp, leaves);
        ins[i] = v;
        neg[i] = !v;
    }
    let tt = lib.cells[gate.cell].truth.expect("comb");
    let mut bits = if k == 6 {
        tt
    } else {
        tt & ((1u64 << (1 << k)) - 1)
    };
    let mut out = 0u64;
    while bits != 0 {
        let m = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        let mut term = u64::MAX;
        for i in 0..k {
            term &= if (m >> i) & 1 == 0 { neg[i] } else { ins[i] };
        }
        out |= term;
    }
    out
}

fn removable(net: &Network, cone: &Cone, root: usize) -> Vec<usize> {
    let mut removed = cone.gates.clone();
    loop {
        let before = removed.len();
        let previous = removed.clone();
        removed.retain(|&g| {
            g == root
                || net.gates[g].outputs.iter().all(|&(_, n)| {
                    net.nodes[n].outputs == 0
                        && net.nodes[n].sinks.iter().all(|(s, _)| previous.contains(s))
                })
        });
        if removed.len() == before {
            return removed;
        }
    }
}

pub(super) fn run(
    net: &mut Network,
    lib: &Model<'_>,
    ctx: Ctx<'_>,
) -> Result<(usize, usize), JobError<String>> {
    let mut count = 0usize;
    let mut two_cell_rewrites = 0;
    let mut decompositions: FxHashMap<(usize, u64), Option<super::decompose::Recipe>> =
        FxHashMap::default();
    for round in 0..2 {
        let mut changes = 0;
        for (iteration, g) in net.order.clone().into_iter().rev().enumerate() {
            if iteration % 256 == 0 {
                ctx.checkpoint()?;
            }
            if !net.gates[g].active {
                continue;
            }
            let out = net.gates[g].outputs[0].1;
            if net.nodes[out].protected {
                continue;
            }
            let mut leaves = net.gates[g].inputs.clone();
            leaves.sort_unstable();
            leaves.dedup();
            let mut seen = FxHashSet::default();
            seen.insert(leaves.clone());
            let mut cones = vec![Cone {
                leaves,
                gates: vec![g],
            }];
            let mut best = None;
            let mut pos = 0;
            while pos < cones.len() && pos < 48 {
                let cone = cones[pos].clone();
                pos += 1;
                if cone.gates.len() > 1 {
                    let mut cut = Cut {
                        n: cone.leaves.len() as u8,
                        leaves: [0; 6],
                        sign: 0,
                        tt: truth(net, lib, out, &cone.leaves),
                    };
                    for (to, &from) in cut.leaves.iter_mut().zip(&cone.leaves) {
                        *to = from as u32;
                    }
                    cut.reduce();
                    let n = cut.n as usize;
                    let mask = if n == 6 {
                        u64::MAX
                    } else {
                        (1u64 << (1 << n)) - 1
                    };
                    if let Some(&(cell, perm)) = lib
                        .matching
                        .get(&(n, cut.tt & mask))
                        .and_then(|v| v.first())
                    {
                        let removed = removable(net, &cone, g);
                        let area: f64 = removed
                            .iter()
                            .map(|&id| lib.cells[net.gates[id].cell].raw.area)
                            .sum();
                        let gain = area - lib.cells[cell].raw.area;
                        if gain > 1e-8
                            && best
                                .as_ref()
                                .is_none_or(|(saved, _, _, _, _)| gain > *saved + 1e-8)
                        {
                            let ins: Vec<_> = perm[..n]
                                .iter()
                                .map(|&i| cut.leaves[i as usize] as usize)
                                .collect();
                            best = Some((gain, cell, ins, removed, None));
                        }
                    }
                    if cone.gates.len() >= 3 && cone.leaves.len() >= 3 {
                        if decompositions.len() >= 4096 {
                            decompositions.clear();
                        }
                        if let Some(recipe) = decompositions
                            .entry((n, cut.tt & mask))
                            .or_insert_with(|| super::decompose::find(lib, n, cut.tt & mask))
                        {
                            let removed = removable(net, &cone, g);
                            let area: f64 = removed
                                .iter()
                                .map(|&id| lib.cells[net.gates[id].cell].raw.area)
                                .sum();
                            let gain = area - recipe.area;
                            if gain > 1e-8
                                && best
                                    .as_ref()
                                    .is_none_or(|(saved, _, _, _, _)| gain > *saved + 1e-8)
                            {
                                let root_inputs = recipe
                                    .root_inputs
                                    .iter()
                                    .map(|&i| {
                                        if i == 6 {
                                            usize::MAX
                                        } else {
                                            cut.leaves[i] as usize
                                        }
                                    })
                                    .collect();
                                let inner_inputs = recipe
                                    .inner_inputs
                                    .iter()
                                    .map(|&i| cut.leaves[i] as usize)
                                    .collect::<Vec<_>>();
                                best = Some((
                                    gain,
                                    recipe.root,
                                    root_inputs,
                                    removed,
                                    Some((recipe.inner, inner_inputs)),
                                ));
                            }
                        }
                    }
                }
                if cone.gates.len() >= 5 {
                    continue;
                }
                for &leaf in &cone.leaves {
                    if cones.len() >= 48 {
                        break;
                    }
                    let node = &net.nodes[leaf];
                    let Some((d, _)) = node.driver else {
                        continue;
                    };
                    let gate = &net.gates[d];
                    let c = &lib.cells[gate.cell];
                    if !gate.active
                        || !c.comb
                        || node.protected
                        || node.state_alias
                        || node.outputs > 0
                        || node.sinks.len() > 4
                        || cone.gates.contains(&d)
                    {
                        continue;
                    }
                    // Keep the logical state wire when a sequential output needs inversion.
                    if !net.known_state_aliases
                        && c.inputs.len() == 1
                        && gate.inputs.iter().any(|&n| {
                            net.nodes[n]
                                .driver
                                .is_some_and(|(s, _)| !lib.cells[net.gates[s].cell].comb)
                        })
                    {
                        continue;
                    }
                    let mut gates = cone.gates.clone();
                    gates.push(d);
                    let mut leaves: Vec<_> = gates
                        .iter()
                        .flat_map(|&id| net.gates[id].inputs.iter().copied())
                        .filter(|&n| {
                            net.nodes[n]
                                .driver
                                .is_none_or(|(driver, _)| !gates.contains(&driver))
                        })
                        .collect();
                    leaves.sort_unstable();
                    leaves.dedup();
                    if leaves.len() > 6 || !seen.insert(leaves.clone()) {
                        continue;
                    }
                    cones.push(Cone { leaves, gates });
                }
            }
            if let Some((_, cell, mut ins, removed, inner)) = best {
                changes += 1;
                for &d in &removed {
                    for &n in &net.gates[d].inputs {
                        net.nodes[n].sinks.retain(|&(s, _)| s != d);
                    }
                    if d != g {
                        net.gates[d].active = false;
                        for &(_, n) in &net.gates[d].outputs {
                            net.nodes[n].driver = None;
                        }
                        count += 1;
                    }
                }
                if let Some((cell, inputs)) = inner {
                    two_cell_rewrites += 1;
                    let out = net.nodes.len();
                    let gate = net.gates.len();
                    net.nodes.push(Node::default());
                    net.refs.push(NlRef::Net(String::new()));
                    net.gates.push(Gate {
                        active: true,
                        cell,
                        inputs,
                        outputs: vec![(lib.cells[cell].outputs[0], out)],
                    });
                    net.nodes[out].driver = Some((gate, lib.cells[cell].outputs[0]));
                    for (pin, &n) in net.gates[gate].inputs.iter().enumerate() {
                        net.nodes[n].sinks.push((gate, pin));
                    }
                    for n in &mut ins {
                        if *n == usize::MAX {
                            *n = out;
                        }
                    }
                    count = count.saturating_sub(1);
                }
                net.gates[g].inputs = ins;
                net.change_cell(g, cell, lib);
                for (pin, &n) in net.gates[g].inputs.iter().enumerate() {
                    net.nodes[n].sinks.push((g, pin));
                }
            }
        }
        net.rebuild(lib).map_err(JobError::Failed)?;
        if changes == 0 || round == 1 {
            break;
        }
    }
    Ok((count, two_cell_rewrites))
}
