// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `merge`: merges nodes proven functionally equivalent (or complementary, or constant) by random
//! simulation followed by SAT (SYNTH-REQ-0411) [@kuehlmann2002robust].

use rustc_hash::{FxHashMap, FxHashSet};

use super::{design_roots, set_roots, sweep};
use crate::aig::{Aig, Lit};
use crate::elab::Design;
use crate::sat::AigCnf;

const WORDS: usize = 8;
const CONFLICTS_PER_PROOF: u64 = 500;
const MAX_PROOFS: usize = 20_000;

/// SplitMix64 [@steele2014fast].
fn mix(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub fn merge(d: &mut Design) {
    merge_with(d, false);
}

/// Bounded proofs on local differences. Shared internal nodes become independent inputs;
/// equivalence under this stronger condition implies equivalence in the original graph.
pub fn merge_local(d: &mut Design) {
    merge_with(d, true);
}

pub(super) fn local_equal(g: &Aig, a: Lit, b: Lit) -> bool {
    fn window(g: &Aig, root: Lit) -> FxHashSet<u32> {
        let mut seen = FxHashSet::default();
        let mut todo = vec![root.node()];
        let mut pos = 0;
        while pos < todo.len() && seen.len() < 256 {
            let n = todo[pos];
            pos += 1;
            if !seen.insert(n) || !g.is_and(n) {
                continue;
            }
            let (a, b) = g.fanins(n);
            todo.push(a.node());
            todo.push(b.node());
        }
        seen
    }
    let wa = window(g, a);
    let wb = window(g, b);
    let shared: FxHashSet<_> = wa.intersection(&wb).copied().collect();
    let inside: FxHashSet<_> = wa.union(&wb).copied().collect();
    fn copy(
        g: &Aig,
        h: &mut Aig,
        l: Lit,
        inside: &FxHashSet<u32>,
        shared: &FxHashSet<u32>,
        image: &mut FxHashMap<u32, Lit>,
    ) -> Lit {
        let n = l.node();
        if n == 0 {
            return l;
        }
        if let Some(&v) = image.get(&n) {
            return v.xor(l.is_compl());
        }
        let v = if shared.contains(&n) || !inside.contains(&n) || !g.is_and(n) {
            h.input()
        } else {
            let (a, b) = g.fanins(n);
            let a = copy(g, h, a, inside, shared, image);
            let b = copy(g, h, b, inside, shared, image);
            h.and(a, b)
        };
        image.insert(n, v);
        v.xor(l.is_compl())
    }
    let mut h = Aig::new();
    let mut image = FxHashMap::default();
    let a = copy(g, &mut h, a, &inside, &shared, &mut image);
    let b = copy(g, &mut h, b, &inside, &shared, &mut image);
    if a == b {
        return true;
    }
    if a == !b {
        return false;
    }
    // Abstraction makes cut inputs independent. Most false candidates can be rejected here
    // without allocating clauses; these samples are never used to accept a merge.
    let mut seed = 0x7e25_c190_a386_bf04u64;
    let inputs: Vec<u64> = (0..h.num_inputs())
        .map(|_| (mix(&mut seed) & !3) | 2)
        .collect();
    let sim = h.simulate(&inputs);
    if Aig::lit_value(&sim, a) != Aig::lit_value(&sim, b) {
        return false;
    }
    crate::sat::prove_equal(&h, a, b, 64) == Some(true)
}

fn merge_with(d: &mut Design, local: bool) {
    let roots = design_roots(d);
    let aig = &d.aig;
    let n = aig.num_nodes();
    // Signatures: WORDS x 64 random patterns.
    let mut sig = vec![[0u64; WORDS]; n];
    let mut seed = 0x5eed_1234u64;
    for w in 0..WORDS {
        let inputs: Vec<u64> = (0..aig.num_inputs()).map(|_| mix(&mut seed)).collect();
        let sim = aig.simulate(&inputs);
        for (i, s) in sig.iter_mut().enumerate() {
            s[w] = sim[i];
        }
    }
    // Normalize phase so that complementary nodes share a key.
    let key = |s: &[u64; WORDS]| -> ([u64; WORDS], bool) {
        let inv = s[0] & 1 == 1;
        let mut k = *s;
        if inv {
            for x in k.iter_mut() {
                *x = !*x;
            }
        }
        (k, inv)
    };
    let mut ng = Aig::new();
    let mut img = vec![Lit::FALSE; n];
    for &i in aig.inputs() {
        img[i as usize] = ng.input();
    }
    let mut cnf = AigCnf::new();
    let mut reps: FxHashMap<[u64; WORDS], (u32, bool)> = FxHashMap::default();
    let mut proofs = 0usize;
    let limit = if local { 5000 } else { MAX_PROOFS };
    for id in aig.cone(&roots) {
        if !aig.is_and(id) {
            continue;
        }
        let (a, b) = aig.fanins(id);
        let x = img[a.node() as usize].xor(a.is_compl());
        let y = img[b.node() as usize].xor(b.is_compl());
        let mut l = ng.and(x, y);
        let (k, inv) = key(&sig[id as usize]);
        // Constant candidates: the all-zero key.
        if k.iter().all(|w| *w == 0) && !l.is_const() && proofs < limit {
            proofs += 1;
            let equal = if local {
                local_equal(&ng, l, Lit::from_bool(inv))
            } else {
                let s = cnf.lit(&ng, l);
                let target = if inv { s ^ 1 } else { s };
                cnf.sat.solve(&[target], CONFLICTS_PER_PROOF) == Some(false)
            };
            if equal {
                l = Lit::from_bool(inv);
            }
        } else if let Some(&(r, rinv)) = reps.get(&k) {
            let cand = img[r as usize].xor(rinv != inv);
            if cand != l && !l.is_const() && proofs < limit {
                proofs += 1;
                let equal = if local {
                    local_equal(&ng, l, cand)
                } else {
                    let s1 = cnf.lit(&ng, l);
                    let s2 = cnf.lit(&ng, cand);
                    let diff1 = cnf.sat.solve(&[s1, s2 ^ 1], CONFLICTS_PER_PROOF);
                    diff1 == Some(false)
                        && cnf.sat.solve(&[s1 ^ 1, s2], CONFLICTS_PER_PROOF) == Some(false)
                };
                if equal {
                    l = cand;
                }
            }
        } else {
            reps.insert(k, (id, inv));
        }
        img[id as usize] = l;
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
}
