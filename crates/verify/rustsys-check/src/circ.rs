// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Gate circuit shared by the RTL and netlist models (CHECK-REQ-0010, CHECK-REQ-0020).
//!
//! Every variable has at most one definition. Free variables are inputs of the model. A
//! variable may be defined after it is used (a signal read before its driver is known); the
//! definition is then an alias of the driver. Constants are folded, nothing else is
//! simplified.

use std::collections::HashMap;

/// A bit: a constant or a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum B {
    F,
    T,
    V(u32),
}

impl B {
    pub fn from_bool(b: bool) -> B {
        if b { B::T } else { B::F }
    }

    pub fn konst(self) -> Option<bool> {
        match self {
            B::F => Some(false),
            B::T => Some(true),
            B::V(_) => None,
        }
    }
}

/// Definition of a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Def {
    Free,
    Alias(B),
    Not(u32),
    And(u32, u32),
    Or(u32, u32),
    Xor(u32, u32),
    /// `s ? a : b`
    Mux(u32, u32, u32),
}

/// The circuit.
#[derive(Debug, Clone, Default)]
pub struct Circuit {
    pub defs: Vec<Def>,
    not_cache: HashMap<u32, u32>,
}

/// Error for combinational cycles found while ordering the circuit.
#[derive(Debug, Clone)]
pub struct Cycle(pub u32);

impl Circuit {
    pub fn new() -> Circuit {
        Circuit::default()
    }

    pub fn free(&mut self) -> u32 {
        self.defs.push(Def::Free);
        (self.defs.len() - 1) as u32
    }

    fn add(&mut self, d: Def) -> B {
        self.defs.push(d);
        B::V((self.defs.len() - 1) as u32)
    }

    /// Defines a free variable as an alias. Returns false if it was already defined.
    pub fn define(&mut self, v: u32, b: B) -> bool {
        match self.defs[v as usize] {
            Def::Free => {
                self.defs[v as usize] = Def::Alias(b);
                true
            }
            _ => false,
        }
    }

    pub fn is_free(&self, v: u32) -> bool {
        matches!(self.defs[v as usize], Def::Free)
    }

    pub fn not(&mut self, a: B) -> B {
        match a {
            B::F => B::T,
            B::T => B::F,
            B::V(v) => {
                if let Def::Not(x) = self.defs[v as usize] {
                    return B::V(x);
                }
                if let Some(n) = self.not_cache.get(&v) {
                    return B::V(*n);
                }
                let r = self.add(Def::Not(v));
                if let B::V(n) = r {
                    self.not_cache.insert(v, n);
                }
                r
            }
        }
    }

    pub fn and(&mut self, a: B, b: B) -> B {
        match (a, b) {
            (B::F, _) | (_, B::F) => B::F,
            (B::T, x) | (x, B::T) => x,
            (B::V(x), B::V(y)) if x == y => a,
            (B::V(x), B::V(y)) => self.add(Def::And(x, y)),
        }
    }

    pub fn or(&mut self, a: B, b: B) -> B {
        match (a, b) {
            (B::T, _) | (_, B::T) => B::T,
            (B::F, x) | (x, B::F) => x,
            (B::V(x), B::V(y)) if x == y => a,
            (B::V(x), B::V(y)) => self.add(Def::Or(x, y)),
        }
    }

    pub fn xor(&mut self, a: B, b: B) -> B {
        match (a, b) {
            (B::F, x) | (x, B::F) => x,
            (B::T, x) | (x, B::T) => self.not(x),
            (B::V(x), B::V(y)) if x == y => B::F,
            (B::V(x), B::V(y)) => self.add(Def::Xor(x, y)),
        }
    }

    pub fn mux(&mut self, s: B, a: B, b: B) -> B {
        match s {
            B::T => a,
            B::F => b,
            B::V(sv) => {
                if a == b {
                    return a;
                }
                match (a, b) {
                    (B::V(x), B::V(y)) => self.add(Def::Mux(sv, x, y)),
                    _ => {
                        let t = self.and(s, a);
                        let ns = self.not(s);
                        let e = self.and(ns, b);
                        self.or(t, e)
                    }
                }
            }
        }
    }

    /// Variables in the transitive fanin of `roots` in topological order (definitions before
    /// uses), or the variable on a cycle.
    pub fn order(&self, roots: &[B]) -> Result<Vec<u32>, Cycle> {
        // RTL modeling asks many tiny cone questions inside a very large
        // circuit. Avoid clearing a whole-design color array on each question.
        // The capped trial holds <8 MiB; larger cones retain the dense walk.
        if self.defs.len() > 4096 && roots.len() <= 4096 {
            let cap = if self.defs.len() >= 1_000_000 {
                65536
            } else {
                4096
            };
            if let Some(order) = self.order_sparse(roots, cap, 8 * cap)? {
                return Ok(order);
            }
        }
        self.order_dense(roots)
    }

    /// Bounded small-cone ordering. `None` requests the caller's dense fallback;
    /// reaching a work bound never reports a proof or an acyclic graph.
    pub fn order_sparse(
        &self,
        roots: &[B],
        max_nodes: usize,
        max_expansions: usize,
    ) -> Result<Option<Vec<u32>>, Cycle> {
        if roots.len() > max_nodes {
            return Ok(None);
        }
        let mut colors = HashMap::new();
        let mut order = Vec::new();
        let mut stack = Vec::new();
        for &root in roots {
            if let B::V(v) = root {
                stack.push((v, false));
            }
        }
        let mut expansions = 0usize;
        while let Some((v, exit)) = stack.pop() {
            if expansions >= max_expansions {
                return Ok(None);
            }
            expansions += 1;
            if exit {
                if colors.get(&v) != Some(&2) {
                    colors.insert(v, 2u8);
                    order.push(v);
                }
                continue;
            }
            match colors.get(&v) {
                Some(2) => continue,
                Some(1) => return Err(Cycle(v)),
                _ => {}
            }
            if colors.len() >= max_nodes {
                return Ok(None);
            }
            colors.insert(v, 1);
            stack.push((v, true));
            match self.defs[v as usize] {
                Def::Alias(B::V(x)) | Def::Not(x) => stack.push((x, false)),
                Def::And(x, y) | Def::Or(x, y) | Def::Xor(x, y) => {
                    stack.extend([(x, false), (y, false)]);
                }
                Def::Mux(s, x, y) => stack.extend([(s, false), (x, false), (y, false)]),
                _ => {}
            }
        }
        Ok(Some(order))
    }

    fn order_dense(&self, roots: &[B]) -> Result<Vec<u32>, Cycle> {
        let n = self.defs.len();
        let mut state = vec![0u8; n];
        let mut out = Vec::new();
        let mut stack: Vec<(u32, bool)> = Vec::new();
        for r in roots {
            if let B::V(v) = r {
                stack.push((*v, false));
            }
        }
        while let Some((v, post)) = stack.pop() {
            let vi = v as usize;
            if post {
                if state[vi] != 2 {
                    state[vi] = 2;
                    out.push(v);
                }
                continue;
            }
            match state[vi] {
                2 => continue,
                1 => return Err(Cycle(v)),
                _ => {}
            }
            state[vi] = 1;
            stack.push((v, true));
            for f in self.fanins(v) {
                if state[f as usize] != 2 {
                    if state[f as usize] == 1 {
                        return Err(Cycle(f));
                    }
                    stack.push((f, false));
                }
            }
        }
        Ok(out)
    }

    /// Variable fanins of a definition.
    pub fn fanins(&self, v: u32) -> Vec<u32> {
        match self.defs[v as usize] {
            Def::Free | Def::Alias(B::F | B::T) => vec![],
            Def::Alias(B::V(x)) | Def::Not(x) => vec![x],
            Def::And(a, b) | Def::Or(a, b) | Def::Xor(a, b) => vec![a, b],
            Def::Mux(s, a, b) => vec![s, a, b],
        }
    }

    /// Bit-parallel evaluation of the variables in `order` given free-variable values.
    pub fn eval(&self, order: &[u32], free: &dyn Fn(u32) -> u64, vals: &mut HashMap<u32, u64>) {
        for &v in order {
            let g = |x: u32, vals: &HashMap<u32, u64>| vals.get(&x).copied().unwrap_or(0);
            let r = match self.defs[v as usize] {
                Def::Free => free(v),
                Def::Alias(B::F) => 0,
                Def::Alias(B::T) => u64::MAX,
                Def::Alias(B::V(x)) => g(x, vals),
                Def::Not(x) => !g(x, vals),
                Def::And(a, b) => g(a, vals) & g(b, vals),
                Def::Or(a, b) => g(a, vals) | g(b, vals),
                Def::Xor(a, b) => g(a, vals) ^ g(b, vals),
                Def::Mux(s, a, b) => {
                    let sv = g(s, vals);
                    (sv & g(a, vals)) | (!sv & g(b, vals))
                }
            };
            vals.insert(v, r);
        }
    }

    /// Bit-parallel evaluation into a dense vector indexed by variable (entries outside `order`
    /// are left untouched).
    pub fn eval_dense(&self, order: &[u32], free: &dyn Fn(u32) -> u64, vals: &mut Vec<u64>) {
        if vals.len() < self.defs.len() {
            vals.resize(self.defs.len(), 0);
        }
        for &v in order {
            let g = |x: u32, vals: &[u64]| vals[x as usize];
            let r = match self.defs[v as usize] {
                Def::Free => free(v),
                Def::Alias(B::F) => 0,
                Def::Alias(B::T) => u64::MAX,
                Def::Alias(B::V(x)) => g(x, vals),
                Def::Not(x) => !g(x, vals),
                Def::And(a, b) => g(a, vals) & g(b, vals),
                Def::Or(a, b) => g(a, vals) | g(b, vals),
                Def::Xor(a, b) => g(a, vals) ^ g(b, vals),
                Def::Mux(s, a, b) => {
                    let sv = g(s, vals);
                    (sv & g(a, vals)) | (!sv & g(b, vals))
                }
            };
            vals[v as usize] = r;
        }
    }

    /// Value of a bit after `eval_dense`.
    pub fn value_dense(b: B, vals: &[u64]) -> u64 {
        match b {
            B::F => 0,
            B::T => u64::MAX,
            B::V(v) => vals[v as usize],
        }
    }

    /// Value of a bit after `eval`.
    pub fn value(b: B, vals: &HashMap<u32, u64>) -> u64 {
        match b {
            B::F => 0,
            B::T => u64::MAX,
            B::V(v) => vals.get(&v).copied().unwrap_or(0),
        }
    }

    /// Rebuilds the cone of `root` with variable `var` replaced by `val`.
    pub fn subst(&mut self, root: B, var: u32, val: B) -> Result<B, Cycle> {
        let B::V(_) = root else { return Ok(root) };
        let order = self.order(&[root])?;
        if !order.contains(&var) {
            return Ok(root);
        }
        // Only gates on paths from `var` change; all others are reused as they are.
        let mut img: HashMap<u32, B> = HashMap::new();
        for v in order {
            let m = |x: u32, img: &HashMap<u32, B>| img.get(&x).copied().unwrap_or(B::V(x));
            let same =
                |xs: &[u32], img: &HashMap<u32, B>| xs.iter().all(|x| m(*x, img) == B::V(*x));
            let r = if v == var {
                val
            } else {
                match self.defs[v as usize] {
                    Def::Free => B::V(v),
                    Def::Alias(B::V(x)) => {
                        if same(&[x], &img) {
                            B::V(v)
                        } else {
                            m(x, &img)
                        }
                    }
                    Def::Alias(_) => B::V(v),
                    Def::Not(x) => {
                        if same(&[x], &img) {
                            B::V(v)
                        } else {
                            let a = m(x, &img);
                            self.not(a)
                        }
                    }
                    Def::And(x, y) | Def::Or(x, y) | Def::Xor(x, y) if same(&[x, y], &img) => {
                        B::V(v)
                    }
                    Def::And(x, y) => {
                        let (a, b) = (m(x, &img), m(y, &img));
                        self.and(a, b)
                    }
                    Def::Or(x, y) => {
                        let (a, b) = (m(x, &img), m(y, &img));
                        self.or(a, b)
                    }
                    Def::Xor(x, y) => {
                        let (a, b) = (m(x, &img), m(y, &img));
                        self.xor(a, b)
                    }
                    Def::Mux(s, x, y) => {
                        if same(&[s, x, y], &img) {
                            B::V(v)
                        } else {
                            let (c, a, b) = (m(s, &img), m(x, &img), m(y, &img));
                            self.mux(c, a, b)
                        }
                    }
                }
            };
            img.insert(v, r);
        }
        let B::V(rv) = root else { unreachable!() };
        Ok(img.get(&rv).copied().unwrap_or(root))
    }

    /// Free variables in the fanin of `roots`.
    pub fn support(&self, roots: &[B]) -> Result<Vec<u32>, Cycle> {
        Ok(self
            .order(roots)?
            .into_iter()
            .filter(|v| self.is_free(*v))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_ordering_and_eval() {
        let mut c = Circuit::new();
        let a = c.free();
        let b = c.free();
        let s = c.free();
        assert_eq!(c.and(B::V(a), B::F), B::F);
        assert_eq!(c.or(B::V(a), B::F), B::V(a));
        let na = c.not(B::V(a));
        assert_eq!(c.not(na), B::V(a));
        let m = c.mux(B::V(s), B::V(a), B::V(b));
        // A forward-declared variable defined later.
        let late = c.free();
        let x = c.xor(B::V(late), m);
        assert!(c.define(late, B::V(a)));
        assert!(!c.define(late, B::V(b)));
        let order = c.order(&[x]).unwrap();
        let mut vals = HashMap::new();
        c.eval(
            &order,
            &|v| {
                if v == a {
                    0b1100
                } else if v == b {
                    0b1010
                } else {
                    0b0110
                }
            },
            &mut vals,
        );
        // m = s ? a : b with s = 0110: bits 1,2 from a, bits 0,3 from b.
        assert_eq!(
            Circuit::value(m, &vals) & 0xF,
            0b1100 & 0b0110 | 0b1010 & 0b1001
        );
        // x = late ^ m with late = a: 1100 ^ 1100.
        assert_eq!(Circuit::value(x, &vals) & 0xF, 0);
        // A cycle through aliases is reported.
        let p = c.free();
        let q = c.and(B::V(p), B::V(a));
        c.define(p, q);
        assert!(c.order(&[B::V(p)]).is_err());
    }

    #[test]
    fn sparse_order_matches_dense_and_falls_back_at_budgets() {
        let mut c = Circuit::new();
        for _ in 0..8192 {
            c.free();
        }
        let a = B::V(8000);
        let b = B::V(8001);
        let x = c.xor(a, b);
        let nx = c.not(x);
        let y = c.mux(a, x, nx);
        let roots = [y, nx];
        assert_eq!(c.order(&roots).unwrap(), c.order_dense(&roots).unwrap());
        assert!(c.order_sparse(&roots, 2, 100).unwrap().is_none());
        assert!(c.order_sparse(&roots, 100, 2).unwrap().is_none());
        let cycle = c.free();
        let p = c.and(B::V(cycle), a);
        c.define(cycle, p);
        assert!(c.order_sparse(&[p], 100, 100).is_err());
        assert!(c.order(&[p]).is_err());
    }
}
