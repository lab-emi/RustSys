// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The kernel words of a cut-pass problem (COMPUTE-REQ-0103), shared by every backend. All
//! arrays are `u32`:
//! - `nodes`: fanin literals of every node (2 per node), then the AND nodes in level order from
//!   `ord_off`;
//! - `cuts`: cut count of every node, then `slots` rows of `CW` words per node: six leaves, leaf
//!   count, truth table (low, high word), leaf signature (low, high word);
//! - `best`: per node and phase: implementation code, biased arrival, area flow (3 words);
//! - `lib`: match keys (vars, table high, table low, first match, match count) ascending, matches
//!   (cell, packed permutation and phase) from `moff`, cells (inputs, area, six delays) from
//!   `coff`;
//! - `costs` (per pass): half fanouts of every node, then biased required times (2 per node).
//!
//! Arrivals and required times are stored biased (`x ^ 0x8000_0000` of the `i32`), which keeps
//! their order under unsigned comparison: kernels use 32-bit unsigned integers only.

use super::{Choice, CutPassProblem, CutRow, Imp, PassCosts, Rank};
use crate::ComputeError;

/// Threads per block of the cut kernel.
#[cfg_attr(not(feature = "cuda"), allow(dead_code))]
pub(super) const BLOCK: u32 = 128;
/// Words per cut row.
pub(super) const CW: usize = 11;
/// Words per candidate record: pair index, leaf count, six leaves, truth table (2), arrival key,
/// area-flow key.
pub(super) const SW: usize = 12;
/// Largest scratch array per launch.
const SCRATCH_BUDGET: u64 = 256 << 20;
/// Kernels index every array with 32-bit words.
pub(super) const MAX_WORDS: u64 = u32::MAX as u64;

/// Order-preserving map of an arrival or required time to `u32`.
pub(super) fn bias(x: i32) -> u32 {
    (x as u32) ^ 0x8000_0000
}

pub(super) fn unbias(x: u32) -> i32 {
    (x ^ 0x8000_0000) as i32
}

/// Everything a backend needs, packed once per session.
#[derive(Debug)]
#[cfg_attr(not(feature = "cuda"), allow(dead_code))]
pub(super) struct Packed {
    pub n: usize,
    pub n32: u32,
    pub nodes: Vec<u32>,
    pub ord_off: u32,
    /// (first index into the level order, node count) of every level.
    pub levels: Vec<(u32, u32)>,
    pub cuts: Vec<u32>,
    pub best: Vec<u32>,
    pub lib: Vec<u32>,
    pub nkeys: u32,
    pub moff: u32,
    pub coff: u32,
    pub slots: u32,
    pub max_cuts: u32,
    pub kmax: u32,
    pub d_inv: u32,
    pub a_inv: u32,
    pub scr_words: usize,
    pub scr_words32: u32,
    /// Largest number of blocks per launch (scratch budget, grid axis, widest level).
    pub chunk: usize,
}

/// Packs a validated problem; `max_bytes` is the largest device array.
pub(super) fn pack(p: &CutPassProblem<'_>, max_bytes: u64) -> Result<Packed, ComputeError> {
    let n = p.fanins.len();
    let slots = p.max_cuts as usize + 1;
    let mut nodes = Vec::with_capacity(2 * n + p.level_nodes.len());
    for f in p.fanins {
        nodes.extend_from_slice(f);
    }
    let ord_off = nodes.len() as u32;
    nodes.extend_from_slice(p.level_nodes);
    let levels: Vec<(u32, u32)> = p
        .level_ptr
        .windows(2)
        .map(|w| (w[0], w[1] - w[0]))
        .collect();
    let row_words = (n as u64) * (slots * CW) as u64;
    let total = row_words + n as u64;
    if total >= MAX_WORDS || total * 4 > max_bytes {
        return Err(ComputeError::TooLarge(format!(
            "{n} nodes with {slots} cuts each exceed 32-bit device indices or the device buffer limit"
        )));
    }
    let mut cuts = vec![0u32; total as usize];
    for &id in p.inputs {
        cuts[id as usize] = 1;
        let r = n + id as usize * slots * CW;
        cuts[r] = id;
        cuts[r + 6] = 1;
        cuts[r + 7] = 0xAAAA_AAAA;
        cuts[r + 8] = 0xAAAA_AAAA;
        let m = id % 64;
        cuts[r + 9] = if m < 32 { 1 << m } else { 0 };
        cuts[r + 10] = if m < 32 { 0 } else { 1 << (m - 32) };
    }
    let mut best = Vec::with_capacity(6 * n);
    for c in p.init {
        for x in c {
            best.extend([x.imp.0, bias(x.arr), x.af]);
        }
    }
    let t = &p.lib;
    let keys = t.key_vars.len();
    let mut lib = Vec::with_capacity(keys * 5 + t.m_cell.len() * 2 + t.c_inputs.len() * 8);
    for i in 0..keys {
        let tt = t.key_tt[i];
        lib.extend([
            u32::from(t.key_vars[i]),
            (tt >> 32) as u32,
            tt as u32,
            t.key_first[i],
            t.key_first[i + 1] - t.key_first[i],
        ]);
    }
    let moff = lib.len() as u32;
    for m in 0..t.m_cell.len() {
        let mut pp = 0u32;
        for (i, x) in t.m_perm[m].iter().enumerate() {
            pp |= u32::from(*x & 15) << (4 * i);
        }
        lib.extend([t.m_cell[m], pp | (u32::from(t.m_phase[m]) << 24)]);
    }
    let coff = lib.len() as u32;
    for c in 0..t.c_inputs.len() {
        let mut rec = [0u32; 8];
        rec[0] = u32::from(t.c_inputs[c]);
        rec[1] = t.c_area[c];
        for i in 0..rec[0] as usize {
            rec[2 + i] = t.c_delay[c][i] as u32;
        }
        lib.extend(rec);
    }
    // Candidates: every pair of fanin cuts plus the two previous cuts.
    let pcap = slots * slots + 2;
    let scr_words = pcap * (SW + 3) + slots * 8;
    let budget = SCRATCH_BUDGET.min(max_bytes);
    let widest = levels
        .iter()
        .map(|l| l.1 as usize)
        .max()
        .unwrap_or(1)
        .max(1);
    let chunk = ((budget / (scr_words as u64 * 4)) as usize)
        .clamp(1, 65_535)
        .min(widest);
    Ok(Packed {
        n,
        n32: n as u32,
        nodes,
        ord_off,
        levels,
        cuts,
        best,
        lib,
        nkeys: keys as u32,
        moff,
        coff,
        slots: slots as u32,
        max_cuts: p.max_cuts,
        kmax: p.max_leaves,
        d_inv: p.inv_delay as u32,
        a_inv: p.inv_area,
        scr_words,
        scr_words32: scr_words as u32,
        chunk,
    })
}

/// The cost words of a pass and the kernel mode (bit 0: area ranking, bit 1: required times).
pub(super) fn costs(n: usize, c: &PassCosts<'_>) -> (Vec<u32>, u32) {
    let mut w = vec![0u32; 3 * n];
    w[..n].copy_from_slice(c.fo2);
    if let Some(req) = c.required {
        for (id, r) in req.iter().enumerate() {
            w[n + 2 * id] = bias(r[0]);
            w[n + 2 * id + 1] = bias(r[1]);
        }
    }
    let mode = u32::from(c.rank == Rank::AreaFlow) | (u32::from(c.required.is_some()) << 1);
    (w, mode)
}

/// A choice from its three words.
pub(super) fn choice(w: &[u32]) -> Choice {
    Choice {
        imp: Imp(w[0]),
        arr: unbias(w[1]),
        af: w[2],
    }
}

/// A cut from its `CW` words.
pub(super) fn row(r: &[u32]) -> CutRow {
    let mut leaves = [0u32; 6];
    leaves.copy_from_slice(&r[..6]);
    CutRow {
        leaves,
        n: r[6],
        tt: u64::from(r[7]) | (u64::from(r[8]) << 32),
        sign: u64::from(r[9]) | (u64::from(r[10]) << 32),
    }
}

/// Fills `out` from the choice words and either the chosen rows (`2 n` rows of `CW` words) or
/// the whole cut table.
pub(super) fn read_into(
    n: usize,
    slots: u32,
    best: &[u32],
    rows: &[u32],
    counts: Option<&[u32]>,
    out: &mut super::PassOutput,
) {
    out.slots = slots;
    out.choices.clear();
    out.choices
        .extend((0..n).map(|id| [choice(&best[6 * id..]), choice(&best[6 * id + 3..])]));
    out.rows.clear();
    out.rows.extend(rows.chunks_exact(CW).map(row));
    out.cut_count.clear();
    if let Some(c) = counts {
        out.cut_count.extend_from_slice(c);
    }
}
