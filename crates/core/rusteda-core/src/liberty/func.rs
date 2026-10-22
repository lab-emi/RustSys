// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Liberty Boolean functions: names, evaluation and truth tables (LIBERTY-REQ-0022). The parser
//! of function text is `rusteda_formats::liberty::parse_function` (LIBERTY-REQ-0020).

use std::fmt;

/// A Boolean function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Func {
    Const(bool),
    Var(String),
    Not(Box<Func>),
    And(Box<Func>, Box<Func>),
    Or(Box<Func>, Box<Func>),
    Xor(Box<Func>, Box<Func>),
}

impl Func {
    /// Names referenced by the function, in order of first appearance.
    pub fn vars(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_vars(&mut out);
        out
    }

    fn collect_vars(&self, out: &mut Vec<String>) {
        match self {
            Func::Const(_) => {}
            Func::Var(v) => {
                if !out.contains(v) {
                    out.push(v.clone());
                }
            }
            Func::Not(a) => a.collect_vars(out),
            Func::And(a, b) | Func::Or(a, b) | Func::Xor(a, b) => {
                a.collect_vars(out);
                b.collect_vars(out);
            }
        }
    }

    /// Evaluates the function under an assignment given by a lookup closure.
    pub fn eval(&self, value: &dyn Fn(&str) -> Option<bool>) -> Option<bool> {
        Some(match self {
            Func::Const(c) => *c,
            Func::Var(v) => value(v)?,
            Func::Not(a) => !a.eval(value)?,
            Func::And(a, b) => a.eval(value)? & b.eval(value)?,
            Func::Or(a, b) => a.eval(value)? | b.eval(value)?,
            Func::Xor(a, b) => a.eval(value)? ^ b.eval(value)?,
        })
    }

    /// Truth table over at most 6 ordered variables (LIBERTY-REQ-0022). Bit i of the result is
    /// the value under the assignment where variable j equals bit j of i. Returns `None` if the
    /// function refers to a name not in `vars` or if more than 6 variables are given.
    pub fn truth_table(&self, vars: &[&str]) -> Option<u64> {
        if vars.len() > 6 {
            return None;
        }
        self.tt_rec(vars)
    }

    fn tt_rec(&self, vars: &[&str]) -> Option<u64> {
        let n = vars.len();
        let mask = if n == 6 {
            u64::MAX
        } else {
            (1u64 << (1u32 << n)) - 1
        };
        Some(match self {
            Func::Const(false) => 0,
            Func::Const(true) => mask,
            Func::Var(v) => {
                let j = vars.iter().position(|x| x == v)?;
                var_mask(j) & mask
            }
            Func::Not(a) => !a.tt_rec(vars)? & mask,
            Func::And(a, b) => a.tt_rec(vars)? & b.tt_rec(vars)?,
            Func::Or(a, b) => a.tt_rec(vars)? | b.tt_rec(vars)?,
            Func::Xor(a, b) => a.tt_rec(vars)? ^ b.tt_rec(vars)?,
        })
    }
}

/// Truth-table mask of variable j (0..6) over 6 variables.
pub fn var_mask(j: usize) -> u64 {
    const M: [u64; 6] = [
        0xAAAA_AAAA_AAAA_AAAA,
        0xCCCC_CCCC_CCCC_CCCC,
        0xF0F0_F0F0_F0F0_F0F0,
        0xFF00_FF00_FF00_FF00,
        0xFFFF_0000_FFFF_0000,
        0xFFFF_FFFF_0000_0000,
    ];
    M[j]
}

impl fmt::Display for Func {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Func::Const(c) => write!(f, "{}", u8::from(*c)),
            Func::Var(v) => write!(f, "{v}"),
            Func::Not(a) => write!(f, "!({a})"),
            Func::And(a, b) => write!(f, "({a} & {b})"),
            Func::Or(a, b) => write!(f, "({a} | {b})"),
            Func::Xor(a, b) => write!(f, "({a} ^ {b})"),
        }
    }
}
