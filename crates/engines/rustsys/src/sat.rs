// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! SAT solving for the engine: a conflict-driven clause-learning solver [@marquessilva1999grasp;
//! @moskewicz2001chaff] with a flat clause arena, blocking literals in watch lists, a
//! move-to-front decision queue, geometric restarts, and solving under assumptions; plus the
//! Tseitin encoding of AIG cones [@tseitin1968complexity].

use crate::aig::{Aig, Lit};

/// Solver literal: variable times two plus sign (1 = negative).
pub type SLit = u32;

fn neg(l: SLit) -> SLit {
    l ^ 1
}

fn var(l: SLit) -> usize {
    (l >> 1) as usize
}

#[derive(Clone, Copy)]
struct Watch {
    clause: u32,
    blocker: SLit,
}

/// The solver.
pub struct Sat {
    /// Clause arena: `[len, lit0, lit1, ...]` per clause.
    arena: Vec<u32>,
    watches: Vec<Vec<Watch>>,
    /// Per variable: 0 unassigned, 1 true, 2 false.
    val: Vec<u8>,
    level: Vec<u32>,
    reason: Vec<u32>,
    trail: Vec<SLit>,
    lim: Vec<usize>,
    head: usize,
    // Move-to-front queue as a doubly linked list over variables, most recent first.
    prev: Vec<u32>,
    next: Vec<u32>,
    first: u32,
    search_from: u32,
    stamp: Vec<u64>,
    stamp_now: u64,
    saved: Vec<bool>,
    mark: Vec<bool>,
    unsat: bool,
    /// Variables that must be assigned for a model (`solve_within`); all when not restricted.
    relevant: Vec<bool>,
    restricted: bool,
    /// Unassigned relevant variables while restricted.
    relevant_left: usize,
}

const NIL: u32 = u32::MAX;

impl std::fmt::Debug for Sat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sat({} vars)", self.val.len())
    }
}

impl Default for Sat {
    fn default() -> Sat {
        Sat::new()
    }
}

impl Sat {
    pub fn new() -> Sat {
        Sat {
            arena: Vec::new(),
            watches: Vec::new(),
            val: Vec::new(),
            level: Vec::new(),
            reason: Vec::new(),
            trail: Vec::new(),
            lim: Vec::new(),
            head: 0,
            prev: Vec::new(),
            next: Vec::new(),
            first: NIL,
            search_from: NIL,
            stamp: Vec::new(),
            stamp_now: 0,
            saved: Vec::new(),
            mark: Vec::new(),
            unsat: false,
            relevant: Vec::new(),
            restricted: false,
            relevant_left: 0,
        }
    }

    pub fn new_var(&mut self) -> u32 {
        let v = self.val.len() as u32;
        self.val.push(0);
        self.level.push(0);
        self.reason.push(NIL);
        self.watches.push(Vec::new());
        self.watches.push(Vec::new());
        self.saved.push(false);
        self.mark.push(false);
        self.relevant.push(false);
        self.stamp.push(0);
        // Insert at the front of the queue.
        self.prev.push(NIL);
        self.next.push(self.first);
        if self.first != NIL {
            self.prev[self.first as usize] = v;
        }
        self.first = v;
        self.search_from = v;
        v
    }

    fn lit_val(&self, l: SLit) -> u8 {
        match self.val[var(l)] {
            0 => 0,
            x => {
                let t = x == 1;
                if (l & 1 == 0) == t { 1 } else { 2 }
            }
        }
    }

    fn assign(&mut self, l: SLit, reason: u32) {
        let v = var(l);
        if self.restricted && self.relevant[v] {
            self.relevant_left -= 1;
        }
        self.val[v] = if l & 1 == 0 { 1 } else { 2 };
        self.level[v] = self.lim.len() as u32;
        self.reason[v] = reason;
        self.trail.push(l);
    }

    /// Adds a clause at decision level 0.
    pub fn add(&mut self, lits: &[SLit]) {
        if self.unsat {
            return;
        }
        self.backtrack(0);
        let mut c: Vec<SLit> = Vec::with_capacity(lits.len());
        for &l in lits {
            match self.lit_val(l) {
                1 => return,
                2 => {}
                _ => {
                    if c.contains(&neg(l)) {
                        return;
                    }
                    if !c.contains(&l) {
                        c.push(l);
                    }
                }
            }
        }
        match c.len() {
            0 => self.unsat = true,
            1 => {
                self.assign(c[0], NIL);
                if self.propagate() != NIL {
                    self.unsat = true;
                }
            }
            _ => {
                self.attach(&c);
            }
        }
    }

    fn attach(&mut self, c: &[SLit]) -> u32 {
        let cref = self.arena.len() as u32;
        self.arena.push(c.len() as u32);
        self.arena.extend_from_slice(c);
        self.watches[c[0] as usize].push(Watch {
            clause: cref,
            blocker: c[1],
        });
        self.watches[c[1] as usize].push(Watch {
            clause: cref,
            blocker: c[0],
        });
        cref
    }

    /// Propagates; returns a conflicting clause or `NIL`. Each visited watch list is compacted
    /// in place (kept watches move to its front in their order), without allocating.
    fn propagate(&mut self) -> u32 {
        while self.head < self.trail.len() {
            let p = self.trail[self.head];
            self.head += 1;
            let f = neg(p);
            // `f` is false, so no watch moves to its own list while it is taken.
            let mut ws = std::mem::take(&mut self.watches[f as usize]);
            let mut conflict = NIL;
            let (mut i, mut j) = (0, 0);
            while i < ws.len() {
                let w = ws[i];
                i += 1;
                if self.lit_val(w.blocker) == 1 {
                    ws[j] = w;
                    j += 1;
                    continue;
                }
                let base = w.clause as usize + 1;
                let len = self.arena[w.clause as usize] as usize;
                if self.arena[base] == f {
                    self.arena.swap(base, base + 1);
                }
                let other = self.arena[base];
                if other != w.blocker && self.lit_val(other) == 1 {
                    ws[j] = Watch {
                        clause: w.clause,
                        blocker: other,
                    };
                    j += 1;
                    continue;
                }
                let mut moved = false;
                for k in 2..len {
                    let l = self.arena[base + k];
                    if self.lit_val(l) != 2 {
                        self.arena.swap(base + 1, base + k);
                        self.watches[l as usize].push(Watch {
                            clause: w.clause,
                            blocker: other,
                        });
                        moved = true;
                        break;
                    }
                }
                if moved {
                    continue;
                }
                ws[j] = w;
                j += 1;
                if self.lit_val(other) == 2 {
                    conflict = w.clause;
                    while i < ws.len() {
                        ws[j] = ws[i];
                        i += 1;
                        j += 1;
                    }
                    break;
                }
                self.assign(other, w.clause);
            }
            ws.truncate(j);
            self.watches[f as usize] = ws;
            if conflict != NIL {
                return conflict;
            }
        }
        NIL
    }

    fn backtrack(&mut self, lvl: usize) {
        if self.lim.len() <= lvl {
            return;
        }
        let to = self.lim[lvl];
        for i in (to..self.trail.len()).rev() {
            let v = var(self.trail[i]);
            if self.restricted && self.relevant[v] {
                self.relevant_left += 1;
            }
            self.saved[v] = self.val[v] == 1;
            self.val[v] = 0;
            self.reason[v] = NIL;
        }
        self.search_from = self.first;
        self.trail.truncate(to);
        self.lim.truncate(lvl);
        self.head = to;
    }

    fn move_to_front(&mut self, v: usize) {
        self.stamp_now += 1;
        self.stamp[v] = self.stamp_now;
        let v32 = v as u32;
        if self.first == v32 {
            return;
        }
        let (p, n) = (self.prev[v], self.next[v]);
        if p != NIL {
            self.next[p as usize] = n;
        }
        if n != NIL {
            self.prev[n as usize] = p;
        }
        self.prev[v] = NIL;
        self.next[v] = self.first;
        if self.first != NIL {
            self.prev[self.first as usize] = v32;
        }
        self.first = v32;
    }

    /// First-UIP learning; returns the learnt clause (asserting literal first) and the level to
    /// backtrack to.
    fn learn(&mut self, mut confl: u32) -> (Vec<SLit>, usize) {
        let cur = self.lim.len() as u32;
        let mut out: Vec<SLit> = vec![0];
        let mut pending = 0usize;
        let mut idx = self.trail.len();
        let mut uip: SLit;
        let mut bumped = Vec::new();
        let mut first = true;
        loop {
            let len = self.arena[confl as usize] as usize;
            let base = confl as usize + 1;
            for k in usize::from(!first)..len {
                let q = self.arena[base + k];
                let v = var(q);
                if self.mark[v] || self.level[v] == 0 {
                    continue;
                }
                self.mark[v] = true;
                bumped.push(v);
                if self.level[v] == cur {
                    pending += 1;
                } else {
                    out.push(q);
                }
            }
            first = false;
            loop {
                idx -= 1;
                if self.mark[var(self.trail[idx])] {
                    break;
                }
            }
            uip = self.trail[idx];
            self.mark[var(uip)] = false;
            pending -= 1;
            if pending == 0 {
                break;
            }
            confl = self.reason[var(uip)];
            // Reason clauses keep their implied literal first.
            let b = confl as usize + 1;
            if self.arena[b] != uip {
                let len = self.arena[confl as usize] as usize;
                if let Some(pos) = (0..len).find(|k| self.arena[b + k] == uip) {
                    self.arena.swap(b, b + pos);
                }
            }
        }
        out[0] = neg(uip);
        for l in &out {
            self.mark[var(*l)] = false;
        }
        for v in bumped {
            self.move_to_front(v);
        }
        let mut bt = 0usize;
        let mut at = 1usize;
        for (i, l) in out.iter().enumerate().skip(1) {
            let lv = self.level[var(*l)] as usize;
            if lv > bt {
                bt = lv;
                at = i;
            }
        }
        if out.len() > 1 {
            out.swap(1, at);
        }
        (out, bt)
    }

    /// Next decision: the first unassigned variable in queue order. Variables before
    /// `search_from` are assigned (it is reset to the front on backtracking).
    fn decide(&mut self) -> Option<SLit> {
        if self.restricted && self.relevant_left == 0 {
            return None;
        }
        let mut cur = self.search_from;
        while cur != NIL {
            if self.val[cur as usize] == 0 && (!self.restricted || self.relevant[cur as usize]) {
                self.search_from = cur;
                return Some((cur << 1) | u32::from(!self.saved[cur as usize]));
            }
            cur = self.next[cur as usize];
        }
        None
    }

    /// Solves under assumptions. `Some(true)` = satisfiable, `Some(false)` = unsatisfiable
    /// (under the assumptions), `None` = conflict limit reached.
    pub fn solve(&mut self, assumptions: &[SLit], limit: u64) -> Option<bool> {
        if self.unsat {
            return Some(false);
        }
        self.backtrack(0);
        if self.propagate() != NIL {
            self.unsat = true;
            return Some(false);
        }
        let mut conflicts = 0u64;
        let mut restart_at = 64u64;
        loop {
            let confl = self.propagate();
            if confl != NIL {
                conflicts += 1;
                if self.lim.is_empty() {
                    self.unsat = true;
                    return Some(false);
                }
                let (c, bt) = self.learn(confl);
                self.backtrack(bt);
                if c.len() == 1 {
                    self.assign(c[0], NIL);
                } else {
                    let cref = self.attach(&c);
                    self.assign(c[0], cref);
                }
                if conflicts >= limit {
                    self.backtrack(0);
                    return None;
                }
                if conflicts >= restart_at {
                    restart_at = restart_at * 3 / 2 + 64;
                    self.backtrack(0);
                    self.search_from = self.first;
                }
                continue;
            }
            // Assumptions occupy the first decision levels.
            if self.lim.len() < assumptions.len() {
                let a = assumptions[self.lim.len()];
                match self.lit_val(a) {
                    1 => {
                        self.lim.push(self.trail.len());
                    }
                    2 => {
                        self.backtrack(0);
                        return Some(false);
                    }
                    _ => {
                        self.lim.push(self.trail.len());
                        self.assign(a, NIL);
                    }
                }
                continue;
            }
            match self.decide() {
                None => return Some(true),
                Some(l) => {
                    self.lim.push(self.trail.len());
                    self.assign(l, NIL);
                }
            }
        }
    }

    /// `solve` where a model needs to assign only `vars`: the caller guarantees that every
    /// assignment of `vars` that satisfies the clauses over them extends to the other variables
    /// (as for the input cone of a Tseitin-encoded circuit, whose other gates follow from their
    /// inputs). In a model, the other variables may be unassigned.
    pub fn solve_within(&mut self, assumptions: &[SLit], limit: u64, vars: &[u32]) -> Option<bool> {
        self.backtrack(0);
        for &v in vars {
            self.relevant[v as usize] = true;
        }
        self.relevant_left = vars.iter().filter(|&&v| self.val[v as usize] == 0).count();
        self.restricted = true;
        let r = self.solve(assumptions, limit);
        self.restricted = false;
        for &v in vars {
            self.relevant[v as usize] = false;
        }
        r
    }

    /// Whether a variable is assigned (after `solve`, in the model).
    pub fn assigned(&self, v: u32) -> bool {
        self.val[v as usize] != 0
    }

    /// Sets the saved phase that the next decision on `v` uses.
    pub fn set_phase(&mut self, v: u32, value: bool) {
        self.saved[v as usize] = value;
    }

    /// Value of a variable in the last model.
    pub fn value(&self, v: u32) -> bool {
        self.val[v as usize] == 1
    }
}

/// Tseitin encoding of AIG cones into a solver, created on demand.
#[derive(Debug, Default)]
pub struct AigCnf {
    pub sat: Sat,
    /// Solver variable of each encoded AIG node, indexed by node; `NIL` when not encoded.
    map: Vec<u32>,
    t: Option<u32>,
    /// Cone marks of `solve_cone`: a node is marked when its stamp equals `epoch`.
    stamp: Vec<u32>,
    epoch: u32,
}

impl AigCnf {
    pub fn new() -> AigCnf {
        AigCnf::default()
    }

    fn var_of(&self, n: u32) -> Option<u32> {
        self.map.get(n as usize).copied().filter(|&v| v != NIL)
    }

    fn set_var(&mut self, n: u32, v: u32) {
        if n as usize >= self.map.len() {
            self.map.resize(n as usize + 1, NIL);
        }
        self.map[n as usize] = v;
    }

    fn true_var(&mut self) -> u32 {
        if let Some(t) = self.t {
            return t;
        }
        let t = self.sat.new_var();
        self.sat.add(&[t << 1]);
        self.t = Some(t);
        t
    }

    /// Solver literal of an AIG literal, encoding its cone if needed.
    pub fn lit(&mut self, aig: &Aig, l: Lit) -> SLit {
        if l.is_const() {
            let t = self.true_var();
            return (t << 1) | u32::from(l == Lit::FALSE);
        }
        let mut stack = vec![l.node()];
        while let Some(n) = stack.pop() {
            if self.var_of(n).is_some() {
                continue;
            }
            if aig.is_and(n) {
                let (a, b) = aig.fanins(n);
                let ma = self.var_of(a.node());
                let mb = self.var_of(b.node());
                if ma.is_none() && !a.is_const() || mb.is_none() && !b.is_const() {
                    stack.push(n);
                    if ma.is_none() && !a.is_const() {
                        stack.push(a.node());
                    }
                    if mb.is_none() && !b.is_const() {
                        stack.push(b.node());
                    }
                    continue;
                }
                let v = self.sat.new_var();
                let x = self.lit_of(a);
                let y = self.lit_of(b);
                let o = v << 1;
                self.sat.add(&[o ^ 1, x]);
                self.sat.add(&[o ^ 1, y]);
                self.sat.add(&[o, x ^ 1, y ^ 1]);
                self.set_var(n, v);
            } else {
                let v = self.sat.new_var();
                self.set_var(n, v);
            }
        }
        self.lit_of(l)
    }

    /// Solves the conjunction of `lits` over their input cones only (`Sat::solve_within`).
    pub fn solve_cone(&mut self, aig: &Aig, lits: &[Lit], limit: u64) -> Option<bool> {
        let assumptions: Vec<SLit> = lits.iter().map(|&l| self.lit(aig, l)).collect();
        let mut vars = Vec::new();
        if self.stamp.len() < self.map.len() {
            self.stamp.resize(self.map.len(), 0);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamp.fill(0);
            self.epoch = 1;
        }
        let mut stack: Vec<u32> = lits
            .iter()
            .filter(|l| !l.is_const())
            .map(|l| l.node())
            .collect();
        while let Some(n) = stack.pop() {
            if self.stamp[n as usize] == self.epoch {
                continue;
            }
            self.stamp[n as usize] = self.epoch;
            vars.push(self.map[n as usize]);
            if aig.is_and(n) {
                let (a, b) = aig.fanins(n);
                for x in [a, b] {
                    if !x.is_const() {
                        stack.push(x.node());
                    }
                }
            }
        }
        if let Some(t) = self.t {
            vars.push(t);
        }
        self.sat.solve_within(&assumptions, limit, &vars)
    }

    /// Value of AIG node `n` in the last model, when its variable is encoded and assigned.
    pub fn model_value(&self, n: u32) -> Option<bool> {
        self.var_of(n)
            .filter(|&v| self.sat.assigned(v))
            .map(|v| self.sat.value(v))
    }

    /// Sets the decision phase of AIG node `n`'s variable, when it is encoded.
    pub fn set_phase(&mut self, n: u32, value: bool) {
        if let Some(v) = self.var_of(n) {
            self.sat.set_phase(v, value);
        }
    }

    fn lit_of(&mut self, l: Lit) -> SLit {
        if l.is_const() {
            let t = self.true_var();
            return (t << 1) | u32::from(l == Lit::FALSE);
        }
        (self.map[l.node() as usize] << 1) | u32::from(l.is_compl())
    }
}

/// Proves that `l` is constant `value` for all inputs (`Some(true)`), refutes it
/// (`Some(false)`), or gives up.
pub fn prove_const(aig: &Aig, l: Lit, value: bool, limit: u64) -> Option<bool> {
    if let Some(c) = l.const_value() {
        return Some(c == value);
    }
    let mut cnf = AigCnf::new();
    let s = cnf.lit(aig, l);
    // Look for an assignment where l differs from `value`.
    let target = if value { s ^ 1 } else { s };
    cnf.sat.solve(&[target], limit).map(|sat| !sat)
}

/// Proves `a == b` for all inputs.
pub fn prove_equal(aig: &Aig, a: Lit, b: Lit, limit: u64) -> Option<bool> {
    if a == b {
        return Some(true);
    }
    if a == !b {
        return Some(false);
    }
    let mut cnf = AigCnf::new();
    let x = cnf.lit(aig, a);
    let y = cnf.lit(aig, b);
    // a != b is satisfiable iff (a & !b) or (!a & b).
    let r1 = cnf.sat.solve(&[x, y ^ 1], limit)?;
    if r1 {
        return Some(false);
    }
    let r2 = cnf.sat.solve(&[x ^ 1, y], limit)?;
    Some(!r2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brute_force_agreement() {
        let mut seed = 99u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..3000 {
            let nv = 5u32;
            let nc = (rnd() % 14) as usize;
            let clauses: Vec<Vec<SLit>> = (0..nc)
                .map(|_| {
                    (0..1 + rnd() % 3)
                        .map(|_| (rnd() % u64::from(2 * nv)) as u32)
                        .collect()
                })
                .collect();
            let brute = (0..32u32).any(|m| {
                clauses
                    .iter()
                    .all(|c| c.iter().any(|l| ((m >> (l >> 1)) & 1 == 1) == (l & 1 == 0)))
            });
            let mut s = Sat::new();
            for _ in 0..nv {
                s.new_var();
            }
            for c in &clauses {
                s.add(c);
            }
            let r = s.solve(&[], 100_000).unwrap();
            assert_eq!(r, brute, "{clauses:?}");
            if r {
                assert!(
                    clauses
                        .iter()
                        .all(|c| c.iter().any(|l| s.value(l >> 1) == (l & 1 == 0)))
                );
            }
        }
    }

    // A model of `solve_cone` assigns the input cones of the assumptions consistently, even when
    // the solver also holds unrelated cones, and agrees with plain solving on satisfiability.
    #[test]
    fn cone_restricted_models() {
        let mut seed = 0x51ce_7a3d_9e01_44b2u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200 {
            let mut g = Aig::new();
            let mut lits: Vec<Lit> = (0..6).map(|_| g.input()).collect();
            for _ in 0..40 {
                let a = lits[(rnd() % lits.len() as u64) as usize].xor(rnd() & 1 == 1);
                let b = lits[(rnd() % lits.len() as u64) as usize].xor(rnd() & 1 == 1);
                lits.push(g.and(a, b));
            }
            let mut cnf = AigCnf::new();
            // Encode unrelated cones first, so the solver holds more than the asked cones.
            for &l in lits.iter().rev().take(8) {
                cnf.lit(&g, l);
            }
            let x = lits[(rnd() % lits.len() as u64) as usize].xor(rnd() & 1 == 1);
            let y = lits[(rnd() % lits.len() as u64) as usize].xor(rnd() & 1 == 1);
            let r = cnf.solve_cone(&g, &[x, y], 10_000);
            let brute = (0..64u64).any(|m| {
                let words: Vec<u64> = (0..6)
                    .map(|i| if (m >> i) & 1 == 1 { 1 } else { 0 })
                    .collect();
                let sim = g.simulate(&words);
                Aig::lit_value(&sim, x) & Aig::lit_value(&sim, y) & 1 == 1
            });
            assert_eq!(r, Some(brute));
            if brute {
                let words: Vec<u64> = g
                    .inputs()
                    .iter()
                    .map(|&i| u64::from(cnf.model_value(i).unwrap_or(false)))
                    .collect();
                let sim = g.simulate(&words);
                assert_eq!(Aig::lit_value(&sim, x) & Aig::lit_value(&sim, y) & 1, 1);
            }
        }
    }

    #[test]
    fn aig_proofs() {
        let mut g = Aig::new();
        let a = g.input();
        let b = g.input();
        let c = g.input();
        // (a & b) | (a & c) == a & (b | c)
        let ab = g.and(a, b);
        let ac = g.and(a, c);
        let l = g.or(ab, ac);
        let bc = g.or(b, c);
        let r = g.and(a, bc);
        assert_eq!(prove_equal(&g, l, r, 1000), Some(true));
        assert_eq!(prove_equal(&g, l, ab, 1000), Some(false));
        // Case with full coverage: (a&b) | (a&!b) | (!a&b) | (!a&!b) == 1
        let t1 = g.and(a, !b);
        let t2 = g.and(!a, b);
        let t3 = g.and(!a, !b);
        let o1 = g.or(ab, t1);
        let o2 = g.or(t2, t3);
        let all = g.or(o1, o2);
        assert_eq!(prove_const(&g, all, true, 1000), Some(true));
        assert_eq!(prove_const(&g, ab, true, 1000), Some(false));
    }
}
