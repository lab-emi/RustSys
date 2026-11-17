// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Re-encoded registers by exploration from the reset code (state-map export only).
//!
//! For an RTL variable `V` (see `encode::variables`) the candidates are the elements without
//! another owner, or owned only by a relation without reset-trace evidence (both traces
//! constant), whose clock equals V's clock and whose asynchronous controls are V's reset
//! condition; each starts at its reset value, V at its reset code. With every other state set by
//! the current hypothesis (named and proposed elements equal to their RTL state, state still
//! without a relation at its reset value), the search collects transitions
//! (RTL code -> RTL next code, candidates -> their next values) from
//! (a) the reset traces (reachable states of both models),
//! (b) random simulation from every collected code, and
//! (c) SAT from every collected code: any input and any hypothesis-consistent state that leads
//!     to an RTL next code not yet collected from it (the reachability a synthesis tool's FSM
//!     extraction uses with free control inputs, here over the actual control logic).
//!
//! A candidate whose next value is not one function of the RTL next code leaves; its relation,
//! if it had one, constrains it again from then on, and (c) runs again while candidates leave.
//! The re-encoded register is the remaining candidates whose value differs between codes, when
//! distinct codes have distinct element codes and not every element copies one RTL bit (a plain
//! renaming stays with element relations).
//!
//! Proposals only: the export proves the reset base, the step of the valid-code invariant and
//! every other obligation under it, so a wrong or incomplete table fails those obligations.
//!
//! Budgets per variable: `encode::MAX_BITS` bits (so at most `2^MAX_BITS` codes),
//! `MAX_CANDIDATES` candidates, `SIM_ROUNDS` rounds of 64 lanes per code, `MAX_SAT_CALLS` SAT
//! calls of at most `SAT_CONFLICTS` conflicts each, `PASSES` passes; evaluation and encoding
//! work are charged to the discovery work budget. Memory: one CNF of the variable's next-state
//! cones (at most six clauses per circuit variable) at a time.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use super::encode::{self, lane_codes, reset_code};
use super::trace::Traces;
use super::{Hyp, Search, mask};
use crate::Stop;
use crate::circ::B;
use crate::corr::{Encoding, Relation};
use crate::prove;
use crate::sat::{Answer, Lit, Solver};

const MAX_CANDIDATES: usize = 4096;
const SIM_ROUNDS: usize = 4;
const MAX_SAT_CALLS: usize = 4096;
const SAT_CONFLICTS: u64 = 50_000;
const PASSES: usize = 6;

/// Transitions collected for one variable.
struct Table {
    cands: Vec<usize>,
    reset: Vec<bool>,
    alive: Vec<bool>,
    /// Candidate values per collected RTL code.
    known: BTreeMap<u64, Vec<bool>>,
    queue: VecDeque<u64>,
    kills: usize,
}

impl Table {
    fn kill(&mut self, e: usize) {
        if self.alive[e] {
            self.alive[e] = false;
            self.kills += 1;
            for v in self.known.values_mut() {
                v[e] = self.reset[e];
            }
        }
    }

    /// One class of observed transitions into `code`: `next[e]` is the candidate's next value
    /// word over the class's `lanes` (a candidate with both values leaves).
    fn absorb(&mut self, code: u64, lanes: u64, next: &[u64]) {
        let mut values = vec![false; self.cands.len()];
        for (e, w) in next.iter().enumerate() {
            if !self.alive[e] {
                continue;
            }
            let x = w & lanes;
            if x != 0 && x != lanes {
                self.kill(e);
            } else {
                values[e] = x != 0;
            }
        }
        if let Some(have) = self.known.get(&code).cloned() {
            for e in 0..self.cands.len() {
                if self.alive[e] && have[e] != values[e] {
                    self.kill(e);
                }
            }
        } else {
            let v = (0..self.cands.len())
                .map(|e| {
                    if self.alive[e] {
                        values[e]
                    } else {
                        self.reset[e]
                    }
                })
                .collect();
            self.known.insert(code, v);
            self.queue.push_back(code);
        }
    }
}

fn bit(code: u64, b: usize) -> bool {
    (code >> b) & 1 == 1
}

/// Lanes per code of `words` (one word per bit).
fn classes(words: &[u64], active: u64) -> BTreeMap<u64, u64> {
    let mut out: BTreeMap<u64, u64> = BTreeMap::new();
    for (lane, code) in lane_codes(words).iter().enumerate() {
        if (active >> lane) & 1 == 1 {
            *out.entry(*code).or_default() |= 1 << lane;
        }
    }
    out
}

impl Search<'_, '_> {
    /// Explores every candidate variable, again while a pass changes a register (a register
    /// found first makes the states that read it consistent for the next). A register already
    /// in `self.encodings` is explored again with its elements as candidates and kept when
    /// exploration finds nothing. Returns whether a register changed.
    pub(super) fn explore_all(&mut self, hyp: &Hyp) -> Result<bool, Stop> {
        let vars = encode::variables(self.s, self.corr);
        let debug = std::env::var_os("RUSTSYS_DISCOVERY_TRACE").is_some();
        // Relations with reset-trace evidence keep their element; others may give it up.
        let evident = |ci: usize| {
            self.traces
                .as_ref()
                .is_some_and(|t| Traces::constant(&t.gate[ci]).is_none())
        };
        let strong: HashSet<usize> = hyp
            .values()
            .filter_map(|r| match r {
                Relation::Cell(ci, _) if evident(*ci) || self.named_cells.contains(ci) => Some(*ci),
                _ => None,
            })
            .collect();
        let mut changed = false;
        // A variable is explored again only after another register changed.
        let mut version = 0usize;
        let mut seen: Vec<Option<usize>> = vec![None; vars.len()];
        for _ in 0..PASSES {
            let mut pass_changed = false;
            for (k, (var, bits)) in vars.iter().enumerate() {
                if seen[k] == Some(version) || self.hinted.contains(var) {
                    continue;
                }
                let old = self
                    .encodings
                    .iter()
                    .position(|e| e.variable == *var)
                    .map(|i| self.encodings.remove(i));
                let other: HashSet<usize> = self
                    .encodings
                    .iter()
                    .flat_map(|e| e.gate.iter().copied())
                    .collect();
                let own: HashSet<usize> = old.iter().flat_map(|e| e.gate.iter().copied()).collect();
                let of_bits = |ci: usize| {
                    self.corr.owners[ci].is_some_and(|si| bits.contains(&si))
                        || bits
                            .iter()
                            .any(|si| matches!(hyp.get(si), Some(Relation::Cell(x, _)) if *x == ci))
                };
                let cands: Vec<usize> = (0..self.s.nm.seq.len())
                    .filter(|&ci| {
                        own.contains(&ci)
                            || (!other.contains(&ci)
                                && (of_bits(ci)
                                    || (self.corr.owners[ci].is_none() && !strong.contains(&ci))))
                    })
                    .collect();
                let found = if cands.is_empty() || cands.len() > MAX_CANDIDATES {
                    None
                } else {
                    self.explore(hyp, var, bits, &cands)?
                };
                // A table that loses codes was explored under fewer relations: keep the old.
                let found = found.filter(|new| {
                    old.as_ref().is_none_or(|o| {
                        o.codes
                            .iter()
                            .all(|(r, _)| new.codes.iter().any(|(x, _)| x == r))
                    })
                });
                let next = match (found, old) {
                    (Some(new), old) => {
                        if old.as_ref() != Some(&new) {
                            pass_changed = true;
                            version += 1;
                            if debug {
                                eprintln!(
                                    "explored {}: {} elements, codes {:?}",
                                    new.variable,
                                    new.gate.len(),
                                    new.codes
                                );
                            }
                        }
                        Some(new)
                    }
                    (None, old) => old,
                };
                if let Some(enc) = next {
                    self.encodings.push(enc);
                }
                seen[k] = Some(version);
            }
            changed |= pass_changed;
            if !pass_changed {
                break;
            }
        }
        self.encoding_failures = vec![0; self.encodings.len()];
        Ok(changed)
    }

    /// Explores one variable (see the module documentation).
    #[allow(clippy::too_many_lines)]
    fn explore(
        &mut self,
        hyp: &Hyp,
        var: &str,
        bits: &[usize],
        cands: &[usize],
    ) -> Result<Option<Encoding>, Stop> {
        let (model, nm) = (self.s.model, self.s.nm);
        let debug = std::env::var_os("RUSTSYS_DISCOVERY_TRACE").is_some();
        let first = &model.states[bits[0]];
        let (Some((cond, _)), Some(c0)) = (first.arst, reset_code(self.s, bits)) else {
            return Ok(None);
        };
        let cand_set: HashSet<usize> = cands.iter().copied().collect();
        let without = |keep: &dyn Fn(usize, &Relation) -> bool| -> Hyp {
            hyp.iter()
                .filter(|(si, rel)| !bits.contains(si) && keep(**si, rel))
                .map(|(a, b)| (*a, *b))
                .collect()
        };
        // Candidates: V's clock and V's reset condition as their only asynchronous control.
        let probe =
            without(&|_, rel| !matches!(rel, Relation::Cell(ci, _) if cand_set.contains(ci)));
        let mut reset: Vec<Option<bool>> = vec![None; cands.len()];
        let mut ok = vec![true; cands.len()];
        for round in 0..2 {
            self.randomize();
            self.derive(&probe, None);
            self.evaluate()?;
            let (clk, cnd) = (self.v(first.clk), self.v(cond));
            for (e, &ci) in cands.iter().enumerate() {
                let x = &nm.seq[ci];
                let (clear, preset) = (self.v(x.clear), self.v(x.preset));
                let r = if clear == cnd && preset == 0 {
                    Some(false)
                } else if preset == cnd && clear == 0 {
                    Some(true)
                } else {
                    None
                };
                if x.latch || self.v(x.clk) != clk || r.is_none() || (round > 0 && reset[e] != r) {
                    ok[e] = false;
                }
                reset[e] = r;
            }
        }
        let kept: Vec<usize> = (0..cands.len()).filter(|&e| ok[e]).collect();
        if kept.is_empty() {
            return Ok(None);
        }
        let mut t = Table {
            cands: kept.iter().map(|&e| cands[e]).collect(),
            reset: kept.iter().map(|&e| reset[e] == Some(true)).collect(),
            alive: vec![true; kept.len()],
            known: BTreeMap::new(),
            queue: VecDeque::new(),
            kills: 0,
        };
        t.known.insert(c0, t.reset.clone());
        t.queue.push_back(c0);
        let n = t.cands.len();
        let index: HashMap<usize, usize> =
            t.cands.iter().enumerate().map(|(e, ci)| (*ci, e)).collect();
        // Relations that a candidate gives up while it is one, and gets back when it leaves.
        let claims: HashMap<usize, (usize, Relation)> = hyp
            .iter()
            .filter(|(si, _)| !bits.contains(si))
            .filter_map(|(si, rel)| match rel {
                Relation::Cell(ci, _) => index.get(ci).map(|&e| (e, (*si, *rel))),
                _ => None,
            })
            .collect();
        let mut hypv =
            without(&|_, rel| !matches!(rel, Relation::Cell(ci, _) if index.contains_key(ci)));
        let restore = |t: &Table, hypv: &mut Hyp| {
            for (e, (si, rel)) in &claims {
                if !t.alive[*e] {
                    hypv.insert(*si, *rel);
                }
            }
        };
        // (a) Reset traces.
        if let Some(tr) = self.traces.take() {
            for w in 0..tr.rtl[bits[0]].len() {
                let words: Vec<u64> = bits.iter().map(|&si| tr.rtl[si][w]).collect();
                let next: Vec<u64> = t.cands.iter().map(|&ci| tr.gate[ci][w]).collect();
                for (code, lanes) in classes(&words, u64::MAX) {
                    t.absorb(code, lanes, &next);
                }
            }
            self.traces = Some(tr);
        }
        restore(&t, &mut hypv);
        let next_bits: Vec<B> = bits.iter().map(|&si| model.states[si].next).collect();
        // (b) Random simulation from every collected code.
        let mut simulated = HashSet::new();
        while let Some(c) = t.queue.pop_front() {
            if !simulated.insert(c) {
                continue;
            }
            for _ in 0..SIM_ROUNDS {
                let values = t.known[&c].clone();
                self.randomize();
                self.derive(&hypv, Some(false));
                for (b, &si) in bits.iter().enumerate() {
                    let q = self.q(si);
                    self.fv[q] = mask(bit(c, b));
                }
                for e in 0..n {
                    if t.alive[e] {
                        let iq = self.iq(t.cands[e]);
                        self.fv[iq] = mask(values[e]);
                    }
                }
                self.evaluate()?;
                let active = !self.v(cond);
                let words: Vec<u64> = next_bits.iter().map(|b| self.v(*b)).collect();
                let next: Vec<u64> = t.cands.iter().map(|&ci| self.v(nm.seq[ci].next)).collect();
                for (code, lanes) in classes(&words, active) {
                    t.absorb(code, lanes, &next);
                }
                restore(&t, &mut hypv);
            }
        }
        // (c) SAT from every collected code.
        let mut roots = next_bits.clone();
        roots.push(cond);
        roots.extend(t.cands.iter().map(|&ci| nm.seq[ci].next));
        let order = self
            .s
            .c
            .order(&roots)
            .map_err(|cy| Stop::Error(format!("combinational cycle through {}", cy.0)))?;
        if let Some(left) = &mut self.remaining_work {
            let work = (order.len() as u64).saturating_mul(8);
            if work > *left {
                return Err(Stop::Undecided(
                    "re-encoded register exploration work budget reached".into(),
                ));
            }
            *left -= work;
        }
        let (mut solver, map) = prove::encode(self.s.c, &order);
        let konst = order.len() as u32;
        solver.add_clause(&[Lit::pos(konst)]);
        let lit = |b: B| -> Option<Lit> {
            match b {
                B::T => Some(Lit::pos(konst)),
                B::F => Some(Lit::neg(konst)),
                B::V(v) => map.get(&v).map(|&x| Lit::pos(x)),
            }
        };
        let q = |si: usize| lit(B::V(model.states[si].q));
        let iq = |ci: usize| lit(B::V(nm.seq[ci].iq));
        let equal = |s: &mut Solver, a: Option<Lit>, b: Option<Lit>, inv: bool| {
            if let (Some(a), Some(b)) = (a, b) {
                let b = if inv { !b } else { b };
                s.add_clause(&[!a, b]);
                s.add_clause(&[a, !b]);
            }
        };
        let fix = |s: &mut Solver, a: Option<Lit>, v: bool| {
            if let Some(a) = a {
                s.add_clause(&[if v { a } else { !a }]);
            }
        };
        let relate = |s: &mut Solver, si: usize, rel: Relation| match rel {
            Relation::Cell(ci, p) => equal(s, iq(ci), q(si), p),
            Relation::Shares(ri, inv) => equal(s, q(si), q(ri), inv),
            Relation::Constant(k) => fix(s, q(si), k),
        };
        // The hypothesis on the cone's free variables; re-encoded registers already found
        // take their valid codes instead of any relation of their bits and elements.
        let encoded_rtl: HashSet<usize> = self
            .encodings
            .iter()
            .flat_map(|e| e.rtl.iter().copied())
            .collect();
        let encoded_gate: HashSet<usize> = self
            .encodings
            .iter()
            .flat_map(|e| e.gate.iter().copied())
            .collect();
        let touches = |si: usize, ci: Option<usize>| {
            encoded_rtl.contains(&si) || ci.is_some_and(|ci| encoded_gate.contains(&ci))
        };
        for (ci, owner) in self.corr.owners.iter().enumerate() {
            if let Some(si) = owner {
                if !bits.contains(si) && !index.contains_key(&ci) && !touches(*si, Some(ci)) {
                    let parity = self.corr.paired[*si].is_some_and(|(_, p)| p);
                    equal(&mut solver, iq(ci), q(*si), parity);
                }
            }
        }
        for &(si, ri, inv) in &self.corr.equivalents {
            if !touches(si, None) && !touches(ri, None) {
                equal(&mut solver, q(si), q(ri), inv);
            }
        }
        for (si, k) in self.corr.constants.iter().enumerate() {
            if let Some(k) = k {
                fix(&mut solver, q(si), *k);
            }
        }
        let claimed_rtl: HashMap<usize, usize> =
            claims.iter().map(|(e, (si, _))| (*si, *e)).collect();
        for (&si, &rel) in &hypv {
            let other = match rel {
                Relation::Cell(ci, _) => touches(si, Some(ci)),
                Relation::Shares(ri, _) => touches(si, None) || touches(ri, None),
                Relation::Constant(_) => touches(si, None),
            };
            if !other && !claimed_rtl.contains_key(&si) {
                relate(&mut solver, si, rel);
            }
        }
        let hyp_cells: HashSet<usize> = hypv
            .values()
            .filter_map(|r| match r {
                Relation::Cell(ci, _) => Some(*ci),
                _ => None,
            })
            .collect();
        for &si in &self.unmatched {
            if !bits.contains(&si)
                && !encoded_rtl.contains(&si)
                && !hypv.contains_key(&si)
                && !claimed_rtl.contains_key(&si)
            {
                fix(&mut solver, q(si), self.rtl_phase(si));
            }
        }
        for &ci in &self.unused {
            if !index.contains_key(&ci) && !encoded_gate.contains(&ci) && !hyp_cells.contains(&ci) {
                fix(&mut solver, iq(ci), self.cell_phase(ci));
            }
        }
        for enc in &self.encodings {
            let selectors: Vec<Lit> = enc
                .codes
                .iter()
                .map(|_| Lit::pos(solver.new_var()))
                .collect();
            solver.add_clause(&selectors);
            for (&sel, &(rc, gc)) in selectors.iter().zip(&enc.codes) {
                for (b, &si) in enc.rtl.iter().enumerate() {
                    if let Some(l) = q(si) {
                        solver.add_clause(&[!sel, if bit(rc, b) { l } else { !l }]);
                    }
                }
                for (i, &ci) in enc.gate.iter().enumerate() {
                    if let Some(l) = iq(ci) {
                        solver.add_clause(&[!sel, if bit(gc, i) { l } else { !l }]);
                    }
                }
            }
        }
        // A candidate that left is constrained by its relation, or held at its reset value.
        let mut settled = vec![false; n];
        let mut settle = |solver: &mut Solver, t: &Table| {
            for e in 0..n {
                if !t.alive[e] && !settled[e] {
                    settled[e] = true;
                    match claims.get(&e) {
                        Some((si, rel)) => relate(solver, *si, *rel),
                        None => fix(solver, iq(t.cands[e]), t.reset[e]),
                    }
                }
            }
        };
        settle(&mut solver, &t);
        let not_cond = lit(cond).map(|l| !l);
        let mut calls = 0usize;
        for _ in 0..PASSES {
            let kills = t.kills;
            let mut pending: VecDeque<u64> = t.known.keys().copied().collect();
            let mut done = HashSet::new();
            while let Some(c) = pending.pop_front() {
                if !done.insert(c) {
                    continue;
                }
                let act = Lit::pos(solver.new_var());
                loop {
                    calls += 1;
                    if calls > MAX_SAT_CALLS {
                        if debug {
                            eprintln!("explore {var}: SAT call budget reached");
                        }
                        return Ok(None);
                    }
                    let values = t.known[&c].clone();
                    let mut assume = vec![act];
                    assume.extend(not_cond);
                    for (b, &si) in bits.iter().enumerate() {
                        if let Some(l) = q(si) {
                            assume.push(if bit(c, b) { l } else { !l });
                        }
                    }
                    for e in 0..n {
                        if !t.alive[e] {
                            continue;
                        }
                        if let Some(l) = iq(t.cands[e]) {
                            assume.push(if values[e] { l } else { !l });
                        }
                        // The relation's RTL state waits at its reset value meanwhile.
                        if let Some((si, _)) = claims.get(&e) {
                            if let Some(l) = q(*si) {
                                assume.push(if self.rtl_phase(*si) { l } else { !l });
                            }
                        }
                    }
                    let m = match solver.solve_limited(&assume, SAT_CONFLICTS, u32::MAX) {
                        Answer::Unsat => break,
                        Answer::Unknown => {
                            if debug {
                                eprintln!("explore {var}: SAT conflict budget reached");
                            }
                            return Ok(None);
                        }
                        Answer::Sat(m) => m,
                    };
                    let val = |b: B| match b {
                        B::T => true,
                        B::F => false,
                        B::V(v) => map.get(&v).is_some_and(|&x| m[x as usize]),
                    };
                    let code = next_bits
                        .iter()
                        .enumerate()
                        .fold(0u64, |acc, (b, x)| acc | (u64::from(val(*x)) << b));
                    let next: Vec<u64> = t
                        .cands
                        .iter()
                        .map(|&ci| u64::from(val(nm.seq[ci].next)))
                        .collect();
                    let mut block = vec![!act];
                    for (b, x) in next_bits.iter().enumerate() {
                        if let Some(l) = lit(*x) {
                            block.push(if bit(code, b) { !l } else { l });
                        }
                    }
                    solver.add_clause(&block);
                    let had = t.known.contains_key(&code);
                    t.absorb(code, 1, &next);
                    settle(&mut solver, &t);
                    if !had {
                        pending.push_back(code);
                    }
                }
            }
            t.queue.clear();
            if t.kills == kills {
                break;
            }
        }
        // The register: candidates left whose value differs between codes.
        let codes: Vec<u64> = t.known.keys().copied().collect();
        let base = &t.known[&codes[0]];
        let cluster: Vec<usize> = (0..n)
            .filter(|&e| t.alive[e] && codes.iter().any(|c| t.known[c][e] != base[e]))
            .collect();
        if debug {
            eprintln!(
                "explore {var}: {} codes {codes:?} after {calls} SAT calls; elements {:?}",
                codes.len(),
                cluster
                    .iter()
                    .map(|&e| nm.seq[t.cands[e]].inst.as_str())
                    .collect::<Vec<_>>()
            );
        }
        // Without a re-encoding, a code set smaller than all codes is still a candidate
        // invariant: synthesis may have dropped the other codes' behavior (the export adds it
        // only when a refuted obligation needs it, and then proves it).
        let invariant = |s: &mut Self| {
            s.invariants.retain(|e| e.variable != var);
            if codes.len() < 1 << bits.len() {
                s.invariants.push(Encoding {
                    variable: var.to_string(),
                    rtl: bits.to_vec(),
                    gate: Vec::new(),
                    codes: codes.iter().map(|&c| (c, 0)).collect(),
                });
            }
        };
        if cluster.is_empty() || cluster.len() > 64 {
            invariant(self);
            return Ok(None);
        }
        let table: Vec<(u64, u64)> = codes
            .iter()
            .map(|c| {
                let g = cluster
                    .iter()
                    .enumerate()
                    .fold(0u64, |acc, (i, &e)| acc | (u64::from(t.known[c][e]) << i));
                (*c, g)
            })
            .collect();
        let distinct: HashSet<u64> = table.iter().map(|(_, g)| *g).collect();
        let copy = |e: usize| {
            (0..bits.len()).any(|b| {
                [false, true]
                    .iter()
                    .any(|&inv| codes.iter().all(|c| t.known[c][e] == (bit(*c, b) ^ inv)))
            })
        };
        if distinct.len() != table.len() || cluster.iter().all(|&e| copy(e)) {
            invariant(self);
            return Ok(None);
        }
        self.invariants.retain(|e| e.variable != var);
        Ok(Some(Encoding {
            variable: var.to_string(),
            rtl: bits.to_vec(),
            gate: cluster.iter().map(|&e| t.cands[e]).collect(),
            codes: table,
        }))
    }
}
