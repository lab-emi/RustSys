// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Proofs over the circuit: random simulation, then SAT on a Tseitin encoding
//! (CHECK-REQ-0042, CHECK-REQ-0043) [@tseitin1968complexity].

use std::collections::HashMap;

use crate::circ::{B, Circuit, Def};
use crate::sat::{Answer, Lit, Solver};

/// Outcome of a proof attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum Proof {
    Holds,
    /// Values of the free variables of a counterexample.
    Fails(Vec<(u32, bool)>),
    Unknown,
}

/// Deterministic pseudo-random generator (SplitMix64) [@steele2014fast].
#[derive(Debug, Clone)]
pub struct Rng(pub u64);

impl Rng {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Encodes the cone of `roots` into a solver; returns the solver and the variable map.
pub fn encode(c: &Circuit, order: &[u32]) -> (Solver, HashMap<u32, u32>) {
    let mut map = HashMap::new();
    for (i, v) in order.iter().enumerate() {
        map.insert(*v, i as u32);
    }
    // One extra variable reserved for the caller (miter output).
    let mut s = Solver::new(order.len() + 2);
    let l = |v: u32, map: &HashMap<u32, u32>| Lit::pos(map[&v]);
    for &v in order {
        let o = l(v, &map);
        match c.defs[v as usize] {
            Def::Free => {}
            Def::Alias(B::F) => s.add_clause(&[!o]),
            Def::Alias(B::T) => s.add_clause(&[o]),
            Def::Alias(B::V(x)) => {
                let a = l(x, &map);
                s.add_clause(&[!o, a]);
                s.add_clause(&[o, !a]);
            }
            Def::Not(x) => {
                let a = l(x, &map);
                s.add_clause(&[!o, !a]);
                s.add_clause(&[o, a]);
            }
            Def::And(x, y) => {
                let (a, b) = (l(x, &map), l(y, &map));
                s.add_clause(&[!o, a]);
                s.add_clause(&[!o, b]);
                s.add_clause(&[o, !a, !b]);
            }
            Def::Or(x, y) => {
                let (a, b) = (l(x, &map), l(y, &map));
                s.add_clause(&[o, !a]);
                s.add_clause(&[o, !b]);
                s.add_clause(&[!o, a, b]);
            }
            Def::Xor(x, y) => {
                let (a, b) = (l(x, &map), l(y, &map));
                s.add_clause(&[!o, a, b]);
                s.add_clause(&[!o, !a, !b]);
                s.add_clause(&[o, !a, b]);
                s.add_clause(&[o, a, !b]);
            }
            Def::Mux(sel, x, y) => {
                let (q, a, b) = (l(sel, &map), l(x, &map), l(y, &map));
                s.add_clause(&[!q, !a, o]);
                s.add_clause(&[!q, a, !o]);
                s.add_clause(&[q, !b, o]);
                s.add_clause(&[q, b, !o]);
                s.add_clause(&[!a, !b, o]);
                s.add_clause(&[a, b, !o]);
            }
        }
    }
    (s, map)
}

/// Literal of a bit in an encoding; constants use the reserved variable.
fn bit_lit(b: B, map: &HashMap<u32, u32>, konst: u32) -> Lit {
    match b {
        B::T => Lit::pos(konst),
        B::F => Lit::neg(konst),
        B::V(v) => Lit::pos(map[&v]),
    }
}

/// Proves that `f` is 1 whenever `assume` is 1, for all values of the free variables.
pub fn always(c: &Circuit, f: B, assume: B, budget: u64, rng: &mut Rng) -> Result<Proof, String> {
    always_with(c, f, assume, budget, rng, 8)
}

/// As `always`, with a given number of random simulation rounds before SAT.
pub fn always_with(
    c: &Circuit,
    f: B,
    assume: B,
    budget: u64,
    rng: &mut Rng,
    rounds: usize,
) -> Result<Proof, String> {
    if f == B::T || assume == B::F {
        return Ok(Proof::Holds);
    }
    let order = c
        .order(&[f, assume])
        .map_err(|cy| format!("combinational cycle through circuit variable {}", cy.0))?;
    // Random simulation first.
    let frees: Vec<u32> = order.iter().copied().filter(|v| c.is_free(*v)).collect();
    let mut vals = Vec::new();
    // Small cones in a multi-million-variable RTL model need only cone-sized
    // simulation storage. Preserve the same free-variable order and RNG draws.
    // Up to 262k entries in each cone-local table (<32 MiB total). Beyond
    // that bound, or when most variables are live, keep the dense evaluator.
    let sparse = order.len() <= 262_144 && c.defs.len() > 16 * order.len().max(1);
    let mut sparse_vals = HashMap::new();
    for _ in 0..rounds {
        let words: HashMap<u32, u64> = frees.iter().map(|v| (*v, rng.next())).collect();
        let bad = if sparse {
            c.eval(
                &order,
                &|v| words.get(&v).copied().unwrap_or(0),
                &mut sparse_vals,
            );
            Circuit::value(assume, &sparse_vals) & !Circuit::value(f, &sparse_vals)
        } else {
            c.eval_dense(&order, &|v| words.get(&v).copied().unwrap_or(0), &mut vals);
            Circuit::value_dense(assume, &vals) & !Circuit::value_dense(f, &vals)
        };
        if bad != 0 {
            let k = bad.trailing_zeros();
            return Ok(Proof::Fails(
                frees
                    .iter()
                    .map(|v| (*v, (words[v] >> k) & 1 == 1))
                    .collect(),
            ));
        }
    }
    let (mut s, map) = encode(c, &order);
    let konst = order.len() as u32;
    s.add_clause(&[Lit::pos(konst)]);
    s.add_clause(&[bit_lit(assume, &map, konst)]);
    s.add_clause(&[!bit_lit(f, &map, konst)]);
    Ok(match s.solve(budget) {
        Answer::Unsat => Proof::Holds,
        Answer::Sat(m) => Proof::Fails(frees.iter().map(|v| (*v, m[map[v] as usize])).collect()),
        Answer::Unknown => Proof::Unknown,
    })
}

/// Proves `a == b` whenever `assume` holds.
pub fn equal(
    c: &mut Circuit,
    a: B,
    b: B,
    assume: B,
    budget: u64,
    rng: &mut Rng,
) -> Result<Proof, String> {
    equal_with(c, a, b, assume, budget, rng, 8)
}

/// As `equal`, with a given number of random simulation rounds before SAT.
pub fn equal_with(
    c: &mut Circuit,
    a: B,
    b: B,
    assume: B,
    budget: u64,
    rng: &mut Rng,
    rounds: usize,
) -> Result<Proof, String> {
    if a == b {
        return Ok(Proof::Holds);
    }
    let x = c.xor(a, b);
    let nx = c.not(x);
    always_with(c, nx, assume, budget, rng, rounds)
}

/// Decides `a == b` whenever `assume` holds by evaluating every assignment of the free variables
/// in the cone, if there are at most `max_vars` of them; `None` otherwise.
pub fn exhaustive(
    c: &Circuit,
    a: B,
    b: B,
    assume: B,
    max_vars: usize,
) -> Result<Option<Proof>, String> {
    exhaustive_cmp(c, a, b, false, assume, max_vars)
}

/// As `exhaustive`, comparing `a` with `b` or, if `complement`, with its complement.
pub fn exhaustive_cmp(
    c: &Circuit,
    a: B,
    b: B,
    complement: bool,
    assume: B,
    max_vars: usize,
) -> Result<Option<Proof>, String> {
    // This is only a small-cone fast path. A dense array sized to the whole
    // circuit here costs tens of MiB *per obligation* on cache-heavy CPUs.
    // Two bounded maps plus stack/order stay below 256 KiB. Larger supports
    // fall back to the caller's SAT proof without allocating the full cone.
    const MAX_NODES: usize = 2048;
    const MAX_INPUTS: usize = 16;
    let max_vars = max_vars.min(MAX_INPUTS);
    let mut color = HashMap::<u32, u8>::new();
    let mut order = Vec::new();
    let mut stack = Vec::new();
    for root in [a, b, assume] {
        if let B::V(v) = root {
            stack.push((v, false));
        }
    }
    let mut inputs = 0;
    let mut expansions = 0;
    while let Some((v, exit)) = stack.pop() {
        expansions += 1;
        if expansions > 8 * MAX_NODES {
            return Ok(None);
        }
        if exit {
            color.insert(v, 2);
            order.push(v);
            continue;
        }
        match color.get(&v) {
            Some(2) => continue,
            Some(1) => return Err(format!("combinational cycle through circuit variable {v}")),
            _ => {}
        }
        if color.len() == MAX_NODES {
            return Ok(None);
        }
        color.insert(v, 1);
        stack.push((v, true));
        match c.defs[v as usize] {
            Def::Free => {
                inputs += 1;
                if inputs > max_vars {
                    return Ok(None);
                }
            }
            Def::Alias(B::V(x)) | Def::Not(x) => stack.push((x, false)),
            Def::And(x, y) | Def::Or(x, y) | Def::Xor(x, y) => {
                stack.extend([(x, false), (y, false)]);
            }
            Def::Mux(s, x, y) => stack.extend([(s, false), (x, false), (y, false)]),
            Def::Alias(_) => {}
        }
    }
    let frees: Vec<u32> = order.iter().copied().filter(|v| c.is_free(*v)).collect();
    let k = frees.len();
    if k > max_vars {
        return Ok(None);
    }
    // Lane j of word w is assignment number 64 w + j; free variable i takes bit i of it.
    const LANES: [u64; 6] = [
        0xAAAA_AAAA_AAAA_AAAA,
        0xCCCC_CCCC_CCCC_CCCC,
        0xF0F0_F0F0_F0F0_F0F0,
        0xFF00_FF00_FF00_FF00,
        0xFFFF_0000_FFFF_0000,
        0xFFFF_FFFF_0000_0000,
    ];
    let pos: HashMap<u32, usize> = frees.iter().enumerate().map(|(i, v)| (*v, i)).collect();
    let words = if k <= 6 { 1u64 } else { 1u64 << (k - 6) };
    let valid = if k >= 6 {
        u64::MAX
    } else {
        (1u64 << (1u32 << k)) - 1
    };
    let mut vals = HashMap::new();
    for w in 0..words {
        let word = |v: u32| {
            let i = pos[&v];
            if i < 6 {
                LANES[i]
            } else if (w >> (i - 6)) & 1 == 1 {
                u64::MAX
            } else {
                0
            }
        };
        c.eval(&order, &word, &mut vals);
        let flip = if complement { u64::MAX } else { 0 };
        let bad = valid
            & Circuit::value(assume, &vals)
            & (Circuit::value(a, &vals) ^ Circuit::value(b, &vals) ^ flip);
        if bad != 0 {
            let n = 64 * w + u64::from(bad.trailing_zeros());
            return Ok(Some(Proof::Fails(
                frees
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (*v, (n >> i) & 1 == 1))
                    .collect(),
            )));
        }
    }
    Ok(Some(Proof::Holds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_cone_simulation_preserves_rng_and_counterexamples() {
        let mut small = Circuit::new();
        let a = B::V(small.free());
        let b = B::V(small.free());
        let nb = small.not(b);
        let ab = small.and(a, b);
        let tautology = small.or(b, nb);
        let mut large = small.clone();
        for _ in 0..8192 {
            large.free();
        }
        for (f, assume) in [(ab, B::T), (tautology, B::T), (a, ab)] {
            let mut r1 = Rng(42);
            let mut r2 = Rng(42);
            assert_eq!(
                always(&small, f, assume, 1000, &mut r1).unwrap(),
                always(&large, f, assume, 1000, &mut r2).unwrap()
            );
            assert_eq!(r1.0, r2.0);
        }
    }

    #[test]
    fn exhaustive_is_sparse_and_bounded_on_large_circuits() {
        let mut c = Circuit::new();
        for _ in 0..100_000 {
            c.free();
        }
        let a = B::V(c.free());
        let b = B::V(c.free());
        let ab = c.and(a, b);
        let na = c.not(a);
        let nb = c.not(b);
        let or = c.or(na, nb);
        let demorgan = c.not(or);
        assert_eq!(
            exhaustive(&c, ab, demorgan, B::T, 8).unwrap(),
            Some(Proof::Holds)
        );
        assert!(matches!(
            exhaustive(&c, ab, a, B::T, 8).unwrap(),
            Some(Proof::Fails(_))
        ));
        let mut deep = a;
        for _ in 0..2100 {
            let v = c.free();
            c.defs[v as usize] = Def::Alias(deep);
            deep = B::V(v);
        }
        assert_eq!(exhaustive(&c, deep, a, B::T, 8).unwrap(), None);
    }

    #[test]
    fn exhaustive_detects_cycles_and_caps_enumeration() {
        let mut c = Circuit::new();
        let x = c.free();
        c.defs[x as usize] = Def::Alias(B::V(x));
        assert!(exhaustive(&c, B::V(x), B::T, B::T, 8).is_err());
        let mut c = Circuit::new();
        let mut all = B::T;
        for _ in 0..17 {
            let x = c.free();
            all = c.and(all, B::V(x));
        }
        assert_eq!(exhaustive(&c, all, B::T, B::T, usize::MAX).unwrap(), None);
    }

    #[test]
    fn proves_and_refutes() {
        let mut c = Circuit::new();
        let a = c.free();
        let b = c.free();
        let na = c.not(B::V(a));
        let t = c.or(B::V(a), na);
        let mut r = Rng(1);
        assert_eq!(always(&c, t, B::T, 1000, &mut r).unwrap(), Proof::Holds);
        let ab = c.and(B::V(a), B::V(b));
        match always(&c, ab, B::T, 1000, &mut r).unwrap() {
            Proof::Fails(m) => assert!(m.iter().any(|(_, v)| !v)),
            other => panic!("{other:?}"),
        }
        // (a & b) == !( !a | !b )
        let nb = c.not(B::V(b));
        let o = c.or(na, nb);
        let no = c.not(o);
        assert_eq!(
            equal(&mut c, ab, no, B::T, 1000, &mut r).unwrap(),
            Proof::Holds
        );
        // Exhaustive evaluation agrees.
        assert_eq!(exhaustive(&c, ab, no, B::T, 4).unwrap(), Some(Proof::Holds));
        match exhaustive(&c, ab, B::V(a), B::T, 4).unwrap() {
            Some(Proof::Fails(m)) => {
                let m: HashMap<u32, bool> = m.into_iter().collect();
                assert!(m[&a] && !m[&b]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(exhaustive(&c, ab, no, B::T, 1).unwrap(), None);
        // a == b under the assumption a xnor b.
        let x = c.xor(B::V(a), B::V(b));
        let nx = c.not(x);
        assert_eq!(
            equal(&mut c, B::V(a), B::V(b), nx, 1000, &mut r).unwrap(),
            Proof::Holds
        );
    }
}
