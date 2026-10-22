// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Structural area compaction under a binding area limit: De Morgan two-cell merge with
//! shared or dual-output state inversions, mux-to-AOI/AO decomposition, sum-of-products
//! tree repacking, and oversized-driver splitting.

use rustc_hash::{FxHashMap, FxHashSet};
use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::verilog::write::NlRef;

use super::{
    Options, Report,
    model::{CellModel, Model},
    network::{Gate, Network, Node},
    polarity,
    rules::Limits,
    timing::Timing,
};

fn same_state(old: &CellModel<'_>, new: &CellModel<'_>) -> bool {
    let a = &old.raw.sequentials[0];
    let b = &new.raw.sequentials[0];
    a.kind == b.kind
        && a.data == b.data
        && a.clock == b.clock
        && a.clocked_on_also == b.clocked_on_also
        && a.clear == b.clear
        && a.preset == b.preset
        && a.clear_preset_var1 == b.clear_preset_var1
        && a.clear_preset_var2 == b.clear_preset_var2
}

fn alloc_node(net: &mut Network) -> usize {
    let id = net.nodes.len();
    net.nodes.push(Node::default());
    net.refs.push(NlRef::Net(String::new()));
    id
}

fn emit_match(net: &mut Network, lib: &Model<'_>, m: (usize, [u8; 6]), vars: &[usize], out: usize) {
    let (cell, perm) = m;
    let inputs = perm[..vars.len()]
        .iter()
        .map(|&i| vars[i as usize])
        .collect();
    net.gates.push(Gate {
        active: true,
        cell,
        inputs,
        outputs: vec![(lib.cells[cell].outputs[0], out)],
    });
}

fn set_match(net: &mut Network, lib: &Model<'_>, g: usize, m: (usize, [u8; 6]), vars: &[usize]) {
    let (cell, perm) = m;
    net.gates[g].cell = cell;
    net.gates[g].outputs[0].0 = lib.cells[cell].outputs[0];
    net.gates[g].inputs = perm[..vars.len()]
        .iter()
        .map(|&i| vars[i as usize])
        .collect();
}

fn free_neg_node(net: &Network, lib: &Model<'_>, n: usize) -> Option<usize> {
    if let Some((d, _)) = net.nodes[n].driver {
        let dg = &net.gates[d];
        let dc = &lib.cells[dg.cell];
        if dg.active && dc.comb && dg.inputs.len() == 1 && dc.truth == Some(1) {
            return Some(dg.inputs[0]);
        }
        if dg.active
            && dc.clock.is_some()
            && dg.outputs.len() == 2
            && let Some(&(_, other)) = dg.outputs.iter().find(|&&(_, m)| m != n)
        {
            return Some(other);
        }
    }
    if let Some(out) = net.nodes[n].sinks.iter().find_map(|&(s, _)| {
        let sg = &net.gates[s];
        let sc = &lib.cells[sg.cell];
        (sg.active
            && sc.comb
            && sg.inputs.len() == 1
            && sc.truth == Some(1)
            && !net.nodes[sg.outputs[0].1].protected)
            .then(|| sg.outputs[0].1)
    }) {
        return Some(out);
    }
    if let Some((d, _)) = net.nodes[n].driver {
        let dg = &net.gates[d];
        let dc = &lib.cells[dg.cell];
        if dg.active && dc.comb && dg.inputs.len() == 1 && dc.truth == Some(2) {
            return free_neg_node(net, lib, dg.inputs[0]);
        }
    }
    None
}

struct DeMorganCandidate {
    root: usize,
    inner: usize,
    pos: usize,
    u: usize,
    v: usize,
    new_cell: usize,
    perm: [u8; 6],
    gain: f64,
}

/// Replaces `root(..., inner(u, v), ...)` with a single `(k + 1)`-input cell over `(!u, !v)`
/// when the combined cell savings exceed the amortized cost of providing `!u` and `!v`.
fn demorgan_mux(net: &mut Network, lib: &Model<'_>) -> Result<usize, String> {
    let Some(&(inv_cell, _)) = lib.matching.get(&(1, 0b01)).and_then(|v| v.first()) else {
        return Ok(0);
    };
    let inv_area = lib.cells[inv_cell].raw.area;
    let dual_seq: Vec<Option<usize>> = lib
        .cells
        .iter()
        .map(|old| {
            old.clock?;
            lib.cells
                .iter()
                .enumerate()
                .filter(|(_, new)| {
                    new.clock.is_some()
                        && new.q_pins.iter().all(Option::is_some)
                        && same_state(old, new)
                        && old.inputs.len() == new.inputs.len()
                        && old
                            .inputs
                            .iter()
                            .zip(&new.inputs)
                            .all(|(&a, &b)| old.raw.pins[a].name == new.raw.pins[b].name)
                        && new.raw.area <= old.raw.area + inv_area + 1e-8
                })
                .min_by(|(i, a), (j, b)| a.raw.area.total_cmp(&b.raw.area).then(i.cmp(j)))
                .map(|(i, _)| i)
        })
        .collect();

    let mut candidates = Vec::new();
    let mut claimed_inners = FxHashSet::default();
    for root in 0..net.gates.len() {
        let rg = &net.gates[root];
        let rc = &lib.cells[rg.cell];
        let k = rg.inputs.len();
        if !rg.active
            || !rc.comb
            || !(2..=5).contains(&k)
            || rg.outputs.len() != 1
            || net.nodes[rg.outputs[0].1].protected
        {
            continue;
        }
        let root_tt = rc.truth.expect("combinational truth table");
        let mut best: Option<DeMorganCandidate> = None;
        for pos in 0..k {
            let mid = rg.inputs[pos];
            let node = &net.nodes[mid];
            if node.protected || node.state_alias || node.outputs > 0 || node.sinks.len() != 1 {
                continue;
            }
            let Some((inner, _)) = node.driver else {
                continue;
            };
            if inner == root || claimed_inners.contains(&inner) {
                continue;
            }
            let ig = &net.gates[inner];
            let ic = &lib.cells[ig.cell];
            if !ig.active || !ic.comb || ig.inputs.len() != 2 {
                continue;
            }
            let (u, v) = (ig.inputs[0], ig.inputs[1]);
            if u == v {
                continue;
            }
            let inner_tt = ic.truth.expect("combinational truth table");
            let k1 = k + 1;
            let mut new_tt = 0u64;
            for mask in 0usize..(1 << k1) {
                let u_val = ((mask >> pos) & 1) ^ 1;
                let v_val = ((mask >> k) & 1) ^ 1;
                let inner_out = ((inner_tt >> (u_val | (v_val << 1))) & 1) as usize;
                let root_in = (mask & ((1 << k) - 1) & !(1 << pos)) | (inner_out << pos);
                let root_out = (root_tt >> root_in) & 1;
                new_tt |= root_out << mask;
            }
            if k1 < 6 {
                new_tt &= (1u64 << (1 << k1)) - 1;
            }
            let Some(&(new_cell, perm)) = lib.matching.get(&(k1, new_tt)).and_then(|v| v.first())
            else {
                continue;
            };
            let gain = rc.raw.area + ic.raw.area - lib.cells[new_cell].raw.area;
            if gain > 1e-8 && best.as_ref().is_none_or(|b| gain > b.gain + 1e-8) {
                best = Some(DeMorganCandidate {
                    root,
                    inner,
                    pos,
                    u,
                    v,
                    new_cell,
                    perm,
                    gain,
                });
            }
        }
        if let Some(c) = best {
            claimed_inners.insert(c.inner);
            candidates.push(c);
        }
    }
    if candidates.is_empty() {
        return Ok(0);
    }

    let base_neg_cost = |n: usize, uses: &FxHashMap<usize, usize>| -> f64 {
        if free_neg_node(net, lib, n).is_some() {
            return 0.0;
        }
        if let Some((d, pin)) = net.nodes[n].driver {
            let dg = &net.gates[d];
            let dc = &lib.cells[dg.cell];
            if dg.active
                && !net.nodes[n].protected
                && !net.nodes[n].state_alias
                && net.nodes[n].outputs == 0
                && dc.comb
                && dg.inputs.len() == 1
                && dc.truth == Some(2)
                && uses.get(&n).copied().unwrap_or(0) == net.nodes[n].sinks.len()
            {
                return (inv_area - dc.raw.area).max(0.0);
            }
            if dg.active
                && !net.nodes[n].protected
                && dg.outputs.len() == 1
                && dc.q_pins.contains(&Some(pin))
                && let Some(dual) = dual_seq[dg.cell]
            {
                return (lib.cells[dual].raw.area - dc.raw.area).max(0.0);
            }
        }
        inv_area
    };

    let mut active = vec![true; candidates.len()];
    loop {
        let mut uses = FxHashMap::<usize, usize>::default();
        let mut sum_gain = FxHashMap::<usize, f64>::default();
        for (i, c) in candidates.iter().enumerate() {
            if active[i] {
                *uses.entry(c.u).or_default() += 1;
                *uses.entry(c.v).or_default() += 1;
                *sum_gain.entry(c.u).or_default() += c.gain;
                *sum_gain.entry(c.v).or_default() += c.gain;
            }
        }
        let mut dropped = false;
        for (i, c) in candidates.iter().enumerate() {
            if active[i] {
                let cu = base_neg_cost(c.u, &uses) * (c.gain / sum_gain[&c.u].max(1e-12));
                let cv = base_neg_cost(c.v, &uses) * (c.gain / sum_gain[&c.v].max(1e-12));
                if cu + cv >= c.gain - 1e-8 {
                    active[i] = false;
                    dropped = true;
                }
            }
        }
        if !dropped {
            break;
        }
    }

    let mut final_uses = FxHashMap::<usize, usize>::default();
    let mut needed = Vec::new();
    let mut needed_set = FxHashSet::default();
    let mut removed_inners = FxHashSet::default();
    let mut total_gain = 0.0;
    for (i, c) in candidates.iter().enumerate() {
        if active[i] {
            total_gain += c.gain;
            removed_inners.insert(c.inner);
            *final_uses.entry(c.u).or_default() += 1;
            *final_uses.entry(c.v).or_default() += 1;
            for n in [c.u, c.v] {
                if needed_set.insert(n) {
                    needed.push(n);
                }
            }
        }
    }
    let total_cost: f64 = needed.iter().map(|&n| base_neg_cost(n, &final_uses)).sum();
    if removed_inners.is_empty() || total_gain <= total_cost + 1e-8 {
        return Ok(0);
    }

    let mut neg_node = FxHashMap::<usize, usize>::default();
    for n in needed {
        if let Some(neg) = free_neg_node(net, lib, n) {
            neg_node.insert(n, neg);
            continue;
        }
        if let Some((d, pin)) = net.nodes[n].driver {
            let dg = &net.gates[d];
            let dc = &lib.cells[dg.cell];
            if dg.active
                && !net.nodes[n].protected
                && dg.outputs.len() == 1
                && let Some(phase) = dc.q_pins.iter().position(|p| *p == Some(pin))
                && let Some(dual) = dual_seq[dg.cell]
            {
                let qn = alloc_node(net);
                let nc = &lib.cells[dual];
                net.gates[d].cell = dual;
                net.gates[d].outputs = vec![
                    (nc.q_pins[phase].unwrap(), n),
                    (nc.q_pins[1 - phase].unwrap(), qn),
                ];
                neg_node.insert(n, qn);
                continue;
            }
            if dg.active
                && !net.nodes[n].protected
                && !net.nodes[n].state_alias
                && net.nodes[n].outputs == 0
                && dc.comb
                && dg.inputs.len() == 1
                && dc.truth == Some(2)
                && net.nodes[n]
                    .sinks
                    .iter()
                    .all(|&(s, _)| removed_inners.contains(&s))
            {
                net.gates[d].cell = inv_cell;
                net.gates[d].outputs[0].0 = lib.cells[inv_cell].outputs[0];
                neg_node.insert(n, n);
                continue;
            }
        }
        let inv_out = alloc_node(net);
        net.gates.push(Gate {
            active: true,
            cell: inv_cell,
            inputs: vec![n],
            outputs: vec![(lib.cells[inv_cell].outputs[0], inv_out)],
        });
        neg_node.insert(n, inv_out);
    }

    let mut count = 0;
    for (i, c) in candidates.iter().enumerate() {
        if !active[i] {
            continue;
        }
        let mut vars = net.gates[c.root].inputs.clone();
        vars[c.pos] = neg_node[&c.u];
        vars.push(neg_node[&c.v]);
        set_match(net, lib, c.root, (c.new_cell, c.perm), &vars);
        net.gates[c.inner].active = false;
        count += 1;
    }
    net.rebuild(lib)?;
    Ok(count)
}

/// Replaces 3-input `mux2(a0, a1, s)` / `mux2i(a0, a1, s)` cells with 4-input `a22o` / `a22oi` /
/// `o22ai` cells when sharing `!s` across a bus reduces total cell area.
fn rewrite_mux2(net: &mut Network, lib: &Model<'_>) -> Result<usize, String> {
    let tt_a22o = (0..16usize).fold(0u64, |acc, x| {
        let v = ((x & 3) == 3) || (((x >> 2) & 3) == 3);
        acc | (u64::from(v) << x)
    });
    let tt_a22oi = (0..16usize).fold(0u64, |acc, x| {
        let v = ((x & 3) == 3) || (((x >> 2) & 3) == 3);
        acc | (u64::from(!v) << x)
    });
    let tt_o22ai = (0..16usize).fold(0u64, |acc, x| {
        let v = ((x & 3) != 0) && (((x >> 2) & 3) != 0);
        acc | (u64::from(!v) << x)
    });
    let (Some(&(inv_cell, _)), Some(&m_a22o), Some(&m_a22oi), Some(&m_o22ai)) = (
        lib.matching.get(&(1, 0b01)).and_then(|v| v.first()),
        lib.matching.get(&(4, tt_a22o)).and_then(|v| v.first()),
        lib.matching.get(&(4, tt_a22oi)).and_then(|v| v.first()),
        lib.matching.get(&(4, tt_o22ai)).and_then(|v| v.first()),
    ) else {
        return Ok(0);
    };
    let inv_area = lib.cells[inv_cell].raw.area;

    struct MuxCand {
        gate: usize,
        s: usize,
        a0: usize,
        a1: usize,
        inverted: bool,
        free_a0_a1: Option<(usize, usize)>,
        gain: f64,
    }

    let mut by_select: FxHashMap<usize, Vec<MuxCand>> = FxHashMap::default();
    let mut select_order = Vec::new();
    for g in 0..net.gates.len() {
        let gate = &net.gates[g];
        let c = &lib.cells[gate.cell];
        if !gate.active
            || !c.comb
            || gate.inputs.len() != 3
            || gate.outputs.len() != 1
            || net.nodes[gate.outputs[0].1].protected
        {
            continue;
        }
        let tt = c.truth.expect("comb") & 0xff;
        let mut matched = None;
        for s_idx in 0..3usize {
            let others: Vec<usize> = (0..3).filter(|&i| i != s_idx).collect();
            for (i0, i1) in [(others[0], others[1]), (others[1], others[0])] {
                let exp_pos = (0..8usize).fold(0u64, |acc, x| {
                    let s = ((x >> s_idx) & 1) != 0;
                    let a0 = ((x >> i0) & 1) != 0;
                    let a1 = ((x >> i1) & 1) != 0;
                    let out = if s { a1 } else { a0 };
                    acc | (u64::from(out) << x)
                });
                if tt == exp_pos {
                    matched = Some((gate.inputs[s_idx], gate.inputs[i0], gate.inputs[i1], false));
                    break;
                }
                if tt == (exp_pos ^ 0xff) {
                    matched = Some((gate.inputs[s_idx], gate.inputs[i0], gate.inputs[i1], true));
                    break;
                }
            }
            if matched.is_some() {
                break;
            }
        }
        let Some((s, a0, a1, inverted)) = matched else {
            continue;
        };
        let (free_a0_a1, new_area) = if inverted {
            (None, lib.cells[m_a22oi.0].raw.area)
        } else if let (Some(na0), Some(na1)) =
            (free_neg_node(net, lib, a0), free_neg_node(net, lib, a1))
        {
            (Some((na0, na1)), lib.cells[m_o22ai.0].raw.area)
        } else {
            (None, lib.cells[m_a22o.0].raw.area)
        };
        let gain = c.raw.area - new_area;
        if gain <= 1e-8 {
            continue;
        }
        if !by_select.contains_key(&s) {
            select_order.push(s);
        }
        by_select.entry(s).or_default().push(MuxCand {
            gate: g,
            s,
            a0,
            a1,
            inverted,
            free_a0_a1,
            gain,
        });
    }

    let mut count = 0;
    for s in select_order {
        let cands = &by_select[&s];
        let total_gain: f64 = cands.iter().map(|c| c.gain).sum();
        let existing_ns = free_neg_node(net, lib, s);
        let cost = if existing_ns.is_some() { 0.0 } else { inv_area };
        if total_gain <= cost + 1e-8 {
            continue;
        }
        let ns = existing_ns.unwrap_or_else(|| {
            let out = alloc_node(net);
            net.gates.push(Gate {
                active: true,
                cell: inv_cell,
                inputs: vec![s],
                outputs: vec![(lib.cells[inv_cell].outputs[0], out)],
            });
            out
        });
        for c in cands {
            if c.inverted {
                set_match(net, lib, c.gate, m_a22oi, &[c.a0, ns, c.a1, c.s]);
            } else if let Some((na0, na1)) = c.free_a0_a1 {
                set_match(net, lib, c.gate, m_o22ai, &[c.s, na0, ns, na1]);
            } else {
                set_match(net, lib, c.gate, m_a22o, &[c.a0, ns, c.a1, c.s]);
            }
            count += 1;
        }
    }
    if count > 0 {
        net.rebuild(lib)?;
    }
    Ok(count)
}

fn f1_driver(net: &Network, n: usize) -> Option<usize> {
    let node = &net.nodes[n];
    if node.protected || node.state_alias || node.outputs > 0 || node.sinks.len() != 1 {
        return None;
    }
    let (g, _) = node.driver?;
    net.gates[g].active.then_some(g)
}

fn as_nand(net: &Network, lib: &Model<'_>, g: usize) -> Option<usize> {
    let gate = &net.gates[g];
    let c = &lib.cells[gate.cell];
    let k = gate.inputs.len();
    (gate.active && c.comb && (2..=4).contains(&k) && c.truth == Some((1u64 << ((1 << k) - 1)) - 1))
        .then_some(k)
}

fn as_a22_pol(
    net: &Network,
    lib: &Model<'_>,
    g: usize,
    invert: bool,
) -> Option<[(usize, usize); 2]> {
    let gate = &net.gates[g];
    let c = &lib.cells[gate.cell];
    if !gate.active || !c.comb || gate.inputs.len() != 4 {
        return None;
    }
    let tt = c.truth? & 0xffff;
    for ([p0, p1], [q0, q1]) in [([0, 1], [2, 3]), ([0, 2], [1, 3]), ([0, 3], [1, 2])] {
        let expected = (0..16usize).fold(0u64, |acc, x| {
            let a = ((x >> p0) & (x >> p1) & 1) != 0;
            let b = ((x >> q0) & (x >> q1) & 1) != 0;
            let v = if invert { !(a || b) } else { a || b };
            acc | (u64::from(v) << x)
        });
        if tt == expected {
            let ins = &gate.inputs;
            return Some([(ins[p0], ins[p1]), (ins[q0], ins[q1])]);
        }
    }
    None
}

fn as_a22oi(net: &Network, lib: &Model<'_>, g: usize) -> Option<[(usize, usize); 2]> {
    as_a22_pol(net, lib, g, true)
}

fn as_a22o(net: &Network, lib: &Model<'_>, g: usize) -> Option<[(usize, usize); 2]> {
    as_a22_pol(net, lib, g, false)
}

fn as_a221_pol(
    net: &Network,
    lib: &Model<'_>,
    g: usize,
    invert: bool,
) -> Option<([(usize, usize); 2], usize)> {
    let gate = &net.gates[g];
    let c = &lib.cells[gate.cell];
    if !gate.active || !c.comb || gate.inputs.len() != 5 {
        return None;
    }
    let tt = c.truth? & 0xffff_ffff;
    for c_idx in 0..5usize {
        let rem: Vec<usize> = (0..5).filter(|&i| i != c_idx).collect();
        for ([a0, a1], [b0, b1]) in [([0, 1], [2, 3]), ([0, 2], [1, 3]), ([0, 3], [1, 2])] {
            let (p0, p1, q0, q1) = (rem[a0], rem[a1], rem[b0], rem[b1]);
            let expected = (0..32usize).fold(0u64, |acc, x| {
                let a = ((x >> p0) & (x >> p1) & 1) != 0;
                let b = ((x >> q0) & (x >> q1) & 1) != 0;
                let c_val = ((x >> c_idx) & 1) != 0;
                let v = if invert {
                    !(a || b || c_val)
                } else {
                    a || b || c_val
                };
                acc | (u64::from(v) << x)
            });
            if tt == expected {
                let ins = &gate.inputs;
                return Some(([(ins[p0], ins[p1]), (ins[q0], ins[q1])], ins[c_idx]));
            }
        }
    }
    None
}

fn as_a221o(net: &Network, lib: &Model<'_>, g: usize) -> Option<([(usize, usize); 2], usize)> {
    as_a221_pol(net, lib, g, false)
}

fn as_a221oi(net: &Network, lib: &Model<'_>, g: usize) -> Option<([(usize, usize); 2], usize)> {
    as_a221_pol(net, lib, g, true)
}

fn as_a32o(net: &Network, lib: &Model<'_>, g: usize) -> Option<([usize; 3], (usize, usize))> {
    let gate = &net.gates[g];
    let c = &lib.cells[gate.cell];
    if !gate.active || !c.comb || gate.inputs.len() != 5 {
        return None;
    }
    let tt = c.truth? & 0xffff_ffff;
    for b0 in 0..5usize {
        for b1 in (b0 + 1)..5usize {
            let a_idx: Vec<usize> = (0..5).filter(|&i| i != b0 && i != b1).collect();
            let expected = (0..32usize).fold(0u64, |acc, x| {
                let a = a_idx.iter().all(|&i| ((x >> i) & 1) != 0);
                let b = ((x >> b0) & (x >> b1) & 1) != 0;
                acc | (u64::from(a || b) << x)
            });
            if tt == expected {
                let ins = &gate.inputs;
                return Some((
                    [ins[a_idx[0]], ins[a_idx[1]], ins[a_idx[2]]],
                    (ins[b0], ins[b1]),
                ));
            }
        }
    }
    None
}

fn as_o41ai(net: &Network, lib: &Model<'_>, g: usize) -> Option<(usize, [usize; 4])> {
    let gate = &net.gates[g];
    let c = &lib.cells[gate.cell];
    if !gate.active || !c.comb || gate.inputs.len() != 5 {
        return None;
    }
    let tt = c.truth? & 0xffff_ffff;
    for b_idx in 0..5usize {
        let others: Vec<usize> = (0..5).filter(|&i| i != b_idx).collect();
        let expected = (0..32usize).fold(0u64, |acc, x| {
            let any_a = others.iter().any(|&i| ((x >> i) & 1) != 0);
            let b = ((x >> b_idx) & 1) != 0;
            acc | (u64::from(!(any_a && b)) << x)
        });
        if tt == expected {
            let ins = &gate.inputs;
            return Some((
                ins[b_idx],
                [
                    ins[others[0]],
                    ins[others[1]],
                    ins[others[2]],
                    ins[others[3]],
                ],
            ));
        }
    }
    None
}

fn strip_buffers(net: &Network, lib: &Model<'_>, mut n: usize) -> usize {
    while let Some((g, _)) = net.nodes[n].driver {
        let gate = &net.gates[g];
        let c = &lib.cells[gate.cell];
        if gate.active && c.comb && gate.inputs.len() == 1 && c.truth == Some(2) {
            n = gate.inputs[0];
        } else {
            break;
        }
    }
    n
}

fn is_neg_of(net: &Network, lib: &Model<'_>, a: usize, b: usize) -> bool {
    let a = strip_buffers(net, lib, a);
    let b = strip_buffers(net, lib, b);
    free_neg_node(net, lib, a).is_some_and(|na| strip_buffers(net, lib, na) == b)
        || free_neg_node(net, lib, b).is_some_and(|nb| strip_buffers(net, lib, nb) == a)
}

fn collect_leaf(
    net: &Network,
    lib: &Model<'_>,
    ln: usize,
    old_gates: &mut Vec<usize>,
    and2_terms: &mut Vec<(usize, usize)>,
    new_nand3s: &mut Vec<[usize; 3]>,
    other_low: &mut Vec<usize>,
) {
    if let Some(lg) = f1_driver(net, ln) {
        if let Some(pairs) = as_a22oi(net, lib, lg) {
            old_gates.push(lg);
            and2_terms.extend_from_slice(&pairs);
            return;
        }
        if let Some((pairs, c1)) = as_a221oi(net, lib, lg)
            && let Some(c1g) = f1_driver(net, c1)
        {
            if let Some(pairs2) = as_a22o(net, lib, c1g) {
                old_gates.extend([lg, c1g]);
                and2_terms.extend_from_slice(&pairs);
                and2_terms.extend_from_slice(&pairs2);
                return;
            }
            if let Some((and3, pair2)) = as_a32o(net, lib, c1g) {
                old_gates.extend([lg, c1g]);
                and2_terms.extend_from_slice(&pairs);
                and2_terms.push(pair2);
                new_nand3s.push(and3);
                return;
            }
        }
    }
    other_low.push(ln);
}

/// Repacks 4-input AOI22 leaves feeding NAND/OAI reduction trees into 6-input AOI222 leaves.
fn repack_sop(net: &mut Network, lib: &Model<'_>) -> Result<usize, String> {
    let tt_a222oi = (0..64usize).fold(0u64, |acc, x| {
        let v = ((x & 3) == 3) || (((x >> 2) & 3) == 3) || (((x >> 4) & 3) == 3);
        acc | (u64::from(!v) << x)
    });
    let tt_a22oi = (0..16usize).fold(0u64, |acc, x| {
        let v = ((x & 3) == 3) || (((x >> 2) & 3) == 3);
        acc | (u64::from(!v) << x)
    });
    let tt_o31ai = (0..16usize).fold(0u64, |acc, x| {
        let v = ((x & 7) != 0) && ((x & 8) != 0);
        acc | (u64::from(!v) << x)
    });
    let tt_o41ai = (0..32usize).fold(0u64, |acc, x| {
        let v = ((x & 15) != 0) && ((x & 16) != 0);
        acc | (u64::from(!v) << x)
    });
    let (
        Some(&m_a222oi),
        Some(&m_a22oi),
        Some(&m_o31ai),
        Some(&m_o41ai),
        Some(&m_nand4),
        Some(&m_nand3),
        Some(&m_nand2),
        Some(&m_inv),
    ) = (
        lib.matching.get(&(6, tt_a222oi)).and_then(|v| v.first()),
        lib.matching.get(&(4, tt_a22oi)).and_then(|v| v.first()),
        lib.matching.get(&(4, tt_o31ai)).and_then(|v| v.first()),
        lib.matching.get(&(5, tt_o41ai)).and_then(|v| v.first()),
        lib.matching.get(&(4, 0x7fff)).and_then(|v| v.first()),
        lib.matching.get(&(3, 0x7f)).and_then(|v| v.first()),
        lib.matching.get(&(2, 0x7)).and_then(|v| v.first()),
        lib.matching.get(&(1, 0x1)).and_then(|v| v.first()),
    )
    else {
        return Ok(0);
    };
    let nand_match = [m_inv, m_inv, m_nand2, m_nand3, m_nand4];
    let mut repacked = 0;

    let gate_count = net.gates.len();
    for root in 0..gate_count {
        if net.gates[root].outputs.len() != 1 || net.nodes[net.gates[root].outputs[0].1].protected {
            continue;
        }
        let Some((b1, branches)) = as_o41ai(net, lib, root) else {
            continue;
        };
        let mut old_gates = vec![root];
        let mut and2_terms = Vec::new();
        let mut new_nand3s = Vec::new();
        let mut other_low = Vec::new();
        let mut valid = true;
        for br in branches {
            let Some(cg) = f1_driver(net, br) else {
                valid = false;
                break;
            };
            if as_nand(net, lib, cg).is_some() {
                old_gates.push(cg);
                for &ln in &net.gates[cg].inputs {
                    collect_leaf(
                        net,
                        lib,
                        ln,
                        &mut old_gates,
                        &mut and2_terms,
                        &mut new_nand3s,
                        &mut other_low,
                    );
                }
            } else if let Some((pairs, c1)) = as_a221o(net, lib, cg)
                && let Some(c1g) = f1_driver(net, c1)
                && as_nand(net, lib, c1g).is_some()
            {
                old_gates.extend([cg, c1g]);
                and2_terms.extend_from_slice(&pairs);
                for &ln in &net.gates[c1g].inputs {
                    collect_leaf(
                        net,
                        lib,
                        ln,
                        &mut old_gates,
                        &mut and2_terms,
                        &mut new_nand3s,
                        &mut other_low,
                    );
                }
            } else {
                valid = false;
                break;
            }
        }
        if !valid || and2_terms.len() < 12 {
            continue;
        }
        // Any product term (u & !b1) inside ~((sum) & b1) is combinationally redundant.
        and2_terms.retain(|&(u, v)| !is_neg_of(net, lib, u, b1) && !is_neg_of(net, lib, v, b1));
        let n3 = and2_terms.len() / 3;
        let rem = and2_terms.len() % 3;
        let (n2, n1) = (usize::from(rem == 2), usize::from(rem == 1));
        let total_leaves = n3 + n2 + n1 + new_nand3s.len() + other_low.len();
        if !(6..=16).contains(&total_leaves) {
            continue;
        }
        let use_o31 = total_leaves <= 12;
        let sizes: Vec<usize> = if use_o31 {
            let s0 = total_leaves.div_ceil(3);
            let s1 = (total_leaves - s0).div_ceil(2);
            let s2 = total_leaves - s0 - s1;
            vec![s0, s1, s2]
        } else {
            let s0 = total_leaves.div_ceil(4);
            let rem1 = total_leaves - s0;
            let s1 = rem1.div_ceil(3);
            let rem2 = rem1 - s1;
            let s2 = rem2.div_ceil(2);
            let s3 = rem2 - s2;
            vec![s0, s1, s2, s3]
        };
        let root_match = if use_o31 { m_o31ai } else { m_o41ai };
        let old_area: f64 = old_gates
            .iter()
            .map(|&g| lib.cells[net.gates[g].cell].raw.area)
            .sum();
        let new_area = (n3 as f64) * lib.cells[m_a222oi.0].raw.area
            + (n2 as f64) * lib.cells[m_a22oi.0].raw.area
            + (n1 as f64) * lib.cells[m_nand2.0].raw.area
            + (new_nand3s.len() as f64) * lib.cells[m_nand3.0].raw.area
            + sizes
                .iter()
                .map(|&s| lib.cells[nand_match[s].0].raw.area)
                .sum::<f64>()
            + lib.cells[root_match.0].raw.area;
        if new_area >= old_area - 1e-8 {
            continue;
        }
        for &g in &old_gates[1..] {
            net.gates[g].active = false;
        }
        let mut low_leaves = Vec::with_capacity(total_leaves);
        for chunk in and2_terms.chunks(3) {
            let out = alloc_node(net);
            match *chunk {
                [(a1, a2), (b1, b2), (c1, c2)] => {
                    emit_match(net, lib, m_a222oi, &[a1, a2, b1, b2, c1, c2], out);
                }
                [(a1, a2), (b1, b2)] => {
                    emit_match(net, lib, m_a22oi, &[a1, a2, b1, b2], out);
                }
                [(a1, a2)] => {
                    emit_match(net, lib, m_nand2, &[a1, a2], out);
                }
                _ => unreachable!(),
            }
            low_leaves.push(out);
        }
        for and3 in new_nand3s {
            let out = alloc_node(net);
            emit_match(net, lib, m_nand3, &and3, out);
            low_leaves.push(out);
        }
        low_leaves.extend(other_low);
        let mut root_vars = Vec::with_capacity(sizes.len() + 1);
        let mut offset = 0;
        for size in sizes {
            let mid = alloc_node(net);
            emit_match(
                net,
                lib,
                nand_match[size],
                &low_leaves[offset..offset + size],
                mid,
            );
            root_vars.push(mid);
            offset += size;
        }
        root_vars.push(b1);
        set_match(net, lib, root, root_match, &root_vars);
        repacked += 1;
    }

    for root in 0..gate_count {
        if net.gates[root].outputs.len() != 1 || net.nodes[net.gates[root].outputs[0].1].protected {
            continue;
        }
        let Some(k) = as_nand(net, lib, root) else {
            continue;
        };
        if k < 3 {
            continue;
        }
        let mut old_leaves = Vec::new();
        let mut and2_terms = Vec::new();
        let mut new_nand3s = Vec::new();
        let mut other_low = Vec::new();
        for &ln in &net.gates[root].inputs {
            collect_leaf(
                net,
                lib,
                ln,
                &mut old_leaves,
                &mut and2_terms,
                &mut new_nand3s,
                &mut other_low,
            );
        }
        if old_leaves.len() < 3 {
            continue;
        }
        let n3 = and2_terms.len() / 3;
        let rem = and2_terms.len() % 3;
        let (n2, n1) = (usize::from(rem == 2), usize::from(rem == 1));
        let total_leaves = n3 + n2 + n1 + new_nand3s.len() + other_low.len();
        if !(2..=4).contains(&total_leaves) {
            continue;
        }
        let old_area = lib.cells[net.gates[root].cell].raw.area
            + old_leaves
                .iter()
                .map(|&g| lib.cells[net.gates[g].cell].raw.area)
                .sum::<f64>();
        let new_area = lib.cells[nand_match[total_leaves].0].raw.area
            + (n3 as f64) * lib.cells[m_a222oi.0].raw.area
            + (n2 as f64) * lib.cells[m_a22oi.0].raw.area
            + (n1 as f64) * lib.cells[m_nand2.0].raw.area
            + (new_nand3s.len() as f64) * lib.cells[m_nand3.0].raw.area;
        if new_area >= old_area - 1e-8 {
            continue;
        }
        for &g in &old_leaves {
            net.gates[g].active = false;
        }
        let mut low_leaves = Vec::with_capacity(total_leaves);
        for chunk in and2_terms.chunks(3) {
            let out = alloc_node(net);
            match *chunk {
                [(a1, a2), (b1, b2), (c1, c2)] => {
                    emit_match(net, lib, m_a222oi, &[a1, a2, b1, b2, c1, c2], out);
                }
                [(a1, a2), (b1, b2)] => {
                    emit_match(net, lib, m_a22oi, &[a1, a2, b1, b2], out);
                }
                [(a1, a2)] => {
                    emit_match(net, lib, m_nand2, &[a1, a2], out);
                }
                _ => unreachable!(),
            }
            low_leaves.push(out);
        }
        for and3 in new_nand3s {
            let out = alloc_node(net);
            emit_match(net, lib, m_nand3, &and3, out);
            low_leaves.push(out);
        }
        low_leaves.extend(other_low);
        set_match(net, lib, root, nand_match[total_leaves], &low_leaves);
        repacked += 1;
    }

    if repacked > 0 {
        net.rebuild(lib)?;
    }
    Ok(repacked)
}

/// Replaces oversized multi-input drivers (such as `_4` cells chosen across sequential corner
/// repair passes) with two or three minimum-sized clones when that reduces cell area while
/// satisfying all corner electrical rules.
pub(super) fn clone_oversized(
    net: &mut Network,
    lib: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    report: &mut Report,
) -> Result<usize, JobError<String>> {
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
    let mut cloned = 0usize;
    for g in net.order.clone() {
        let gate = &net.gates[g];
        if !gate.active || gate.outputs.len() != 1 {
            continue;
        }
        let cell = gate.cell;
        let c = &lib.cells[cell];
        if !c.comb || c.family >= lib.families.len() {
            continue;
        }
        let smallest = lib.families[c.family][0];
        let small_area = lib.cells[smallest].raw.area;
        let (_, n) = gate.outputs[0];
        let node = &net.nodes[n];
        let count = node.sinks.len();
        if node.protected || node.outputs > 0 || count < 4 || 2.0 * small_area >= c.raw.area - 1e-8
        {
            continue;
        }
        let original_inputs = gate.inputs.clone();
        if original_inputs.iter().any(|&inp| inp >= views[0].1.len()) {
            continue;
        }
        let mut selected = None;
        for copies in [1usize, 2usize] {
            let total_gates = copies + 1;
            if ((total_gates as f64) * small_area) >= c.raw.area - 1e-8 {
                continue;
            }
            let width = count.div_ceil(total_gates);
            let legal = views.iter().all(|(m, loads, fanouts, limits, t)| {
                let sc = &m.cells[smallest];
                let sop = sc.outputs[0];
                let max_load = node
                    .sinks
                    .iter()
                    .map(|&(s, i)| {
                        let scell = &m.cells[net.gates[s].cell];
                        scell.input_cap(scell.inputs[i])
                    })
                    .fold(0.0_f64, f64::max)
                    * (width as f64);
                let max_fo = node
                    .sinks
                    .iter()
                    .map(|&(s, i)| {
                        let scell = &m.cells[net.gates[s].cell];
                        scell.input_fanout(scell.inputs[i])
                    })
                    .fold(0.0_f64, f64::max)
                    * (width as f64);
                let sink_slew = node
                    .sinks
                    .iter()
                    .map(|&(s, i)| {
                        let scell = &m.cells[net.gates[s].cell];
                        scell.max_slew(scell.inputs[i])
                    })
                    .fold(sc.max_slew(sop), f64::min);
                if max_load > sc.max_cap(sop) + 1e-12 || max_fo > sc.max_fanout(sop) + 1e-8 {
                    return false;
                }
                let slew = t.slew_at_load_inputs(
                    m,
                    smallest,
                    sop,
                    max_load,
                    opts.clock_slew_ns,
                    &snapshot_inputs[g],
                );
                if slew[0].max(slew[1]) > sink_slew + 1e-10 {
                    return false;
                }
                let old_c = &m.cells[cell];
                let mut changes: Vec<(usize, f64, f64)> = Vec::new();
                for (idx, &input) in original_inputs.iter().enumerate() {
                    let dc = (total_gates as f64) * sc.input_cap(sc.inputs[idx])
                        - old_c.input_cap(old_c.inputs[idx]);
                    let df = (total_gates as f64) * sc.input_fanout(sc.inputs[idx])
                        - old_c.input_fanout(old_c.inputs[idx]);
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
                        return dc <= 0.0 && df <= 0.0;
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
                selected = Some((width, copies));
                break;
            }
        }
        let Some((width, copies)) = selected else {
            continue;
        };
        let sop = lib.cells[smallest].outputs[0];
        net.change_cell(g, smallest, lib);
        let sinks = net.nodes[n].sinks.clone();
        for chunk in sinks.chunks(width).take(copies) {
            let out = alloc_node(net);
            for &(s, pin) in chunk {
                net.gates[s].inputs[pin] = out;
            }
            net.gates.push(Gate {
                active: true,
                cell: smallest,
                inputs: original_inputs.clone(),
                outputs: vec![(sop, out)],
            });
        }
        let total_gates = copies + 1;
        for (m, loads, fanouts, _, _) in &mut views {
            let sc = &m.cells[smallest];
            let old_c = &m.cells[cell];
            for (idx, &input) in original_inputs.iter().enumerate() {
                loads[input] += (total_gates as f64) * sc.input_cap(sc.inputs[idx])
                    - old_c.input_cap(old_c.inputs[idx]);
                fanouts[input] += (total_gates as f64) * sc.input_fanout(sc.inputs[idx])
                    - old_c.input_fanout(old_c.inputs[idx]);
            }
        }
        report.rule_clones += copies;
        cloned += copies;
    }
    if cloned > 0 {
        net.rebuild(lib).map_err(JobError::Failed)?;
    }
    Ok(cloned)
}

pub(super) fn recover(
    net: &mut Network,
    lib: &Model<'_>,
    report: &mut Report,
    ctx: Ctx<'_>,
) -> Result<usize, JobError<String>> {
    ctx.checkpoint()?;
    let mux = demorgan_mux(net, lib).map_err(JobError::Failed)?;
    let mux2 = rewrite_mux2(net, lib).map_err(JobError::Failed)?;
    let folded = if mux + mux2 > 0 {
        polarity::fold(net, lib)
    } else {
        0
    };
    report.inverters_absorbed += folded;
    ctx.checkpoint()?;
    let sop = repack_sop(net, lib).map_err(JobError::Failed)?;
    report.two_cell_rewrites += mux + mux2 + sop;
    Ok(mux + mux2 + sop + folded)
}
