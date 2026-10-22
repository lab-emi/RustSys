// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! k-feasible cuts with truth tables (SYNTH-REQ-0510) [@cong1999cut].
//!
//! A cut's truth table is a 64-bit function of six variables where variable i is leaf i;
//! variables at or above the leaf count are don't-cares (the table does not depend on them).

use crate::aig::Lit;

/// Truth-table mask of variable j over six variables.
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

/// Swaps adjacent variables k and k+1.
pub fn swap_adjacent(t: u64, k: usize) -> u64 {
    const P: [(u64, u64, u32); 5] = [
        (0x9999_9999_9999_9999, 0x2222_2222_2222_2222, 1),
        (0xC3C3_C3C3_C3C3_C3C3, 0x0C0C_0C0C_0C0C_0C0C, 2),
        (0xF00F_F00F_F00F_F00F, 0x00F0_00F0_00F0_00F0, 4),
        (0xFF00_00FF_FF00_00FF, 0x0000_FF00_0000_FF00, 8),
        (0xFFFF_0000_0000_FFFF, 0x0000_0000_FFFF_0000, 16),
    ];
    let (keep, up, s) = P[k];
    (t & keep) | ((t & up) << s) | ((t >> s) & up)
}

/// Moves variable `from` to position `to` (to >= from), shifting the variables in between down.
fn move_up(mut t: u64, from: usize, to: usize) -> u64 {
    for k in from..to {
        t = swap_adjacent(t, k);
    }
    t
}

/// True if the function depends on variable i.
pub fn depends(t: u64, i: usize) -> bool {
    let s = 1u32 << i;
    ((t >> s) ^ t) & !var_mask(i) != 0
}

/// A cut: sorted leaf node ids and the root function over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    pub leaves: [u32; 6],
    pub n: u8,
    pub tt: u64,
    pub sign: u64,
}

impl Cut {
    pub fn trivial(node: u32) -> Cut {
        let mut leaves = [0u32; 6];
        leaves[0] = node;
        Cut {
            leaves,
            n: 1,
            tt: var_mask(0),
            sign: 1u64 << (node % 64),
        }
    }

    /// Canonical fixed-size key; reduction may leave unused trailing leaf slots dirty.
    pub fn leaf_key(&self) -> (u8, [u32; 6]) {
        let mut leaves = [0; 6];
        leaves[..self.n as usize].copy_from_slice(self.leaves());
        (self.n, leaves)
    }

    pub fn leaves(&self) -> &[u32] {
        &self.leaves[..self.n as usize]
    }

    pub fn is_trivial_of(&self, node: u32) -> bool {
        self.n == 1 && self.leaves[0] == node && self.tt == var_mask(0)
    }

    /// Re-expresses the function over a superset of leaves at known positions.
    #[inline]
    fn stretch_at(&self, pos: &[usize; 6]) -> u64 {
        if self.n == 1 && self.tt == var_mask(0) {
            return var_mask(pos[0]);
        }
        let mut t = self.tt;
        // Move variables from the highest down so the destination slots are free.
        for i in (0..self.n as usize).rev() {
            t = move_up(t, i, pos[i]);
        }
        t
    }

    /// Merges two fanin cuts of an AND node with fanin literals `l0`, `l1`.
    pub fn merge(a: &Cut, l0: Lit, b: &Cut, l1: Lit, k: usize) -> Option<Cut> {
        if ((a.sign | b.sign).count_ones() as usize) > k {
            return None;
        }
        let mut u = [0u32; 6];
        let mut pos_a = [0usize; 6];
        let mut pos_b = [0usize; 6];
        let (mut i, mut j, mut n) = (0usize, 0usize, 0usize);
        let (la, lb) = (a.leaves(), b.leaves());
        while i < la.len() || j < lb.len() {
            if n >= k {
                return None;
            }
            let x = if j >= lb.len() || (i < la.len() && la[i] < lb[j]) {
                pos_a[i] = n;
                i += 1;
                la[i - 1]
            } else if i >= la.len() || lb[j] < la[i] {
                pos_b[j] = n;
                j += 1;
                lb[j - 1]
            } else {
                pos_a[i] = n;
                pos_b[j] = n;
                i += 1;
                j += 1;
                la[i - 1]
            };
            u[n] = x;
            n += 1;
        }
        let ta = a.stretch_at(&pos_a) ^ if l0.is_compl() { u64::MAX } else { 0 };
        let tb = b.stretch_at(&pos_b) ^ if l1.is_compl() { u64::MAX } else { 0 };
        let mut c = Cut {
            leaves: u,
            n: n as u8,
            tt: ta & tb,
            sign: a.sign | b.sign,
        };
        c.reduce();
        Some(c)
    }

    /// Removes leaves the function does not depend on.
    pub fn reduce(&mut self) {
        let mut i = 0usize;
        let mut removed = false;
        while i < self.n as usize {
            if depends(self.tt, i) {
                i += 1;
                continue;
            }
            let n = self.n as usize;
            self.tt = move_up(self.tt, i, n - 1);
            for k in i..n - 1 {
                self.leaves[k] = self.leaves[k + 1];
            }
            self.leaves[n - 1] = 0;
            self.n -= 1;
            removed = true;
        }
        if removed || self.sign == 0 {
            self.sign = self.leaves().iter().fold(0u64, |s, l| s | 1u64 << (l % 64));
        }
    }

    /// True if every leaf of `self` is a leaf of `other`.
    pub fn subset_of(&self, other: &Cut) -> bool {
        if self.sign & !other.sign != 0 {
            return false;
        }
        self.leaves().iter().all(|l| other.leaves().contains(l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tt_eval(t: u64, m: u32) -> bool {
        (t >> m) & 1 == 1
    }

    #[test]
    fn swaps_and_moves() {
        // f = x0 & !x1 over vars; swapping 0 and 1 gives x1 & !x0.
        let f = var_mask(0) & !var_mask(1);
        let g = swap_adjacent(f, 0);
        assert_eq!(g, var_mask(1) & !var_mask(0));
        for k in 0..5 {
            let h = swap_adjacent(var_mask(k), k);
            assert_eq!(h, var_mask(k + 1));
            assert_eq!(swap_adjacent(h, k), var_mask(k));
        }
        assert_eq!(move_up(var_mask(1), 1, 4), var_mask(4));
    }

    #[test]
    fn merge_and_reduce() {
        // a = x & y (nodes 1, 2), b = y & z (nodes 2, 3); root = a & !b over {1,2,3}.
        let mut ca = Cut {
            leaves: [1, 2, 0, 0, 0, 0],
            n: 2,
            tt: var_mask(0) & var_mask(1),
            sign: 0,
        };
        ca.reduce();
        let mut cb = Cut {
            leaves: [2, 3, 0, 0, 0, 0],
            n: 2,
            tt: var_mask(0) & var_mask(1),
            sign: 0,
        };
        cb.reduce();
        let c = Cut::merge(&ca, Lit(10), &cb, Lit(13), 6).unwrap();
        assert_eq!(c.leaves(), &[1, 2, 3]);
        for m in 0..8u32 {
            let (x, y, z) = (m & 1 == 1, m & 2 != 0, m & 4 != 0);
            assert_eq!(tt_eval(c.tt, m), x && y && !(y && z));
        }
        // x & !x-free function: (x & y) & !(y & z) does depend on all three.
        let mut d = Cut {
            leaves: [1, 2, 3, 0, 0, 0],
            n: 3,
            tt: var_mask(0) & var_mask(2),
            sign: 0,
        };
        d.reduce();
        assert_eq!(d.leaves(), &[1, 3]);
        assert_eq!(d.tt & 0xF, 0b1000);
        assert!(Cut::merge(&c, Lit(2), &c, Lit(2), 2).is_none());
    }
}
