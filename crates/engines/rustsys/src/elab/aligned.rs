// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Aligned dynamic selects (SYNTH-REQ-0419). A select offset that depends on few index bits is
//! tabulated by evaluating its own logic for every value of those bits. When no two windows of
//! the select can overlap, the select is decoded from the index bits instead of shifted by the
//! offset.

use crate::aig::{Aig, Lit};
use crate::bits::{self, Bv};
use rustc_hash::FxHashSet;

/// Most index bits of a tabulated offset.
pub(crate) const MAX_INDEX_BITS: usize = 10;
/// Most AND nodes between an offset and its index bits.
pub(crate) const MAX_CONE: usize = 1 << 14;
/// Most literals of the windows of one select.
pub(crate) const MAX_WINDOW_LITS: usize = 1 << 22;

/// The windows of a dynamic select whose windows cannot overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Windows {
    /// The bits the offset depends on; bit `j` of an index value is `index[j]`.
    pub index: Vec<Lit>,
    /// Bit offset of the window of each index value; `None` if no bit of it is in range.
    pub offsets: Vec<Option<i64>>,
}

/// The inputs `off` depends on and its signed value for each of their assignments.
pub(crate) fn offset_table(aig: &Aig, off: &[Lit]) -> Option<(Vec<Lit>, Vec<i64>)> {
    if off.is_empty() || off.len() > 64 {
        return None;
    }
    let mut seen = FxHashSet::default();
    let mut stack: Vec<u32> = off.iter().map(|l| l.node()).collect();
    let mut ands = Vec::new();
    let mut index = Vec::new();
    while let Some(n) = stack.pop() {
        if n == 0 || !seen.insert(n) {
            continue;
        }
        if aig.is_and(n) {
            if ands.len() == MAX_CONE {
                return None;
            }
            ands.push(n);
            let (a, b) = aig.fanins(n);
            stack.push(a.node());
            stack.push(b.node());
        } else {
            if index.len() == MAX_INDEX_BITS {
                return None;
            }
            index.push(n);
        }
    }
    // Node identifiers are a topological order.
    ands.sort_unstable();
    index.sort_unstable();
    let rows = 1usize << index.len();
    let words = rows.div_ceil(64);
    let mut slot = rustc_hash::FxHashMap::default();
    let mut sim = vec![0u64; (1 + index.len() + ands.len()) * words];
    for (j, n) in index.iter().enumerate() {
        slot.insert(*n, 1 + j);
        for (w, word) in sim[(1 + j) * words..(2 + j) * words].iter_mut().enumerate() {
            *word = if j < 6 {
                // Bit `r` of the word is set when bit `j` of row `r` is.
                [
                    0xAAAA_AAAA_AAAA_AAAA,
                    0xCCCC_CCCC_CCCC_CCCC,
                    0xF0F0_F0F0_F0F0_F0F0,
                    0xFF00_FF00_FF00_FF00,
                    0xFFFF_0000_FFFF_0000,
                    0xFFFF_FFFF_0000_0000u64,
                ][j]
            } else if (w >> (j - 6)) & 1 == 1 {
                u64::MAX
            } else {
                0
            };
        }
    }
    let value = |sim: &[u64], slot: &rustc_hash::FxHashMap<u32, usize>, l: Lit, w: usize| {
        let s = if l.node() == 0 { 0 } else { slot[&l.node()] };
        sim[s * words + w] ^ if l.is_compl() { u64::MAX } else { 0 }
    };
    for (k, n) in ands.iter().enumerate() {
        let s = 1 + index.len() + k;
        let (a, b) = aig.fanins(*n);
        for w in 0..words {
            sim[s * words + w] = value(&sim, &slot, a, w) & value(&sim, &slot, b, w);
        }
        slot.insert(*n, s);
    }
    let sign = off.len() - 1;
    let table = (0..rows)
        .map(|r| {
            let mut x = 0u64;
            for (i, l) in off.iter().enumerate() {
                x |= ((value(&sim, &slot, *l, r / 64) >> (r % 64)) & 1) << i;
            }
            // Offsets are signed: extend the top bit.
            if sign < 63 && (x >> sign) & 1 == 1 {
                x |= u64::MAX << sign;
            }
            x as i64
        })
        .collect();
    let index = index.into_iter().map(|n| Lit::new(n, false)).collect();
    Some((index, table))
}

/// The windows `[off * scale, off * scale + width)` of a select within `len` bits, if `off`
/// can be tabulated and no two different windows share a bit.
pub(crate) fn windows(
    aig: &Aig,
    off: &[Lit],
    scale: usize,
    width: usize,
    len: usize,
) -> Option<Windows> {
    if width == 0 || len == 0 {
        return None;
    }
    let (index, table) = offset_table(aig, off)?;
    if table.len().checked_mul(width)? > MAX_WINDOW_LITS {
        return None;
    }
    let offsets: Vec<Option<i64>> = table
        .iter()
        .map(|o| {
            let o = i128::from(*o) * scale as i128;
            (o < len as i128 && o + width as i128 > 0).then_some(o as i64)
        })
        .collect();
    let mut distinct: Vec<i64> = offsets.iter().flatten().copied().collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.windows(2).any(|p| p[1] - p[0] < width as i64) {
        return None;
    }
    Some(Windows { index, offsets })
}

/// True for exactly the index values marked in `rows`.
fn decode(aig: &mut Aig, index: &[Lit], rows: &[bool]) -> Lit {
    if rows.iter().all(|r| !*r) {
        return Lit::FALSE;
    }
    if rows.iter().all(|r| *r) {
        return Lit::TRUE;
    }
    let (low, high) = rows.split_at(rows.len() / 2);
    let j = index.len() - 1;
    let l = decode(aig, &index[..j], low);
    let h = decode(aig, &index[..j], high);
    aig.mux(index[j], h, l)
}

impl Windows {
    /// The different window offsets, each with the condition that selects it.
    fn enables(&self, aig: &mut Aig) -> Vec<(i64, Lit)> {
        let mut distinct: Vec<i64> = self.offsets.iter().flatten().copied().collect();
        distinct.sort_unstable();
        distinct.dedup();
        distinct
            .into_iter()
            .map(|o| {
                let rows: Vec<bool> = self.offsets.iter().map(|x| *x == Some(o)).collect();
                (o, decode(aig, &self.index, &rows))
            })
            .collect()
    }

    /// The window of `width` bits of `v` for each index value; bits out of range are zero.
    /// `None` if two index values select the same window.
    pub(crate) fn chunks(&self, v: &[Lit], width: usize) -> Option<Vec<Bv>> {
        let mut used = FxHashSet::default();
        if !self.offsets.iter().flatten().all(|o| used.insert(*o)) {
            return None;
        }
        let bit = |o: i64, b: usize| {
            usize::try_from(o + b as i64)
                .ok()
                .and_then(|i| v.get(i))
                .copied()
                .unwrap_or(Lit::FALSE)
        };
        let mut chunks: Vec<Bv> = self
            .offsets
            .iter()
            .map(|o| match o {
                Some(o) => (0..width).map(|b| bit(*o, b)).collect(),
                None => vec![Lit::FALSE; width],
            })
            .collect();
        // Positions past the last chunk read zero.
        while chunks.len() > 1
            && chunks
                .last()
                .is_some_and(|c| c.iter().all(|l| *l == Lit::FALSE))
        {
            chunks.pop();
        }
        Some(chunks)
    }

    /// The selected window of `width` bits of `v`; bits out of range are zero.
    pub(crate) fn read(&self, aig: &mut Aig, v: &[Lit], width: usize) -> Bv {
        let enables = self.enables(aig);
        (0..width)
            .map(|b| {
                let terms: Vec<Lit> = enables
                    .iter()
                    .filter_map(|(o, en)| {
                        let i = usize::try_from(*o + b as i64).ok()?;
                        Some(aig.and(*en, *v.get(i)?))
                    })
                    .filter(|l| *l != Lit::FALSE)
                    .collect();
                bits::red_or(aig, &terms)
            })
            .collect()
    }

    /// For each of `len` target bits: whether the selected window of `width` bits covers it,
    /// and the bit of `v` written to it.
    pub(crate) fn write(&self, aig: &mut Aig, v: &[Lit], width: usize, len: usize) -> (Bv, Bv) {
        let mut mask = vec![Lit::FALSE; len];
        let mut value = vec![Lit::FALSE; len];
        for (o, en) in self.enables(aig) {
            for b in 0..width {
                let Ok(i) = usize::try_from(o + b as i64) else {
                    continue;
                };
                if i < len {
                    mask[i] = en;
                    value[i] = v.get(b).copied().unwrap_or(Lit::FALSE);
                }
            }
        }
        (mask, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(aig: &mut Aig, n: usize) -> Bv {
        (0..n).map(|_| aig.input()).collect()
    }

    /// `x * factor + add` in `w` bits, as elaboration builds an offset.
    fn affine(aig: &mut Aig, x: &[Lit], signed: bool, factor: u64, add: i64, w: usize) -> Bv {
        let x = bits::resize(x, w, signed);
        let product = bits::mul(aig, &x, &bits::constant(factor, w));
        bits::add_const(aig, &product, add)
    }

    /// Input words that enumerate every value of `n <= 6` inputs.
    fn rows(n: usize) -> Vec<u64> {
        (0..n)
            .map(|j| {
                (0..64u64)
                    .filter(|r| (r >> j) & 1 == 1)
                    .fold(0, |a, r| a | 1 << r)
            })
            .collect()
    }

    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    // SYNTH-REQ-0419: the table is the offset's own function, signed.
    #[test]
    fn offsets_are_tabulated_as_signed_values() {
        let mut aig = Aig::new();
        let x = inputs(&mut aig, 3);
        let off = affine(&mut aig, &x, false, 4, -2, 8);
        let (index, table) = offset_table(&aig, &off).unwrap();
        assert_eq!(index, x);
        assert_eq!(table, (0..8).map(|m| m * 4 - 2).collect::<Vec<i64>>());

        let off = affine(&mut aig, &x, true, 4, 0, 8);
        let (_, table) = offset_table(&aig, &off).unwrap();
        assert_eq!(table, [0, 4, 8, 12, -16, -12, -8, -4]);

        // An offset is as wide as it is: the top bit of three bits is the sign.
        let (_, table) = offset_table(&aig, &x).unwrap();
        assert_eq!(table, [0, 1, 2, 3, -4, -3, -2, -1]);

        // Constants have no index bits and one row, also at the full 64 bits.
        let (index, table) = offset_table(&aig, &bits::constant(-7i64 as u64, 64)).unwrap();
        assert!(index.is_empty());
        assert_eq!(table, [-7]);
        assert_eq!(offset_table(&aig, &bits::constant(5, 4)).unwrap().1, [5i64]);
        assert!(offset_table(&aig, &[]).is_none());
        assert!(offset_table(&aig, &bits::constant(0, 65)).is_none());
    }

    // Complemented index bits and index bits above the sixth (more than one word of rows).
    #[test]
    fn offsets_of_ten_index_bits_have_1024_rows() {
        let mut aig = Aig::new();
        let x = inputs(&mut aig, 10);
        let inverted: Bv = x.iter().map(|l| !*l).collect();
        let off = affine(&mut aig, &inverted, false, 3, 1, 14);
        let (index, table) = offset_table(&aig, &off).unwrap();
        assert_eq!(index, x);
        assert_eq!(
            table,
            (0..1024).map(|m| (1023 - m) * 3 + 1).collect::<Vec<i64>>()
        );
    }

    // The budgets: eleven index bits, and more AND nodes than `MAX_CONE`.
    #[test]
    fn offsets_beyond_the_budgets_are_not_tabulated() {
        let mut aig = Aig::new();
        let x = inputs(&mut aig, MAX_INDEX_BITS + 1);
        assert!(offset_table(&aig, &x[..MAX_INDEX_BITS]).is_some());
        assert!(offset_table(&aig, &x).is_none());

        // A chain over six inputs that structural hashing does not fold.
        let (mut a, mut b) = (x[0], x[1]);
        let mut at = Vec::new();
        for k in 0.. {
            let t = aig.and(x[k % 6], b);
            (a, b) = (b, aig.xor(a, t));
            at.push((aig.num_ands(), b));
            if aig.num_ands() > 2 * MAX_CONE {
                break;
            }
        }
        let small = at.iter().rev().find(|(n, _)| *n <= MAX_CONE).unwrap().1;
        let large = at.last().unwrap().1;
        assert!(aig.cone(&[large]).len() > MAX_CONE);
        assert!(offset_table(&aig, &[large, Lit::FALSE]).is_none());
        // Within the budget the table is the simulated function.
        let (index, table) = offset_table(&aig, &[small, Lit::FALSE]).unwrap();
        assert_eq!(index, x[..6]);
        let sim = aig.simulate(&rows(6));
        let expected: Vec<i64> = (0..64)
            .map(|r| ((Aig::lit_value(&sim, small) >> r) & 1) as i64)
            .collect();
        assert_eq!(table, expected);
    }

    // Windows that share a bit are not decoded; windows out of range are no windows.
    #[test]
    fn only_windows_that_cannot_overlap_are_decoded() {
        let mut aig = Aig::new();
        let x = inputs(&mut aig, 3);
        let off = bits::resize(&x, 5, false);
        assert!(windows(&aig, &off, 1, 4, 16).is_none());
        assert!(windows(&aig, &off, 3, 4, 32).is_none());
        let w = windows(&aig, &off, 4, 4, 16).unwrap();
        assert_eq!(w.index, x);
        assert_eq!(
            w.offsets,
            [Some(0), Some(4), Some(8), Some(12), None, None, None, None]
        );
        // A narrower window at the same stride, starting below the vector and ending above it.
        let off = affine(&mut aig, &x, false, 4, -6, 8);
        let w = windows(&aig, &off, 1, 3, 20).unwrap();
        assert_eq!(
            w.offsets,
            [
                None,
                Some(-2),
                Some(2),
                Some(6),
                Some(10),
                Some(14),
                Some(18),
                None
            ]
        );
        assert!(windows(&aig, &off, 1, 0, 20).is_none());
        assert!(windows(&aig, &off, 1, 3, 0).is_none());
        // Equal windows of different index values do not overlap each other.
        let either = aig.or(x[0], x[1]);
        let w = windows(&aig, &[either, Lit::FALSE], 8, 8, 64).unwrap();
        assert_eq!(w.index, x[..2]);
        assert_eq!(w.offsets, [Some(0), Some(8), Some(8), Some(8)]);
        // The window literals of one select are bounded.
        let off = bits::resize(&inputs(&mut aig, 10), 12, false);
        assert!(windows(&aig, &off, 1 << 13, 1 << 12, 1 << 23).is_some());
        assert!(windows(&aig, &off, 1 << 13, (1 << 12) + 1, 1 << 23).is_none());
    }

    /// The model of a select: target bit, selected window offset and bit within the window.
    fn covered(offset: Option<i64>, width: usize, bit: usize) -> Option<usize> {
        let b = bit as i64 - offset?;
        (0..width as i64).contains(&b).then_some(b as usize)
    }

    // Reads (by chunks and by enables) and writes against the definition, for tables with
    // repeated windows, windows partly out of range and index values without a window.
    #[test]
    fn decoded_reads_and_writes_follow_the_table() {
        let tables: [(&[Option<i64>], usize, usize); 5] = [
            (&[Some(0), Some(4), Some(8), Some(12)], 4, 16),
            // Bits between the windows belong to none, also next to a window that starts below.
            (&[Some(-2), Some(6), None, Some(13)], 4, 16),
            (
                &[
                    Some(8),
                    None,
                    Some(-2),
                    Some(8),
                    Some(13),
                    Some(2),
                    None,
                    Some(-2),
                ],
                4,
                15,
            ),
            (
                &[Some(6), Some(0), Some(3), None, Some(9), None, None, None],
                3,
                11,
            ),
            (&[Some(5)], 2, 9),
        ];
        let mut seed = 0x9E37_79B9_7F4A_7C15;
        for (offsets, width, len) in tables {
            let mut aig = Aig::new();
            let k = offsets.len().trailing_zeros() as usize;
            let index = inputs(&mut aig, k);
            let v = inputs(&mut aig, len);
            let data = inputs(&mut aig, width);
            let win = Windows {
                index: index.clone(),
                offsets: offsets.to_vec(),
            };
            let read = win.read(&mut aig, &v, width);
            let chunks = win.chunks(&v, width);
            let distinct: FxHashSet<i64> = offsets.iter().flatten().copied().collect();
            assert_eq!(
                chunks.is_some(),
                distinct.len() == offsets.iter().flatten().count()
            );
            let muxed =
                chunks.map(|c| super::super::select::lower_select(&mut aig, &c, &index, false));
            let (mask, value) = win.write(&mut aig, &data, width, len);
            for _ in 0..4 {
                let mut words = rows(k);
                words.extend((0..len + width).map(|_| xorshift(&mut seed)));
                let sim = aig.simulate(&words);
                let get = |l: Lit, r: usize| (Aig::lit_value(&sim, l) >> r) & 1 == 1;
                for r in 0..1usize << k {
                    for b in 0..width {
                        let expected = offsets[r]
                            .and_then(|o| usize::try_from(o + b as i64).ok())
                            .filter(|i| *i < len)
                            .is_some_and(|i| get(v[i], r));
                        assert_eq!(get(read[b], r), expected, "read {offsets:?} {r} {b}");
                        if let Some(m) = &muxed {
                            assert_eq!(get(m[b], r), expected, "mux {offsets:?} {r} {b}");
                        }
                    }
                    for i in 0..len {
                        let c = covered(offsets[r], width, i);
                        assert_eq!(get(mask[i], r), c.is_some(), "mask {offsets:?} {r} {i}");
                        if let Some(b) = c {
                            assert_eq!(get(value[i], r), get(data[b], r), "value {r} {i}");
                        }
                    }
                }
            }
        }
    }
}
