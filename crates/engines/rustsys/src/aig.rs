// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! And-Inverter Graph with structural hashing (SYNTH-REQ-0401) [@kuehlmann2002robust].
//!
//! Nodes are stored as structure-of-arrays. Node 0 is the constant false; every other node is a
//! combinational input or a two-input AND whose fanins have smaller ids, so ids are a
//! topological order.

use rustc_hash::FxHashMap;

/// A literal: node id times two, plus one if complemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Lit(pub u32);

impl Lit {
    pub const FALSE: Lit = Lit(0);
    pub const TRUE: Lit = Lit(1);
    /// Marker stored in fanin arrays of non-AND nodes.
    const NONE: Lit = Lit(u32::MAX);

    pub fn new(node: u32, compl: bool) -> Lit {
        Lit(node << 1 | u32::from(compl))
    }

    pub fn node(self) -> u32 {
        self.0 >> 1
    }

    pub fn is_compl(self) -> bool {
        self.0 & 1 == 1
    }

    pub fn regular(self) -> Lit {
        Lit(self.0 & !1)
    }

    pub fn is_const(self) -> bool {
        self.0 <= 1
    }

    /// Complements the literal if `c`.
    pub fn xor(self, c: bool) -> Lit {
        Lit(self.0 ^ u32::from(c))
    }

    pub fn from_bool(b: bool) -> Lit {
        if b { Lit::TRUE } else { Lit::FALSE }
    }

    /// The constant value, if constant.
    pub fn const_value(self) -> Option<bool> {
        match self.0 {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl std::ops::Not for Lit {
    type Output = Lit;
    fn not(self) -> Lit {
        Lit(self.0 ^ 1)
    }
}

/// One wide addition recorded during timing-aware elaboration.
#[derive(Debug, Clone)]
pub(crate) struct AddSite {
    pub a: Vec<Lit>,
    pub b: Vec<Lit>,
    pub cin: Lit,
    pub sum: Vec<Lit>,
    pub carry: Lit,
}

/// An optional pre-optimization arithmetic observation for external proofs.
#[derive(Debug, Clone)]
pub struct MulSite {
    pub a: Vec<Lit>,
    pub b: Vec<Lit>,
    pub product: Vec<Lit>,
    pub signed: bool,
}

/// And-Inverter Graph.
#[derive(Debug, Clone)]
pub struct Aig {
    pub(crate) mul_sites: Option<Vec<MulSite>>,
    pub(crate) mul_trace_overflow: bool,
    /// Use bounded local carry chains during word-level lowering.
    pub(crate) parallel_carry: bool,
    pub(crate) xor_carry: bool,
    pub(crate) has_parallel_arithmetic: bool,
    pub(crate) add_sites: Vec<Option<AddSite>>,
    pub(crate) carry_plan: Vec<usize>,
    pub(crate) carry_cost_weight: Option<f64>,
    level: Vec<u32>,
    fanin0: Vec<Lit>,
    fanin1: Vec<Lit>,
    /// Input index for input nodes, `u32::MAX` otherwise.
    input_index: Vec<u32>,
    inputs: Vec<u32>,
    strash: FxHashMap<(Lit, Lit), u32>,
}

impl Default for Aig {
    fn default() -> Aig {
        Aig::new()
    }
}

impl Aig {
    pub fn new() -> Aig {
        Aig {
            mul_sites: None,
            mul_trace_overflow: false,
            parallel_carry: false,
            xor_carry: false,
            has_parallel_arithmetic: false,
            add_sites: Vec::new(),
            carry_plan: Vec::new(),
            carry_cost_weight: None,
            level: vec![0],
            fanin0: vec![Lit::NONE],
            fanin1: vec![Lit::NONE],
            input_index: vec![u32::MAX],
            inputs: Vec::new(),
            strash: FxHashMap::default(),
        }
    }

    /// Multipliers captured only by the explicit diagnostic elaboration API.
    pub fn multiplier_sites(&self) -> &[MulSite] {
        self.mul_sites.as_deref().unwrap_or(&[])
    }

    pub(crate) fn note_multiplier(&mut self, a: &[Lit], b: &[Lit], product: &[Lit], signed: bool) {
        let Some(sites) = &mut self.mul_sites else {
            return;
        };
        if !a.iter().any(|l| !l.is_const()) || !b.iter().any(|l| !l.is_const()) {
            return;
        }
        if sites
            .iter()
            .any(|s| s.a == a && s.b == b && s.product == product && s.signed == signed)
        {
            return;
        }
        let stored: usize = sites
            .iter()
            .map(|s| s.a.len() + s.b.len() + s.product.len())
            .sum();
        if sites.len() >= 64 || stored + a.len() + b.len() + product.len() > 262_144 {
            self.mul_trace_overflow = true;
            return;
        }
        sites.push(MulSite {
            a: a.to_vec(),
            b: b.to_vec(),
            product: product.to_vec(),
            signed,
        });
    }

    pub(crate) fn level(&self, lit: Lit) -> u32 {
        self.level[lit.node() as usize]
    }

    pub fn num_nodes(&self) -> usize {
        self.fanin0.len()
    }

    pub fn num_inputs(&self) -> usize {
        self.inputs.len()
    }

    /// Number of AND nodes.
    pub fn num_ands(&self) -> usize {
        self.fanin0.len() - 1 - self.inputs.len()
    }

    /// Creates a combinational input.
    pub fn input(&mut self) -> Lit {
        let id = self.fanin0.len() as u32;
        self.level.push(0);
        self.fanin0.push(Lit::NONE);
        self.fanin1.push(Lit::NONE);
        self.input_index.push(self.inputs.len() as u32);
        self.inputs.push(id);
        Lit::new(id, false)
    }

    /// Node id of input `i`.
    pub fn input_node(&self, i: usize) -> u32 {
        self.inputs[i]
    }

    pub fn inputs(&self) -> &[u32] {
        &self.inputs
    }

    /// Input index of a node, if it is an input.
    pub fn input_of(&self, node: u32) -> Option<u32> {
        let v = self.input_index[node as usize];
        (v != u32::MAX).then_some(v)
    }

    pub fn is_and(&self, node: u32) -> bool {
        node != 0 && self.input_index[node as usize] == u32::MAX
    }

    pub fn is_input(&self, node: u32) -> bool {
        self.input_index[node as usize] != u32::MAX
    }

    pub fn fanins(&self, node: u32) -> (Lit, Lit) {
        (self.fanin0[node as usize], self.fanin1[node as usize])
    }

    /// AND of two literals with constant propagation, trivial and two-level simplifications,
    /// and structural hashing.
    pub fn and(&mut self, a: Lit, b: Lit) -> Lit {
        if a == Lit::FALSE || b == Lit::FALSE || a == !b {
            return Lit::FALSE;
        }
        if a == Lit::TRUE || a == b {
            return b;
        }
        if b == Lit::TRUE {
            return a;
        }
        // Two-level rules: a & (a & x) = a & x; a & (!a & x) = 0.
        for (x, y) in [(a, b), (b, a)] {
            if !y.is_compl() && self.is_and(y.node()) {
                let (p, q) = self.fanins(y.node());
                if p == x || q == x {
                    return y;
                }
                if p == !x || q == !x {
                    return Lit::FALSE;
                }
            }
        }
        let (a, b) = if a.0 < b.0 { (a, b) } else { (b, a) };
        if let Some(&n) = self.strash.get(&(a, b)) {
            return Lit::new(n, false);
        }
        let id = self.fanin0.len() as u32;
        self.level
            .push(1 + self.level[a.node() as usize].max(self.level[b.node() as usize]));
        self.fanin0.push(a);
        self.fanin1.push(b);
        self.input_index.push(u32::MAX);
        self.strash.insert((a, b), id);
        Lit::new(id, false)
    }

    /// The literal `and(a, b)` would return, if it needs no new node.
    pub fn lookup_and(&self, a: Lit, b: Lit) -> Option<Lit> {
        if a == Lit::FALSE || b == Lit::FALSE || a == !b {
            return Some(Lit::FALSE);
        }
        if a == Lit::TRUE || a == b {
            return Some(b);
        }
        if b == Lit::TRUE {
            return Some(a);
        }
        for (x, y) in [(a, b), (b, a)] {
            if !y.is_compl() && self.is_and(y.node()) {
                let (p, q) = self.fanins(y.node());
                if p == x || q == x {
                    return Some(y);
                }
                if p == !x || q == !x {
                    return Some(Lit::FALSE);
                }
            }
        }
        let (a, b) = if a.0 < b.0 { (a, b) } else { (b, a) };
        self.strash.get(&(a, b)).map(|&n| Lit::new(n, false))
    }

    pub fn or(&mut self, a: Lit, b: Lit) -> Lit {
        !self.and(!a, !b)
    }

    pub fn xor(&mut self, a: Lit, b: Lit) -> Lit {
        if a == b {
            return Lit::FALSE;
        }
        if a == !b {
            return Lit::TRUE;
        }
        if let Some(c) = a.const_value() {
            return b.xor(c);
        }
        if let Some(c) = b.const_value() {
            return a.xor(c);
        }
        let x = self.and(a, !b);
        let y = self.and(!a, b);
        self.or(x, y)
    }

    pub fn xnor(&mut self, a: Lit, b: Lit) -> Lit {
        !self.xor(a, b)
    }

    /// `s ? t : e`
    pub fn mux(&mut self, s: Lit, t: Lit, e: Lit) -> Lit {
        match s.const_value() {
            Some(true) => return t,
            Some(false) => return e,
            None => {}
        }
        if t == e {
            return t;
        }
        if t == !e {
            return self.xnor(s, t);
        }
        if t == Lit::TRUE || t == s {
            return self.or(s, e);
        }
        if t == Lit::FALSE || t == !s {
            return self.and(!s, e);
        }
        if e == Lit::TRUE || e == !s {
            return self.or(!s, t);
        }
        if e == Lit::FALSE || e == s {
            return self.and(s, t);
        }
        let x = self.and(s, t);
        let y = self.and(!s, e);
        self.or(x, y)
    }

    /// `s ? t : e`, merging a data input that is a multiplexer sharing the other data input into
    /// this one (SYNTH-REQ-0406).
    pub fn mux_merging(&mut self, s: Lit, t: Lit, e: Lit) -> Lit {
        if s.is_const() || t.is_const() || e.is_const() || t == e || t == !e {
            return self.mux(s, t, e);
        }
        for (s2, t2, e2) in self.mux_readings(t) {
            if e2 == e {
                let s = self.and(s, s2);
                return self.mux_merging(s, t2, e);
            }
        }
        for (s2, t2, e2) in self.mux_readings(e) {
            if t2 == t {
                let s = self.or(s, s2);
                return self.mux_merging(s, t, e2);
            }
        }
        self.mux(s, t, e)
    }

    /// The readings `(s, t, e)` of `l` as `s ? t : e` when `l` has the structure of a
    /// multiplexer: two AND nodes, on complementary literals, under an OR. Each structure has two
    /// readings, with the select complemented and the data inputs swapped.
    pub fn as_mux(&self, l: Lit) -> Vec<(Lit, Lit, Lit)> {
        self.mux_readings(l).into_vec()
    }

    /// The same ordered readings, with a hard four-entry inline buffer. A two-input AND
    /// has at most two complementary input matches, each with two selector polarities.
    /// Hot optimization probes therefore need no per-node heap allocation.
    pub fn mux_readings(&self, l: Lit) -> smallvec::SmallVec<[(Lit, Lit, Lit); 4]> {
        let n = l.node();
        if !self.is_and(n) {
            return smallvec::SmallVec::new();
        }
        let (p, q) = self.fanins(n);
        if !p.is_compl() || !q.is_compl() || !self.is_and(p.node()) || !self.is_and(q.node()) {
            return smallvec::SmallVec::new();
        }
        let (a0, a1) = self.fanins(p.node());
        let (b0, b1) = self.fanins(q.node());
        // `n` is !(x | y) with x = a0 & a1 and y = b0 & b1; `l` is x | y when complemented.
        let c = l.is_compl();
        let mut out = smallvec::SmallVec::new();
        for (s, t) in [(a0, a1), (a1, a0)] {
            for (r, e) in [(b0, b1), (b1, b0)] {
                if r == !s {
                    let (t, e) = if c { (t, e) } else { (!t, !e) };
                    out.push((s, t, e));
                    out.push((!s, e, t));
                }
            }
        }
        out
    }

    /// Nodes in the transitive fanin of `roots` (inputs and ANDs, not the constant), in
    /// increasing id order.
    pub fn cone(&self, roots: &[Lit]) -> Vec<u32> {
        let mut mark = vec![false; self.num_nodes()];
        let mut stack: Vec<u32> = roots.iter().map(|l| l.node()).collect();
        let mut out = Vec::new();
        while let Some(n) = stack.pop() {
            if n == 0 || mark[n as usize] {
                continue;
            }
            mark[n as usize] = true;
            out.push(n);
            if self.is_and(n) {
                let (a, b) = self.fanins(n);
                stack.push(a.node());
                stack.push(b.node());
            }
        }
        out.sort_unstable();
        out
    }

    /// Copies the cone of `roots` into `dst`, mapping each input node through `map_input`.
    /// Returns the images of the roots.
    pub fn copy_into(
        &self,
        dst: &mut Aig,
        roots: &[Lit],
        map_input: &mut dyn FnMut(&mut Aig, u32) -> Lit,
    ) -> Vec<Lit> {
        let mut img: FxHashMap<u32, Lit> = FxHashMap::default();
        img.insert(0, Lit::FALSE);
        for n in self.cone(roots) {
            let l = if self.is_input(n) {
                map_input(dst, n)
            } else {
                let (a, b) = self.fanins(n);
                let a = img[&a.node()].xor(a.is_compl());
                let b = img[&b.node()].xor(b.is_compl());
                dst.and(a, b)
            };
            img.insert(n, l);
        }
        roots
            .iter()
            .map(|r| img[&r.node()].xor(r.is_compl()))
            .collect()
    }

    /// Rebuilds the cone of `roots` inside this graph with some nodes substituted.
    /// `subst(node)` returns a replacement literal for a node, or `None` to keep it.
    pub fn substitute(&mut self, roots: &[Lit], subst: &dyn Fn(u32) -> Option<Lit>) -> Vec<Lit> {
        let mut img: FxHashMap<u32, Lit> = FxHashMap::default();
        img.insert(0, Lit::FALSE);
        for n in self.cone(roots) {
            let l = match subst(n) {
                Some(l) => l,
                None if self.is_input(n) => Lit::new(n, false),
                None => {
                    let (a, b) = self.fanins(n);
                    let a = img[&a.node()].xor(a.is_compl());
                    let b = img[&b.node()].xor(b.is_compl());
                    self.and(a, b)
                }
            };
            img.insert(n, l);
        }
        roots
            .iter()
            .map(|r| img[&r.node()].xor(r.is_compl()))
            .collect()
    }

    /// Cofactors of `roots` with the given nodes fixed to constants.
    pub fn cofactor(&mut self, roots: &[Lit], fixed: &[(u32, bool)]) -> Vec<Lit> {
        let fixed: Vec<(u32, bool)> = fixed.to_vec();
        self.substitute(roots, &move |n| {
            fixed
                .iter()
                .find(|(m, _)| *m == n)
                .map(|(_, v)| Lit::from_bool(*v))
        })
    }

    /// True if the cone of `root` contains `node`.
    pub fn depends_on(&self, root: Lit, node: u32) -> bool {
        if root.node() < node {
            return false;
        }
        self.cone(&[root]).binary_search(&node).is_ok()
    }

    /// Bit-parallel simulation of all nodes with 64 patterns per word; `input_words[i]` gives
    /// the patterns of input i.
    pub fn simulate(&self, input_words: &[u64]) -> Vec<u64> {
        let mut v = vec![0u64; self.num_nodes()];
        for n in 1..self.num_nodes() as u32 {
            v[n as usize] = if let Some(i) = self.input_of(n) {
                input_words.get(i as usize).copied().unwrap_or(0)
            } else {
                let (a, b) = self.fanins(n);
                let x = v[a.node() as usize] ^ if a.is_compl() { u64::MAX } else { 0 };
                let y = v[b.node() as usize] ^ if b.is_compl() { u64::MAX } else { 0 };
                x & y
            };
        }
        v
    }

    /// Value of a literal in a simulation vector.
    pub fn lit_value(sim: &[u64], l: Lit) -> u64 {
        sim[l.node() as usize] ^ if l.is_compl() { u64::MAX } else { 0 }
    }

    /// Logic level of every node (inputs 0).
    pub fn levels(&self) -> Vec<u32> {
        let mut lv = vec![0u32; self.num_nodes()];
        for n in 1..self.num_nodes() as u32 {
            if self.is_and(n) {
                let (a, b) = self.fanins(n);
                lv[n as usize] = 1 + lv[a.node() as usize].max(lv[b.node() as usize]);
            }
        }
        lv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SYNTH-REQ-0401
    #[test]
    fn hashing_and_simplification() {
        let mut g = Aig::new();
        let a = g.input();
        let b = g.input();
        let x = g.and(a, b);
        assert_eq!(g.and(b, a), x);
        assert_eq!(g.and(a, !a), Lit::FALSE);
        assert_eq!(g.and(a, Lit::TRUE), a);
        assert_eq!(g.and(a, x), x);
        assert_eq!(g.and(!a, x), Lit::FALSE);
        assert_eq!(g.xor(a, a), Lit::FALSE);
        assert_eq!(g.mux(Lit::TRUE, a, b), a);
        assert_eq!(g.mux(a, b, b), b);
        assert_eq!(g.num_ands(), 1);
        let y = g.xor(a, b);
        let sim = g.simulate(&[0b1100, 0b1010]);
        assert_eq!(Aig::lit_value(&sim, y) & 0xF, 0b0110);
        assert_eq!(Aig::lit_value(&sim, x) & 0xF, 0b1000);
    }

    #[test]
    fn cofactor_and_copy() {
        let mut g = Aig::new();
        let a = g.input();
        let b = g.input();
        let m = g.mux(a, b, !b);
        let c = g.cofactor(&[m], &[(a.node(), true)]);
        assert_eq!(c[0], b);
        let c = g.cofactor(&[m], &[(a.node(), false)]);
        assert_eq!(c[0], !b);
        assert!(g.depends_on(m, a.node()));
        let mut h = Aig::new();
        let i0 = h.input();
        let img = g.copy_into(&mut h, &[m], &mut |_, n| {
            if n == a.node() { Lit::TRUE } else { i0 }
        });
        assert_eq!(img[0], i0);
    }
}
