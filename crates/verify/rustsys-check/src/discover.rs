// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Discovered state correspondence (CHECK-REQ-0032): signal correspondence by induction
//! [@vaneijk2000sequential], decided by seeded random simulation. Every relation is assumed for
//! the current state values while the next-state, clock and asynchronous functions are evaluated;
//! a relation whose obligation fails in some lane is dropped. Discovery only proposes relations:
//! each is then validated and proved like a named one.

mod encode;
mod explore;
mod export;
mod propose;
mod support;
mod trace;

pub(crate) use export::for_export;
use export::seeded;

use std::collections::{HashMap, HashSet};

use rusteda_core::job::Ctx;

use crate::Stop;
use crate::circ::{B, Circuit};
use crate::corr::{Corr, Encoding, Relation, Setting};
use crate::netlist;
use crate::prove::Rng;
use crate::reset;

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
    /// Optional charge for dense evaluation work; export proposals always set it.
    remaining_work: Option<u64>,
    /// Structural support sets are ranking hints, never proof premises.
    /// Disable them before allocation when their conservative size exceeds the budget.
    rank_supports: bool,
    sketch_bytes: usize,
    /// Normalize only proposal samples to each register's reset encoding.
    /// This is a heuristic environment, never a reset or proof premise.
    normalized_own: bool,
    /// Reset-trace signatures of a state-map search; rank and filter proposals only.
    traces: Option<trace::Traces>,
    /// Re-encoded registers of a state-map search; refinement may add codes it observes.
    encodings: Vec<Encoding>,
    /// Refinement rounds whose lanes contradicted an encoding (a diagnostic; the export decides).
    encoding_failures: Vec<u32>,
    /// Propose only relations with equal changing reset traces (the seed of `explore`).
    exact_only: bool,
    /// Variables of hinted re-encoded registers, which exploration leaves as given.
    hinted: HashSet<String>,
    /// Elements that a name hint relates to an RTL state (kept out of exploration).
    named_cells: HashSet<usize>,
    /// Registers that exploration found in fewer codes than they have, without a re-encoding:
    /// candidate valid-code invariants (`gate` empty).
    invariants: Vec<Encoding>,
    /// The netlist's names, for tie breaks between proposals simulation cannot tell apart.
    names: Option<std::rc::Rc<crate::alias::Names>>,
    /// State-map refinement: elements without any relation keep their reset value instead of a
    /// random one, as a re-encoded register's element for a code not yet in its table does
    /// while the register stays in its table (a filter only; the export proves everything).
    optimistic: bool,
}

impl<'a, 'b> Search<'a, 'b> {
    fn new(s: &'a Setting<'b>, corr: &'a Corr, seed: u64) -> Result<Self, String> {
        let needed = needed(s, corr)?;
        Self::with_needed(s, corr, seed, needed)
    }

    fn with_needed(
        s: &'a Setting<'b>,
        corr: &'a Corr,
        seed: u64,
        needed: Vec<usize>,
    ) -> Result<Self, String> {
        let (c, model, nm) = (s.c, s.model, s.nm);
        let flop = |si: usize| !s.restricted(si);
        // Under an implementation state map the map chooses RTL initial values, so constants
        // and shared elements need no reset base; the exported obligations still prove them.
        let map = s.initial_state_map;
        let consts: Vec<usize> = needed
            .iter()
            .copied()
            .filter(|&si| {
                flop(si)
                    && (map
                        || ((!s.pulse.is_empty() || s.uninitialized_dont_care)
                            && (s.base(si).is_some()
                                || (s.uninitialized_dont_care
                                    && model.states[si].arst.is_none()))))
            })
            .collect();
        let sharers: Vec<usize> = if map {
            needed.iter().copied().filter(|&si| flop(si)).collect()
        } else if s.pulse.is_empty() {
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
            .filter(|&ri| flop(ri) && (map || s.base(ri).is_some()))
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
            .chain(corr.encodings.iter().flat_map(|e| e.rtl.iter()))
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
            remaining_work: None,
            rank_supports: true,
            sketch_bytes: 0,
            normalized_own: false,
            traces: None,
            encodings: corr.encodings.clone(),
            encoding_failures: vec![0; corr.encodings.len()],
            exact_only: false,
            hinted: HashSet::new(),
            named_cells: HashSet::new(),
            invariants: Vec::new(),
            names: None,
            optimistic: false,
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

    fn rtl_phase(&self, si: usize) -> bool {
        self.normalized_own && self.s.model.states[si].arst.is_some_and(|(_, value)| value)
    }

    fn cell_phase(&self, ci: usize) -> bool {
        let cell = &self.s.nm.seq[ci];
        self.normalized_own
            && reset::constant(self.s.c, cell.clear) == Some(false)
            && reset::constant(self.s.c, cell.preset) != Some(false)
    }

    /// Fresh random values for every free variable.
    fn randomize(&mut self) {
        for &v in &self.frees {
            self.fv[v as usize] = self.rng.next();
        }
    }

    /// Round `r` of a refinement under a state map: every fourth round sets free values to 1
    /// with probability 7/8, every other fourth round with 1/8, so carries into high counter
    /// bits and rare updates occur. Any distribution keeps refinement sound: a lane that
    /// satisfies all relations and violates one after a step refutes that one.
    fn randomize_round(&mut self, r: usize) {
        if !self.optimistic || r.is_multiple_of(2) {
            return self.randomize();
        }
        for &v in &self.frees {
            let (a, b, c) = (self.rng.next(), self.rng.next(), self.rng.next());
            self.fv[v as usize] = if r % 4 == 1 { a | b | c } else { a & b & c };
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
                    self.fv[q] = mask(x ^ self.rtl_phase(self.unmatched[i]));
                }
            }
            for i in 0..self.unused.len() {
                let iq = self.iq(self.unused[i]);
                self.fv[iq] = mask(x ^ self.cell_phase(self.unused[i]));
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
        // Re-encoded registers take a random valid code per lane, the same on both sides, over
        // any element relation of their bits and elements.
        for e in 0..self.encodings.len() {
            let n = self.encodings[e].codes.len() as u64;
            let (mut rtl, mut gate) = (
                vec![0u64; self.encodings[e].rtl.len()],
                vec![0u64; self.encodings[e].gate.len()],
            );
            for lane in 0..64 {
                let (rc, gc) = self.encodings[e].codes[(self.rng.next() % n) as usize];
                for (b, w) in rtl.iter_mut().enumerate() {
                    *w |= ((rc >> b) & 1) << lane;
                }
                for (i, w) in gate.iter_mut().enumerate() {
                    *w |= ((gc >> i) & 1) << lane;
                }
            }
            for (b, w) in rtl.into_iter().enumerate() {
                let q = self.q(self.encodings[e].rtl[b]);
                self.fv[q] = w;
            }
            for (i, w) in gate.into_iter().enumerate() {
                let iq = self.iq(self.encodings[e].gate[i]);
                self.fv[iq] = w;
            }
        }
    }

    fn evaluate(&mut self) -> Result<(), Stop> {
        if let Some(left) = &mut self.remaining_work {
            let work = self
                .order
                .len()
                .saturating_add(self.frees.len())
                .saturating_add(self.s.model.states.len())
                .saturating_add(self.s.nm.seq.len()) as u64;
            if work > *left {
                return Err(Stop::Undecided(
                    "state proposal evaluation-work budget reached".into(),
                ));
            }
            *left -= work;
        }
        let fv = &self.fv;
        self.s
            .c
            .eval_dense(&self.order, &|v| fv[v as usize], &mut self.vals);
        Ok(())
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
                // Under a state map no pulse establishes the constant: a reset to the other
                // value must never be active.
                let reset_ok = match st.arst {
                    Some((cnd, v)) if self.s.initial_state_map && v != k => self.v(cnd) == 0,
                    _ => true,
                };
                reset_ok && (self.v(st.next) ^ mask(k)) & active == 0
            }
            Relation::Shares(ri, inv) => match self.element(ri, hyp) {
                Some((ci, parity)) => self.matches_cell(si, ci, parity ^ inv),
                None => false,
            },
            Relation::Cell(ci, parity) => self.matches_cell(si, ci, parity),
        }
    }

    /// Counts the refinement rounds whose lanes contradict a re-encoded register: clocks or
    /// asynchronous controls, or, outside an asynchronous reset, a next element code that is
    /// invalid or decodes to another RTL code (a diagnostic; the export's obligations decide).
    fn check_encodings(&mut self) {
        for e in 0..self.encodings.len() {
            let enc = &self.encodings[e];
            let v = |b: B| Circuit::value_dense(b, &self.vals);
            let first = &self.s.model.states[enc.rtl[0]];
            let active = !first.arst.map_or(0, |(cnd, _)| v(cnd));
            let rtl_next: Vec<u64> = enc.rtl.iter().map(|&si| v(self.s.model.states[si].next)).collect();
            let gate_next: Vec<u64> = enc.gate.iter().map(|&ci| v(self.s.nm.seq[ci].next)).collect();
            let (rn, gn) = (encode::lane_codes(&rtl_next), encode::lane_codes(&gate_next));
            let bad = !encode::async_ok(enc, self.s, &v)
                || (0..64).any(|lane| (active >> lane) & 1 == 1 && enc.decode(gn[lane]) != Some(rn[lane]));
            if bad {
                self.encoding_failures[e] += 1;
            }
        }
    }

    /// Drops relations until all of them hold in `ROUNDS` rounds: in a failing round only the
    /// least trusted failing relations go. An unreset constant chosen under CHECK-REQ-0050 may
    /// change its value once.
    fn refine(
        &mut self,
        hyp: &mut Hyp,
        failures: &mut HashMap<(usize, Relation), u32>,
    ) -> Result<(), Stop> {
        'again: loop {
            for r in 0..ROUNDS {
                if hyp.is_empty() {
                    return Ok(());
                }
                self.randomize_round(r);
                self.derive(hyp, None);
                if self.optimistic {
                    let claimed: HashSet<usize> = hyp
                        .values()
                        .filter_map(|r| match r {
                            Relation::Cell(ci, _) => Some(*ci),
                            _ => None,
                        })
                        .collect();
                    for i in 0..self.unused.len() {
                        let ci = self.unused[i];
                        if !claimed.contains(&ci) {
                            let iq = self.iq(ci);
                            self.fv[iq] = mask(self.cell_phase(ci));
                        }
                    }
                }
                self.evaluate().map_err(|error| match error {
                    Stop::Undecided(message) => Stop::Undecided(format!(
                        "{message}; {} of {} state proposals remain",
                        hyp.len(),
                        self.corr.unused.len()
                    )),
                    other => other,
                })?;
                self.check_encodings();
                let bad: Vec<(usize, Relation)> = hyp
                    .iter()
                    .filter(|(si, rel)| !self.holds(**si, **rel, hyp))
                    .map(|(si, rel)| (*si, *rel))
                    .collect();
                // Under a state map, constants come from short reset traces and are the
                // weakest evidence: they go before shared states and elements.
                let map = self.s.initial_state_map;
                let rank = |r: Relation| match (map, r) {
                    (true, Relation::Constant(_)) => 0,
                    (true, other) => trust(other) + 1,
                    (false, other) => trust(other),
                };
                let Some(low) = bad.iter().map(|(_, r)| rank(*r)).min() else {
                    continue;
                };
                for (si, rel) in bad.into_iter().filter(|(_, r)| rank(*r) == low) {
                    if std::env::var_os("RUSTSYS_DISCOVERY_REMOVALS").is_some() {
                        eprintln!("removal {} {:?}", self.s.model.states[si].name, rel);
                    }
                    if let Ok(w) = std::env::var("RUSTSYS_DISCOVERY_WATCH") {
                        if self.s.model.states[si].name.contains(w.as_str()) {
                            self.explain_failure(si, rel, hyp);
                        }
                    }
                    hyp.remove(&si);
                    *failures.entry((si, rel)).or_default() += 1;
                    if let Relation::Constant(k) = rel {
                        let flip = (si, Relation::Constant(!k));
                        // A state map takes constants from reset traces only; no flip.
                        if !self.s.initial_state_map
                            && self.s.model.states[si].arst.is_none()
                            && !failures.contains_key(&flip)
                        {
                            hyp.insert(si, flip.1);
                        }
                    }
                }
                continue 'again;
            }
            return Ok(());
        }
    }

    /// Debug: why `rel` of `si` failed in the current lanes: the unexplained RTL states and
    /// elements its two next functions read, and paired reads whose values differ.
    fn explain_failure(&self, si: usize, rel: Relation, hyp: &Hyp) {
        let (model, nm) = (self.s.model, self.s.nm);
        let st = &model.states[si];
        let (ci, p) = match rel {
            Relation::Cell(ci, p) => (ci, p),
            Relation::Shares(ri, inv) => match self.element(ri, hyp) {
                Some((ci, parity)) => {
                    eprintln!("watch {}: shares {} (element {})", st.name, model.states[ri].name, nm.seq[ci].inst);
                    (ci, parity ^ inv)
                }
                None => {
                    eprintln!("watch {}: shares {} without element", st.name, model.states[ri].name);
                    return;
                }
            },
            Relation::Constant(_) => {
                eprintln!("watch {}: {:?} failed", st.name, rel);
                return;
            }
        };
        let x = &nm.seq[ci];
        let q_of: HashMap<u32, usize> = model.states.iter().enumerate().map(|(i, s)| (s.q, i)).collect();
        let iq_of: HashMap<u32, usize> = nm.seq.iter().enumerate().map(|(i, e)| (e.iq, i)).collect();
        let diff = (self.v(st.next) ^ self.v(x.next) ^ mask(p)) & !st.arst.map_or(0, |(c, _)| self.v(c));
        let lane = diff.trailing_zeros();
        let bitv = |w: u64| (w >> lane) & 1;
        let claimed: HashMap<usize, usize> = hyp
            .iter()
            .filter_map(|(r, rl)| match rl {
                Relation::Cell(c, _) => Some((*c, *r)),
                _ => None,
            })
            .collect();
        let mut out: Vec<String> = Vec::new();
        for v in self.s.c.support(&[st.next]).unwrap_or_default() {
            if let Some(&r) = q_of.get(&v) {
                let explained = hyp.contains_key(&r)
                    || self.corr.paired[r].is_some()
                    || self.corr.constants[r].is_some()
                    || self.encodings.iter().any(|e| e.rtl.contains(&r));
                if !explained {
                    out.push(format!("rtl:{}={}", model.states[r].name, bitv(self.fv[v as usize])));
                }
            }
        }
        for v in self.s.c.support(&[x.next]).unwrap_or_default() {
            if let Some(&c) = iq_of.get(&v) {
                let explained = claimed.contains_key(&c)
                    || self.corr.owners[c].is_some()
                    || self.encodings.iter().any(|e| e.gate.contains(&c));
                if !explained {
                    out.push(format!("gate:{}={}", nm.seq[c].inst, bitv(self.fv[v as usize])));
                }
            }
        }
        // Encodings the cones read, with the lane's RTL code.
        for enc in &self.encodings {
            let code = enc.rtl.iter().enumerate().fold(0u64, |a, (b, &r)| a | (bitv(self.fv[model.states[r].q as usize]) << b));
            let gcode = enc.gate.iter().enumerate().fold(0u64, |a, (b, &c)| a | (bitv(self.fv[nm.seq[c].iq as usize]) << b));
            let rn = enc.rtl.iter().enumerate().fold(0u64, |a, (b, &r)| a | (bitv(self.v(model.states[r].next)) << b));
            let gn = enc.gate.iter().enumerate().fold(0u64, |a, (b, &c)| a | (bitv(self.v(nm.seq[c].next)) << b));
            out.push(format!("enc:{}={code}/{gcode}->{rn}/{gn}", enc.variable.rsplit('.').next().unwrap_or("")));
        }
        // Relations of states this next reads that fail in the same lane.
        for v in self.s.c.support(&[st.next]).unwrap_or_default() {
            if let Some(&r) = q_of.get(&v) {
                if let Some(rl) = hyp.get(&r) {
                    if r != si && !self.holds(r, *rl, hyp) {
                        out.push(format!("failing:{}", model.states[r].name));
                    }
                }
            }
        }
        eprintln!(
            "watch {} ~ {}: clk {} async {} next differs in {} lanes (lane {lane}: rtl {} gate {}); unexplained reads: {}",
            st.name,
            x.inst,
            self.v(st.clk) == self.v(x.clk),
            self.matches_cell(si, ci, p) || diff != 0,
            diff.count_ones(),
            bitv(self.v(st.next)),
            bitv(self.v(x.next)),
            out.join(" ")
        );
    }

    /// Debug: RTL states without a relation, and the unexplained states their next reads.
    fn blockers(&self, hyp: &Hyp) {
        let model = self.s.model;
        let q_of: HashMap<u32, usize> =
            model.states.iter().enumerate().map(|(si, st)| (st.q, si)).collect();
        let explained = |si: usize| {
            hyp.contains_key(&si)
                || self.corr.paired[si].is_some()
                || self.corr.constants[si].is_some()
                || self.encodings.iter().any(|e| e.rtl.contains(&si))
                || self.corr.equivalents.iter().any(|e| e.0 == si)
        };
        let mut count: HashMap<usize, usize> = HashMap::new();
        let mut text = String::new();
        for &si in &self.unmatched {
            if explained(si) {
                continue;
            }
            let st = &model.states[si];
            let sup = self.s.c.support(&[st.next]).unwrap_or_default();
            let open: Vec<usize> = sup
                .iter()
                .filter_map(|v| q_of.get(v).copied())
                .filter(|&x| x != si && !explained(x))
                .collect();
            for &x in &open {
                *count.entry(x).or_default() += 1;
            }
            let tr = self.traces.as_ref().map(|t| trace::Traces::constant(&t.rtl[si]));
            text.push_str(&format!(
                "open {} trace-constant {:?} reads {} open: {}\n",
                st.name,
                tr,
                open.len(),
                open.iter().take(6).map(|x| model.states[*x].name.as_str()).collect::<Vec<_>>().join(" ")
            ));
        }
        if let Ok(names) = std::env::var("RUSTSYS_DISCOVERY_ELEMENTS") {
            for (si, rel) in hyp {
                if let Relation::Cell(ci, p) = rel {
                    if names.split(',').any(|n| n == self.s.nm.seq[*ci].inst) {
                        text.push_str(&format!("element {} ~ {} parity {p}\n", self.s.nm.seq[*ci].inst, model.states[*si].name));
                    }
                }
            }
        }
        let mut top: Vec<(usize, usize)> = count.into_iter().collect();
        top.sort_by_key(|b| std::cmp::Reverse(b.1));
        for (x, n) in top.iter().take(60) {
            text.push_str(&format!("blocker {} read by {n}\n", model.states[*x].name));
        }
        if let Some(path) = std::env::var_os("RUSTSYS_DISCOVERY_BLOCKERS") {
            let _ = std::fs::write(path, text);
        }
    }

    /// Records `f` of every item in the three environments of `PROPOSE_ROUNDS` rounds.
    fn record(&mut self, hyp: &Hyp, items: &[B]) -> Result<Vec<Words>, Stop> {
        let mut out = vec![Words::default(); items.len()];
        for _ in 0..PROPOSE_ROUNDS {
            self.randomize();
            let base = self.fv.clone();
            for own in [None, Some(false), Some(true)] {
                self.fv.copy_from_slice(&base);
                self.derive(hyp, own);
                self.evaluate()?;
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
        Ok(out)
    }

}

/// RTL state bits without a counterpart that the compared logic reads, transitively.
fn needed(s: &Setting<'_>, corr: &Corr) -> Result<Vec<usize>, String> {
    needed_limited(s, corr, None)
}

fn needed_limited(
    s: &Setting<'_>,
    corr: &Corr,
    max_walks: Option<usize>,
) -> Result<Vec<usize>, String> {
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
    let mut walks = 0usize;
    while !roots.is_empty() {
        if max_walks.is_some_and(|limit| walks >= limit) {
            return Err("warning[C0005]: state-proposal support-walk budget reached".into());
        }
        walks += 1;
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
    discover_inner(s, corr, seed, excluded, ctx, None, None, (&[], None), &mut None)
        .map(|(relations, _)| relations)
}

#[allow(clippy::too_many_arguments)]
fn discover_inner(
    s: &Setting<'_>,
    corr: &Corr,
    seed: u64,
    excluded: &HashSet<(usize, Relation)>,
    ctx: Ctx<'_>,
    limits: Option<&crate::ObligationExport>,
    traces: Option<trace::Traces>,
    (seed_relations, names): (&[(usize, Relation)], Option<&std::rc::Rc<crate::alias::Names>>),
    work: &mut Option<u64>,
) -> Result<(Vec<(usize, Relation)>, Vec<Encoding>), Stop> {
    if corr.unmatched.is_empty() && corr.unused.is_empty() {
        return Ok((Vec::new(), corr.encodings.clone()));
    }
    let mut search = if limits.is_some() {
        Search::with_needed(s, corr, seed, corr.unmatched.clone())?
    } else {
        Search::new(s, corr, seed)?
    };
    search.remaining_work = if limits.is_some() { *work } else { None };
    search.sketch_bytes = limits.map_or(0, |limit| limit.max_discovery_sketch_bytes);
    search.normalized_own = limits.is_some();
    search.rank_supports = limits.is_none_or(|limit| {
        (corr.unmatched.len() as u128 + corr.unused.len() as u128) * s.c.defs.len() as u128
            <= limit.max_discovery_support_entries as u128
    });
    if s.initial_state_map && limits.is_some() {
        search.traces = match traces {
            Some(t) => Some(t),
            None => Some(trace::simulate(s, corr, seed, &mut search.remaining_work)?),
        };
    }
    let map = search.traces.is_some();
    search.optimistic = map;
    search.names = names.cloned();
    // Name hints from wire aliases start as proposals; refinement may drop them.
    let mut hyp: Hyp = seeded(corr, seed_relations, excluded, &corr.encodings);
    if std::env::var_os("RUSTSYS_DISCOVERY_TRACE").is_some() {
        eprintln!(
            "relation search: {} unmatched, {} unused, {} seeded, {} encodings",
            corr.unmatched.len(),
            corr.unused.len(),
            hyp.len(),
            corr.encodings.len()
        );
    }
    // Relations a proof rejected are never proposed again.
    let mut failures: HashMap<(usize, Relation), u32> =
        excluded.iter().map(|r| (*r, MAX_FAILURES)).collect();
    // Constants first, from the largest set; then shared and cell relations for the states left,
    // and constants again for states whose constant may hold now.
    let mut first = true;
    let mut rounds = 0usize;
    loop {
        ctx.checkpoint()?;
        if limits.is_some_and(|limit| rounds >= limit.max_discovery_rounds) {
            return Err(Stop::Undecided(
                "state proposal round budget reached".into(),
            ));
        }
        rounds += 1;
        let before = hyp.clone();
        let mut proposed = 0;
        for i in 0..if map { 0 } else { search.consts.len() } {
            let si = search.consts[i];
            if hyp.contains_key(&si) {
                continue;
            }
            let base = s.base(si);
            // A state map tries the reset value first; the map still chooses either value.
            let first = base
                .or_else(|| {
                    s.initial_state_map
                        .then(|| s.model.states[si].arst.map(|(_, v)| v))
                        .flatten()
                })
                .unwrap_or(false);
            let pick = [first, !first]
                .into_iter()
                .take(if base.is_some() { 1 } else { 2 })
                .map(Relation::Constant)
                .find(|r| failures.get(&(si, *r)).copied().unwrap_or(0) < MAX_FAILURES);
            if let Some(rel) = pick {
                hyp.insert(si, rel);
                proposed += 1;
            }
        }
        // Seeded name hints are refined together with the first proposals: refining them
        // alone would blame them for states that no proposal explains yet.
        if !(first && !seed_relations.is_empty()) {
            search.refine(&mut hyp, &mut failures)?;
        }
        let props = search.propose(&hyp, &failures)?;
        if let Ok(w) = std::env::var("RUSTSYS_DISCOVERY_PROPOSALS") {
            for (si, rel) in &props {
                if s.model.states[*si].name.contains(w.as_str()) {
                    eprintln!("proposal round {rounds}: {} {:?}", s.model.states[*si].name, match rel {
                        Relation::Cell(ci, p) => format!("cell {} {p}", s.nm.seq[*ci].inst),
                        Relation::Shares(ri, inv) => format!("shares {} {inv}", s.model.states[*ri].name),
                        Relation::Constant(k) => format!("constant {k}"),
                    });
                }
            }
        }
        proposed += props.len();
        for (si, rel) in props {
            hyp.insert(si, rel);
        }
        search.refine(&mut hyp, &mut failures)?;
        // A state map proposes a constant only for state whose reset trace never changes and
        // that no element or shared state explains.
        if let Some(t) = &search.traces {
            for &si in &search.consts {
                if hyp.contains_key(&si) {
                    continue;
                }
                let Some(v) = trace::Traces::constant(&t.rtl[si]) else {
                    continue;
                };
                let rel = Relation::Constant(v);
                if failures.get(&(si, rel)).copied().unwrap_or(0) < MAX_FAILURES {
                    hyp.insert(si, rel);
                    proposed += 1;
                }
            }
            search.refine(&mut hyp, &mut failures)?;
        }
        if std::env::var_os("RUSTSYS_DISCOVERY_TRACE").is_some() {
            let count = |k: u8| hyp.values().filter(|r| trust(**r) == k).count();
            eprintln!(
                "discovery round {rounds}: proposed {proposed}, kept {} (shares {}, cells {}, constants {}), failures {}",
                hyp.len(),
                count(0),
                count(1),
                count(2),
                failures.len()
            );
        }
        if proposed == 0 || (hyp == before && !first) {
            break;
        }
        first = false;
    }
    if std::env::var_os("RUSTSYS_DISCOVERY_BLOCKERS").is_some() {
        search.blockers(&hyp);
    }
    let mut out: Vec<(usize, Relation)> = hyp.into_iter().collect();
    out.sort_by_key(|x| x.0);
    if std::env::var_os("RUSTSYS_DISCOVERY_TRACE").is_some() {
        for (enc, failures) in search.encodings.iter().zip(&search.encoding_failures) {
            eprintln!(
                "encoding {}: {} bits, {} elements, {} codes {:?}, {failures} contradicted rounds",
                enc.variable,
                enc.rtl.len(),
                enc.gate.len(),
                enc.codes.len(),
                enc.codes
            );
        }
    }
    if limits.is_some() {
        *work = search.remaining_work;
    }
    Ok((out, search.encodings))
}

#[cfg(test)]
mod tests;
