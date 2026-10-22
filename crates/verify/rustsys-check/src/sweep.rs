// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Equivalence sweeping for the final proofs (CHECK-REQ-0046) [@kuehlmann2002robust].
//!
//! Random simulation groups the circuit variables into candidate classes of equal (or
//! complementary) values. The circuit is then encoded into one incremental solver in topological
//! order. Each variable is proved equal to the first variable of its class under assumptions
//! [@een2003extensible]; once proved, it shares that variable's solver literal, so everything
//! built on it sees a smaller problem. Counterexamples of failed candidates become new
//! simulation patterns that split the classes. The proof obligations are finally solved on the
//! same solver.

use std::collections::HashMap;

use crate::circ::{B, Circuit, Def};
use crate::prove::{Proof, Rng};
use crate::sat::{Answer, Lit, Solver};

/// Class representative standing for the constants.
const CONST: u32 = u32::MAX;

fn mix(h: u64, w: u64) -> u64 {
    (h ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29) ^ 0x2545_F491_4F6C_DD1D
}

/// Statistics of a sweep.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct SweepStats {
    pub candidates: usize,
    pub merged: usize,
    pub refuted: usize,
    pub undecided: usize,
    pub solver_calls: usize,
    /// Candidates left unmerged when the optional work limit was reached.
    #[serde(default)]
    pub skipped_candidates: usize,
}

/// The circuit encoded with proven equivalences merged.
#[derive(Debug)]
pub struct Sweep<'c> {
    c: &'c Circuit,
    s: Solver,
    /// Solver literal of each circuit variable (valid once encoded).
    lit: Vec<Lit>,
    tru: Lit,
    /// Structural hashing of the encoded gates.
    strash: HashMap<(u8, Lit, Lit, Lit), Lit>,
    /// Fanin variables of each solver variable (`NONE` where absent).
    fanin: Vec<[u32; 3]>,
    /// Visit stamps for cone traversal.
    mark: Vec<u32>,
    stamp: u32,
    pub stats: SweepStats,
}

const NONE: u32 = u32::MAX;

/// Largest support decided by exhaustive evaluation (2^16 assignments).
const EXHAUSTIVE_VARS: usize = 16;

/// Conflict budget of the first, cone-limited attempt at an obligation.
const OBLIGATION_FIRST_BUDGET: u64 = 20_000;

fn neg_if(l: Lit, c: bool) -> Lit {
    if c { !l } else { l }
}

/// Transpose at most 64 counterexamples into the simulator's dense variable table.
/// Keep pattern-major RNG order: the seed produces the same future-input words as before.
fn refine_words(
    frees: &[u32],
    positions: &[usize],
    encoded: usize,
    patterns: &[Vec<bool>],
    words: &mut [u64],
    rng: &mut Rng,
) {
    assert!(patterns.len() <= 64);
    debug_assert_eq!(frees.len(), positions.len());
    for &f in frees {
        words[f as usize] = 0;
    }
    for (k, pattern) in patterns.iter().enumerate() {
        debug_assert_eq!(pattern.len(), frees.len());
        for (i, &f) in frees.iter().enumerate() {
            let bit = if positions[i] <= encoded {
                pattern[i]
            } else {
                rng.next() & 1 == 1
            };
            words[f as usize] |= u64::from(bit) << k;
        }
    }
}

impl<'c> Sweep<'c> {
    /// Encodes the variables of `order` (topological) and merges proven equivalences.
    /// `lemma_budget` bounds the conflicts of each candidate proof.
    pub fn run(
        c: &'c Circuit,
        order: &[u32],
        rounds: usize,
        seed: u64,
        lemma_budget: u64,
        max_candidates: usize,
        verbose: bool,
        ctx: rusteda_core::job::Ctx<'_>,
    ) -> Result<Self, rusteda_core::job::Cancelled> {
        let report_step = (order.len() / 8).max(1);
        let mut s = Solver::new(1);
        let tru = Lit::pos(0);
        s.add_clause(&[tru]);
        let mut sw = Sweep {
            c,
            s,
            lit: vec![tru; c.defs.len()],
            tru,
            strash: HashMap::new(),
            fanin: vec![[NONE; 3]],
            mark: vec![0],
            stamp: 0,
            stats: SweepStats::default(),
        };
        let frees: Vec<u32> = order.iter().copied().filter(|v| c.is_free(*v)).collect();
        // Signatures: a hash of the values normalized so that the first pattern is 0.
        let mut rng = Rng(seed ^ 0x5EED_5EED);
        let mut vals = Vec::new();
        let mut phase = vec![false; c.defs.len()];
        let mut hash = vec![0u64; c.defs.len()];
        let mut words = vec![0u64; c.defs.len()];
        let mut hconst = 0u64;
        for r in 0..rounds.max(1) {
            for &v in &frees {
                words[v as usize] = rng.next();
            }
            c.eval_dense(order, &|v| words[v as usize], &mut vals);
            for &v in order {
                let x = vals[v as usize];
                if r == 0 {
                    phase[v as usize] = x & 1 == 1;
                }
                let norm = if phase[v as usize] { !x } else { x };
                hash[v as usize] = mix(hash[v as usize], norm);
            }
            hconst = mix(hconst, 0);
        }
        let classes = |hash: &[u64], hconst: u64, reps: &mut HashMap<u64, u32>| {
            reps.clear();
            reps.insert(hconst, CONST);
            for &v in order {
                reps.entry(hash[v as usize]).or_insert(v);
            }
        };
        let mut reps = HashMap::new();
        classes(&hash, hconst, &mut reps);
        // Counterexample patterns collected for the next refinement.
        let mut pending: Vec<Vec<bool>> = Vec::new();
        let free_positions: Vec<usize> = order
            .iter()
            .enumerate()
            .filter_map(|(i, v)| c.is_free(*v).then_some(i))
            .collect();
        for (idx, &v) in order.iter().enumerate() {
            if idx % 64 == 0 {
                ctx.checkpoint()?;
            }
            ctx.progress("sweep", idx as u64 + 1, order.len() as u64);
            // Verbose notes at fixed fractions of the sweep, so that the events of a run depend
            // only on its inputs (JOB-REQ-0005).
            if verbose && idx > 0 && idx % report_step == 0 {
                ctx.diag(|| {
                    rusteda_core::job::Diag::note(
                        "C0002",
                        format!("sweep {idx}/{}: {:?}", order.len(), sw.stats),
                    )
                });
            }
            let l = sw.encode(v);
            sw.lit[v as usize] = l;
            if c.is_free(v) {
                continue;
            }
            let rep = reps[&hash[v as usize]];
            let target = if rep == CONST {
                neg_if(sw.tru, !phase[v as usize])
            } else if rep == v {
                continue;
            } else {
                neg_if(
                    sw.lit[rep as usize],
                    phase[v as usize] != phase[rep as usize],
                )
            };
            if target == l || target == !l {
                continue;
            }
            if sw.stats.candidates >= max_candidates {
                if verbose && sw.stats.skipped_candidates == 0 {
                    ctx.diag(|| {
                        rusteda_core::job::Diag::note(
                            "C0002",
                            format!(
                                "sweep candidate limit {max_candidates} reached; remaining \
                                 nodes use structural hashing, all final obligations still run"
                            ),
                        )
                    });
                }
                sw.stats.skipped_candidates += 1;
                continue;
            }
            sw.stats.candidates += 1;
            let limit = l.var().max(target.var());
            let inputs = sw.cone_decisions(&[l, target]);
            let mut outcome = Answer::Unsat;
            for assume in [[l, !target], [!l, target]] {
                sw.stats.solver_calls += 1;
                outcome = sw.s.solve_deciding(&assume, lemma_budget, &inputs);
                if outcome != Answer::Unsat {
                    break;
                }
            }
            // A refutation yields a pattern over all free variables.
            let mut refutation: Option<Vec<bool>> = None;
            let mut proved = outcome == Answer::Unsat;
            match outcome {
                Answer::Unsat => {}
                Answer::Sat(m) => {
                    // Free variables of the cone take the model value; the others are free.
                    refutation = Some(
                        frees
                            .iter()
                            .enumerate()
                            .map(|(i, f)| {
                                free_positions[i] <= idx && {
                                    let fl = sw.lit[*f as usize];
                                    fl.var() <= limit
                                        && sw.mark[fl.var() as usize] == sw.stamp
                                        && m[fl.var() as usize] != fl.sign()
                                }
                            })
                            .collect(),
                    );
                }
                Answer::Unknown => {
                    // Hard for the solver; small supports are settled by evaluation.
                    let (rb, comp) = if rep == CONST {
                        (B::from_bool(phase[v as usize]), false)
                    } else {
                        (B::V(rep), phase[v as usize] != phase[rep as usize])
                    };
                    match crate::prove::exhaustive_cmp(c, B::V(v), rb, comp, B::T, EXHAUSTIVE_VARS)
                    {
                        Ok(Some(Proof::Holds)) => proved = true,
                        Ok(Some(Proof::Fails(m))) => {
                            let cex: HashMap<u32, bool> = m.into_iter().collect();
                            refutation = Some(
                                frees
                                    .iter()
                                    .map(|f| cex.get(f).copied().unwrap_or(false))
                                    .collect(),
                            );
                        }
                        _ => {}
                    }
                }
            }
            if proved {
                sw.stats.merged += 1;
                sw.s.add_clause(&[!l, target]);
                sw.s.add_clause(&[l, !target]);
                sw.lit[v as usize] = target;
                continue;
            }
            let Some(p) = refutation else {
                sw.stats.undecided += 1;
                continue;
            };
            sw.stats.refuted += 1;
            pending.push(p);
            if pending.len() == 64 {
                refine_words(&frees, &free_positions, idx, &pending, &mut words, &mut rng);
                pending.clear();
                c.eval_dense(order, &|x| words[x as usize], &mut vals);
                for &x in order {
                    let w = vals[x as usize];
                    let norm = if phase[x as usize] { !w } else { w };
                    hash[x as usize] = mix(hash[x as usize], norm);
                }
                hconst = mix(hconst, 0);
                classes(&hash, hconst, &mut reps);
            }
        }
        Ok(sw)
    }

    fn bit(&self, b: B) -> Lit {
        match b {
            B::F => !self.tru,
            B::T => self.tru,
            B::V(v) => self.lit[v as usize],
        }
    }

    fn fresh(&mut self) -> Lit {
        self.fresh_with([NONE; 3])
    }

    fn fresh_with(&mut self, fanin: [u32; 3]) -> Lit {
        self.fanin.push(fanin);
        self.mark.push(0);
        Lit::pos(self.s.new_var())
    }

    /// Decision variables for a proof about `lits`: up to `NEAR` gates closest to them
    /// (breadth-first), then every input of their cone, so that assigning the list determines the
    /// whole cone by propagation. Cone variables are marked with the current stamp.
    fn cone_decisions(&mut self, lits: &[Lit]) -> Vec<u32> {
        const NEAR: usize = 48;
        self.stamp += 1;
        let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
        for l in lits {
            if self.mark[l.var() as usize] != self.stamp {
                self.mark[l.var() as usize] = self.stamp;
                queue.push_back(l.var());
            }
        }
        let mut near = Vec::new();
        let mut inputs = Vec::new();
        while let Some(v) = queue.pop_front() {
            let f = self.fanin[v as usize];
            if f[0] == NONE {
                if v != self.tru.var() {
                    inputs.push(v);
                }
                continue;
            }
            if near.len() < NEAR {
                near.push(v);
            }
            for x in f {
                if x != NONE && self.mark[x as usize] != self.stamp {
                    self.mark[x as usize] = self.stamp;
                    queue.push_back(x);
                }
            }
        }
        near.extend(inputs);
        near
    }

    fn and2(&mut self, a: Lit, b: Lit) -> Lit {
        let f = !self.tru;
        if a == f || b == f || a == !b {
            return f;
        }
        if a == self.tru || a == b {
            return b;
        }
        if b == self.tru {
            return a;
        }
        let (a, b) = if a.0 < b.0 { (a, b) } else { (b, a) };
        if let Some(o) = self.strash.get(&(0, a, b, a)) {
            return *o;
        }
        let o = self.fresh_with([a.var(), b.var(), NONE]);
        self.s.add_clause(&[!o, a]);
        self.s.add_clause(&[!o, b]);
        self.s.add_clause(&[o, !a, !b]);
        self.strash.insert((0, a, b, a), o);
        o
    }

    fn xor2(&mut self, a: Lit, b: Lit) -> Lit {
        let t = self.tru;
        if a == !t || a == t {
            return neg_if(b, a == t);
        }
        if b == !t || b == t {
            return neg_if(a, b == t);
        }
        if a == b {
            return !t;
        }
        if a == !b {
            return t;
        }
        let inv = a.sign() != b.sign();
        let (a, b) = (Lit::pos(a.var()), Lit::pos(b.var()));
        let (a, b) = if a.0 < b.0 { (a, b) } else { (b, a) };
        let o = if let Some(o) = self.strash.get(&(1, a, b, a)) {
            *o
        } else {
            let o = self.fresh_with([a.var(), b.var(), NONE]);
            self.s.add_clause(&[!o, a, b]);
            self.s.add_clause(&[!o, !a, !b]);
            self.s.add_clause(&[o, !a, b]);
            self.s.add_clause(&[o, a, !b]);
            self.strash.insert((1, a, b, a), o);
            o
        };
        neg_if(o, inv)
    }

    /// `s ? a : b`
    fn mux2(&mut self, s: Lit, a: Lit, b: Lit) -> Lit {
        let t = self.tru;
        if s == t {
            return a;
        }
        if s == !t {
            return b;
        }
        if a == b {
            return a;
        }
        let (s, a, b) = if s.sign() { (!s, b, a) } else { (s, a, b) };
        if a == !b {
            return self.xor2(!s, a);
        }
        if a == t || a == s {
            let (ns, nb) = (!s, !b);
            return !self.and2(ns, nb);
        }
        if a == !t || a == !s {
            return self.and2(!s, b);
        }
        if b == t || b == !s {
            let (na, ss) = (!a, s);
            return !self.and2(ss, na);
        }
        if b == !t || b == s {
            return self.and2(s, a);
        }
        let inv = a.sign();
        let (a, b) = if inv { (!a, !b) } else { (a, b) };
        let o = if let Some(o) = self.strash.get(&(2, s, a, b)) {
            *o
        } else {
            let o = self.fresh_with([s.var(), a.var(), b.var()]);
            self.s.add_clause(&[!s, !a, o]);
            self.s.add_clause(&[!s, a, !o]);
            self.s.add_clause(&[s, !b, o]);
            self.s.add_clause(&[s, b, !o]);
            self.s.add_clause(&[!a, !b, o]);
            self.s.add_clause(&[a, b, !o]);
            self.strash.insert((2, s, a, b), o);
            o
        };
        neg_if(o, inv)
    }

    fn encode(&mut self, v: u32) -> Lit {
        let l = |x: u32, sw: &Self| sw.lit[x as usize];
        match self.c.defs[v as usize] {
            Def::Free => self.fresh(),
            Def::Alias(b) => self.bit(b),
            Def::Not(x) => !l(x, self),
            Def::And(x, y) => self.and2(l(x, self), l(y, self)),
            Def::Or(x, y) => {
                let (a, b) = (!l(x, self), !l(y, self));
                !self.and2(a, b)
            }
            Def::Xor(x, y) => self.xor2(l(x, self), l(y, self)),
            Def::Mux(s, x, y) => self.mux2(l(s, self), l(x, self), l(y, self)),
        }
    }

    /// Proves `a == b` whenever `assume` holds, for encoded bits. A counterexample lists the
    /// free variables of `frees` that the proof constrains; the others may take any value.
    pub fn equal(&mut self, a: B, b: B, assume: B, budget: u64, frees: &[u32]) -> Proof {
        let (la, lb, lc) = (self.bit(a), self.bit(b), self.bit(assume));
        if lc == !self.tru || la == lb {
            return Proof::Holds;
        }
        let mut remaining = budget;
        // Small supports are decided by evaluating every assignment.
        if let Ok(Some(p)) = crate::prove::exhaustive(self.c, a, b, assume, EXHAUSTIVE_VARS) {
            return p;
        }
        let d = self.fresh();
        self.s.add_clause(&[!d, la, lb]);
        self.s.add_clause(&[!d, !la, !lb]);
        self.s.add_clause(&[d, !la, lb]);
        self.s.add_clause(&[d, la, !lb]);
        let limit = la.var().max(lb.var()).max(lc.var());
        // First with decisions limited to the cone (cheap counterexamples, short local proofs),
        // then, if that is inconclusive, with activity-ordered decisions.
        let decide = self.cone_decisions(&[la, lb, lc]);
        self.stats.solver_calls += 1;
        let before = self.s.conflicts;
        let mut answer =
            self.s
                .solve_deciding(&[lc, d], remaining.min(OBLIGATION_FIRST_BUDGET), &decide);
        remaining = remaining.saturating_sub(self.s.conflicts - before);
        if answer == Answer::Unknown && remaining > 0 {
            self.stats.solver_calls += 1;
            answer = self.s.solve_limited(&[lc, d], remaining, limit);
        }
        match answer {
            Answer::Unsat => Proof::Holds,
            Answer::Unknown => Proof::Unknown,
            Answer::Sat(m) => Proof::Fails(
                frees
                    .iter()
                    .map(|f| {
                        let fl = self.lit[*f as usize];
                        (*f, fl.var() <= limit && m[fl.var() as usize] != fl.sign())
                    })
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_refinement_preserves_sparse_inputs_and_seed_order() {
        let frees = [2u32, 9, 17, 30, 64, 80];
        let positions = [0, 3, 7, 8, 42, 88];
        let mut dense = vec![u64::MAX; 81];
        let mut rng = Rng(0x1234);
        let mut legacy_rng = rng.clone();
        // Reuse the buffer with a short batch after a full 64-lane batch: stale high lanes
        // must be cleared, and future variables must consume RNG values in pattern order.
        for (lanes, encoded) in [(64, 5), (17, 18), (0, 88)] {
            let patterns: Vec<Vec<bool>> = (0..lanes)
                .map(|k| (0..frees.len()).map(|i| (k + 3 * i) % 5 < 2).collect())
                .collect();
            let mut legacy = HashMap::<u32, u64>::new();
            for (k, pattern) in patterns.iter().enumerate() {
                for (i, &v) in frees.iter().enumerate() {
                    let bit = if positions[i] <= encoded {
                        pattern[i]
                    } else {
                        legacy_rng.next() & 1 == 1
                    };
                    *legacy.entry(v).or_default() |= u64::from(bit) << k;
                }
            }
            refine_words(&frees, &positions, encoded, &patterns, &mut dense, &mut rng);
            for v in frees {
                assert_eq!(dense[v as usize], legacy.get(&v).copied().unwrap_or(0));
            }
            assert_eq!(
                dense[1],
                u64::MAX,
                "non-input scratch entries are untouched"
            );
            assert_eq!(rng.0, legacy_rng.0);
        }
    }

    #[test]
    fn optional_candidate_limit_does_not_skip_final_obligations() {
        let mut c = Circuit::new();
        let inputs: Vec<B> = (0..12).map(|_| B::V(c.free())).collect();
        let mut roots = Vec::new();
        for pattern in 0..64u32 {
            let mut term = B::T;
            for (bit, &input) in inputs.iter().enumerate() {
                let value = if pattern & (1 << bit) == 0 {
                    c.not(input)
                } else {
                    input
                };
                term = c.and(term, value);
            }
            roots.push(term);
        }
        let mut equivalent = B::T;
        for &input in inputs.iter().rev() {
            let value = c.not(input);
            equivalent = c.and(equivalent, value);
        }
        roots.push(equivalent);
        let bad = c.not(roots[0]);
        roots.push(bad);
        let order = c.order(&roots).unwrap();
        let frees: Vec<_> = order.iter().copied().filter(|v| c.is_free(*v)).collect();
        for limit in [0, 3] {
            let mut sw = Sweep::run(
                &c,
                &order,
                1,
                1,
                1_000,
                limit,
                false,
                rusteda_core::job::Ctx::NONE,
            )
            .unwrap();
            assert!(sw.stats.candidates <= limit);
            assert!(sw.stats.solver_calls <= 2 * limit);
            assert!(sw.stats.skipped_candidates > 0);
            assert_eq!(
                sw.equal(roots[0], equivalent, B::T, 100_000, &frees),
                Proof::Holds
            );
            assert!(matches!(
                sw.equal(roots[0], bad, B::T, 100_000, &frees),
                Proof::Fails(_)
            ));
        }
    }

    /// Two structurally different adders merge bit by bit; a planted difference is found.
    #[test]
    fn merges_equal_logic_and_refutes_differences() {
        let mut c = Circuit::new();
        let n = 12;
        let a: Vec<B> = (0..n).map(|_| B::V(c.free())).collect();
        let b: Vec<B> = (0..n).map(|_| B::V(c.free())).collect();
        // Ripple-carry with majority carries.
        let mut s1 = Vec::new();
        let mut cy = B::F;
        for i in 0..n {
            let x = c.xor(a[i], b[i]);
            s1.push(c.xor(x, cy));
            let g = c.and(a[i], b[i]);
            let p = c.and(x, cy);
            cy = c.or(g, p);
        }
        // Carries as multiplexers.
        let mut s2 = Vec::new();
        let mut cy = B::F;
        for i in 0..n {
            let na = c.not(a[i]);
            let nb = c.not(b[i]);
            let xn = c.xor(na, nb);
            let t = c.not(cy);
            let u = c.xor(xn, t);
            s2.push(c.not(u));
            cy = c.mux(xn, cy, a[i]);
        }
        // A planted difference in the top bit.
        let bad = c.not(s2[n - 1]);
        let mut roots: Vec<B> = s1.iter().chain(&s2).copied().collect();
        roots.push(bad);
        let order = c.order(&roots).unwrap();
        let frees: Vec<u32> = order.iter().copied().filter(|v| c.is_free(*v)).collect();
        let mut sw = Sweep::run(
            &c,
            &order,
            4,
            1,
            1000,
            10_000,
            false,
            rusteda_core::job::Ctx::NONE,
        )
        .unwrap();
        for i in 0..n {
            assert_eq!(sw.equal(s1[i], s2[i], B::T, 10_000, &frees), Proof::Holds);
        }
        assert!(sw.stats.merged > 0);
        match sw.equal(s1[n - 1], bad, B::T, 10_000, &frees) {
            Proof::Fails(m) => {
                let assign: HashMap<u32, bool> = m.into_iter().collect();
                let mut vals = HashMap::new();
                let ord = c.order(&[s1[n - 1], bad]).unwrap();
                c.eval(&ord, &|v| if assign[&v] { u64::MAX } else { 0 }, &mut vals);
                assert_ne!(
                    Circuit::value(s1[n - 1], &vals) & 1,
                    Circuit::value(bad, &vals) & 1
                );
            }
            other => panic!("{other:?}"),
        }
        // Under an assumption that masks the difference the proof holds.
        assert_eq!(sw.equal(s1[0], bad, B::F, 10_000, &frees), Proof::Holds);
    }
}
