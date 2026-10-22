// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Filter expressions (PROTO-REQ-0131).

use crate::proto::table::{TableModel, Value, keys};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Debug, Clone, PartialEq)]
enum Term {
    Num { col: usize, op: Op, v: f64 },
    Word { col: usize, word: Word },
}

#[derive(Debug, Clone, PartialEq)]
struct Word {
    pattern: String,
    glob: bool,
    fold: bool,
}

impl Word {
    fn new(w: &str) -> Word {
        let fold = !w.chars().any(char::is_uppercase);
        Word {
            pattern: if fold { w.to_lowercase() } else { w.into() },
            glob: w.contains(['*', '?']),
            fold,
        }
    }

    fn matches(&self, text: &str) -> bool {
        let owned;
        let t = if self.fold {
            owned = text.to_lowercase();
            owned.as_str()
        } else {
            text
        };
        if self.glob {
            glob(self.pattern.as_bytes(), t.as_bytes())
        } else {
            t.contains(&self.pattern)
        }
    }
}

/// Whole-string glob with `*` and `?` (the syntax of `--dont-use`).
pub fn glob(p: &[u8], t: &[u8]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// A parsed filter: every term must hold.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    terms: Vec<Term>,
}

impl Filter {
    /// Parses `text` against the columns of `m`; errors list the known keys.
    pub fn parse(text: &str, m: &dyn TableModel) -> Result<Filter, String> {
        let mut terms = Vec::new();
        for t in text.split_whitespace() {
            let ops = [
                ("<=", Op::Le),
                (">=", Op::Ge),
                ("<", Op::Lt),
                (">", Op::Gt),
                ("=", Op::Eq),
            ];
            let found = ops
                .iter()
                .filter_map(|(s, op)| t.find(s).map(|i| (i, *s, *op)))
                .min_by_key(|x| (x.0, std::cmp::Reverse(x.1.len())));
            match found {
                Some((i, s, op)) if i > 0 => {
                    let key = &t[..i];
                    let rhs = &t[i + s.len()..];
                    let col = m
                        .column(key)
                        .ok_or_else(|| format!("unknown key '{key}'; keys: {}", keys(m)))?;
                    match rhs.parse::<f64>() {
                        Ok(v) if v.is_finite() => terms.push(Term::Num { col, op, v }),
                        _ if op == Op::Eq => terms.push(Term::Word {
                            col,
                            word: Word::new(rhs),
                        }),
                        _ => return Err(format!("'{rhs}' is not a number in '{t}'")),
                    }
                }
                _ => terms.push(Term::Word {
                    col: m.name_column(),
                    word: Word::new(t),
                }),
            }
        }
        Ok(Filter { terms })
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether row `r` of `m` satisfies every term.
    pub fn matches(&self, m: &dyn TableModel, r: usize) -> bool {
        self.terms.iter().all(|t| match t {
            Term::Num { col, op, v } => match m.value(r, *col) {
                Value::Num(x) if !x.is_nan() => match op {
                    Op::Lt => x < *v,
                    Op::Le => x <= *v,
                    Op::Gt => x > *v,
                    Op::Ge => x >= *v,
                    Op::Eq => x == *v,
                },
                _ => false,
            },
            Term::Word { col, word } => match m.value(r, *col) {
                Value::Text(s) => word.matches(s),
                Value::Num(x) if !x.is_nan() => word.matches(&format!("{x}")),
                _ => false,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::table::PhaseRows;

    // PROTO-REQ-0131
    #[test]
    fn filters() {
        let m = PhaseRows(vec![
            ("read".into(), 0.5),
            ("Graph".into(), 2.0),
            ("setup".into(), f64::NAN),
        ]);
        let rows = |f: &str| -> Vec<usize> {
            let f = Filter::parse(f, &m).unwrap();
            (0..m.len()).filter(|&r| f.matches(&m, r)).collect()
        };
        assert_eq!(rows(""), vec![0, 1, 2]);
        assert_eq!(rows("seconds<1"), vec![0]);
        assert_eq!(rows("seconds>=0.5"), vec![0, 1]);
        assert_eq!(rows("graph"), vec![1]);
        assert_eq!(rows("Graph"), vec![1]);
        assert_eq!(rows("graPH"), Vec::<usize>::new());
        assert_eq!(rows("?e*"), vec![0, 2]);
        assert_eq!(rows("s*p"), vec![2]);
        assert_eq!(rows("phase=read"), vec![0]);
        assert!(
            Filter::parse("bogus<1", &m)
                .unwrap_err()
                .contains("keys: phase, seconds")
        );
        assert!(Filter::parse("seconds<x", &m).is_err());
        assert!(glob(b"u_*/*_reg*/D", b"u_core/acc_reg_31/D"));
        assert!(!glob(b"a?c", b"abbc"));
    }
}
