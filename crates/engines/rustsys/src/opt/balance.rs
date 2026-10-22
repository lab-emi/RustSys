// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `balance`: rebuilds multi-input AND and exclusive-OR trees by level to reduce depth
//! (SYNTH-REQ-0411).

use rayon::prelude::*;
use smallvec::SmallVec;

use super::parallel::{WorkLimits, WorkStats};
use super::{design_roots, set_roots, sweep};
use crate::aig::{Aig, Lit};
use crate::elab::Design;

/// Collects the leaves of the maximal AND tree rooted at `n` (through uncomplemented AND fanins
/// with a single fanout).
const MAX_LEAVES: usize = 256;
const MAX_VISITS: usize = 512;
type Leaves = SmallVec<[Lit; 2]>;

/// `(x, y)` with `l == x ^ y` when `l` has the structure of an exclusive OR.
fn as_xor(aig: &Aig, l: Lit) -> Option<(Lit, Lit)> {
    if !aig.is_and(l.node()) {
        return None;
    }
    let (p, q) = aig.fanins(l.node());
    if !p.is_compl() || !q.is_compl() || !aig.is_and(p.node()) || !aig.is_and(q.node()) {
        return None;
    }
    let (a0, a1) = aig.fanins(p.node());
    let (b0, b1) = aig.fanins(q.node());
    for (s, t) in [(a0, a1), (a1, a0)] {
        for (r, e) in [(b0, b1), (b1, b0)] {
            if r == !s && t == !e {
                return Some((s, if l.is_compl() { e } else { !e }));
            }
        }
    }
    None
}

/// An exclusive OR, used as an operand of another exclusive OR (which reads each operand twice),
/// whose three AND nodes feed nothing else.
fn private_xor(aig: &Aig, l: Lit, fanout: &[u32]) -> Option<(Lit, Lit)> {
    let n = l.node();
    if !aig.is_and(n) || fanout[n as usize] != 2 {
        return None;
    }
    let (p, q) = aig.fanins(n);
    if fanout[p.node() as usize] != 1 || fanout[q.node() as usize] != 1 {
        return None;
    }
    as_xor(aig, l)
}

/// Collects the operands of the maximal exclusive-OR tree of `x ^ y` through private exclusive
/// ORs; complements are folded into `parity`.
fn collect(
    aig: &Aig,
    (x, y): (Lit, Lit),
    fanout: &[u32],
    xor: bool,
    budget: usize,
) -> Option<(Leaves, bool)> {
    let mut stack: SmallVec<[Lit; 16]> = smallvec::smallvec![y, x];
    let mut out = Leaves::new();
    let mut parity = false;
    let mut visits = 0;
    while let Some(l) = stack.pop() {
        visits += 1;
        if visits > budget || out.len() >= MAX_LEAVES {
            return None;
        }
        let pair = if out.len() >= 64 {
            None
        } else if xor {
            private_xor(aig, l.regular(), fanout)
        } else if !l.is_compl() && aig.is_and(l.node()) && fanout[l.node() as usize] == 1 {
            Some(aig.fanins(l.node()))
        } else {
            None
        };
        if let Some((a, b)) = pair {
            parity ^= xor && l.is_compl();
            // Valid AIG edges strictly decrease: no cyclic traversal or path materialization.
            debug_assert!(a.node() < l.node() && b.node() < l.node());
            stack.push(b);
            stack.push(a);
        } else {
            out.push(l);
        }
    }
    Some((out, parity))
}

struct Plan {
    leaves: Leaves,
    xor: bool,
    parity: bool,
    bounded: bool,
}

fn plan(aig: &Aig, id: u32, fanout: &[u32], budget: usize) -> Plan {
    let mut bounded = false;
    if let Some(pair) = as_xor(aig, Lit::new(id, false)) {
        if let Some((leaves, parity)) = collect(aig, pair, fanout, true, budget) {
            // Keep the shared XOR of a three-operand full adder.
            if leaves.len() > 3 {
                return Plan {
                    leaves,
                    xor: true,
                    parity,
                    bounded,
                };
            }
        } else {
            bounded = true;
        }
    }
    let pair = aig.fanins(id);
    let leaves = collect(aig, pair, fanout, false, budget).map_or_else(
        || {
            bounded = true;
            smallvec::smallvec![pair.0, pair.1]
        },
        |(leaves, _)| leaves,
    );
    Plan {
        leaves,
        xor: false,
        parity: false,
        bounded,
    }
}

/// Combines operands pairwise, shallowest first; among operands of equal level those of equal
/// polarity pair first, so that wide functions of inputs map onto cells without inverters.
fn combine(ng: &mut Aig, level: &mut Vec<u32>, mut items: Leaves, xor: bool) -> Lit {
    let op = if xor { Aig::xor } else { Aig::and };
    while items.len() > 1 {
        if !xor && items.len() == 2 {
            // AND canonicalizes its operands. Sorting this last pair cannot affect the graph.
            let r = ng.and(items[0], items[1]);
            update_levels(ng, level);
            return r;
        }
        items.sort_by(|a, b| {
            level[b.node() as usize]
                .cmp(&level[a.node() as usize])
                .then(b.is_compl().cmp(&a.is_compl()))
                .then(b.cmp(a))
        });
        let a = items.pop().expect("len > 1");
        let b = items.pop().expect("len > 1");
        let r = op(ng, a, b);
        update_levels(ng, level);
        items.push(r);
    }
    items[0]
}

/// Level bookkeeping for newly created nodes.
fn update_levels(ng: &Aig, level: &mut Vec<u32>) {
    while level.len() < ng.num_nodes() {
        let id = level.len() as u32;
        let lv = if ng.is_and(id) {
            let (x, y) = ng.fanins(id);
            1 + level[x.node() as usize].max(level[y.node() as usize])
        } else {
            0
        };
        level.push(lv);
    }
}

pub fn balance(d: &mut Design) {
    balance_with_limits(d, WorkLimits::default());
}

/// Read-only tree planning in bounded batches, followed by canonical ordered rebuilding.
/// Shared image/fanout/liveness/levels cost about 13 bytes per old node; proposals retain at
/// most 256 literals per node and 16K nodes per batch. No graph is copied per worker.
pub fn balance_with_limits(d: &mut Design, limits: WorkLimits) -> WorkStats {
    let mut stats = WorkStats::default();
    let aig = &d.aig;
    let n = aig.num_nodes();
    if n > limits.max_nodes {
        stats.node_budget_reached = true;
        return stats;
    }
    let batch = limits.batch().min(16_384);
    let bytes = n
        .saturating_mul(32)
        .saturating_add(batch * (size_of::<Plan>() + MAX_LEAVES * 4));
    if bytes > limits.max_plan_bytes {
        stats.storage_budget_reached = true;
        return stats;
    }
    let mut fanout = vec![0u32; n];
    for id in 1..n as u32 {
        if aig.is_and(id) {
            let (a, b) = aig.fanins(id);
            fanout[a.node() as usize] += 1;
            fanout[b.node() as usize] += 1;
        }
    }
    let roots = design_roots(d);
    for r in &roots {
        fanout[r.node() as usize] += 2;
    }
    // A reverse topological closure avoids sorting a materialized cone.
    let mut live = vec![false; n];
    for r in &roots {
        live[r.node() as usize] = true;
    }
    for id in (1..n as u32).rev() {
        if live[id as usize] && aig.is_and(id) {
            let (a, b) = aig.fanins(id);
            live[a.node() as usize] = true;
            live[b.node() as usize] = true;
        }
    }
    let mut ng = Aig::new();
    let mut level = vec![0u32];
    let mut img = vec![Lit::FALSE; n];
    for &i in aig.inputs() {
        let l = ng.input();
        level.push(0);
        img[i as usize] = l;
    }
    let workers = limits.workers();
    for first in (1..n).step_by(batch) {
        let nodes: Vec<_> = (first..(first + batch).min(n))
            .filter(|&id| live[id] && aig.is_and(id as u32))
            .map(|id| id as u32)
            .collect();
        let make = |&id: &u32| plan(aig, id, &fanout, limits.max_window_nodes.min(MAX_VISITS));
        let plans: Vec<_> = if workers > 1 && nodes.len() >= 512 {
            nodes
                .par_chunks(nodes.len().div_ceil(workers))
                .flat_map_iter(|ids| ids.iter().map(make))
                .collect()
        } else {
            nodes.iter().map(make).collect()
        };
        for (id, p) in nodes.into_iter().zip(plans) {
            if ng.num_nodes().saturating_add(MAX_LEAVES * 3)
                > limits
                    .max_nodes
                    .min(n.saturating_mul(4).max(MAX_LEAVES * 4))
            {
                stats.node_budget_reached = true;
                return stats;
            }
            stats.windows += 1;
            stats.bounded_windows += u64::from(p.bounded);
            let items = p
                .leaves
                .iter()
                .map(|l| img[l.node() as usize].xor(l.is_compl()))
                .collect();
            let r = combine(&mut ng, &mut level, items, p.xor);
            img[id as usize] = r.xor(p.parity);
        }
    }
    let new_roots: Vec<Lit> = roots
        .iter()
        .map(|r| img[r.node() as usize].xor(r.is_compl()))
        .collect();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in p.bits.iter_mut() {
            *b = img[b.node() as usize];
        }
    }
    for s in d.states.iter_mut() {
        s.q = img[s.q.node() as usize];
    }
    d.aig = ng;
    set_roots(d, &new_roots);
    sweep(d);
    stats
}
