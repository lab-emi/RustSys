// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Ranking hints from the inputs and already corresponding state of a next-state cone.
//! These hints never establish a relation; simulation and SAT still validate each proposal.

use super::{Corr, Hyp, Relation, Setting};
use crate::circ::{B, Def};
use std::collections::HashMap;

type Sketch = [u32; 4];
const EMPTY: Sketch = [u32::MAX; 4];

fn fingerprint(value: u32) -> Sketch {
    std::array::from_fn(|lane| {
        let mut x = u64::from(value) ^ (0x9e37_79b9_7f4a_7c15u64.wrapping_mul(lane as u64 + 1));
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((x ^ (x >> 31)) as u32).min(u32::MAX - 1)
    })
}

fn union(a: Sketch, b: Sketch) -> Sketch {
    std::array::from_fn(|i| a[i].min(b[i]))
}

/// Four deterministic MinHash lanes rank large candidate sets. Set union is
/// associative/idempotent, so reconvergence never materializes support paths.
/// Unpaired state is omitted only from this ranking hint; final CEC quantifies
/// it unchanged. No sketch value establishes or constrains a state relation.
pub(super) fn sketches(
    s: &Setting<'_>,
    corr: &Corr,
    rtl: &[usize],
    cells: &[usize],
    order: &[u32],
    max_bytes: usize,
) -> Option<(HashMap<usize, Sketch>, HashMap<usize, Sketch>)> {
    if s.c.defs.len().checked_mul(size_of::<Sketch>())? > max_bytes {
        return None;
    }
    let mut canonical: HashMap<u32, Option<u32>> = s
        .model
        .states
        .iter()
        .map(|st| (st.q, None))
        .chain(s.nm.seq.iter().map(|st| (st.iq, None)))
        .collect();
    for (si, pair) in corr.paired.iter().enumerate() {
        if let Some((ci, _)) = pair {
            let q = s.model.states[si].q;
            canonical.insert(q, Some(q));
            canonical.insert(s.nm.seq[*ci].iq, Some(q));
        }
    }
    let mut images = vec![EMPTY; s.c.defs.len()];
    for &v in order {
        images[v as usize] = match s.c.defs[v as usize] {
            Def::Free => match canonical.get(&v) {
                Some(Some(q)) => fingerprint(*q),
                Some(None) => EMPTY,
                None => fingerprint(v),
            },
            Def::Alias(B::F | B::T) => EMPTY,
            Def::Alias(B::V(x)) | Def::Not(x) => images[x as usize],
            Def::And(a, b) | Def::Or(a, b) | Def::Xor(a, b) => {
                union(images[a as usize], images[b as usize])
            }
            Def::Mux(c, a, b) => union(
                images[c as usize],
                union(images[a as usize], images[b as usize]),
            ),
        };
    }
    let image = |root: B| match root {
        B::V(v) => images[v as usize],
        B::F | B::T => EMPTY,
    };
    Some((
        rtl.iter()
            .map(|&si| (si, image(s.model.states[si].next)))
            .collect(),
        cells
            .iter()
            .map(|&ci| (ci, image(s.nm.seq[ci].next)))
            .collect(),
    ))
}

pub(super) fn sketch_similarity(a: Sketch, b: Sketch) -> f64 {
    if a == EMPTY && b == EMPTY {
        return 1.0;
    }
    a.iter().zip(b).filter(|(x, y)| **x == *y).count() as f64 / 4.0
}

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
