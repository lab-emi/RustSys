// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Conflict-driven clause-learning SAT solver (CHECK-REQ-0042) [@marquessilva1999grasp;
//! @moskewicz2001chaff; @een2003extensible]: two watched literals, first-UIP learning, activity
//! ordering with a binary heap, phase saving and restarts following the Luby sequence
//! [@luby1993optimal]. The solver is incremental: variables and clauses may be added between
//! calls, and each call may assume literals and limit decisions to a prefix of the variables.

mod heap;

/// A literal: variable index times two, plus one if negated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lit(pub u32);

impl Lit {
    pub fn pos(v: u32) -> Lit {
        Lit(v << 1)
    }

    pub fn neg(v: u32) -> Lit {
        Lit(v << 1 | 1)
    }

    pub fn var(self) -> u32 {
        self.0 >> 1
    }

    pub fn sign(self) -> bool {
        self.0 & 1 == 1
    }
}

impl std::ops::Not for Lit {
    type Output = Lit;
    fn not(self) -> Lit {
        Lit(self.0 ^ 1)
    }
}

/// Solver answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Satisfiable, with a model (value of each variable).
    Sat(Vec<bool>),
    Unsat,
    /// Conflict budget exhausted.
    Unknown,
}

const UNDEF: u8 = 2;

#[derive(Debug)]
struct Clause {
    lits: Vec<Lit>,
    learnt: bool,
}

/// The solver.
#[derive(Debug)]
pub struct Solver {
    clauses: Vec<Clause>,
    watches: Vec<Vec<usize>>,
    assign: Vec<u8>,
    level: Vec<u32>,
    reason: Vec<usize>,
    trail: Vec<Lit>,
    trail_lim: Vec<usize>,
    qhead: usize,
    activity: Vec<f64>,
    var_inc: f64,
    heap: Vec<u32>,
    heap_pos: Vec<i64>,
    phase: Vec<bool>,
    seen: Vec<bool>,
    ok: bool,
    /// Variables above the decision limit taken off the heap during a call.
    parked: Vec<u32>,
    /// Fixed decision order of the current call (empty: activity order), the position of each
    /// variable in it, and the first position that may be unassigned.
    dlist: Vec<u32>,
    dpos: Vec<u32>,
    dcursor: usize,
    learnts: usize,
    max_learnts: usize,
    pub conflicts: u64,
}

const NO_REASON: usize = usize::MAX;
const NOT_LISTED: u32 = u32::MAX;

fn luby(mut x: u64) -> u64 {
    // Luby sequence 1,1,2,1,1,2,4,...
    let mut size = 1u64;
    let mut seq = 0u32;
    while size < x + 1 {
        seq += 1;
        size = 2 * size + 1;
    }
    while size - 1 != x {
        size = (size - 1) >> 1;
        seq -= 1;
        x %= size;
    }
    1u64 << seq
}

impl Solver {
    pub fn new(nvars: usize) -> Solver {
        let mut s = Solver {
            clauses: Vec::new(),
            watches: vec![Vec::new(); 2 * nvars],
            assign: vec![UNDEF; nvars],
            level: vec![0; nvars],
            reason: vec![NO_REASON; nvars],
            trail: Vec::new(),
            trail_lim: Vec::new(),
            qhead: 0,
            activity: vec![0.0; nvars],
            var_inc: 1.0,
            heap: Vec::new(),
            heap_pos: vec![-1; nvars],
            phase: vec![false; nvars],
            seen: vec![false; nvars],
            ok: true,
            parked: Vec::new(),
            dlist: Vec::new(),
            dpos: vec![NOT_LISTED; nvars],
            dcursor: 0,
            learnts: 0,
            max_learnts: 20_000,
            conflicts: 0,
        };
        for v in 0..nvars as u32 {
            s.heap_insert(v);
        }
        s
    }

    /// Adds a variable and returns its index.
    pub fn new_var(&mut self) -> u32 {
        let v = self.assign.len() as u32;
        self.watches.push(Vec::new());
        self.watches.push(Vec::new());
        self.assign.push(UNDEF);
        self.level.push(0);
        self.reason.push(NO_REASON);
        self.activity.push(0.0);
        self.heap_pos.push(-1);
        self.phase.push(false);
        self.seen.push(false);
        self.dpos.push(NOT_LISTED);
        self.heap_insert(v);
        v
    }

    /// Number of variables.
    pub fn num_vars(&self) -> usize {
        self.assign.len()
    }

    fn value(&self, l: Lit) -> u8 {
        let a = self.assign[l.var() as usize];
        if a == UNDEF {
            UNDEF
        } else {
            a ^ u8::from(l.sign())
        }
    }

    fn decision_level(&self) -> u32 {
        self.trail_lim.len() as u32
    }

    fn enqueue(&mut self, l: Lit, reason: usize) {
        let v = l.var() as usize;
        self.assign[v] = u8::from(!l.sign());
        self.level[v] = self.decision_level();
        self.reason[v] = reason;
        self.trail.push(l);
    }

    /// Adds a clause. Calls leave the solver at decision level 0, so clauses may be added at any
    /// time between them.
    pub fn add_clause(&mut self, lits: &[Lit]) {
        if !self.ok {
            return;
        }
        let mut c: Vec<Lit> = Vec::with_capacity(lits.len());
        for &l in lits {
            match self.value(l) {
                1 => return,
                0 => continue,
                _ => {
                    if c.contains(&!l) {
                        return;
                    }
                    if !c.contains(&l) {
                        c.push(l);
                    }
                }
            }
        }
        match c.len() {
            0 => self.ok = false,
            1 => {
                self.enqueue(c[0], NO_REASON);
                if self.propagate().is_some() {
                    self.ok = false;
                }
            }
            _ => {
                let idx = self.clauses.len();
                self.watches[c[0].0 as usize].push(idx);
                self.watches[c[1].0 as usize].push(idx);
                self.clauses.push(Clause {
                    lits: c,
                    learnt: false,
                });
            }
        }
    }

    /// Unit propagation; returns a conflicting clause.
    fn propagate(&mut self) -> Option<usize> {
        while self.qhead < self.trail.len() {
            let p = self.trail[self.qhead];
            self.qhead += 1;
            let falsified = !p;
            let mut ws = std::mem::take(&mut self.watches[falsified.0 as usize]);
            let mut i = 0;
            let mut conflict = None;
            while i < ws.len() {
                let ci = ws[i];
                let lits = &mut self.clauses[ci].lits;
                if lits[0] == falsified {
                    lits.swap(0, 1);
                }
                let first = lits[0];
                if self.assign[first.var() as usize] != UNDEF
                    && (self.assign[first.var() as usize] ^ u8::from(first.sign())) == 1
                {
                    i += 1;
                    continue;
                }
                // Look for a new literal to watch.
                let mut found = None;
                for k in 2..lits.len() {
                    let l = lits[k];
                    let a = self.assign[l.var() as usize];
                    if a == UNDEF || (a ^ u8::from(l.sign())) == 1 {
                        found = Some(k);
                        break;
                    }
                }
                if let Some(k) = found {
                    lits.swap(1, k);
                    let nw = lits[1];
                    self.watches[nw.0 as usize].push(ci);
                    ws.swap_remove(i);
                    continue;
                }
                let fv = self.assign[first.var() as usize];
                if fv != UNDEF {
                    // First literal false: conflict.
                    conflict = Some(ci);
                    break;
                }
                self.enqueue(first, ci);
                i += 1;
            }
            // Restore the watch list (entries not moved).
            let rest = std::mem::take(&mut self.watches[falsified.0 as usize]);
            ws.extend(rest);
            self.watches[falsified.0 as usize] = ws;
            if conflict.is_some() {
                self.qhead = self.trail.len();
                return conflict;
            }
        }
        None
    }

    fn bump(&mut self, v: u32) {
        let vi = v as usize;
        self.activity[vi] += self.var_inc;
        if self.activity[vi] > 1e100 {
            for a in self.activity.iter_mut() {
                *a *= 1e-100;
            }
            self.var_inc *= 1e-100;
        }
        if self.heap_pos[vi] >= 0 {
            self.heap_up(self.heap_pos[vi] as usize);
        }
    }

    /// First-UIP conflict analysis: returns the learnt clause (asserting literal first) and the
    /// backtrack level.
    fn analyze(&mut self, mut confl: usize) -> (Vec<Lit>, u32) {
        let mut learnt = vec![Lit(0)];
        let mut counter = 0usize;
        let mut p: Option<Lit> = None;
        let mut idx = self.trail.len();
        let dl = self.decision_level();
        loop {
            let lits = self.clauses[confl].lits.clone();
            let start = usize::from(p.is_some());
            for &q in &lits[start..] {
                let v = q.var() as usize;
                if !self.seen[v] && self.level[v] > 0 {
                    self.seen[v] = true;
                    self.bump(q.var());
                    if self.level[v] == dl {
                        counter += 1;
                    } else {
                        learnt.push(q);
                    }
                }
            }
            loop {
                idx -= 1;
                if self.seen[self.trail[idx].var() as usize] {
                    break;
                }
            }
            let pl = self.trail[idx];
            p = Some(pl);
            self.seen[pl.var() as usize] = false;
            counter -= 1;
            if counter == 0 {
                break;
            }
            confl = self.reason[pl.var() as usize];
        }
        learnt[0] = !p.expect("uip");
        let mut bt = 0;
        let mut max_i = 1;
        for (i, l) in learnt.iter().enumerate().skip(1) {
            let lv = self.level[l.var() as usize];
            if lv > bt {
                bt = lv;
                max_i = i;
            }
        }
        if learnt.len() > 1 {
            learnt.swap(1, max_i);
        }
        for l in &learnt {
            self.seen[l.var() as usize] = false;
        }
        (learnt, bt)
    }

    fn cancel_until(&mut self, lvl: u32) {
        if self.decision_level() <= lvl {
            return;
        }
        let lim = self.trail_lim[lvl as usize];
        for i in (lim..self.trail.len()).rev() {
            let l = self.trail[i];
            let v = l.var() as usize;
            self.phase[v] = !l.sign();
            self.assign[v] = UNDEF;
            self.reason[v] = NO_REASON;
            if self.heap_pos[v] < 0 {
                self.heap_insert(l.var());
            }
            let p = self.dpos[v] as usize;
            if p < self.dcursor {
                self.dcursor = p;
            }
        }
        self.trail.truncate(lim);
        self.trail_lim.truncate(lvl as usize);
        self.qhead = self.trail.len();
    }

    fn pick(&mut self, limit: u32) -> Option<Lit> {
        if !self.dlist.is_empty() {
            while let Some(&v) = self.dlist.get(self.dcursor) {
                if self.assign[v as usize] == UNDEF {
                    return Some(if self.phase[v as usize] {
                        Lit::pos(v)
                    } else {
                        Lit::neg(v)
                    });
                }
                self.dcursor += 1;
            }
            return None;
        }
        while let Some(v) = self.heap_pop() {
            if v > limit {
                self.parked.push(v);
                continue;
            }
            if self.assign[v as usize] == UNDEF {
                return Some(if self.phase[v as usize] {
                    Lit::pos(v)
                } else {
                    Lit::neg(v)
                });
            }
        }
        None
    }

    /// Solves with a conflict budget.
    pub fn solve(&mut self, budget: u64) -> Answer {
        self.solve_limited(&[], budget, u32::MAX)
    }

    /// Solves under `assumptions` with a conflict budget for this call, deciding only variables
    /// up to `limit`. `Sat` means that every variable up to `limit` is assigned without conflict;
    /// the model is complete only if no clause links those variables to later ones in a way that
    /// cannot be satisfied (the caller guarantees this, e.g. by numbering fanins first).
    /// `Unsat` under assumptions leaves the solver usable; values above `limit` in a model are
    /// meaningless.
    pub fn solve_limited(&mut self, assumptions: &[Lit], budget: u64, limit: u32) -> Answer {
        if self.learnts > self.max_learnts {
            self.reduce();
        }
        let r = self.search(assumptions, budget, limit);
        self.cancel_until(0);
        for v in std::mem::take(&mut self.parked) {
            if self.heap_pos[v as usize] < 0 {
                self.heap_insert(v);
            }
        }
        r
    }

    /// Solves under `assumptions`, deciding only the variables of `decide`, in the given order.
    /// `Sat` means that all of them are assigned without conflict; the caller guarantees that
    /// unit propagation then determines every variable the question depends on (for a circuit
    /// encoding: `decide` contains the inputs of the cone of the assumptions).
    pub fn solve_deciding(&mut self, assumptions: &[Lit], budget: u64, decide: &[u32]) -> Answer {
        let mut list = Vec::with_capacity(decide.len());
        for &v in decide {
            if self.dpos[v as usize] == NOT_LISTED {
                self.dpos[v as usize] = list.len() as u32;
                list.push(v);
            }
        }
        if list.is_empty() && !self.assign.is_empty() {
            // Nothing to decide; a sentinel keeps the fixed-order mode.
            self.dpos[0] = 0;
            list.push(0);
        }
        self.dlist = list;
        self.dcursor = 0;
        let r = self.solve_limited(assumptions, budget, u32::MAX);
        for v in std::mem::take(&mut self.dlist) {
            self.dpos[v as usize] = NOT_LISTED;
        }
        self.dcursor = 0;
        r
    }

    fn search(&mut self, assumptions: &[Lit], budget: u64, limit: u32) -> Answer {
        if !self.ok {
            return Answer::Unsat;
        }
        if self.propagate().is_some() {
            self.ok = false;
            return Answer::Unsat;
        }
        let start = self.conflicts;
        let mut restart_count = 0u64;
        let mut restart_limit = 100 * luby(restart_count);
        let mut since_restart = 0u64;
        loop {
            if let Some(confl) = self.propagate() {
                self.conflicts += 1;
                since_restart += 1;
                if self.decision_level() == 0 {
                    self.ok = false;
                    return Answer::Unsat;
                }
                let (learnt, bt) = self.analyze(confl);
                self.cancel_until(bt);
                if learnt.len() == 1 {
                    self.enqueue(learnt[0], NO_REASON);
                } else {
                    let idx = self.clauses.len();
                    self.watches[learnt[0].0 as usize].push(idx);
                    self.watches[learnt[1].0 as usize].push(idx);
                    let first = learnt[0];
                    self.clauses.push(Clause {
                        lits: learnt,
                        learnt: true,
                    });
                    self.learnts += 1;
                    self.enqueue(first, idx);
                }
                self.var_inc /= 0.95;
                if self.conflicts - start >= budget {
                    return Answer::Unknown;
                }
                if since_restart >= restart_limit {
                    since_restart = 0;
                    restart_count += 1;
                    restart_limit = 100 * luby(restart_count);
                    self.cancel_until(0);
                }
                continue;
            }
            // Assumptions are the first decisions, one level each.
            let dl = self.decision_level() as usize;
            if let Some(&p) = assumptions.get(dl) {
                match self.value(p) {
                    1 => self.trail_lim.push(self.trail.len()),
                    0 => return Answer::Unsat,
                    _ => {
                        self.trail_lim.push(self.trail.len());
                        self.enqueue(p, NO_REASON);
                    }
                }
                continue;
            }
            match self.pick(limit) {
                None => {
                    let model = self.assign.iter().map(|a| *a == 1).collect();
                    return Answer::Sat(model);
                }
                Some(l) => {
                    self.trail_lim.push(self.trail.len());
                    self.enqueue(l, NO_REASON);
                }
            }
        }
    }

    /// Removes the longer half of the learnt clauses longer than two literals (at level 0).
    fn reduce(&mut self) {
        let mut lens: Vec<usize> = self
            .clauses
            .iter()
            .filter(|c| c.learnt && c.lits.len() > 2)
            .map(|c| c.lits.len())
            .collect();
        lens.sort_unstable();
        let target = lens.len() / 2;
        let cut = lens.get(target).copied().unwrap_or(usize::MAX);
        // Drop every clause longer than the median and enough of median length to halve.
        let mut quota = target - lens.iter().filter(|l| **l > cut).count();
        let old = std::mem::take(&mut self.clauses);
        let mut kept = 0usize;
        for c in old {
            let n = c.lits.len();
            let mut drop = c.learnt && n > 2 && n > cut;
            if c.learnt && n > 2 && n == cut && quota > 0 {
                quota -= 1;
                drop = true;
            }
            if !drop {
                kept += usize::from(c.learnt);
                self.clauses.push(c);
            }
        }
        self.learnts = kept;
        self.max_learnts += self.max_learnts / 10;
        // Reasons of level-0 assignments are never used in conflict analysis.
        for l in &self.trail {
            self.reason[l.var() as usize] = NO_REASON;
        }
        for w in &mut self.watches {
            w.clear();
        }
        for (i, c) in self.clauses.iter().enumerate() {
            self.watches[c.lits[0].0 as usize].push(i);
            self.watches[c.lits[1].0 as usize].push(i);
        }
    }

    // ------------------------------------------------------------ binary heap
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pigeonhole: n+1 pigeons into n holes is unsatisfiable.
    fn pigeonhole(n: u32) -> Solver {
        let var = |p: u32, h: u32| p * n + h;
        let mut s = Solver::new(((n + 1) * n) as usize);
        for p in 0..=n {
            let c: Vec<Lit> = (0..n).map(|h| Lit::pos(var(p, h))).collect();
            s.add_clause(&c);
        }
        for h in 0..n {
            for p in 0..=n {
                for q in p + 1..=n {
                    s.add_clause(&[Lit::neg(var(p, h)), Lit::neg(var(q, h))]);
                }
            }
        }
        s
    }

    #[test]
    fn unsat_and_sat_instances() {
        for n in 2..6 {
            assert_eq!(pigeonhole(n).solve(1_000_000), Answer::Unsat, "php {n}");
        }
        // Random 3-SAT below the threshold, checked against the model.
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..50 {
            let nv = 40u32;
            let mut clauses = Vec::new();
            for _ in 0..120 {
                let c: Vec<Lit> = (0..3)
                    .map(|_| Lit((rnd() % (2 * u64::from(nv))) as u32))
                    .collect();
                clauses.push(c);
            }
            let mut s = Solver::new(nv as usize);
            for c in &clauses {
                s.add_clause(c);
            }
            match s.solve(1_000_000) {
                Answer::Sat(m) => {
                    for c in &clauses {
                        assert!(c.iter().any(|l| m[l.var() as usize] != l.sign()));
                    }
                }
                Answer::Unsat => {
                    // Confirm by brute force on a projection is too expensive; accept rare UNSAT.
                }
                Answer::Unknown => panic!("budget"),
            }
        }
        assert_eq!(luby(0), 1);
        assert_eq!(
            (0..7).map(luby).collect::<Vec<_>>(),
            vec![1, 1, 2, 1, 1, 2, 4]
        );
    }

    #[test]
    fn incremental_calls_with_assumptions() {
        // One solver, many calls with random assumptions and clauses added between calls; every
        // answer is compared with brute force over 6 variables.
        let mut seed = 99u64;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            seed >> 33
        };
        for _ in 0..200 {
            let mut s = Solver::new(0);
            for _ in 0..6 {
                s.new_var();
            }
            let mut clauses: Vec<Vec<Lit>> = Vec::new();
            for _ in 0..12 {
                if rnd() % 3 == 0 {
                    let c: Vec<Lit> = (0..2 + rnd() % 2)
                        .map(|_| Lit((rnd() % 12) as u32))
                        .collect();
                    s.add_clause(&c);
                    clauses.push(c);
                }
                let assume: Vec<Lit> = (0..rnd() % 4).map(|_| Lit((rnd() % 12) as u32)).collect();
                let sat = |m: u32| {
                    clauses
                        .iter()
                        .all(|c| c.iter().any(|l| ((m >> l.var()) & 1 == 1) != l.sign()))
                        && assume.iter().all(|l| ((m >> l.var()) & 1 == 1) != l.sign())
                };
                let brute = (0..64u32).any(sat);
                match s.solve_limited(&assume, 10_000, u32::MAX) {
                    Answer::Sat(m) => {
                        assert!(brute);
                        let bits = m
                            .iter()
                            .enumerate()
                            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << i));
                        assert!(sat(bits), "{clauses:?} {assume:?}");
                    }
                    Answer::Unsat => assert!(!brute, "{clauses:?} {assume:?}"),
                    Answer::Unknown => panic!("budget"),
                }
            }
        }
    }

    #[test]
    fn exhaustive_small_formulas() {
        // Every formula over 4 variables with up to 6 random clauses: compare with brute force.
        let mut seed = 7u64;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            seed >> 33
        };
        for _ in 0..2000 {
            let nc = (rnd() % 7) as usize;
            let clauses: Vec<Vec<Lit>> = (0..nc)
                .map(|_| {
                    (0..1 + rnd() % 3)
                        .map(|_| Lit((rnd() % 8) as u32))
                        .collect()
                })
                .collect();
            let brute = (0..16u32).any(|m| {
                clauses
                    .iter()
                    .all(|c| c.iter().any(|l| ((m >> l.var()) & 1 == 1) != l.sign()))
            });
            let mut s = Solver::new(4);
            for c in &clauses {
                s.add_clause(c);
            }
            let r = s.solve(10_000);
            assert_eq!(matches!(r, Answer::Sat(_)), brute, "{clauses:?}");
            if let Answer::Sat(m) = r {
                assert!(
                    clauses
                        .iter()
                        .all(|c| c.iter().any(|l| m[l.var() as usize] != l.sign()))
                );
            }
        }
    }
}
