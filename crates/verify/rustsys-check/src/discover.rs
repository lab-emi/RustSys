// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Discovered state correspondence (CHECK-REQ-0032): signal correspondence by induction
//! [@vaneijk2000sequential], decided by seeded random simulation. Every relation is assumed for
//! the current state values while the next-state, clock and asynchronous functions are evaluated;
//! a relation whose obligation fails in some lane is dropped. Discovery only proposes relations:
//! each is then validated and proved like a named one.

mod support;

use std::collections::{HashMap, HashSet};

use rusteda_core::job::Ctx;

use crate::Stop;
use crate::circ::{B, Circuit};
use crate::corr::{Corr, Relation, Setting};
use crate::netlist;
use crate::prove::Rng;

/// Rounds of 64 lanes that must all accept the current relations.
const ROUNDS: usize = 32;
/// Rounds that form the proposals of shared and cell relations.
const PROPOSE_ROUNDS: usize = 8;
/// A proposal that refinement rejected this often is not made again; this bounds the
/// propose-and-refine rounds of `discover`.
const MAX_FAILURES: u32 = 2;
/// A shared or cell proposal must agree in all but this fraction of active lanes.
const MAX_DISAGREEMENT: f64 = 0.25;

/// Refinement drops the least trusted failing relations first: a wrong guess of an element or a
/// shared state should not take the constants that depend on it along (CHECK-REQ-0032).
fn trust(rel: Relation) -> u8 {
    match rel {
        Relation::Shares(..) => 0,
        Relation::Cell(..) => 1,
        Relation::Constant(_) => 2,
    }
}

fn mask(b: bool) -> u64 {
    if b { u64::MAX } else { 0 }
}

/// Whether the after-reset pulse asserts `b` (through aliases and inversions to an input).
fn asserted(c: &Circuit, b: B, pulse: &HashMap<u32, bool>) -> bool {
    let B::V(v) = b else { return false };
    netlist::trace(c, v).is_some_and(|(i, inv)| pulse.get(&i).is_some_and(|x| *x ^ inv))
}

type Hyp = HashMap<usize, Relation>;

/// Values recorded per lane word of one proposal round: random state, state 0, state 1.
#[derive(Default, Clone)]
struct Words {
    rnd: Vec<u64>,
    zero: Vec<u64>,
    one: Vec<u64>,
}

struct Search<'a, 'b> {
    s: &'a Setting<'b>,
    corr: &'a Corr,
    order: Vec<u32>,
    frees: Vec<u32>,
    fv: Vec<u64>,
    vals: Vec<u64>,
    rng: Rng,
    /// Kind (a) candidates: needed flip-flops that may be constants.
    consts: Vec<usize>,
    /// Kind (b) candidates: needed flip-flops with an established base value.
    sharers: Vec<usize>,
    /// Kind (b) representatives: owners of netlist elements with an established base value.
    anchors: Vec<usize>,
    /// Kind (c): every unmatched RTL state and every unused netlist element.
    unmatched: Vec<usize>,
    unused: Vec<usize>,
    cell_base: Vec<Option<bool>>,
    /// Support of each RTL state's next function without the state itself, sorted.
    supports: HashMap<usize, Vec<u32>>,
}

impl<'a, 'b> Search<'a, 'b> {
    fn new(s: &'a Setting<'b>, corr: &'a Corr, seed: u64) -> Result<Self, String> {
        let (c, model, nm) = (s.c, s.model, s.nm);
        let needed = needed(s, corr)?;
        let flop = |si: usize| !s.restricted(si);
        let consts: Vec<usize> = needed
            .iter()
            .copied()
            .filter(|&si| {
                flop(si)
                    && (!s.pulse.is_empty() || s.uninitialized_dont_care)
                    && (s.base(si).is_some()
                        || (s.uninitialized_dont_care && model.states[si].arst.is_none()))
            })
            .collect();
        let sharers: Vec<usize> = if s.pulse.is_empty() {
            Vec::new()
        } else {
            needed
                .iter()
                .copied()
                .filter(|&si| flop(si) && s.base(si).is_some())
                .collect()
        };
        let anchors: Vec<usize> = corr
            .owners
            .iter()
            .flatten()
            .copied()
            .filter(|&ri| flop(ri) && s.base(ri).is_some())
            .collect();
        let cell_base = nm
            .seq
            .iter()
            .map(|x| {
                match (
                    asserted(c, x.clear, s.pulse),
                    asserted(c, x.preset, s.pulse),
                ) {
                    (true, false) => Some(false),
                    (false, true) => Some(true),
                    _ => None,
                }
            })
            .collect();
        // Everything the checks and proposals evaluate, in a fixed order: the order of the free
        // variables decides which random word each gets (CHECK-REQ-0043).
        let mut roots = Vec::new();
        let mut rtl: Vec<usize> = needed
            .iter()
            .chain(&corr.unmatched)
            .chain(&anchors)
            .copied()
            .collect();
        rtl.sort_unstable();
        rtl.dedup();
        for &si in &rtl {
            let st = &model.states[si];
            roots.extend([st.next, st.clk]);
            roots.extend(st.arst.map(|(cnd, _)| cnd));
        }
        for x in &nm.seq {
            roots.extend([x.next, x.clk, x.clear, x.preset]);
        }
        let order = c
            .order(&roots)
            .map_err(|cy| format!("combinational cycle through circuit variable {}", cy.0))?;
        let frees = order.iter().copied().filter(|v| c.is_free(*v)).collect();
        Ok(Search {
            s,
            corr,
            order,
            frees,
            fv: vec![0; c.defs.len()],
            vals: vec![0; c.defs.len()],
            rng: Rng(seed ^ 0x5eed_c0de),
            consts,
            sharers,
            anchors,
            unmatched: corr.unmatched.clone(),
            unused: corr.unused.clone(),
            cell_base,
            supports: HashMap::new(),
        })
    }

    /// Structural similarity of two states' next functions: the Jaccard index of their supports
    /// without the states themselves [@vaneijk2000sequential]. Random simulation cannot tell
    /// apart registers that rarely change; their next functions read different signals.
    fn similarity(&mut self, a: usize, b: usize) -> f64 {
        for si in [a, b] {
            if !self.supports.contains_key(&si) {
                let st = &self.s.model.states[si];
                let mut sup = self.s.c.support(&[st.next]).unwrap_or_default();
                sup.retain(|v| *v != st.q);
                sup.sort_unstable();
                self.supports.insert(si, sup);
            }
        }
        let (x, y) = (&self.supports[&a], &self.supports[&b]);
        let (mut i, mut j, mut both) = (0, 0, 0usize);
        while i < x.len() && j < y.len() {
            match x[i].cmp(&y[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    both += 1;
                    i += 1;
                    j += 1;
                }
            }
        }
        let all = x.len() + y.len() - both;
        if all == 0 {
            1.0
        } else {
            both as f64 / all as f64
        }
    }

    fn q(&self, si: usize) -> usize {
        self.s.model.states[si].q as usize
    }

    fn iq(&self, ci: usize) -> usize {
        self.s.nm.seq[ci].iq as usize
    }

    /// Fresh random values for every free variable.
    fn randomize(&mut self) {
        for &v in &self.frees {
            self.fv[v as usize] = self.rng.next();
        }
    }

    /// Overwrites the values that the correspondence and the relations determine. `own` sets
    /// every state still without a relation (and every unused element) to that constant.
    fn derive(&mut self, hyp: &Hyp, own: Option<bool>) {
        let corr = self.corr;
        for (si, k) in corr.constants.iter().enumerate() {
            if let Some(k) = k {
                let q = self.q(si);
                self.fv[q] = mask(*k);
            }
        }
        if let Some(x) = own {
            for i in 0..self.unmatched.len() {
                let si = self.unmatched[i];
                if !matches!(
                    hyp.get(&si),
                    Some(Relation::Shares(..) | Relation::Cell(..))
                ) {
                    let q = self.q(si);
                    self.fv[q] = mask(x);
                }
            }
            for i in 0..self.unused.len() {
                let iq = self.iq(self.unused[i]);
                self.fv[iq] = mask(x);
            }
        }
        for (&si, rel) in hyp {
            if let Relation::Constant(k) = rel {
                let q = self.q(si);
                self.fv[q] = mask(*k);
            }
        }
        for &(si, ri, inv) in &corr.equivalents {
            let (q, r) = (self.q(si), self.q(ri));
            self.fv[q] = self.fv[r] ^ mask(inv);
        }
        for (&si, rel) in hyp {
            if let Relation::Shares(ri, inv) = rel {
                let (q, r) = (self.q(si), self.q(*ri));
                self.fv[q] = self.fv[r] ^ mask(*inv);
            }
        }
        for (ci, owner) in corr.owners.iter().enumerate() {
            if let Some(si) = owner {
                let parity = corr.paired[*si].expect("owner is paired").1;
                let (iq, q) = (self.iq(ci), self.q(*si));
                self.fv[iq] = self.fv[q] ^ mask(parity);
            }
        }
        for (&si, rel) in hyp {
            if let Relation::Cell(ci, parity) = rel {
                let (iq, q) = (self.iq(*ci), self.q(si));
                self.fv[iq] = self.fv[q] ^ mask(*parity);
            }
        }
    }

    fn evaluate(&mut self) {
        let fv = &self.fv;
        self.s
            .c
            .eval_dense(&self.order, &|v| fv[v as usize], &mut self.vals);
    }

    fn v(&self, b: B) -> u64 {
        Circuit::value_dense(b, &self.vals)
    }

    /// The obligations of CHECK-REQ-0040 between RTL state `si` and element `ci` in this round.
    fn matches_cell(&self, si: usize, ci: usize, parity: bool) -> bool {
        let (st, x) = (&self.s.model.states[si], &self.s.nm.seq[ci]);
        if st.latch != x.latch || self.v(st.clk) != self.v(x.clk) {
            return false;
        }
        let (force0, force1) = if parity {
            (self.v(x.preset), self.v(x.clear))
        } else {
            (self.v(x.clear), self.v(x.preset))
        };
        let (want0, want1) = match st.arst {
            None => (0, 0),
            Some((cnd, false)) => (self.v(cnd), 0),
            Some((cnd, true)) => (0, self.v(cnd)),
        };
        if force0 != want0 || force1 != want1 {
            return false;
        }
        let mut active = !st.arst.map_or(0, |(cnd, _)| self.v(cnd));
        if st.latch {
            active &= self.v(st.clk);
        }
        (self.v(st.next) ^ self.v(x.next) ^ mask(parity)) & active == 0
    }

    /// The element of an RTL state and its parity, by name or by a cell relation in `hyp`.
    fn element(&self, si: usize, hyp: &Hyp) -> Option<(usize, bool)> {
        match (self.corr.paired[si], hyp.get(&si)) {
            (Some(x), _) => Some(x),
            (None, Some(Relation::Cell(ci, parity))) => Some((*ci, *parity)),
            _ => None,
        }
    }

    fn holds(&self, si: usize, rel: Relation, hyp: &Hyp) -> bool {
        let st = &self.s.model.states[si];
        match rel {
            Relation::Constant(k) => {
                let active = !st.arst.map_or(0, |(cnd, _)| self.v(cnd));
                (self.v(st.next) ^ mask(k)) & active == 0
            }
            Relation::Shares(ri, inv) => match self.element(ri, hyp) {
                Some((ci, parity)) => self.matches_cell(si, ci, parity ^ inv),
                None => false,
            },
            Relation::Cell(ci, parity) => self.matches_cell(si, ci, parity),
        }
    }

    /// Drops relations until all of them hold in `ROUNDS` rounds: in a failing round only the
    /// least trusted failing relations go. An unreset constant chosen under CHECK-REQ-0050 may
    /// change its value once.
    fn refine(&mut self, hyp: &mut Hyp, failures: &mut HashMap<(usize, Relation), u32>) {
        'again: loop {
            for _ in 0..ROUNDS {
                if hyp.is_empty() {
                    return;
                }
                self.randomize();
                self.derive(hyp, None);
                self.evaluate();
                let bad: Vec<(usize, Relation)> = hyp
                    .iter()
                    .filter(|(si, rel)| !self.holds(**si, **rel, hyp))
                    .map(|(si, rel)| (*si, *rel))
                    .collect();
                let Some(low) = bad.iter().map(|(_, r)| trust(*r)).min() else {
                    continue;
                };
                for (si, rel) in bad.into_iter().filter(|(_, r)| trust(*r) == low) {
                    hyp.remove(&si);
                    *failures.entry((si, rel)).or_default() += 1;
                    if let Relation::Constant(k) = rel {
                        let flip = (si, Relation::Constant(!k));
                        if self.s.model.states[si].arst.is_none() && !failures.contains_key(&flip) {
                            hyp.insert(si, flip.1);
                        }
                    }
                }
                continue 'again;
            }
            return;
        }
    }

    /// Records `f` of every item in the three environments of `PROPOSE_ROUNDS` rounds.
    fn record(&mut self, hyp: &Hyp, items: &[B]) -> Vec<Words> {
        let mut out = vec![Words::default(); items.len()];
        for _ in 0..PROPOSE_ROUNDS {
            self.randomize();
            let base = self.fv.clone();
            for own in [None, Some(false), Some(true)] {
                self.fv.copy_from_slice(&base);
                self.derive(hyp, own);
                self.evaluate();
                for (w, b) in out.iter_mut().zip(items) {
                    // A state value is a free variable that may lie outside the evaluated cones.
                    let x = match *b {
                        B::V(v) if self.s.c.is_free(v) => self.fv[v as usize],
                        _ => self.v(*b),
                    };
                    match own {
                        None => w.rnd.push(x),
                        Some(false) => w.zero.push(x),
                        Some(true) => w.one.push(x),
                    }
                }
            }
            self.fv.copy_from_slice(&base);
        }
        out
    }

    /// Proposes shared (b) and cell (c) relations for states without one.
    fn propose(
        &mut self,
        hyp: &Hyp,
        failures: &HashMap<(usize, Relation), u32>,
    ) -> Vec<(usize, Relation)> {
        let (model, nm) = (self.s.model, self.s.nm);
        let free_sharers: Vec<usize> = self
            .sharers
            .iter()
            .copied()
            .filter(|si| !hyp.contains_key(si))
            .collect();
        let taken: HashSet<usize> = hyp
            .values()
            .filter_map(|r| match r {
                Relation::Cell(ci, _) => Some(*ci),
                _ => None,
            })
            .collect();
        let free_cells: Vec<usize> = self
            .unused
            .iter()
            .copied()
            .filter(|ci| !taken.contains(ci))
            .collect();
        let free_rtl: Vec<usize> = self
            .unmatched
            .iter()
            .copied()
            .filter(|si| !matches!(hyp.get(si), Some(Relation::Shares(..) | Relation::Cell(..))))
            .collect();
        let mut anchors = self.anchors.clone();
        anchors.extend(hyp.iter().filter_map(|(si, r)| {
            (matches!(r, Relation::Cell(..))
                && !self.s.restricted(*si)
                && self.s.base(*si).is_some())
            .then_some(*si)
        }));
        if (free_sharers.is_empty() || anchors.is_empty())
            && (free_cells.is_empty() || free_rtl.is_empty())
        {
            return Vec::new();
        }
        // Items: for every RTL state of interest next, clock and asynchronous condition and the
        // state value itself; for every free element its four functions.
        let mut rtl_ids: Vec<usize> = free_sharers.clone();
        rtl_ids.extend(&anchors);
        rtl_ids.extend(&free_rtl);
        rtl_ids.sort_unstable();
        rtl_ids.dedup();
        let mut items = Vec::new();
        for &si in &rtl_ids {
            let st = &model.states[si];
            items.extend([
                st.next,
                st.clk,
                st.arst.map_or(B::F, |(cnd, _)| cnd),
                B::V(st.q),
            ]);
        }
        for &ci in &free_cells {
            let x = &nm.seq[ci];
            items.extend([x.next, x.clk, x.clear, x.preset]);
        }
        let w = self.record(hyp, &items);
        let at: HashMap<usize, usize> = rtl_ids
            .iter()
            .enumerate()
            .map(|(k, si)| (*si, 4 * k))
            .collect();
        let cell_at = |k: usize| 4 * rtl_ids.len() + 4 * k;
        let key = |a: &[u64], b: &[u64]| -> Vec<u64> { a.iter().chain(b).copied().collect() };
        let ok = |bad: u32, active: u32| f64::from(bad) <= MAX_DISAGREEMENT * f64::from(active);
        let tried = |si: usize, rel: Relation| {
            failures.get(&(si, rel)).copied().unwrap_or(0) >= MAX_FAILURES
        };
        let mut out = Vec::new();
        // (b) Shares: same clock and asynchronous condition; the candidate's next value with its
        // own value set to the representative's (through `zero` and `one`) agrees in most lanes.
        // All candidates are proposed together, so states that depend on each other's relation
        // can hold together under refinement.
        let mut by_key: HashMap<Vec<u64>, Vec<usize>> = HashMap::new();
        for &ri in &anchors {
            let k = at[&ri];
            by_key
                .entry(key(&w[k + 1].rnd, &w[k + 2].rnd))
                .or_default()
                .push(ri);
        }
        for &si in &free_sharers {
            let k = at[&si];
            let Some(group) = by_key.get(&key(&w[k + 1].rnd, &w[k + 2].rnd)) else {
                continue;
            };
            let base = self.s.base(si).expect("sharer base");
            let passing: Vec<(u32, usize, bool)> = group
                .iter()
                .copied()
                .filter_map(|ri| {
                    let inv = base ^ self.s.base(ri).expect("anchor base");
                    if tried(si, Relation::Shares(ri, inv)) {
                        return None;
                    }
                    let r = at[&ri];
                    let (mut bad, mut active) = (0, 0);
                    for t in 0..PROPOSE_ROUNDS {
                        let own = w[r + 3].rnd[t] ^ mask(inv);
                        let next = (own & w[k].one[t]) | (!own & w[k].zero[t]);
                        let live = !w[k + 2].rnd[t];
                        bad += ((next ^ mask(inv) ^ w[r].rnd[t]) & live).count_ones();
                        active += live.count_ones();
                    }
                    ok(bad, active).then_some((bad, ri, inv))
                })
                .collect();
            // Rank: bits of the same RTL variable first, then structural similarity, then
            // simulation agreement.
            let var = variable(&model.states[si].name);
            let mut best: Option<(bool, f64, u32, usize, bool)> = None;
            for (bad, ri, inv) in passing {
                let same = variable(&model.states[ri].name) == var;
                let sim = self.similarity(si, ri);
                let better = match best {
                    None => true,
                    Some((s0, m0, b0, r0, _)) => {
                        (same, sim) > (s0, m0) || ((same, sim) == (s0, m0) && (bad, ri) < (b0, r0))
                    }
                };
                if better {
                    best = Some((same, sim, bad, ri, inv));
                }
            }
            let best = best.map(|(_, _, b, r, i)| (b, r, i));
            if let Some((_, ri, inv)) = best {
                out.push((si, Relation::Shares(ri, inv)));
            }
        }
        // (c) Cells: an unused element and an unmatched RTL state of the same kind with equal
        // clock and asynchronous behavior whose next values agree under one parity in most lanes.
        let (rtl_support, cell_support) =
            support::signatures(self.s, self.corr, hyp, &free_rtl, &free_cells);
        let mut used: HashSet<usize> = out.iter().map(|x| x.0).collect();
        for (n, &ci) in free_cells.iter().enumerate() {
            let x = &nm.seq[ci];
            let k = cell_at(n);
            let mut best: Option<(u32, f64, usize, bool)> = None;
            for &si in free_rtl.iter().filter(|si| !used.contains(si)) {
                let st = &model.states[si];
                let r = at[&si];
                if st.latch != x.latch || w[r + 1].rnd != w[k + 1].rnd {
                    continue;
                }
                for p in [false, true] {
                    if tried(si, Relation::Cell(ci, p)) {
                        continue;
                    }
                    if let (Some(bc), Some(br)) = (self.cell_base[ci], self.s.base(si)) {
                        if bc != br ^ p {
                            continue;
                        }
                    }
                    let (mut bad, mut active, mut forced) = (0, 0, false);
                    for t in 0..PROPOSE_ROUNDS {
                        let (clear, preset) = (w[k + 2].rnd[t], w[k + 3].rnd[t]);
                        let (f0, f1) = if p { (preset, clear) } else { (clear, preset) };
                        let cnd = w[r + 2].rnd[t];
                        let (want0, want1) = match st.arst {
                            None => (0, 0),
                            Some((_, false)) => (cnd, 0),
                            Some((_, true)) => (0, cnd),
                        };
                        forced |= f0 != want0 || f1 != want1;
                        let mut live = !cnd;
                        if st.latch {
                            live &= w[r + 1].rnd[t];
                        }
                        let (c0, c1) = (w[k].zero[t], w[k].one[t]);
                        let (s0, s1) = (w[r].zero[t], w[r].one[t]);
                        let differ = if p {
                            (c1 ^ !s0) | (c0 ^ !s1)
                        } else {
                            (c0 ^ s0) | (c1 ^ s1)
                        };
                        bad += (differ & live).count_ones();
                        active += live.count_ones();
                    }
                    let similarity = support::similarity(&rtl_support[&si], &cell_support[&ci]);
                    if !forced
                        && ok(bad, active)
                        && best
                            .is_none_or(|(b, sim, _, _)| bad < b || (bad == b && similarity > sim))
                    {
                        best = Some((bad, similarity, si, p));
                    }
                }
            }
            if let Some((_, _, si, p)) = best {
                used.insert(si);
                out.push((si, Relation::Cell(ci, p)));
            }
        }
        out
    }
}

/// The RTL variable of a state name: the name without its last bit index.
fn variable(name: &str) -> &str {
    match name.rfind('[') {
        Some(i) if name.ends_with(']') => &name[..i],
        _ => name,
    }
}

/// RTL state bits without a counterpart that the compared logic reads, transitively.
fn needed(s: &Setting<'_>, corr: &Corr) -> Result<Vec<usize>, String> {
    let model = s.model;
    let q_of: HashMap<u32, usize> = corr
        .unmatched
        .iter()
        .map(|&si| (model.states[si].q, si))
        .collect();
    let cone = |si: usize| {
        let st = &model.states[si];
        let mut r = vec![st.next, st.clk];
        r.extend(st.arst.map(|(cnd, _)| cnd));
        r
    };
    let mut roots: Vec<B> = model
        .outputs
        .iter()
        .flat_map(|(_, b, _)| b.clone())
        .collect();
    for si in 0..model.states.len() {
        if corr.paired[si].is_some() || corr.constants[si].is_some() {
            roots.extend(cone(si));
        }
    }
    let mut found: Vec<usize> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut visited: HashSet<u32> = HashSet::new();
    while !roots.is_empty() {
        let order =
            s.c.order(&roots)
                .map_err(|cy| format!("combinational cycle through circuit variable {}", cy.0))?;
        roots.clear();
        for v in order {
            if !visited.insert(v) {
                continue;
            }
            if let Some(&si) = q_of.get(&v) {
                if seen.insert(si) {
                    found.push(si);
                    roots.extend(cone(si));
                }
            }
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// Proposes and refines relations for the RTL state bits and netlist elements that names left
/// without a counterpart (CHECK-REQ-0032), never proposing an `excluded` relation. Proposing and
/// refining repeat until no relation changes, without a bound on the rounds: a round that changes
/// the relations either counts a failure of a dropped relation, and a relation that failed
/// `MAX_FAILURES` times is not proposed again, or only adds relations or replaces a constant by a
/// cell relation, so the rounds end. A chain of state whose relations each need the one before
/// takes a round per link. A cancel stops between rounds (JOB-REQ-0006).
pub(crate) fn discover(
    s: &Setting<'_>,
    corr: &Corr,
    seed: u64,
    excluded: &HashSet<(usize, Relation)>,
    ctx: Ctx<'_>,
) -> Result<Vec<(usize, Relation)>, Stop> {
    if corr.unmatched.is_empty() && corr.unused.is_empty() {
        return Ok(Vec::new());
    }
    let mut search = Search::new(s, corr, seed)?;
    let mut hyp: Hyp = HashMap::new();
    // Relations a proof rejected are never proposed again.
    let mut failures: HashMap<(usize, Relation), u32> =
        excluded.iter().map(|r| (*r, MAX_FAILURES)).collect();
    // Constants first, from the largest set; then shared and cell relations for the states left,
    // and constants again for states whose constant may hold now.
    let mut first = true;
    loop {
        ctx.checkpoint()?;
        let before = hyp.clone();
        let mut proposed = 0;
        for i in 0..search.consts.len() {
            let si = search.consts[i];
            if hyp.contains_key(&si) {
                continue;
            }
            let base = s.base(si);
            let pick = [base.unwrap_or(false), !base.unwrap_or(false)]
                .into_iter()
                .take(if base.is_some() { 1 } else { 2 })
                .map(Relation::Constant)
                .find(|r| failures.get(&(si, *r)).copied().unwrap_or(0) < MAX_FAILURES);
            if let Some(rel) = pick {
                hyp.insert(si, rel);
                proposed += 1;
            }
        }
        search.refine(&mut hyp, &mut failures);
        let props = search.propose(&hyp, &failures);
        proposed += props.len();
        for (si, rel) in props {
            hyp.insert(si, rel);
        }
        search.refine(&mut hyp, &mut failures);
        if proposed == 0 || (hyp == before && !first) {
            break;
        }
        first = false;
    }
    let mut out: Vec<(usize, Relation)> = hyp.into_iter().collect();
    out.sort_by_key(|x| x.0);
    Ok(out)
}

#[cfg(test)]
mod tests;
