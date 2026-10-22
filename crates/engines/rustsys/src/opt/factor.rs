// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Irredundant sum-of-products from truth tables [@minato1992fast] and algebraic factoring
//! [@brayton1987factoring]: single-word truth tables for at most 6 variables, factoring of covers
//! over at most 16 variables.

use crate::aig::{Aig, Lit};
use crate::map::cuts::var_mask;

/// A product term: variables that appear positive and negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cube {
    pub pos: u16,
    pub neg: u16,
}

/// Largest number of variables of a cover.
pub const MAX_VARS: usize = 16;

impl Cube {
    fn lits(self) -> u32 {
        self.pos.count_ones() + self.neg.count_ones()
    }
}

fn mask(n: usize) -> u64 {
    if n >= 6 {
        u64::MAX
    } else {
        (1u64 << (1u32 << n)) - 1
    }
}

/// Cofactors of `t` with respect to variable `v`, each replicated over both halves.
fn cofactors(t: u64, v: usize) -> (u64, u64) {
    let m = var_mask(v);
    let s = 1u32 << v;
    let hi = t & m;
    let lo = t & !m;
    (lo | (lo << s), hi | (hi >> s))
}

/// Irredundant cover of a function between `on` and `upper` (on <= f <= upper).
/// Returns the cubes and the function they implement.
fn isop(on: u64, upper: u64, nvars: usize) -> (Vec<Cube>, u64) {
    let all = mask(nvars);
    let (on, upper) = (on & all, upper & all);
    if on == 0 {
        return (Vec::new(), 0);
    }
    if upper == all {
        return (vec![Cube { pos: 0, neg: 0 }], all);
    }
    // Highest variable the interval depends on.
    let mut v = nvars;
    while v > 0 {
        let (a0, a1) = cofactors(on, v - 1);
        let (b0, b1) = cofactors(upper, v - 1);
        if a0 != a1 || b0 != b1 {
            break;
        }
        v -= 1;
    }
    if v == 0 {
        // Constant interval: on is non-zero, so the constant 1 fits.
        return (vec![Cube { pos: 0, neg: 0 }], all);
    }
    let x = v - 1;
    let (on0, on1) = cofactors(on, x);
    let (up0, up1) = cofactors(upper, x);
    let (c0, f0) = isop(on0 & !up1, up0, nvars);
    let (c1, f1) = isop(on1 & !up0, up1, nvars);
    let rest_on = (on0 & !f0) | (on1 & !f1);
    let (c2, f2) = isop(rest_on, up0 & up1, nvars);
    let m = var_mask(x);
    let f = ((f0 & !m) | (f1 & m) | f2) & all;
    let mut cubes = Vec::with_capacity(c0.len() + c1.len() + c2.len());
    cubes.extend(c0.into_iter().map(|c| Cube {
        pos: c.pos,
        neg: c.neg | (1 << x),
    }));
    cubes.extend(c1.into_iter().map(|c| Cube {
        pos: c.pos | (1 << x),
        neg: c.neg,
    }));
    cubes.extend(c2);
    (cubes, f)
}

/// Irredundant sum of products of a completely specified function.
pub fn sop(t: u64, nvars: usize) -> Vec<Cube> {
    isop(t, t, nvars).0
}

/// Factored form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Form {
    Const(bool),
    Lit(usize, bool),
    And(Vec<Form>),
    Or(Vec<Form>),
}

impl Form {
    /// Number of two-input gates needed.
    pub fn gates(&self) -> usize {
        match self {
            Form::Const(_) | Form::Lit(..) => 0,
            Form::And(v) | Form::Or(v) => {
                v.iter().map(Form::gates).sum::<usize>() + v.len().saturating_sub(1)
            }
        }
    }
}

fn cube_form(c: Cube) -> Form {
    let mut lits = Vec::new();
    for v in 0..MAX_VARS {
        if c.pos & (1 << v) != 0 {
            lits.push(Form::Lit(v, false));
        }
        if c.neg & (1 << v) != 0 {
            lits.push(Form::Lit(v, true));
        }
    }
    match lits.len() {
        0 => Form::Const(true),
        1 => lits.pop().expect("one"),
        _ => Form::And(lits),
    }
}

/// Algebraic factoring by repeated division by the most frequent literal.
pub fn factor(cubes: &[Cube]) -> Form {
    if cubes.is_empty() {
        return Form::Const(false);
    }
    if cubes.len() == 1 {
        return cube_form(cubes[0]);
    }
    // Most frequent literal.
    let mut best: Option<(usize, bool, usize)> = None;
    for v in 0..MAX_VARS {
        for neg in [false, true] {
            let bit = 1u16 << v;
            let n = cubes
                .iter()
                .filter(|c| {
                    if neg {
                        c.neg & bit != 0
                    } else {
                        c.pos & bit != 0
                    }
                })
                .count();
            if n >= 2 && best.is_none_or(|b| n > b.2) {
                best = Some((v, neg, n));
            }
        }
    }
    let Some((v, neg, _)) = best else {
        return Form::Or(cubes.iter().map(|c| cube_form(*c)).collect());
    };
    let bit = 1u16 << v;
    let has = |c: &Cube| {
        if neg {
            c.neg & bit != 0
        } else {
            c.pos & bit != 0
        }
    };
    let quotient: Vec<Cube> = cubes
        .iter()
        .filter(|c| has(c))
        .map(|c| {
            if neg {
                Cube {
                    pos: c.pos,
                    neg: c.neg & !bit,
                }
            } else {
                Cube {
                    pos: c.pos & !bit,
                    neg: c.neg,
                }
            }
        })
        .collect();
    let rest: Vec<Cube> = cubes.iter().filter(|c| !has(c)).copied().collect();
    let q = factor(&quotient);
    let term = match q {
        Form::Const(true) => Form::Lit(v, neg),
        other => Form::And(vec![Form::Lit(v, neg), other]),
    };
    if rest.is_empty() {
        term
    } else {
        Form::Or(vec![term, factor(&rest)])
    }
}

/// Builds a factored form over leaf literals.
pub fn build(g: &mut Aig, f: &Form, leaves: &[Lit]) -> Lit {
    match f {
        Form::Const(c) => Lit::from_bool(*c),
        Form::Lit(v, n) => leaves[*v].xor(*n),
        Form::And(xs) => {
            let mut acc = Lit::TRUE;
            for x in xs {
                let y = build(g, x, leaves);
                acc = g.and(acc, y);
            }
            acc
        }
        Form::Or(xs) => {
            let mut acc = Lit::FALSE;
            for x in xs {
                let y = build(g, x, leaves);
                acc = g.or(acc, y);
            }
            acc
        }
    }
}

/// Best factored implementation of `t` over `nvars` variables: (form, output complemented).
pub fn best_form(t: u64, nvars: usize) -> (Form, bool) {
    let all = mask(nvars);
    let fp = factor(&sop(t & all, nvars));
    let fn_ = factor(&sop(!t & all, nvars));
    if fn_.gates() < fp.gates() {
        (fn_, true)
    } else {
        (fp, false)
    }
}

/// Literal count of a cover (for tests and statistics).
pub fn literals(c: &[Cube]) -> u32 {
    c.iter().map(|x| x.lits()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_form(f: &Form, m: u32) -> bool {
        match f {
            Form::Const(c) => *c,
            Form::Lit(v, n) => ((m >> v) & 1 == 1) != *n,
            Form::And(xs) => xs.iter().all(|x| eval_form(x, m)),
            Form::Or(xs) => xs.iter().any(|x| eval_form(x, m)),
        }
    }

    // SYNTH-REQ-0411: every function of up to 4 variables is reproduced exactly.
    #[test]
    fn all_four_input_functions() {
        for t in 0..=0xFFFFu64 {
            let cubes = sop(t, 4);
            let f = factor(&cubes);
            for m in 0..16u32 {
                assert_eq!(eval_form(&f, m), (t >> m) & 1 == 1, "t={t:04x} m={m}");
            }
            let (bf, inv) = best_form(t, 4);
            for m in 0..16u32 {
                assert_eq!(eval_form(&bf, m) != inv, (t >> m) & 1 == 1);
            }
        }
        // Irredundancy on a classic example: majority has three two-literal cubes.
        let maj = 0b1110_1000u64;
        assert_eq!(literals(&sop(maj, 3)), 6);
    }
}
