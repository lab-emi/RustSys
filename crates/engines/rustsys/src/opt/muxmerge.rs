// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `mux_merge`: merges nested multiplexers that share data inputs (SYNTH-REQ-0406).

use super::{design_roots, set_roots, sweep};
use crate::aig::{Aig, Lit};
use crate::elab::Design;

pub fn mux_merge(d: &mut Design) {
    let backup = d.clone();
    let aig = &backup.aig;
    let n = aig.num_nodes();
    let mut fanout = vec![0u32; n];
    for id in 1..n as u32 {
        if aig.is_and(id) {
            let (a, b) = aig.fanins(id);
            fanout[a.node() as usize] += 1;
            fanout[b.node() as usize] += 1;
        }
    }
    let roots = design_roots(&backup);
    for r in &roots {
        fanout[r.node() as usize] += 2;
    }
    // A multiplexer whose top node and both AND nodes below it feed nothing else.
    let private = |l: Lit| -> bool {
        let m = l.node();
        if !aig.is_and(m) || fanout[m as usize] != 1 {
            return false;
        }
        let (p, q) = aig.fanins(m);
        fanout[p.node() as usize] == 1 && fanout[q.node() as usize] == 1
    };
    // The merge candidate of a node and the select term it creates.
    let candidate = |id: u32| -> Option<(Merge, (Lit, Lit, Lit))> {
        let readings = aig.mux_readings(Lit::new(id, true));
        for &(s, t, e) in &readings {
            if private(t) {
                if let Some(&(s2, _, _)) = aig.mux_readings(t).iter().find(|&&(_, _, e2)| e2 == e) {
                    return Some((Merge::Nested(s, t, e), (s.min(s2), s.max(s2), Lit::TRUE)));
                }
            }
            if private(e) {
                if let Some(&(s2, _, _)) = aig.mux_readings(e).iter().find(|&&(_, t2, _)| t2 == t) {
                    return Some((Merge::Nested(s, t, e), (s.min(s2), s.max(s2), Lit::FALSE)));
                }
            }
        }
        // `r ? (s ? a : b) : (u ? a : b) = (r ? s : u) ? a : b`.
        for &(r, t, e) in &readings {
            if !private(t) || !private(e) {
                continue;
            }
            for (s2, a, b) in aig.mux_readings(t) {
                if let Some(&(u, _, _)) = aig
                    .mux_readings(e)
                    .iter()
                    .find(|&&(_, a2, b2)| a2 == a && b2 == b)
                {
                    return Some((Merge::SameData(r, s2, u, a, b), (r, s2, u)));
                }
            }
        }
        None
    };
    // Only select terms that serve at least two multiplexers (the bits of one word, typically)
    // are created; a term of a single bit gains nothing over the nested form.
    let cone = aig.cone(&roots);
    let mut uses: rustc_hash::FxHashMap<(Lit, Lit, Lit), u32> = rustc_hash::FxHashMap::default();
    let mut cand = vec![None; n];
    for &id in &cone {
        if aig.is_and(id) {
            if let Some((r, key)) = candidate(id) {
                *uses.entry(key).or_default() += 1;
                cand[id as usize] = Some((r, key));
            }
        }
    }
    let mut ng = Aig::new();
    let mut img = vec![Lit::FALSE; n];
    for &i in aig.inputs() {
        img[i as usize] = ng.input();
    }
    let im = |img: &[Lit], l: Lit| img[l.node() as usize].xor(l.is_compl());
    for &id in &cone {
        if !aig.is_and(id) {
            continue;
        }
        img[id as usize] = match cand[id as usize] {
            Some((Merge::Nested(s, t, e), key)) if uses[&key] >= 2 => {
                let (s, t, e) = (im(&img, s), im(&img, t), im(&img, e));
                !ng.mux_merging(s, t, e)
            }
            Some((Merge::SameData(r, s, u, a, b), key)) if uses[&key] >= 2 => {
                let (r, s, u) = (im(&img, r), im(&img, s), im(&img, u));
                let sel = ng.mux(r, s, u);
                let (a, b) = (im(&img, a), im(&img, b));
                !ng.mux_merging(sel, a, b)
            }
            _ => {
                let (a, b) = aig.fanins(id);
                let (a, b) = (im(&img, a), im(&img, b));
                ng.and(a, b)
            }
        };
    }
    let new_roots: Vec<Lit> = roots.iter().map(|&r| im(&img, r)).collect();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in p.bits.iter_mut() {
            *b = im(&img, *b);
        }
    }
    for s in d.states.iter_mut() {
        s.q = im(&img, s.q);
    }
    d.aig = ng;
    set_roots(d, &new_roots);
    sweep(d);
    if d.aig.num_ands() > backup.aig.num_ands() {
        *d = backup;
    }
}

/// How a multiplexer merges with the multiplexers on its data inputs.
#[derive(Debug, Clone, Copy)]
enum Merge {
    /// `s ? t : e` with a data input that is a multiplexer sharing the other data input.
    Nested(Lit, Lit, Lit),
    /// `r ? (s ? a : b) : (u ? a : b)`.
    SameData(Lit, Lit, Lit, Lit, Lit),
}
