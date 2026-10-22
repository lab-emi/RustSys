// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Logic optimization passes (`docs/specs/synth/optimization.md`).

pub mod balance;
pub mod factor;
pub mod lib4;
pub mod merge;
pub mod muxdecode;
pub mod muxmerge;
pub mod parallel;
pub mod refactor;
pub mod reset;
pub mod resub;
pub mod rewrite;

use crate::aig::{Aig, Lit};
use crate::elab::Design;

/// All combinational outputs of a design.
pub fn design_roots(d: &Design) -> Vec<Lit> {
    let mut r = Vec::new();
    for p in d.ports.iter().filter(|p| !p.input) {
        r.extend(p.bits.iter().copied());
    }
    for s in &d.states {
        r.push(s.d);
        r.push(s.clk);
        if let Some((c, _)) = s.arst {
            r.push(c);
        }
    }
    r
}

/// Replaces the roots of a design with new literals (same order as `design_roots`).
pub fn set_roots(d: &mut Design, roots: &[Lit]) {
    let mut it = roots.iter().copied();
    for p in d.ports.iter_mut().filter(|p| !p.input) {
        for b in p.bits.iter_mut() {
            *b = it.next().expect("root");
        }
    }
    for s in d.states.iter_mut() {
        s.d = it.next().expect("root");
        s.clk = it.next().expect("root");
        if let Some((c, _)) = s.arst.as_mut() {
            *c = it.next().expect("root");
        }
    }
}

/// `sweep`: removes unobservable state bits (SYNTH-REQ-0308) and logic that no combinational
/// output depends on; renumbers the graph compactly with inputs first.
pub fn sweep(d: &mut Design) {
    sweep_with_limits(d, parallel::WorkLimits::default());
}

/// Bounded state-observability closure and canonical compaction. Marking before
/// enqueue limits pending work to one entry per node, even on reconvergent DAGs.
pub fn sweep_with_limits(d: &mut Design, limits: parallel::WorkLimits) -> parallel::WorkStats {
    let mut stats = parallel::WorkStats::default();
    let n = d.aig.num_nodes();
    if n > limits.max_nodes || d.states.len() >= u32::MAX as usize {
        stats.node_budget_reached = true;
        return stats;
    }
    let ports = d
        .ports
        .iter()
        .filter(|p| !p.input)
        .map(|p| p.bits.len() as u128)
        .sum::<u128>();
    let roots_bound = ports + 3 * d.states.len() as u128;
    // q-index, seen, worst Vec stack capacity, image, old/new root arrays.
    if n as u128 * 17 + roots_bound * 16 > limits.max_plan_bytes as u128 {
        stats.storage_budget_reached = true;
        return stats;
    }
    let mut q_to_state = vec![u32::MAX; n];
    for (i, state) in d.states.iter().enumerate() {
        q_to_state[state.q.node() as usize] = i as u32;
    }
    let mut observable = vec![false; d.states.len()];
    let mut seen = vec![false; n];
    let mut stack = Vec::new();
    let push = |lit: Lit, seen: &mut [bool], stack: &mut Vec<u32>| {
        let node = lit.node();
        if !std::mem::replace(&mut seen[node as usize], true) {
            stack.push(node);
        }
    };
    for p in d.ports.iter().filter(|p| !p.input) {
        for &b in &p.bits {
            push(b, &mut seen, &mut stack);
        }
    }
    while let Some(node) = stack.pop() {
        stats.windows += 1;
        if d.aig.is_and(node) {
            let (a, b) = d.aig.fanins(node);
            push(a, &mut seen, &mut stack);
            push(b, &mut seen, &mut stack);
        } else {
            let si = q_to_state[node as usize];
            if si != u32::MAX {
                observable[si as usize] = true;
                let s = &d.states[si as usize];
                push(s.d, &mut seen, &mut stack);
                push(s.clk, &mut seen, &mut stack);
                if let Some((c, _)) = s.arst {
                    push(c, &mut seen, &mut stack);
                }
            }
        }
    }
    drop(stack);
    drop(q_to_state);
    let mut si = 0;
    d.states.retain(|_| {
        let keep = observable[si];
        si += 1;
        keep
    });
    let mut ng = Aig::new();
    let mut image = vec![Lit(u32::MAX); n];
    image[0] = Lit::FALSE;
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in &mut p.bits {
            let next = ng.input();
            image[b.node() as usize] = next;
            *b = next;
        }
    }
    for s in &mut d.states {
        let next = ng.input();
        image[s.q.node() as usize] = next;
        s.q = next;
    }
    for node in 1..n as u32 {
        if seen[node as usize] && d.aig.is_and(node) {
            let (a, b) = d.aig.fanins(node);
            let (x, y) = (image[a.node() as usize], image[b.node() as usize]);
            assert!(
                x.0 != u32::MAX && y.0 != u32::MAX,
                "unmapped input of retained cone"
            );
            image[node as usize] = ng.and(x.xor(a.is_compl()), y.xor(b.is_compl()));
        }
    }
    let roots = design_roots(d);
    let new_roots: Vec<_> = roots
        .iter()
        .map(|r| {
            let value = image[r.node() as usize];
            assert_ne!(value.0, u32::MAX, "unmapped retained root");
            value.xor(r.is_compl())
        })
        .collect();
    d.aig = ng;
    set_roots(d, &new_roots);
    stats
}
