// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Exact two-cell decomposition of a small truth table, derived by cofactors.

use super::model::Model;

#[derive(Debug, Clone)]
pub(super) struct Recipe {
    pub inner: usize,
    pub root: usize,
    pub inner_inputs: Vec<usize>,
    /// Boundary variable index, or 6 for the newly created inner output.
    pub root_inputs: Vec<usize>,
    pub area: f64,
}

pub(super) fn find(lib: &Model<'_>, n: usize, truth: u64) -> Option<Recipe> {
    if n < 3 {
        return None;
    }
    let mut best: Option<Recipe> = None;
    for bound_mask in 1u32..(1u32 << n) - 1 {
        let b = bound_mask.count_ones() as usize;
        if b < 2 {
            continue;
        }
        let mut bound = [0usize; 6];
        let mut free = [0usize; 6];
        let mut bi = 0;
        let mut fi = 0;
        for i in 0..n {
            if bound_mask & (1 << i) != 0 {
                bound[bi] = i;
                bi += 1;
            } else {
                free[fi] = i;
                fi += 1;
            }
        }
        // Deposit packed assignment bits once for this partition. Every cofactor bit
        // then needs only an OR and a truth-table shift, without per-variable folding.
        let expand = |vars: &[usize]| {
            let mut values = [0usize; 32];
            for x in 1usize..1usize << vars.len() {
                values[x] = values[x & (x - 1)] | (1 << vars[x.trailing_zeros() as usize]);
            }
            values
        };
        let bx = expand(&bound[..bi]);
        let fx = expand(&free[..fi]);
        let full = (1u64 << (1 << b)) - 1;
        let mut cofactors = [0u64; 32];
        let mut h = None;
        let mut valid = true;
        for outer in 0..1usize << fi {
            let mut cofactor = 0u64;
            for inner in 0..1usize << bi {
                cofactor |= ((truth >> (fx[outer] | bx[inner])) & 1) << inner;
            }
            cofactors[outer] = cofactor;
            if cofactor != 0 && cofactor != full {
                if let Some(first) = h {
                    if cofactor != first && cofactor != (first ^ full) {
                        valid = false;
                        break;
                    }
                } else {
                    if !lib.matching.contains_key(&(b, cofactor))
                        && !lib.matching.contains_key(&(b, cofactor ^ full))
                    {
                        valid = false;
                        break;
                    }
                    h = Some(cofactor);
                }
            }
        }
        if !valid {
            continue;
        }
        let Some(h) = h else {
            continue;
        };
        for inverted in [false, true] {
            let inner_truth = if inverted { h ^ full } else { h };
            let Some(&(inner, inner_perm)) =
                lib.matching.get(&(b, inner_truth)).and_then(|v| v.first())
            else {
                continue;
            };
            let mut root_truth = 0u64;
            for (outer, &cofactor) in cofactors[..1usize << fi].iter().enumerate() {
                for inner in 0..2 {
                    let value = if cofactor == 0 {
                        false
                    } else if cofactor == full {
                        true
                    } else {
                        (inner != 0) ^ inverted ^ (cofactor != h)
                    };
                    root_truth |= u64::from(value) << ((outer << 1) | inner);
                }
            }
            let rn = fi + 1;
            let Some(&(root, root_perm)) =
                lib.matching.get(&(rn, root_truth)).and_then(|v| v.first())
            else {
                continue;
            };
            let area = lib.cells[inner].raw.area + lib.cells[root].raw.area;
            if best.as_ref().is_some_and(|old| area >= old.area - 1e-8) {
                continue;
            }
            best = Some(Recipe {
                inner,
                root,
                area,
                inner_inputs: inner_perm[..b].iter().map(|&p| bound[p as usize]).collect(),
                root_inputs: root_perm[..rn]
                    .iter()
                    .map(|&p| if p == 0 { 6 } else { free[p as usize - 1] })
                    .collect(),
            });
        }
    }
    best
}
