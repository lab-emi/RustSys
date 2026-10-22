// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Ranking hints from the inputs and already corresponding state of a next-state cone.
//! These hints never establish a relation; simulation and SAT still validate each proposal.

use super::{Corr, Hyp, Relation, Setting};
use std::collections::HashMap;

pub(super) fn signatures(
    s: &Setting<'_>,
    corr: &Corr,
    hyp: &Hyp,
    rtl: &[usize],
    cells: &[usize],
) -> (HashMap<usize, Vec<u32>>, HashMap<usize, Vec<u32>>) {
    let mut aliases: HashMap<u32, Option<u32>> = s
        .model
        .states
        .iter()
        .map(|st| (st.q, None))
        .chain(s.nm.seq.iter().map(|st| (st.iq, None)))
        .collect();
    for (si, pair) in corr.paired.iter().enumerate() {
        if let Some((ci, _)) = pair {
            let q = s.model.states[si].q;
            aliases.insert(q, Some(q));
            aliases.insert(s.nm.seq[*ci].iq, Some(q));
        }
    }
    for &(si, ri, _) in &corr.equivalents {
        aliases.insert(s.model.states[si].q, Some(s.model.states[ri].q));
    }
    for (&si, rel) in hyp {
        let q = s.model.states[si].q;
        match *rel {
            Relation::Cell(ci, _) => {
                aliases.insert(q, Some(q));
                aliases.insert(s.nm.seq[ci].iq, Some(q));
            }
            Relation::Shares(ri, _) => {
                aliases.insert(q, Some(s.model.states[ri].q));
            }
            Relation::Constant(_) => {
                aliases.insert(q, None);
            }
        }
    }
    let canonical = |mut v: u32| {
        for _ in 0..=aliases.len() {
            match aliases.get(&v) {
                None => return Some(v), // Shared primary input, or another free boundary.
                Some(None) => return None,
                Some(Some(next)) if *next == v => return Some(v),
                Some(Some(next)) => v = *next,
            }
        }
        None
    };
    let signature = |root, own| {
        let mut values: Vec<_> =
            s.c.support(&[root])
                .unwrap_or_default()
                .into_iter()
                .filter(|v| *v != own)
                .filter_map(canonical)
                .collect();
        values.sort_unstable();
        values.dedup();
        values
    };
    (
        rtl.iter()
            .map(|&si| (si, signature(s.model.states[si].next, s.model.states[si].q)))
            .collect(),
        cells
            .iter()
            .map(|&ci| (ci, signature(s.nm.seq[ci].next, s.nm.seq[ci].iq)))
            .collect(),
    )
}

pub(super) fn similarity(a: &[u32], b: &[u32]) -> f64 {
    let (mut i, mut j, mut both) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let union = a.len() + b.len() - both;
    if union == 0 {
        1.0
    } else {
        both as f64 / union as f64
    }
}
