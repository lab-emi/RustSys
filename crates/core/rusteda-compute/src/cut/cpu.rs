// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! CPU backend of the cut-pass task (COMPUTE-REQ-0105): the CUDA kernel `node_cuts` of `cuda.rs`
//! executed on the host, phase by phase, on the same words. One block of the kernel is one call
//! of [`node_cuts`]; its phases run in the kernel's order, with sequential loops in place of the
//! threads of a block. Two phases use other algorithms for the same result: duplicates are found
//! with a hash table instead of comparing every earlier candidate (the first of equal leaf sets
//! stays), and ranks come from sorting by the candidate order instead of counting smaller
//! candidates (the order is total, so a candidate's rank is its sorted position). The nodes of a
//! level are computed in parallel from the state of the earlier levels and committed in level
//! order, so the result does not depend on the thread count.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::time::Instant;

use rayon::prelude::*;

use super::pack::{self, CW, Packed, SW};
use super::{
    CutPassBackend, CutPassProblem, CutPassSession, PassCosts, PassOutput, PassStats, Readback,
};
use crate::ComputeError;

pub(super) const INF_B: u32 = 0xFFFF_FFFF;
const ZERO_B: u32 = 0x8000_0000;
const AF_MAX: u32 = super::AF_MAX;
const NONE: u32 = 0xFFFF_FFFF;

/// The CPU backend.
#[derive(Debug)]
pub(super) struct CpuBackend;

pub(super) static CPU: CpuBackend = CpuBackend;

impl CutPassBackend for CpuBackend {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn open<'s>(
        &'s self,
        p: &CutPassProblem<'_>,
    ) -> Result<Box<dyn CutPassSession + 's>, ComputeError> {
        super::validate(p)?;
        let pk = pack::pack(p, pack::MAX_WORDS * 4)?;
        Ok(Box::new(CpuSession { pk }))
    }
}

/// The resident state: the kernel words.
#[derive(Debug)]
pub(super) struct CpuSession {
    pub(super) pk: Packed,
}

impl CutPassSession for CpuSession {
    fn run(
        &mut self,
        c: &PassCosts<'_>,
        rb: Readback,
        out: &mut PassOutput,
    ) -> Result<PassStats, ComputeError> {
        super::check_costs(self.pk.n, c)?;
        let t = Instant::now();
        let (costs, mode) = pack::costs(self.pk.n, c);
        let single = rayon::current_num_threads() == 1;
        let levels = self.pk.levels.clone();
        for &(start, count) in &levels {
            let from = (self.pk.ord_off + start) as usize;
            let ids: Vec<u32> = self.pk.nodes[from..from + count as usize].to_vec();
            let pk = &self.pk;
            let outs: Vec<NodeOut> = if single || ids.len() < 16 {
                ids.iter()
                    .map(|&id| node_cuts(pk, &costs, mode, id))
                    .collect()
            } else {
                ids.par_iter()
                    .with_min_len(4)
                    .map(|&id| node_cuts(pk, &costs, mode, id))
                    .collect()
            };
            for o in outs {
                commit(&mut self.pk, &o);
            }
        }
        let kernel_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let pk = &self.pk;
        let n = pk.n;
        match rb {
            Readback::All => pack::read_into(
                n,
                pk.slots,
                &pk.best,
                &pk.cuts[n..],
                Some(&pk.cuts[..n]),
                out,
            ),
            Readback::Chosen => {
                let rows = chosen_rows(pk);
                pack::read_into(n, pk.slots, &pk.best, &rows, None, out);
            }
        }
        Ok(PassStats {
            launches: levels.len() as u32,
            kernel_s,
            readback_s: t.elapsed().as_secs_f64(),
        })
    }
}

/// The kernel `chosen_rows`: the chosen cut row of every (node, phase), zeros unless a cell
/// implements it.
pub(super) fn chosen_rows(pk: &Packed) -> Vec<u32> {
    let n = pk.n;
    let slots = pk.slots as usize;
    let mut out = vec![0u32; 2 * n * CW];
    for i in 0..2 * n {
        let id = i >> 1;
        let code = pk.best[3 * i];
        if code & 7 == 4 {
            let c = ((code >> 3) & 0x7F) as usize;
            let r = n + (id * slots + c) * CW;
            out[i * CW..(i + 1) * CW].copy_from_slice(&pk.cuts[r..r + CW]);
        }
    }
    out
}

/// What one block writes: the node's cut count, its rows `0..count`, its six choice words.
#[derive(Debug)]
pub(super) struct NodeOut {
    node: u32,
    rows: Vec<[u32; CW]>,
    best: [u32; 6],
}

fn commit(pk: &mut Packed, o: &NodeOut) {
    let n = pk.n;
    let slots = pk.slots as usize;
    let id = o.node as usize;
    pk.cuts[id] = o.rows.len() as u32;
    for (s, r) in o.rows.iter().enumerate() {
        let at = n + (id * slots + s) * CW;
        pk.cuts[at..at + CW].copy_from_slice(r);
    }
    pk.best[6 * id..6 * id + 6].copy_from_slice(&o.best);
}

#[inline]
fn swap_adj(lo: &mut u32, hi: &mut u32, k: u32) {
    if k < 4 {
        let (keep, up) = match k {
            0 => (0x9999_9999u32, 0x2222_2222u32),
            1 => (0xC3C3_C3C3, 0x0C0C_0C0C),
            2 => (0xF00F_F00F, 0x00F0_00F0),
            _ => (0xFF00_00FF, 0x0000_FF00),
        };
        let s = 1u32 << k;
        *lo = (*lo & keep) | ((*lo & up) << s) | ((*lo >> s) & up);
        *hi = (*hi & keep) | ((*hi & up) << s) | ((*hi >> s) & up);
    } else {
        let nlo = (*lo & 0x0000_FFFF) | (*hi << 16);
        let nhi = (*lo >> 16) | (*hi & 0xFFFF_0000);
        *lo = nlo;
        *hi = nhi;
    }
}

#[inline]
fn depends(lo: u32, hi: u32, i: u32) -> bool {
    if i == 5 {
        return lo != hi;
    }
    let vm = match i {
        0 => 0xAAAA_AAAAu32,
        1 => 0xCCCC_CCCC,
        2 => 0xF0F0_F0F0,
        3 => 0xFF00_FF00,
        _ => 0xFFFF_0000,
    };
    let s = 1u32 << i;
    ((((lo >> s) ^ lo) & !vm) | (((hi >> s) ^ hi) & !vm)) != 0
}

#[inline]
fn add_arr(a: u32, d: u32) -> u32 {
    if a == INF_B {
        return INF_B;
    }
    if a > INF_B - 1 - d { INF_B - 1 } else { a + d }
}

#[inline]
fn af_add(a: u32, b: u32) -> u32 {
    let s = a + b;
    if s < AF_MAX { s } else { AF_MAX }
}

#[inline]
fn af_share(af: u32, fo2: u32) -> u32 {
    (af * 2) / if fo2 > 2 { fo2 } else { 2 }
}

#[inline]
fn better(aarr: u32, aaf: u32, barr: u32, baf: u32, limit: u32, area: bool) -> bool {
    if barr == INF_B {
        return aarr != INF_B;
    }
    if area {
        let (fa, fb) = (aarr <= limit, barr <= limit);
        if fa != fb {
            return fa;
        }
        if !fa {
            return aarr < barr;
        }
        if aaf != baf {
            return aaf < baf;
        }
        return aarr < barr;
    }
    if aarr != barr {
        return aarr < barr;
    }
    aaf < baf
}

/// The kernel's `merge` of two fanin cut rows into `out`; false when infeasible.
fn merge(a: &[u32], ca: bool, b: &[u32], cb: bool, k: u32, out: &mut [u32; CW]) -> bool {
    let (na, nb) = (a[6], b[6]);
    if (a[9] | b[9]).count_ones() + (a[10] | b[10]).count_ones() > k {
        return false;
    }
    let mut u = [0u32; 12];
    let (mut i, mut j, mut n) = (0u32, 0u32, 0u32);
    while i < na || j < nb {
        let x;
        if j >= nb || (i < na && a[i as usize] < b[j as usize]) {
            x = a[i as usize];
            i += 1;
        } else if i >= na || b[j as usize] < a[i as usize] {
            x = b[j as usize];
            j += 1;
        } else {
            x = a[i as usize];
            i += 1;
            j += 1;
        }
        u[n as usize] = x;
        n += 1;
        if n > k {
            return false;
        }
    }
    let (mut alo, mut ahi, mut blo, mut bhi) = (a[7], a[8], b[7], b[8]);
    for q in (0..na).rev() {
        let mut to = 0u32;
        while u[to as usize] != a[q as usize] {
            to += 1;
        }
        for s in q..to {
            swap_adj(&mut alo, &mut ahi, s);
        }
    }
    for q in (0..nb).rev() {
        let mut to = 0u32;
        while u[to as usize] != b[q as usize] {
            to += 1;
        }
        for s in q..to {
            swap_adj(&mut blo, &mut bhi, s);
        }
    }
    if ca {
        alo = !alo;
        ahi = !ahi;
    }
    if cb {
        blo = !blo;
        bhi = !bhi;
    }
    let (mut lo, mut hi) = (alo & blo, ahi & bhi);
    // Remove leaves the function does not depend on.
    let mut q = 0u32;
    while q < n {
        if depends(lo, hi, q) {
            q += 1;
            continue;
        }
        for s in q..5 {
            swap_adj(&mut lo, &mut hi, s);
        }
        for t in q..n.saturating_sub(1) {
            u[t as usize] = u[t as usize + 1];
        }
        n -= 1;
    }
    let (mut slo, mut shi) = (0u32, 0u32);
    for t in 0..6u32 {
        out[t as usize] = if t < n { u[t as usize] } else { 0 };
    }
    for t in 0..n {
        let m = u[t as usize] % 64;
        if m < 32 {
            slo |= 1 << m;
        } else {
            shi |= 1 << (m - 32);
        }
    }
    out[6] = n;
    out[7] = lo;
    out[8] = hi;
    out[9] = slo;
    out[10] = shi;
    true
}

/// A candidate record (`SW` words): pair index, leaf count, six leaves, truth table (low, high),
/// arrival key, area-flow key.
fn record(pk: &Packed, fo2: &[u32], p: u32, leaves: &[u32], n: u32, lo: u32, hi: u32) -> [u32; SW] {
    let mut x = [0u32; SW];
    x[0] = p;
    x[1] = n;
    x[2..8].copy_from_slice(&leaves[..6]);
    x[8] = lo;
    x[9] = hi;
    let mut karr = ZERO_B;
    let mut kaf = 0u32;
    for t in 0..n as usize {
        let l = leaves[t] as usize;
        let bl = &pk.best[6 * l..6 * l + 6];
        let m = bl[1].min(bl[4]);
        if m > karr {
            karr = m;
        }
        kaf = af_add(kaf, af_share(bl[2].min(bl[5]), fo2[l]));
    }
    x[10] = karr;
    x[11] = kaf;
    x
}

/// Hash of a leaf set (multiplicative, as the leaf ids are dense).
#[derive(Default)]
pub(super) struct LeafHasher(u64);

impl Hasher for LeafHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(*b)).wrapping_mul(0x51_7CC1_B727_220A);
        }
    }
    fn write_u32(&mut self, x: u32) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(x)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

pub(super) type LeafMap = HashMap<[u32; 7], u32, BuildHasherDefault<LeafHasher>>;

fn leaf_key(x: &[u32; SW]) -> [u32; 7] {
    let mut k = [0u32; 7];
    k[0] = x[1];
    k[1..1 + x[1] as usize].copy_from_slice(&x[2..2 + x[1] as usize]);
    k
}

/// Phases 2 and 3 of the kernel: the unique candidates (first of equal leaf sets, in candidate
/// order) and their ranks in the candidate order. Returns the unique candidate indices in rank
/// order and, per candidate, the index of the first candidate with its leaf set.
pub(super) fn unique_ranked(cand: &[[u32; SW]], area: bool) -> (Vec<u32>, LeafMap) {
    let mut first: LeafMap = HashMap::with_capacity_and_hasher(cand.len(), Default::default());
    let mut uniq = Vec::with_capacity(cand.len());
    for (i, x) in cand.iter().enumerate() {
        if let std::collections::hash_map::Entry::Vacant(e) = first.entry(leaf_key(x)) {
            e.insert(i as u32);
            uniq.push(i as u32);
        }
    }
    let key = |i: &u32| {
        let x = &cand[*i as usize];
        if area {
            (x[11], x[1], x[10], x[0])
        } else {
            (x[10], x[1], x[11], x[0])
        }
    };
    uniq.sort_unstable_by_key(key);
    (uniq, first)
}

/// One block of the kernel `node_cuts`: the cuts and choices of `node`.
pub(super) fn node_cuts(pk: &Packed, costs: &[u32], mode: u32, node: u32) -> NodeOut {
    let n = pk.n;
    let slots = pk.slots as usize;
    let area = mode & 1 != 0;
    let has_req = mode & 2 != 0;
    let fo2 = &costs[..n];
    let nd = node as usize;
    let (l0, l1) = (pk.nodes[2 * nd], pk.nodes[2 * nd + 1]);
    let (f0, f1) = ((l0 >> 1) as usize, (l1 >> 1) as usize);
    let (na, nb) = (pk.cuts[f0], pk.cuts[f1]);
    let pcount = na * nb;
    let row = |id: usize, s: usize| &pk.cuts[n + (id * slots + s) * CW..][..CW];
    // Phase 1: the feasible merges in pair order, then the previous choices' cuts.
    let mut cand: Vec<[u32; SW]> = Vec::with_capacity(pcount as usize + 2);
    let mut tmp = [0u32; CW];
    for p in 0..pcount {
        let (ia, ib) = ((p / nb) as usize, (p % nb) as usize);
        if !merge(
            row(f0, ia),
            l0 & 1 != 0,
            row(f1, ib),
            l1 & 1 != 0,
            pk.kmax,
            &mut tmp,
        ) {
            continue;
        }
        if tmp[6] == 0 {
            continue;
        }
        cand.push(record(pk, fo2, p, &tmp[..6], tmp[6], tmp[7], tmp[8]));
    }
    let mut sp = [[0u32; 7]; 2];
    for j in 0..2 {
        let code = pk.best[6 * nd + 3 * j];
        if has_req && costs[n + 2 * nd + j] != INF_B && code & 7 == 4 {
            let pr = row(nd, ((code >> 3) & 0x7F) as usize);
            cand.push(record(
                pk,
                fo2,
                pcount + j as u32,
                &pr[..6],
                pr[6],
                pr[7],
                pr[8],
            ));
            sp[j][0] = pr[6];
            sp[j][1..7].copy_from_slice(&pr[..6]);
        }
    }
    // Phases 2 and 3: unique candidates in rank order; ranks of the forced cuts.
    let (order, first) = unique_ranked(&cand, area);
    let mut rank_of = vec![NONE; cand.len()];
    for (r, &i) in order.iter().enumerate() {
        rank_of[i as usize] = r as u32;
    }
    let lookup = |k: &[u32; 7]| first.get(k).map_or(NONE, |&i| rank_of[i as usize]);
    let mut two = [0u32; 7];
    two[0] = 2;
    two[1] = f0.min(f1) as u32;
    two[2] = f0.max(f1) as u32;
    let mut sv = [NONE; 3];
    // A leaf set with repeated leaves never occurs among the candidates.
    if f0 != f1 {
        sv[0] = lookup(&two);
    }
    for j in 0..2 {
        if sp[j][0] != 0 {
            let mut k = [0u32; 7];
            k[0] = sp[j][0];
            k[1..1 + sp[j][0] as usize].copy_from_slice(&sp[j][1..1 + sp[j][0] as usize]);
            sv[1 + j] = lookup(&k);
        }
    }
    let k = order.len() as u32;
    let max_cuts = pk.max_cuts;
    let kept = k.min(max_cuts);
    let mut sfr = [0u32; 3];
    let mut nf = 0u32;
    for r in sv {
        if r == NONE || nf >= max_cuts {
            continue;
        }
        if !sfr[..nf as usize].contains(&r) {
            sfr[nf as usize] = r;
            nf += 1;
        }
    }
    let mut rows = vec![[0u32; CW]; kept as usize + 1];
    {
        let tr = &mut rows[kept as usize];
        tr[0] = node;
        tr[6] = 1;
        tr[7] = 0xAAAA_AAAA;
        tr[8] = 0xAAAA_AAAA;
        let m = node % 64;
        tr[9] = if m < 32 { 1 << m } else { 0 };
        tr[10] = if m < 32 { 0 } else { 1 << (m - 32) };
    }
    // Phase 4: the kept cuts at their slots.
    for (r, &i) in order.iter().enumerate() {
        let r = r as u32;
        let mut below = 0u32;
        let mut forced = false;
        for &q in &sfr[..nf as usize] {
            if q < r {
                below += 1;
            }
            if q == r {
                forced = true;
            }
        }
        let room = max_cuts - nf;
        let slot = if forced {
            let o = r - below;
            below + o.min(room)
        } else if r - below < room {
            r
        } else {
            continue;
        };
        let xi = &cand[i as usize];
        let rw = &mut rows[slot as usize];
        let nn = xi[1];
        rw[..6].copy_from_slice(&xi[2..8]);
        let (mut slo, mut shi) = (0u32, 0u32);
        for t in 0..nn as usize {
            let m = xi[2 + t] % 64;
            if m < 32 {
                slo |= 1 << m;
            } else {
                shi |= 1 << (m - 32);
            }
        }
        rw[6] = nn;
        rw[7] = xi[8];
        rw[8] = xi[9];
        rw[9] = slo;
        rw[10] = shi;
    }
    // Phase 5: the best match of every (kept cut, phase).
    let lib = &pk.lib;
    let mut res = vec![[NONE, INF_B, AF_MAX]; 2 * kept as usize];
    for q in 0..2 * kept as usize {
        let (ci, ph) = (q >> 1, q & 1);
        let rw = &rows[ci];
        let nn = rw[6];
        let (mut lo, mut hi) = (rw[7], rw[8]);
        if ph == 1 {
            lo = !lo;
            hi = !hi;
        }
        if nn < 6 {
            hi = 0;
            if nn < 5 {
                lo &= (1u32 << (1u32 << nn)) - 1;
            }
        }
        let (mut a, mut b) = (0u32, pk.nkeys);
        while a < b {
            let mid = (a + b) >> 1;
            let kk = &lib[mid as usize * 5..];
            let lt = kk[0] < nn || (kk[0] == nn && (kk[1] < hi || (kk[1] == hi && kk[2] < lo)));
            if lt {
                a = mid + 1;
            } else {
                b = mid;
            }
        }
        let (mut barr, mut baf, mut bm) = (INF_B, AF_MAX, NONE);
        let limit = if has_req {
            costs[n + 2 * nd + ph]
        } else {
            INF_B
        };
        let ka = a as usize * 5;
        if a < pk.nkeys && lib[ka] == nn && lib[ka + 1] == hi && lib[ka + 2] == lo {
            let (ms, mc) = (lib[ka + 3], lib[ka + 4]);
            for mi in ms..ms + mc {
                let cell = lib[(pk.moff + 2 * mi) as usize];
                let pp = lib[(pk.moff + 2 * mi + 1) as usize];
                let ce = &lib[(pk.coff + 8 * cell) as usize..][..8];
                let nin = ce[0];
                let mut arr = ZERO_B;
                let mut af = ce[1];
                let mut ok = true;
                for t in 0..nin {
                    let leaf = rw[((pp >> (4 * t)) & 15) as usize] as usize;
                    let lph = ((pp >> (24 + t)) & 1) as usize;
                    let la = pk.best[6 * leaf + 3 * lph + 1];
                    if la == INF_B {
                        ok = false;
                        break;
                    }
                    let v = add_arr(la, ce[2 + t as usize]);
                    if v > arr {
                        arr = v;
                    }
                    af = af_add(af, af_share(pk.best[6 * leaf + 3 * lph + 2], fo2[leaf]));
                }
                if !ok {
                    continue;
                }
                if better(arr, af, barr, baf, limit, area) {
                    barr = arr;
                    baf = af;
                    bm = mi;
                }
            }
        }
        res[q] = [bm, barr, baf];
    }
    // Phase 6: the first best over the cuts per phase, then the inverters.
    let mut sarr = [INF_B; 2];
    let mut saf = [AF_MAX; 2];
    let mut simp = [0u32; 2];
    for ph in 0..2 {
        let limit = if has_req {
            costs[n + 2 * nd + ph]
        } else {
            INF_B
        };
        for ci in 0..kept as usize {
            let rr = res[ci * 2 + ph];
            if rr[0] == NONE {
                continue;
            }
            if better(rr[1], rr[2], sarr[ph], saf[ph], limit, area) {
                sarr[ph] = rr[1];
                saf[ph] = rr[2];
                simp[ph] = 4 | ((ci as u32) << 3) | (rr[0] << 10);
            }
        }
    }
    for p in 0..2 {
        let limit = if has_req {
            costs[n + 2 * nd + p]
        } else {
            INF_B
        };
        let o = 1 - p;
        if sarr[o] != INF_B && simp[o] & 7 != 3 {
            let carr = add_arr(sarr[o], pk.d_inv);
            let caf = af_add(saf[o], pk.a_inv);
            if better(carr, caf, sarr[p], saf[p], limit, area) {
                sarr[p] = carr;
                saf[p] = caf;
                simp[p] = 3;
            }
        }
    }
    let best = [simp[0], sarr[0], saf[0], simp[1], sarr[1], saf[1]];
    NodeOut { node, rows, best }
}
