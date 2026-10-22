// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Small AIG structures for functions of at most four variables (SYNTH-REQ-0411). The structure of
//! a function is the cheapest of: a decomposition on one variable (AND, OR, exclusive OR with the
//! variable, or a multiplexer on it), a decomposition into two functions of disjoint supports
//! (AND, OR, exclusive OR), and the factored irredundant cover of the function or its complement
//! [@minato1992fast; @brayton1987factoring]. Costs count AND nodes, with shared subfunctions
//! counted once; results are memoized per truth table.

use rustc_hash::{FxHashMap, FxHashSet};

use super::factor::{Form, best_form};
use crate::aig::{Aig, Lit};

/// Truth tables of the four variables (16 minterms).
const VAR: [u16; 4] = [0xAAAA, 0xCCCC, 0xF0F0, 0xFF00];

fn cof(f: u16, v: usize) -> (u16, u16) {
    let m = VAR[v];
    let s = 1u32 << v;
    let lo = f & !m;
    let hi = f & m;
    (lo | (lo << s), hi | (hi >> s))
}

fn depends(f: u16, v: usize) -> bool {
    let (a, b) = cof(f, v);
    a != b
}

fn support(f: u16) -> u8 {
    (0..4).filter(|v| depends(f, *v)).fold(0, |a, v| a | 1 << v)
}

/// Quantification of the variables in `vars`: existential (`or = true`) or universal.
fn quantify(mut f: u16, vars: u8, or: bool) -> u16 {
    for v in 0..4 {
        if vars & (1 << v) != 0 {
            let (a, b) = cof(f, v);
            f = if or { a | b } else { a & b };
        }
    }
    f
}

/// Fixes the variables in `vars` to 0.
fn restrict0(mut f: u16, vars: u8) -> u16 {
    for v in 0..4 {
        if vars & (1 << v) != 0 {
            f = cof(f, v).0;
        }
    }
    f
}

/// How a function is built.
#[derive(Debug, Clone)]
pub enum Dec {
    Const(bool),
    Var(usize, bool),
    /// `x & y` of two functions.
    And(u16, u16),
    /// `x | y` of two functions.
    Or(u16, u16),
    /// `x ^ y` of two functions.
    Xor(u16, u16),
    /// `v ? t : e`.
    Mux(usize, u16, u16),
    /// Factored form, complemented if the flag is set.
    Form(Form, bool),
}

/// Memoized structures.
#[derive(Debug)]
pub struct Lib4 {
    memo: FxHashMap<u16, (u32, Dec)>,
    max_entries: usize,
}

impl Default for Lib4 {
    fn default() -> Self {
        Self::bounded(65_536)
    }
}

impl Lib4 {
    pub fn new() -> Lib4 {
        Lib4::default()
    }

    /// Cache eviction only repeats graph-independent computation; it never
    /// changes the selected decomposition. Bounds per-worker cache storage.
    pub fn bounded(max_entries: usize) -> Self {
        Self {
            memo: FxHashMap::default(),
            max_entries: max_entries.clamp(1, 65_536),
        }
    }

    /// Nodes of the structure of `f` (a tree estimate that counts each distinct subfunction
    /// once).
    pub fn cost(&mut self, f: u16) -> u32 {
        self.get(f).0
    }

    /// Cost and decomposition of `f`.
    pub fn get(&mut self, f: u16) -> (u32, Dec) {
        if let Some(r) = self.memo.get(&f) {
            return r.clone();
        }
        let r = self.compute(f);
        if self.memo.len() >= self.max_entries {
            self.memo.clear();
        }
        self.memo.insert(f, r.clone());
        r
    }

    fn compute(&mut self, f: u16) -> (u32, Dec) {
        if f == 0 || f == 0xFFFF {
            return (0, Dec::Const(f != 0));
        }
        for v in 0..4 {
            if f == VAR[v] || f == !VAR[v] {
                return (0, Dec::Var(v, f == !VAR[v]));
            }
        }
        let s = support(f);
        let mut best: Option<(u32, Dec)> = None;
        let offer = |c: u32, d: Dec, best: &mut Option<(u32, Dec)>| {
            if best.as_ref().is_none_or(|b| c < b.0) {
                *best = Some((c, d));
            }
        };
        // One-variable decompositions.
        for v in 0..4 {
            if s & (1 << v) == 0 {
                continue;
            }
            let (f0, f1) = cof(f, v);
            let x = VAR[v];
            if f0 == 0 {
                let c = self.cost(f1) + 1;
                offer(c, Dec::And(x, f1), &mut best);
            } else if f1 == 0 {
                let c = self.cost(f0) + 1;
                offer(c, Dec::And(!x, f0), &mut best);
            } else if f0 == 0xFFFF {
                let c = self.cost(f1) + 1;
                offer(c, Dec::Or(!x, f1), &mut best);
            } else if f1 == 0xFFFF {
                let c = self.cost(f0) + 1;
                offer(c, Dec::Or(x, f0), &mut best);
            } else if f0 == !f1 {
                let c = self.cost(f0) + 3;
                offer(c, Dec::Xor(x, f0), &mut best);
            } else {
                let c = self.cost(f0) + self.cost(f1) + 3;
                offer(c, Dec::Mux(v, f1, f0), &mut best);
            }
        }
        // Disjoint-support decompositions.
        for a in 1..s {
            if a & !s != 0 || a == s {
                continue;
            }
            let b = s & !a;
            if a > b {
                continue;
            }
            let (ga, gb) = (quantify(f, b, true), quantify(f, a, true));
            if ga & gb == f {
                let c = self.cost(ga) + self.cost(gb) + 1;
                offer(c, Dec::And(ga, gb), &mut best);
            }
            let (ha, hb) = (quantify(f, b, false), quantify(f, a, false));
            if ha | hb == f {
                let c = self.cost(ha) + self.cost(hb) + 1;
                offer(c, Dec::Or(ha, hb), &mut best);
            }
            let (xa, xb) = (restrict0(f, b), restrict0(f, a) ^ restrict0(f, s));
            if xa ^ xb == f {
                let c = self.cost(xa) + self.cost(xb) + 3;
                offer(c, Dec::Xor(xa, xb), &mut best);
            }
        }
        // Factored cover.
        let (form, inv) = best_form(u64::from(f) | (u64::from(f) << 16), 4);
        let c = form.gates() as u32;
        offer(c, Dec::Form(form, inv), &mut best);
        best.expect("a candidate always exists")
    }

    /// Builds `f` over leaf literals, reusing nodes already in the graph.
    pub fn build(&mut self, g: &mut Aig, f: u16, leaves: &[Lit; 4]) -> Lit {
        let mut memo = FxHashMap::default();
        self.build_rec(g, f, leaves, &mut memo)
    }

    fn build_rec(
        &mut self,
        g: &mut Aig,
        f: u16,
        leaves: &[Lit; 4],
        memo: &mut FxHashMap<u16, Lit>,
    ) -> Lit {
        if let Some(l) = memo.get(&f) {
            return *l;
        }
        if let Some(l) = memo.get(&!f) {
            return !*l;
        }
        let (_, d) = self.get(f);
        let l = match d {
            Dec::Const(c) => Lit::from_bool(c),
            Dec::Var(v, n) => leaves[v].xor(n),
            Dec::And(x, y) => {
                let a = self.build_rec(g, x, leaves, memo);
                let b = self.build_rec(g, y, leaves, memo);
                g.and(a, b)
            }
            Dec::Or(x, y) => {
                let a = self.build_rec(g, x, leaves, memo);
                let b = self.build_rec(g, y, leaves, memo);
                g.or(a, b)
            }
            Dec::Xor(x, y) => {
                let a = self.build_rec(g, x, leaves, memo);
                let b = self.build_rec(g, y, leaves, memo);
                g.xor(a, b)
            }
            Dec::Mux(v, t, e) => {
                let a = self.build_rec(g, t, leaves, memo);
                let b = self.build_rec(g, e, leaves, memo);
                g.mux(leaves[v], a, b)
            }
            Dec::Form(form, inv) => super::factor::build(g, &form, leaves).xor(inv),
        };
        memo.insert(f, l);
        l
    }
}

/// Nodes that building `f` would add to `aig`, reusing existing nodes outside `freed`
/// (a dry run of `Lib4::build`).
pub fn dry_cost(
    lib: &mut Lib4,
    aig: &Aig,
    f: u16,
    leaves: &[Lit; 4],
    freed: &FxHashSet<u32>,
) -> usize {
    let mut cost = 0;
    let mut memo: FxHashMap<u16, Option<Lit>> = FxHashMap::default();
    dry_rec(lib, aig, f, leaves, freed, &mut memo, &mut cost);
    cost
}

fn dry_and(
    aig: &Aig,
    x: Option<Lit>,
    y: Option<Lit>,
    freed: &FxHashSet<u32>,
    cost: &mut usize,
) -> Option<Lit> {
    if let (Some(p), Some(q)) = (x, y) {
        if p == Lit::FALSE || q == Lit::FALSE || p == !q {
            return Some(Lit::FALSE);
        }
        if p == Lit::TRUE || p == q {
            return Some(q);
        }
        if q == Lit::TRUE {
            return Some(p);
        }
        if let Some(r) = aig.lookup_and(p, q) {
            if !freed.contains(&r.node()) {
                return Some(r);
            }
        }
    }
    *cost += 1;
    None
}

fn dry_rec(
    lib: &mut Lib4,
    aig: &Aig,
    f: u16,
    leaves: &[Lit; 4],
    freed: &FxHashSet<u32>,
    memo: &mut FxHashMap<u16, Option<Lit>>,
    cost: &mut usize,
) -> Option<Lit> {
    if let Some(l) = memo.get(&f) {
        return *l;
    }
    if let Some(l) = memo.get(&!f) {
        return l.map(|x| !x);
    }
    let (_, d) = lib.get(f);
    let neg = |x: Option<Lit>| x.map(|l| !l);
    let r = match d {
        Dec::Const(c) => Some(Lit::from_bool(c)),
        Dec::Var(v, n) => Some(leaves[v].xor(n)),
        Dec::And(x, y) => {
            let a = dry_rec(lib, aig, x, leaves, freed, memo, cost);
            let b = dry_rec(lib, aig, y, leaves, freed, memo, cost);
            dry_and(aig, a, b, freed, cost)
        }
        Dec::Or(x, y) => {
            let a = dry_rec(lib, aig, x, leaves, freed, memo, cost);
            let b = dry_rec(lib, aig, y, leaves, freed, memo, cost);
            neg(dry_and(aig, neg(a), neg(b), freed, cost))
        }
        Dec::Xor(x, y) => {
            let a = dry_rec(lib, aig, x, leaves, freed, memo, cost);
            let b = dry_rec(lib, aig, y, leaves, freed, memo, cost);
            // a ^ b = !(!(a & !b) & !(!a & b))
            let p = dry_and(aig, a, neg(b), freed, cost);
            let q = dry_and(aig, neg(a), b, freed, cost);
            neg(dry_and(aig, neg(p), neg(q), freed, cost))
        }
        Dec::Mux(v, t, e) => {
            let s = Some(leaves[v]);
            let a = dry_rec(lib, aig, t, leaves, freed, memo, cost);
            let b = dry_rec(lib, aig, e, leaves, freed, memo, cost);
            // s ? a : b = !(!(s & a) & !(!s & b))
            let p = dry_and(aig, s, a, freed, cost);
            let q = dry_and(aig, neg(s), b, freed, cost);
            neg(dry_and(aig, neg(p), neg(q), freed, cost))
        }
        Dec::Form(form, inv) => {
            let ls: Vec<Lit> = leaves.to_vec();
            super::refactor::dry_cost(aig, &form, &ls, freed, cost).map(|l| l.xor(inv))
        }
    };
    memo.insert(f, r);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    // SYNTH-REQ-0411: every function of four variables is built exactly, and parity needs at
    // most 9 nodes.
    #[test]
    fn all_functions_exact() {
        let mut lib = Lib4::new();
        let mut g = Aig::new();
        let leaves = [g.input(), g.input(), g.input(), g.input()];
        let words: Vec<u64> = (0..4)
            .map(|v| (0..16u64).fold(0, |a, m| if (m >> v) & 1 == 1 { a | 1 << m } else { a }))
            .collect();
        for f in 0..=0xFFFFu16 {
            let l = lib.build(&mut g, f, &leaves);
            let sim = g.simulate(&words);
            let got = (Aig::lit_value(&sim, l) & 0xFFFF) as u16;
            assert_eq!(got, f, "function {f:04x}");
        }
        let parity = VAR[0] ^ VAR[1] ^ VAR[2] ^ VAR[3];
        assert!(lib.cost(parity) <= 9, "parity cost {}", lib.cost(parity));
        let xor3 = VAR[0] ^ VAR[1] ^ VAR[2];
        assert!(lib.cost(xor3) <= 6);
        // Never worse than the factored cover alone.
        let (mut ours, mut sop) = (0u64, 0u64);
        for f in 0..=0xFFFFu16 {
            let (form, _) = best_form(u64::from(f) | (u64::from(f) << 16), 4);
            let c = lib.cost(f);
            assert!(c as usize <= form.gates(), "function {f:04x}");
            ours += u64::from(c);
            sop += form.gates() as u64;
        }
        assert!(ours < sop);
        eprintln!("total nodes over all functions: {ours} (factored covers: {sop})");
    }
}
