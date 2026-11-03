// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `merge`: merges nodes proven functionally equivalent (or complementary, or constant) by random
//! simulation followed by SAT (SYNTH-REQ-0411) [@kuehlmann2002robust]. The global pass refines its
//! candidate classes with the counterexamples of failed proofs and keeps every unmerged node as a
//! candidate representative [@mishchenko2005fraigs].

use rustc_hash::{FxHashMap, FxHashSet};

use super::{design_roots, set_roots, sweep};
use crate::aig::{Aig, Lit};
use crate::elab::Design;
use crate::sat::AigCnf;

/// Random simulation words (64 patterns each) and proofs of the local pass.
const WORDS: usize = 8;
const LOCAL_PROOFS: usize = 5000;

/// SplitMix64 [@steele2014fast].
fn mix(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The global SAT sweep (`sat_sweep`).
pub fn merge(d: &mut Design) {
    sat_sweep(d);
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

/// Initial random simulation words of the global sweep (64 patterns each).
const SWEEP_WORDS: usize = 16;
/// Conflict limit of one SAT call of the global sweep.
const SWEEP_CONFLICTS: u64 = 300;
/// SAT calls after which the incremental solver is rebuilt, so that its clause database stays
/// proportional to the cones being compared.
const SWEEP_RECYCLE: usize = 500;
/// SAT calls of one global sweep.
pub const SWEEP_MAX_CALLS: usize = 20_000;
/// Patterns per counterexample: its free inputs take different random values.
const CEX_FILLS: u32 = 8;
/// Justification rounds, words per round, and nodes visited per justified node.
const JUSTIFY_ROUNDS: usize = 4;
const JUSTIFY_WORDS: usize = 8;
const JUSTIFY_NODES: usize = 4096;
/// Requests tried, and requests packed, per justified pattern.
const JUSTIFY_TRIES: usize = 64;
const JUSTIFY_PACK: usize = 16;

/// Work of one global sweep.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepStats {
    /// Candidate pairs (or constant candidates) that reached the SAT solver.
    pub candidates: u64,
    pub sat_calls: u64,
    pub merged: u64,
    /// Proofs refuted by a counterexample, which then refines every class.
    pub refuted: u64,
    pub undecided: u64,
}

/// Simulation state of the original graph: complete 64-pattern words per node, plus one word
/// that collects counterexamples. Its unused bits hold the all-zero pattern, a valid assignment.
struct Signatures {
    cols: Vec<Vec<u64>>,
    /// Per node: hash of its phase-normalized complete words (`key`).
    keys: Vec<u64>,
    cex_inputs: Vec<u64>,
    cex_bits: u32,
    cex_col: Vec<u64>,
}

impl Signatures {
    fn new(aig: &Aig, words: usize, seed: &mut u64) -> Signatures {
        let ni = aig.num_inputs();
        let cols = (0..words)
            .map(|_| {
                let inputs: Vec<u64> = (0..ni).map(|_| mix(seed)).collect();
                aig.simulate(&inputs)
            })
            .collect();
        let cex_inputs = vec![0u64; ni];
        let cex_col = aig.simulate(&cex_inputs);
        Signatures {
            cols,
            keys: Vec::new(),
            cex_inputs,
            cex_bits: 0,
            cex_col,
        }
    }

    /// Rounds of patterns that try to separate apparently constant nodes from their constant,
    /// and apparently equivalent nodes of `cone` from each other, by backward justification of
    /// the wanted values to the inputs (other inputs random). The simulation decides which
    /// attempts succeeded; every pattern is a valid assignment.
    fn justify_classes(&mut self, aig: &Aig, cone: &[u32], seed: &mut u64) {
        let mut j = Justify::new(aig.num_inputs());
        for _ in 0..JUSTIFY_ROUNDS {
            let mut wanted: Vec<[(u32, bool); 2]> = Vec::new();
            let mut first: FxHashMap<u64, u32> = FxHashMap::default();
            for &n in cone {
                if !aig.is_and(n) {
                    continue;
                }
                if self.zero(n) {
                    // The node's value is its phase on every pattern: ask for the other one.
                    wanted.push([(n, !self.phase(n)); 2]);
                } else if let Some(&r) = first.get(&self.key(n)) {
                    if self.same(r, n) {
                        // Normalized values 0 and 1: the node at its phase, the other not.
                        let flip = wanted.len() & 1 == 1;
                        wanted.push([(r, self.phase(r) ^ flip), (n, !self.phase(n) ^ flip)]);
                    }
                } else {
                    first.insert(self.key(n), n);
                }
            }
            if wanted.is_empty() {
                break;
            }
            // Several compatible requests share one pattern: a request whose justification
            // conflicts with the inputs already assigned for this pattern waits for a later one.
            let mut pending: std::collections::VecDeque<usize> = (0..wanted.len()).collect();
            for _ in 0..JUSTIFY_WORDS {
                if pending.is_empty() {
                    break;
                }
                let mut words: Vec<u64> = (0..aig.num_inputs()).map(|_| mix(seed)).collect();
                for bit in 0..64 {
                    j.clear();
                    let mut tries = pending.len().min(JUSTIFY_TRIES);
                    let mut packed = 0;
                    while tries > 0 && packed < JUSTIFY_PACK {
                        tries -= 1;
                        let Some(w) = pending.pop_front() else { break };
                        if j.add(aig, &wanted[w], seed) {
                            packed += 1;
                        } else {
                            pending.push_back(w);
                        }
                    }
                    for &i in &j.touched {
                        let w = &mut words[i as usize];
                        if j.value[i as usize] == 1 {
                            *w |= 1 << bit;
                        } else {
                            *w &= !(1 << bit);
                        }
                    }
                }
                self.cols.push(aig.simulate(&words));
            }
        }
    }

    /// Phase of a node: its value under the first pattern.
    fn phase(&self, n: u32) -> bool {
        self.cols[0][n as usize] & 1 == 1
    }

    /// Hash of the phase-normalized complete words.
    fn key(&self, n: u32) -> u64 {
        if let Some(&k) = self.keys.get(n as usize) {
            return k;
        }
        self.cols
            .iter()
            .fold(0x243f_6a88_85a3_08d3u64, |h, c| self.hash_step(h, c, n))
    }

    fn hash_step(&self, h: u64, col: &[u64], n: u32) -> u64 {
        let mask = if self.phase(n) { u64::MAX } else { 0 };
        let h = (h ^ (col[n as usize] ^ mask)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^ (h >> 29)
    }

    /// Caches the keys of every node; `push` keeps them current.
    fn cache_keys(&mut self) {
        self.keys = (0..self.cols[0].len() as u32)
            .map(|n| {
                self.cols
                    .iter()
                    .fold(0x243f_6a88_85a3_08d3u64, |h, c| self.hash_step(h, c, n))
            })
            .collect();
    }

    /// Adds a complete word.
    fn push(&mut self, col: Vec<u64>) {
        if !self.keys.is_empty() {
            for n in 0..col.len() as u32 {
                self.keys[n as usize] = self.hash_step(self.keys[n as usize], &col, n);
            }
        }
        self.cols.push(col);
    }

    /// Whether `a` and `b` agree (after phase normalization) on every pattern.
    fn same(&self, a: u32, b: u32) -> bool {
        let mask = if self.phase(a) != self.phase(b) {
            u64::MAX
        } else {
            0
        };
        self.cols
            .iter()
            .chain(std::iter::once(&self.cex_col))
            .all(|c| c[a as usize] ^ mask == c[b as usize])
    }

    /// Whether the phase-normalized node is zero on every pattern.
    fn zero(&self, n: u32) -> bool {
        let mask = if self.phase(n) { u64::MAX } else { 0 };
        self.cols
            .iter()
            .chain(std::iter::once(&self.cex_col))
            .all(|c| c[n as usize] ^ mask == 0)
    }

    /// Adds a counterexample (one value per input of `aig`) and resimulates. Returns true when
    /// the counterexample word became complete and the class keys changed.
    fn add(&mut self, aig: &Aig, model: &[Option<bool>], seed: &mut u64) -> bool {
        // The counterexample with several random values of the inputs it leaves free.
        let fills = CEX_FILLS.min(64 - self.cex_bits);
        for f in 0..fills {
            let bit = 1u64 << (self.cex_bits + f);
            for (w, v) in self.cex_inputs.iter_mut().zip(model) {
                if v.unwrap_or_else(|| mix(seed) & 1 == 1) {
                    *w |= bit;
                }
            }
        }
        self.cex_bits += fills;
        if self.cex_bits == 64 {
            let full = aig.simulate(&self.cex_inputs);
            self.push(full);
            self.cex_inputs.fill(0);
            self.cex_bits = 0;
            self.cex_col = aig.simulate(&self.cex_inputs);
            true
        } else {
            self.cex_col = aig.simulate(&self.cex_inputs);
            false
        }
    }
}

/// Backward justification of wanted node values to input values (SAT-free, incomplete).
struct Justify {
    /// Per input: 0, 1, or 2 when unassigned.
    value: Vec<u8>,
    touched: Vec<u32>,
    seen: FxHashSet<(u32, bool)>,
    stack: Vec<(u32, bool)>,
}

impl Justify {
    fn new(inputs: usize) -> Justify {
        Justify {
            value: vec![2; inputs],
            touched: Vec::new(),
            seen: FxHashSet::default(),
            stack: Vec::new(),
        }
    }

    /// Forgets the assignment of the current pattern.
    fn clear(&mut self) {
        for &i in &self.touched {
            self.value[i as usize] = 2;
        }
        self.touched.clear();
    }

    /// Extends the current pattern's input assignment so that each `(node, value)` holds, if
    /// that needs no input to change; otherwise leaves the assignment as it was and returns
    /// false.
    fn add(&mut self, aig: &Aig, wanted: &[(u32, bool)], seed: &mut u64) -> bool {
        let mark = self.touched.len();
        self.seen.clear();
        self.stack.clear();
        self.stack.extend(wanted.iter().rev().copied());
        let mut budget = JUSTIFY_NODES;
        let mut ok = true;
        while let Some((node, want)) = self.stack.pop() {
            if budget == 0 {
                ok = false;
                break;
            }
            budget -= 1;
            if !self.seen.insert((node, want)) {
                continue;
            }
            if self.seen.contains(&(node, !want)) {
                ok = false;
                break;
            }
            if let Some(i) = aig.input_of(node) {
                match self.value[i as usize] {
                    2 => {
                        self.value[i as usize] = u8::from(want);
                        self.touched.push(i);
                    }
                    v if v != u8::from(want) => {
                        ok = false;
                        break;
                    }
                    _ => {}
                }
                continue;
            }
            if !aig.is_and(node) {
                continue;
            }
            let (a, b) = aig.fanins(node);
            if want {
                self.stack.push((b.node(), !b.is_compl()));
                self.stack.push((a.node(), !a.is_compl()));
            } else {
                // One controlling fanin suffices: prefer an input already at its controlling
                // value or a node already wanted there, else choose pseudo-randomly.
                let ca = (a.node(), a.is_compl());
                let cb = (b.node(), b.is_compl());
                let holds = |x: (u32, bool), j: &Justify| {
                    j.seen.contains(&x)
                        || aig
                            .input_of(x.0)
                            .is_some_and(|i| j.value[i as usize] == u8::from(x.1))
                };
                let pick = if holds(ca, self) {
                    ca
                } else if holds(cb, self) || mix(seed) & 1 == 1 {
                    cb
                } else {
                    ca
                };
                self.stack.push(pick);
            }
        }
        if !ok {
            for &i in &self.touched[mark..] {
                self.value[i as usize] = 2;
            }
            self.touched.truncate(mark);
        }
        ok
    }
}

/// Global SAT sweep: the nodes of the cones of the design's roots are rebuilt in topological
/// order; each one is compared with the earlier nodes (inputs included) of its simulation class,
/// in the order they were added, and replaced by the first one proven equivalent up to
/// complementation. A refuted proof adds its counterexample to the simulation, which separates
/// that pair and every other pair it distinguishes. Deterministic: fixed seeds, conflict limits
/// and orders only.
pub fn sat_sweep(d: &mut Design) -> SweepStats {
    let roots = design_roots(d);
    let aig = &d.aig;
    let n = aig.num_nodes();
    let mut stats = SweepStats::default();
    let mut seed = 0x5eed_1234u64;
    let mut sig = Signatures::new(aig, SWEEP_WORDS, &mut seed);
    let cone = aig.cone(&roots);
    sig.justify_classes(aig, &cone, &mut seed);
    sig.cache_keys();
    let mut ng = Aig::new();
    let mut img = vec![Lit::FALSE; n];
    for &i in aig.inputs() {
        img[i as usize] = ng.input();
    }
    let inputs: Vec<u32> = aig.inputs().to_vec();
    let mut classes: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    let mut members: Vec<u32> = Vec::new();
    for &i in &inputs {
        classes.entry(sig.key(i)).or_default().push(i);
        members.push(i);
    }
    let mut cnf = AigCnf::new();
    let mut since_recycle = 0usize;
    for &id in &cone {
        if !aig.is_and(id) {
            continue;
        }
        let (a, b) = aig.fanins(id);
        let x = img[a.node() as usize].xor(a.is_compl());
        let y = img[b.node() as usize].xor(b.is_compl());
        let mut l = ng.and(x, y);
        let inv = sig.phase(id);
        let mut merged = l.is_const();
        if !merged && (stats.sat_calls as usize) < SWEEP_MAX_CALLS && sig.zero(id) {
            stats.candidates += 1;
            recycle(&mut cnf, &mut since_recycle);
            // A difference from the constant `inv` is an assignment where the node is `!inv`.
            let (r, model) = solve(&mut cnf, &ng, &[l.xor(inv)], &mut stats, &mut seed);
            match r {
                Some(false) => {
                    l = Lit::from_bool(inv);
                    merged = true;
                    stats.merged += 1;
                }
                Some(true) => {
                    stats.refuted += 1;
                    if sig.add(aig, &model, &mut seed) {
                        rekey(&sig, &members, &mut classes);
                    }
                }
                None => stats.undecided += 1,
            }
        }
        if !merged {
            let mut k = 0;
            loop {
                let key = sig.key(id);
                let Some(&r) = classes.get(&key).and_then(|c| c.get(k)) else {
                    break;
                };
                k += 1;
                if !sig.same(id, r) {
                    continue;
                }
                let cand = img[r as usize].xor(sig.phase(r) != inv);
                if cand == l {
                    merged = true;
                    break;
                }
                if cand.is_const() || (stats.sat_calls as usize) >= SWEEP_MAX_CALLS {
                    continue;
                }
                // Never deepen the node: a merge into a deeper representative could lengthen
                // the paths through it (prefix carries merged into ripple duplicates).
                if ng.level(cand) > ng.level(l) {
                    continue;
                }
                stats.candidates += 1;
                recycle(&mut cnf, &mut since_recycle);
                let (r1, model) = solve(&mut cnf, &ng, &[l, !cand], &mut stats, &mut seed);
                let (r2, model) = if r1 == Some(false) {
                    solve(&mut cnf, &ng, &[!l, cand], &mut stats, &mut seed)
                } else {
                    (r1, model)
                };
                match r2 {
                    Some(false) => {
                        l = cand;
                        merged = true;
                        stats.merged += 1;
                        break;
                    }
                    Some(true) => {
                        stats.refuted += 1;
                        if sig.add(aig, &model, &mut seed) {
                            rekey(&sig, &members, &mut classes);
                            // The keys changed: restart the scan of this node's new class.
                            k = 0;
                        }
                    }
                    None => stats.undecided += 1,
                }
            }
        }
        if !merged {
            classes.entry(sig.key(id)).or_default().push(id);
            members.push(id);
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
    stats
}

/// Rebuilds the solver every `SWEEP_RECYCLE` proofs.
fn recycle(cnf: &mut AigCnf, since: &mut usize) {
    if *since >= SWEEP_RECYCLE {
        *cnf = AigCnf::new();
        *since = 0;
    }
    *since += 1;
}

/// Solves the conjunction of `lits` over their input cones with a conflict limit; on
/// `Some(true)` also returns the values the model gives the inputs of `ng` (`None` for inputs
/// outside the cones).
fn solve(
    cnf: &mut AigCnf,
    ng: &Aig,
    lits: &[Lit],
    stats: &mut SweepStats,
    seed: &mut u64,
) -> (Option<bool>, Vec<Option<bool>>) {
    // Random decision phases for the inputs, so that counterexamples are not biased to zero.
    for l in lits {
        let _ = cnf.lit(ng, *l);
    }
    for &i in ng.inputs() {
        cnf.set_phase(i, mix(seed) & 1 == 1);
    }
    stats.sat_calls += 1;
    let r = cnf.solve_cone(ng, lits, SWEEP_CONFLICTS);
    let model = if r == Some(true) {
        ng.inputs().iter().map(|&i| cnf.model_value(i)).collect()
    } else {
        Vec::new()
    };
    (r, model)
}

/// Rebuilds the class index after the complete simulation words changed, keeping each class
/// in the order its members were added.
fn rekey(sig: &Signatures, members: &[u32], classes: &mut FxHashMap<u64, Vec<u32>>) {
    classes.clear();
    for &m in members {
        classes.entry(sig.key(m)).or_default().push(m);
    }
}

/// Bounded proofs on local differences. Shared internal nodes become independent inputs;
/// equivalence under this stronger condition implies equivalence in the original graph.
pub fn merge_local(d: &mut Design) {
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
    let mut reps: FxHashMap<[u64; WORDS], (u32, bool)> = FxHashMap::default();
    let mut proofs = 0usize;
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
        if k.iter().all(|w| *w == 0) && !l.is_const() && proofs < LOCAL_PROOFS {
            proofs += 1;
            if local_equal(&ng, l, Lit::from_bool(inv)) {
                l = Lit::from_bool(inv);
            }
        } else if let Some(&(r, rinv)) = reps.get(&k) {
            let cand = img[r as usize].xor(rinv != inv);
            if cand != l && !l.is_const() && proofs < LOCAL_PROOFS {
                proofs += 1;
                if local_equal(&ng, l, cand) {
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
