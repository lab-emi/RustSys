// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Canonical sweep compatibility with the prior independent closure/copy path.
//! <100K nodes per fixture, two copies, <256MiB; external full-netlist LEC is
//! additionally required before accepting a synthesis/performance claim.
mod common;
use rustc_hash::FxHashMap;
use rustsys::{
    aig::{Aig, Lit},
    elab::Design,
    opt::{self, design_roots, set_roots},
};
use rustsys_fixtures::rtl::CORPUS;

#[test]
fn dense_sweep_matches_legacy_graphs_and_state_order() {
    for fixture in CORPUS {
        let src: Vec<_> = fixture.files.iter().map(|(_, text)| *text).collect();
        let base = common::elab(&src, Some(fixture.top)).unwrap();
        assert!(base.aig.num_nodes() < 100_000);
        let mut old = base.clone();
        legacy_sweep(&mut old);
        for threads in [1, 16] {
            let mut new = base.clone();
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
                .install(|| opt::sweep(&mut new));
            assert_eq!(format!("{new:?}"), format!("{old:?}"), "{}", fixture.name);
        }
    }
}

#[test]
fn sweep_budget_exhaustion_preserves_the_entire_design() {
    let base =
        common::design("module m(input clk,d,output reg q); always @(posedge clk) q<=d; endmodule");
    for limit in [
        opt::parallel::WorkLimits {
            max_nodes: 0,
            ..Default::default()
        },
        opt::parallel::WorkLimits {
            max_plan_bytes: 0,
            ..Default::default()
        },
    ] {
        let mut d = base.clone();
        let status = opt::sweep_with_limits(&mut d, limit);
        assert!(status.node_budget_reached || status.storage_budget_reached);
        assert_eq!(format!("{d:?}"), format!("{base:?}"));
    }
}

fn legacy_sweep(d: &mut Design) {
    // Observable states by fixpoint from the primary outputs.
    let q_to_state: FxHashMap<u32, usize> = d
        .states
        .iter()
        .enumerate()
        .map(|(i, s)| (s.q.node(), i))
        .collect();
    let mut observable = vec![false; d.states.len()];
    let mut frontier: Vec<Lit> = d
        .ports
        .iter()
        .filter(|p| !p.input)
        .flat_map(|p| p.bits.iter().copied())
        .collect();
    let mut seen = vec![false; d.aig.num_nodes()];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        let mut stack: Vec<u32> = frontier.iter().map(|l| l.node()).collect();
        while let Some(n) = stack.pop() {
            if seen[n as usize] {
                continue;
            }
            seen[n as usize] = true;
            if d.aig.is_and(n) {
                let (a, b) = d.aig.fanins(n);
                stack.push(a.node());
                stack.push(b.node());
            } else if let Some(&si) = q_to_state.get(&n) {
                if !observable[si] {
                    observable[si] = true;
                    let s = &d.states[si];
                    next.push(s.d);
                    next.push(s.clk);
                    if let Some((c, _)) = s.arst {
                        next.push(c);
                    }
                }
            }
        }
        frontier = next;
    }
    let states: Vec<_> = d
        .states
        .iter()
        .zip(&observable)
        .filter(|(_, o)| **o)
        .map(|(s, _)| s.clone())
        .collect();
    d.states = states;
    // Rebuild with inputs in their original order: primary inputs, then retained state outputs.
    let mut ng = Aig::new();
    let mut map: FxHashMap<u32, Lit> = FxHashMap::default();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in p.bits.iter_mut() {
            let nl = ng.input();
            map.insert(b.node(), nl);
            *b = nl;
        }
    }
    for s in d.states.iter_mut() {
        let nl = ng.input();
        map.insert(s.q.node(), nl);
        s.q = nl;
    }
    let roots = design_roots(d);
    let new_roots = d.aig.copy_into(&mut ng, &roots, &mut |_, n| {
        *map.get(&n).expect("input of a retained cone")
    });
    d.aig = ng;
    set_roots(d, &new_roots);
}
