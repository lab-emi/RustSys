// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Filtered and sorted row orders (PROTO-REQ-0132).

use std::cmp::Ordering;

use rayon::prelude::*;
use rusteda_core::job::{Cancelled, Ctx};
use serde::{Deserialize, Serialize};

use crate::proto::filter::Filter;
use crate::proto::table::{TableModel, Value};

/// Sort by one column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortSpec {
    pub col: usize,
    pub descending: bool,
}

fn key_cmp(a: Value<'_>, b: Value<'_>, desc: bool) -> Ordering {
    let rank = |v: &Value<'_>| match v {
        Value::Num(x) if x.is_nan() => 1,
        Value::Missing => 1,
        _ => 0,
    };
    // Missing values last in both directions.
    match (rank(&a), rank(&b)) {
        (0, 1) => return Ordering::Less,
        (1, 0) => return Ordering::Greater,
        (1, 1) => return Ordering::Equal,
        _ => {}
    }
    let o = match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.total_cmp(&y),
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Num(_), Value::Text(_)) => Ordering::Less,
        (Value::Text(_), Value::Num(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    };
    if desc { o.reverse() } else { o }
}

/// Rows of `m` that pass `filter`, in `sort` order (ties in row order), checking for
/// cancellation every 65 536 rows of the filter pass.
pub fn build(
    m: &dyn TableModel,
    filter: &Filter,
    sort: Option<SortSpec>,
    ctx: Ctx<'_>,
) -> Result<Vec<u32>, Cancelled> {
    let n = m.len();
    let mut rows: Vec<u32> = Vec::with_capacity(if filter.is_empty() { n } else { 0 });
    for start in (0..n).step_by(65_536) {
        ctx.checkpoint()?;
        let end = (start + 65_536).min(n);
        if filter.is_empty() {
            rows.extend(start as u32..end as u32);
        } else {
            let part: Vec<u32> = (start..end)
                .into_par_iter()
                .filter(|&r| filter.matches(m, r))
                .map(|r| r as u32)
                .collect();
            rows.extend(part);
        }
    }
    if let Some(s) = sort {
        ctx.checkpoint()?;
        rows.par_sort_by(|&a, &b| {
            key_cmp(
                m.value(a as usize, s.col),
                m.value(b as usize, s.col),
                s.descending,
            )
            .then(a.cmp(&b))
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::table::PhaseRows;

    // PROTO-REQ-0132
    #[test]
    fn missing_last_and_stable() {
        let m = PhaseRows(vec![
            ("a".into(), 2.0),
            ("b".into(), f64::NAN),
            ("c".into(), 1.0),
            ("d".into(), 2.0),
        ]);
        let f = Filter::default();
        let asc = build(
            &m,
            &f,
            Some(SortSpec {
                col: 1,
                descending: false,
            }),
            Ctx::NONE,
        )
        .unwrap();
        assert_eq!(asc, vec![2, 0, 3, 1]);
        let desc = build(
            &m,
            &f,
            Some(SortSpec {
                col: 1,
                descending: true,
            }),
            Ctx::NONE,
        )
        .unwrap();
        assert_eq!(desc, vec![0, 3, 2, 1]);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let one = pool.install(|| {
            build(
                &m,
                &f,
                Some(SortSpec {
                    col: 1,
                    descending: true,
                }),
                Ctx::NONE,
            )
            .unwrap()
        });
        assert_eq!(one, desc);
    }
}
