// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `balance`: rebuilds multi-input AND and exclusive-OR trees by level to reduce depth
//! (SYNTH-REQ-0411).

use rustc_hash::FxHashMap;

use rustsys::aig::{Aig, Lit};
use rustsys::elab::Design;
use rustsys::opt::{design_roots, set_roots, sweep};

/// Collects the leaves of the maximal AND tree rooted at `n` (through uncomplemented AND fanins
/// with a single fanout).
fn collect(aig: &Aig, n: u32, fanout: &[u32], out: &mut Vec<Lit>) {
    let (a, b) = aig.fanins(n);
    for l in [a, b] {
        if !l.is_compl() && aig.is_and(l.node()) && fanout[l.node() as usize] == 1 && out.len() < 64
        {
            collect(aig, l.node(), fanout, out);
        } else {
            out.push(l);
        }
    }
}

/// `(x, y)` with `l == x ^ y` when `l` has the structure of an exclusive OR.
fn as_xor(aig: &Aig, l: Lit) -> Option<(Lit, Lit)> {
    aig.as_mux(l)
        .into_iter()
        .find(|&(_, t, e)| t == !e)
        .map(|(s, _, e)| (s, e))
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
fn collect_xor(
    aig: &Aig,
    (x, y): (Lit, Lit),
    fanout: &[u32],
    out: &mut Vec<Lit>,
    parity: &mut bool,
) {
    for l in [x, y] {
        match private_xor(aig, l.regular(), fanout).filter(|_| out.len() < 64) {
            Some(pair) => {
                *parity ^= l.is_compl();
                collect_xor(aig, pair, fanout, out, parity);
            }
            None => out.push(l),
        }
    }
}

/// Combines operands pairwise, shallowest first; among operands of equal level those of equal
/// polarity pair first, so that wide functions of inputs map onto cells without inverters.
fn combine(
    ng: &mut Aig,
    level: &mut Vec<u32>,
    mut items: Vec<Lit>,
    op: fn(&mut Aig, Lit, Lit) -> Lit,
) -> Lit {
    while items.len() > 1 {
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

fn legacy_balance(d: &mut Design) {
    let aig = &d.aig;
    let n = aig.num_nodes();
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
    let mut ng = Aig::new();
    let mut level = vec![0u32];
    let mut img: FxHashMap<u32, Lit> = FxHashMap::default();
    img.insert(0, Lit::FALSE);
    for &i in aig.inputs() {
        let l = ng.input();
        level.push(0);
        img.insert(i, l);
    }
    for id in aig.cone(&roots) {
        if !aig.is_and(id) {
            continue;
        }
        let top = Lit::new(id, false);
        if let Some(pair) = as_xor(aig, top) {
            let (mut leaves, mut parity) = (Vec::new(), false);
            collect_xor(aig, pair, &fanout, &mut leaves, &mut parity);
            // Trees of three operands are left alone: a full adder's sum shares its first
            // exclusive OR with the carry once the carry is rewritten.
            if leaves.len() > 3 {
                let items = leaves
                    .iter()
                    .map(|l| img[&l.node()].xor(l.is_compl()))
                    .collect();
                let r = combine(&mut ng, &mut level, items, Aig::xor);
                img.insert(id, r.xor(parity));
                continue;
            }
        }
        let mut leaves = Vec::new();
        collect(aig, id, &fanout, &mut leaves);
        let items: Vec<Lit> = leaves
            .iter()
            .map(|l| img[&l.node()].xor(l.is_compl()))
            .collect();
        let r = combine(&mut ng, &mut level, items, Aig::and);
        img.insert(id, r);
    }
    let new_roots: Vec<Lit> = roots
        .iter()
        .map(|r| img[&r.node()].xor(r.is_compl()))
        .collect();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in p.bits.iter_mut() {
            *b = img[&b.node()];
        }
    }
    for s in d.states.iter_mut() {
        s.q = img[&s.q.node()];
    }
    d.aig = ng;
    set_roots(d, &new_roots);
    sweep(d);
}

// <100K nodes/fixture, two graph copies, <256MiB. External ABC checks additionally required.
mod common;
#[test]
fn bounded_parallel_balance_matches_previous_canonical_graphs() {
    for fixture in rustsys_fixtures::rtl::CORPUS {
        let sources: Vec<_> = fixture.files.iter().map(|(_, text)| *text).collect();
        let base = common::elab(&sources, Some(fixture.top)).unwrap();
        assert!(base.aig.num_nodes() < 100_000);
        let mut old = base.clone();
        legacy_balance(&mut old);
        for threads in [1, 16] {
            let mut new = base.clone();
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
                .install(|| rustsys::opt::balance::balance(&mut new));
            assert_eq!(format!("{new:?}"), format!("{old:?}"), "{}", fixture.name);
        }
    }
}

#[test]
fn balance_budget_preserves_input_and_deep_tree_is_iterative() {
    use rustsys::opt::parallel::WorkLimits;
    let base = common::design("module m(input a,b,c,output y); assign y=a&b&c; endmodule");
    for limits in [
        WorkLimits {
            max_nodes: 0,
            ..Default::default()
        },
        WorkLimits {
            max_plan_bytes: 0,
            ..Default::default()
        },
    ] {
        let mut d = base.clone();
        let stats = rustsys::opt::balance::balance_with_limits(&mut d, limits);
        assert!(stats.node_budget_reached || stats.storage_budget_reached);
        assert_eq!(format!("{d:?}"), format!("{base:?}"));
    }
    let mut d = common::design("module m(input [4095:0] a, output y); assign y=&a; endmodule");
    let before = d.clone();
    let stats = rustsys::opt::balance::balance_with_limits(
        &mut d,
        WorkLimits {
            max_window_nodes: 8,
            ..Default::default()
        },
    );
    assert!(stats.bounded_windows > 0);
    for zero in [None, Some(0), Some(1023), Some(4095)] {
        let words: Vec<_> = (0..4096)
            .map(|i| if Some(i) == zero { 0 } else { u64::MAX })
            .collect();
        let eval = |d: &Design| Aig::lit_value(&d.simulate(&words), d.port("y").unwrap().bits[0]);
        assert_eq!(eval(&d), eval(&before));
    }
}
