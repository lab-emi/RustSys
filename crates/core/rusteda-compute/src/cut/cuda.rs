// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! CUDA backend of the cut-pass task (COMPUTE-REQ-0106): the kernel `node_cuts` in CUDA C, one
//! block of 128 threads per node of a level, launched level by level in chunks on the device
//! layer of `crate::cuda` (COMPUTE-REQ-0002). The cut table and the choices stay on the device
//! between passes; a pass uploads its costs, launches every level without waiting, and waits once
//! before reading back (see `pack.rs` for the words).

// Backend glue over the CUDA driver: every `unsafe` block carries its justification.
#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use super::pack::{self, BLOCK, CW, Packed};
use super::{
    CutPassBackend, CutPassProblem, CutPassSession, PassCosts, PassOutput, PassStats, Readback,
};
use crate::ComputeError;
use crate::cuda::{CudaFunction, CudaSlice, Device, LaunchConfig, PushKernelArg, launch_1d};

const SRC: &str = r#"
#define INF_B 0xFFFFFFFFu
#define ZERO_B 0x80000000u
#define AF_MAX 1073741824u
#define NONE 0xFFFFFFFFu
#define CW 11u
#define SW 12u
#define U 128u

__device__ __forceinline__ void swap_adj(unsigned &lo, unsigned &hi, unsigned k) {
    if (k < 4) {
        unsigned keep = k == 0 ? 0x99999999u : k == 1 ? 0xC3C3C3C3u : k == 2 ? 0xF00FF00Fu : 0xFF0000FFu;
        unsigned up = k == 0 ? 0x22222222u : k == 1 ? 0x0C0C0C0Cu : k == 2 ? 0x00F000F0u : 0x0000FF00u;
        unsigned s = 1u << k;
        lo = (lo & keep) | ((lo & up) << s) | ((lo >> s) & up);
        hi = (hi & keep) | ((hi & up) << s) | ((hi >> s) & up);
    } else {
        unsigned nlo = (lo & 0x0000FFFFu) | (hi << 16);
        unsigned nhi = (lo >> 16) | (hi & 0xFFFF0000u);
        lo = nlo;
        hi = nhi;
    }
}

__device__ __forceinline__ bool depends(unsigned lo, unsigned hi, unsigned i) {
    if (i == 5) return lo != hi;
    unsigned vm = i == 0 ? 0xAAAAAAAAu : i == 1 ? 0xCCCCCCCCu : i == 2 ? 0xF0F0F0F0u : i == 3 ? 0xFF00FF00u : 0xFFFF0000u;
    unsigned s = 1u << i;
    return ((((lo >> s) ^ lo) & ~vm) | (((hi >> s) ^ hi) & ~vm)) != 0;
}

// Arrivals are biased (order-preserving unsigned); INF_B is unmapped.
__device__ __forceinline__ unsigned add_arr(unsigned a, unsigned d) {
    if (a == INF_B) return INF_B;
    return a > INF_B - 1u - d ? INF_B - 1u : a + d;
}

__device__ __forceinline__ unsigned af_add(unsigned a, unsigned b) {
    unsigned s = a + b;
    return s < AF_MAX ? s : AF_MAX;
}

__device__ __forceinline__ unsigned af_share(unsigned af, unsigned fo2) {
    return (af * 2u) / (fo2 > 2u ? fo2 : 2u);
}

// a better than b (COMPUTE-REQ-0102).
__device__ __forceinline__ bool better(unsigned aarr, unsigned aaf, unsigned barr, unsigned baf, unsigned limit, bool area) {
    if (barr == INF_B) return aarr != INF_B;
    if (area) {
        bool fa = aarr <= limit, fb = barr <= limit;
        if (fa != fb) return fa;
        if (!fa) return aarr < barr;
        if (aaf != baf) return aaf < baf;
        return aarr < barr;
    }
    if (aarr != barr) return aarr < barr;
    return aaf < baf;
}

// Merges two fanin cuts (rows of CW words) into out (CW words); false when infeasible.
__device__ bool merge(const unsigned* a, bool ca, const unsigned* b, bool cb, unsigned k, unsigned* out) {
    unsigned na = a[6], nb = b[6];
    if (__popc(a[9] | b[9]) + __popc(a[10] | b[10]) > k) return false;
    unsigned u[12];
    unsigned i = 0, j = 0, n = 0;
    while (i < na || j < nb) {
        unsigned x;
        if (j >= nb || (i < na && a[i] < b[j])) { x = a[i]; i++; }
        else if (i >= na || b[j] < a[i]) { x = b[j]; j++; }
        else { x = a[i]; i++; j++; }
        u[n] = x;
        n++;
        if (n > k) return false;
    }
    unsigned alo = a[7], ahi = a[8], blo = b[7], bhi = b[8];
    for (int q = (int)na - 1; q >= 0; q--) {
        unsigned to = 0;
        while (u[to] != a[q]) to++;
        for (unsigned s = (unsigned)q; s < to; s++) swap_adj(alo, ahi, s);
    }
    for (int q = (int)nb - 1; q >= 0; q--) {
        unsigned to = 0;
        while (u[to] != b[q]) to++;
        for (unsigned s = (unsigned)q; s < to; s++) swap_adj(blo, bhi, s);
    }
    if (ca) { alo = ~alo; ahi = ~ahi; }
    if (cb) { blo = ~blo; bhi = ~bhi; }
    unsigned lo = alo & blo, hi = ahi & bhi;
    // Remove leaves the function does not depend on.
    unsigned q = 0;
    while (q < n) {
        if (depends(lo, hi, q)) { q++; continue; }
        for (unsigned s = q; s < 5; s++) swap_adj(lo, hi, s);
        for (unsigned t = q; t + 1 < n; t++) u[t] = u[t + 1];
        n--;
    }
    unsigned slo = 0, shi = 0;
    for (unsigned t = 0; t < 6; t++) out[t] = t < n ? u[t] : 0;
    for (unsigned t = 0; t < n; t++) {
        unsigned m = u[t] % 64u;
        if (m < 32) slo |= 1u << m; else shi |= 1u << (m - 32);
    }
    out[6] = n;
    out[7] = lo;
    out[8] = hi;
    out[9] = slo;
    out[10] = shi;
    return true;
}

// Candidate order (COMPUTE-REQ-0102): pass cost, leaf count, other cost, then pair index.
__device__ __forceinline__ bool less(const unsigned* x, const unsigned* y, bool area) {
    // x, y: candidate records [p, n, l0..l5, tt_lo, tt_hi, key_arr, key_af]
    unsigned xa = x[10], ya = y[10];
    unsigned xf = x[11], yf = y[11];
    if (area) {
        if (xf != yf) return xf < yf;
        if (x[1] != y[1]) return x[1] < y[1];
        if (xa != ya) return xa < ya;
    } else {
        if (xa != ya) return xa < ya;
        if (x[1] != y[1]) return x[1] < y[1];
        if (xf != yf) return xf < yf;
    }
    return x[0] < y[0];
}

extern "C" __global__ void node_cuts(
    const unsigned* nodes, unsigned* cuts, unsigned* best, const unsigned* lib,
    const unsigned* costs, unsigned* scratch,
    unsigned ord_off, unsigned chunk_base, unsigned n_nodes, unsigned slots, unsigned max_cuts,
    unsigned kmax, unsigned mode, unsigned d_inv, unsigned a_inv, unsigned nkeys, unsigned moff,
    unsigned coff, unsigned scr_words) {
    __shared__ unsigned cnt[U];
    __shared__ unsigned off[U];
    __shared__ unsigned sv[8];
    __shared__ unsigned sarr[2];
    __shared__ unsigned saf[2];
    __shared__ unsigned simp[2];
    // Leaf count and leaves of the previous choices' cuts per phase (0: none).
    __shared__ unsigned sp[14];
    // Ranks of the forced cuts (two-fanin cut, previous cuts), deduplicated.
    __shared__ unsigned sfr[3];
    const unsigned tid = threadIdx.x;
    const bool area = (mode & 1u) != 0;
    const bool has_req = (mode & 2u) != 0;
    const unsigned node = nodes[ord_off + chunk_base + blockIdx.x];
    const unsigned l0 = nodes[2 * node], l1 = nodes[2 * node + 1];
    const unsigned f0 = l0 >> 1, f1 = l1 >> 1;
    const unsigned na = cuts[f0], nb = cuts[f1];
    const unsigned P = na * nb;
    // Candidate capacity: every pair plus the two previous cuts.
    const unsigned pcap = slots * slots + 2;
    unsigned* scr = scratch + (unsigned long long)blockIdx.x * scr_words;
    unsigned* cand = scr;
    unsigned* idx = scr + pcap * SW;
    unsigned* flag = idx + pcap;
    unsigned* rank = flag + pcap;
    unsigned* res = rank + pcap;
    const unsigned* rows = cuts + n_nodes;
    const unsigned* fo2 = costs;
    // Phase 1: merge the pairs of a contiguous range per unit; feasible ones in pair order.
    const unsigned R = (P + U - 1) / U;
    const unsigned beg = tid * R;
    const unsigned end = min(P, beg + R);
    unsigned c = 0;
    unsigned tmp[CW];
    for (unsigned p = beg; p < end; p++) {
        unsigned ia = p / nb, ib = p % nb;
        const unsigned* ra = rows + ((unsigned long long)f0 * slots + ia) * CW;
        const unsigned* rb = rows + ((unsigned long long)f1 * slots + ib) * CW;
        if (!merge(ra, (l0 & 1u) != 0, rb, (l1 & 1u) != 0, kmax, tmp)) continue;
        if (tmp[6] == 0) continue;
        unsigned* x = cand + (beg + c) * SW;
        x[0] = p;
        x[1] = tmp[6];
        for (unsigned t = 0; t < 6; t++) x[2 + t] = tmp[t];
        x[8] = tmp[7];
        x[9] = tmp[8];
        unsigned karr = ZERO_B;
        unsigned kaf = 0;
        for (unsigned t = 0; t < tmp[6]; t++) {
            unsigned l = tmp[t];
            const unsigned* bl = best + 6 * l;
            unsigned a0 = bl[1], a1 = bl[4];
            unsigned m = a0 < a1 ? a0 : a1;
            if (m > karr) karr = m;
            unsigned af0 = bl[2], af1 = bl[5];
            kaf = af_add(kaf, af_share(af0 < af1 ? af0 : af1, fo2[l]));
        }
        x[10] = karr;
        x[11] = kaf;
        c++;
    }
    cnt[tid] = c;
    __syncthreads();
    if (tid == 0) {
        unsigned s = 0;
        for (unsigned u2 = 0; u2 < U; u2++) { off[u2] = s; s += cnt[u2]; }
        // In area passes, the previous choices' cuts of the phases the cover uses follow the
        // pairs as candidates (read before this node's rows are rewritten).
        for (unsigned j = 0; j < 2; j++) {
            sp[7 * j] = 0;
            unsigned code = best[6 * node + 3 * j];
            if (has_req && costs[n_nodes + 2 * node + j] != INF_B && (code & 7u) == 4u) {
                const unsigned* pr = rows + ((unsigned long long)node * slots + ((code >> 3) & 0x7Fu)) * CW;
                unsigned* x = cand + (P + j) * SW;
                x[0] = P + j;
                x[1] = pr[6];
                for (unsigned t = 0; t < 6; t++) x[2 + t] = pr[t];
                x[8] = pr[7];
                x[9] = pr[8];
                unsigned karr = ZERO_B;
                unsigned kaf = 0;
                for (unsigned t = 0; t < pr[6]; t++) {
                    unsigned l = pr[t];
                    const unsigned* bl = best + 6 * l;
                    unsigned a0 = bl[1], a1 = bl[4];
                    unsigned m = a0 < a1 ? a0 : a1;
                    if (m > karr) karr = m;
                    unsigned af0 = bl[2], af1 = bl[5];
                    kaf = af_add(kaf, af_share(af0 < af1 ? af0 : af1, fo2[l]));
                }
                x[10] = karr;
                x[11] = kaf;
                idx[s] = P + j;
                s++;
                sp[7 * j] = pr[6];
                for (unsigned t = 0; t < 6; t++) sp[7 * j + 1 + t] = pr[t];
            }
        }
        sv[0] = s;
        sv[2] = NONE;
        sv[3] = NONE;
        sv[4] = NONE;
    }
    __syncthreads();
    for (unsigned j = 0; j < c; j++) idx[off[tid] + j] = beg + j;
    __syncthreads();
    const unsigned V = sv[0];
    // Phase 2: duplicates of an earlier candidate (equal leaf sets).
    for (unsigned i = tid; i < V; i += U) {
        const unsigned* xi = cand + idx[i] * SW;
        unsigned dup = 0;
        for (unsigned j = 0; j < i && !dup; j++) {
            const unsigned* xj = cand + idx[j] * SW;
            if (xj[1] != xi[1]) continue;
            bool same = true;
            for (unsigned t = 0; t < xi[1]; t++) if (xj[2 + t] != xi[2 + t]) { same = false; break; }
            if (same) dup = 1;
        }
        flag[i] = dup;
    }
    __syncthreads();
    // Phase 3: rank of every unique candidate; the two-fanin cut.
    const unsigned two_lo = f0 < f1 ? f0 : f1, two_hi = f0 < f1 ? f1 : f0;
    unsigned uniq = 0;
    for (unsigned i = tid; i < V; i += U) {
        if (flag[i]) continue;
        uniq++;
        const unsigned* xi = cand + idx[i] * SW;
        unsigned r = 0;
        for (unsigned j = 0; j < V; j++) {
            if (j == i || flag[j]) continue;
            if (less(cand + idx[j] * SW, xi, area)) r++;
        }
        rank[i] = r;
        if (xi[1] == 2 && xi[2] == two_lo && xi[3] == two_hi) sv[2] = r;
        for (unsigned j = 0; j < 2; j++) {
            if (sp[7 * j] == 0 || sp[7 * j] != xi[1]) continue;
            bool same = true;
            for (unsigned t = 0; t < xi[1]; t++) if (sp[7 * j + 1 + t] != xi[2 + t]) same = false;
            if (same) sv[3 + j] = r;
        }
    }
    cnt[tid] = uniq;
    __syncthreads();
    if (tid == 0) {
        unsigned k = 0;
        for (unsigned u2 = 0; u2 < U; u2++) k += cnt[u2];
        unsigned kept = k < max_cuts ? k : max_cuts;
        sv[1] = kept;
        // Forced cuts in order: two-fanin cut, previous cut of phase 0, of phase 1.
        unsigned nf = 0;
        for (unsigned j = 0; j < 3; j++) {
            unsigned r = sv[2 + j];
            if (r == NONE || nf >= max_cuts) continue;
            bool dup = false;
            for (unsigned q = 0; q < nf; q++) if (sfr[q] == r) dup = true;
            if (!dup) { sfr[nf] = r; nf++; }
        }
        sv[5] = nf;
        cuts[node] = kept + 1;
        unsigned* tr = cuts + n_nodes + ((unsigned long long)node * slots + kept) * CW;
        tr[0] = node;
        for (unsigned t = 1; t < 6; t++) tr[t] = 0;
        tr[6] = 1;
        tr[7] = 0xAAAAAAAAu;
        tr[8] = 0xAAAAAAAAu;
        unsigned m = node % 64u;
        tr[9] = m < 32 ? (1u << m) : 0;
        tr[10] = m < 32 ? 0 : (1u << (m - 32));
    }
    __syncthreads();
    const unsigned kept = sv[1];
    const unsigned nf = sv[5];
    // Phase 4: write the kept cuts at their slots: the forced cuts, and the best others up to
    // `max_cuts`, in rank order.
    for (unsigned i = tid; i < V; i += U) {
        if (flag[i]) continue;
        unsigned r = rank[i];
        unsigned below = 0;
        bool forced = false;
        for (unsigned q = 0; q < nf; q++) {
            if (sfr[q] < r) below++;
            if (sfr[q] == r) forced = true;
        }
        unsigned slot = NONE;
        if (forced) {
            unsigned o = r - below;
            slot = below + (o < max_cuts - nf ? o : max_cuts - nf);
        } else if (r - below < max_cuts - nf) {
            slot = r;
        }
        if (slot == NONE) continue;
        const unsigned* xi = cand + idx[i] * SW;
        unsigned* row = cuts + n_nodes + ((unsigned long long)node * slots + slot) * CW;
        unsigned n = xi[1];
        unsigned slo = 0, shi = 0;
        for (unsigned t = 0; t < 6; t++) row[t] = xi[2 + t];
        for (unsigned t = 0; t < n; t++) {
            unsigned m = xi[2 + t] % 64u;
            if (m < 32) slo |= 1u << m; else shi |= 1u << (m - 32);
        }
        row[6] = n;
        row[7] = xi[8];
        row[8] = xi[9];
        row[9] = slo;
        row[10] = shi;
    }
    __syncthreads();
    // Phase 5: best match of every (kept cut, phase).
    for (unsigned q = tid; q < kept * 2; q += U) {
        unsigned ci = q >> 1, ph = q & 1u;
        const unsigned* row = cuts + n_nodes + ((unsigned long long)node * slots + ci) * CW;
        unsigned n = row[6];
        unsigned lo = row[7], hi = row[8];
        if (ph) { lo = ~lo; hi = ~hi; }
        if (n < 6) { hi = 0; if (n < 5) lo &= (1u << (1u << n)) - 1u; }
        // Binary search of (n, hi, lo) among the sorted keys.
        unsigned a = 0, b = nkeys;
        while (a < b) {
            unsigned mid = (a + b) >> 1;
            const unsigned* kk = lib + mid * 5;
            bool lt = kk[0] < n || (kk[0] == n && (kk[1] < hi || (kk[1] == hi && kk[2] < lo)));
            if (lt) a = mid + 1; else b = mid;
        }
        unsigned barr = INF_B;
        unsigned baf = AF_MAX, bm = NONE;
        unsigned limit = has_req ? costs[n_nodes + 2 * node + ph] : INF_B;
        if (a < nkeys && lib[a * 5] == n && lib[a * 5 + 1] == hi && lib[a * 5 + 2] == lo) {
            unsigned ms = lib[a * 5 + 3], mc = lib[a * 5 + 4];
            for (unsigned mi = ms; mi < ms + mc; mi++) {
                unsigned cell = lib[moff + 2 * mi];
                unsigned pp = lib[moff + 2 * mi + 1];
                const unsigned* ce = lib + coff + 8 * cell;
                unsigned nin = ce[0];
                unsigned arr = ZERO_B;
                unsigned af = ce[1];
                bool ok = true;
                for (unsigned t = 0; t < nin; t++) {
                    unsigned leaf = row[(pp >> (4 * t)) & 15u];
                    unsigned lph = (pp >> (24 + t)) & 1u;
                    unsigned la = best[6 * leaf + 3 * lph + 1];
                    if (la == INF_B) { ok = false; break; }
                    unsigned v = add_arr(la, ce[2 + t]);
                    if (v > arr) arr = v;
                    af = af_add(af, af_share(best[6 * leaf + 3 * lph + 2], fo2[leaf]));
                }
                if (!ok) continue;
                if (better(arr, af, barr, baf, limit, area)) { barr = arr; baf = af; bm = mi; }
            }
        }
        unsigned* rr = res + q * 4;
        rr[0] = bm;
        rr[1] = barr;
        rr[2] = baf;
    }
    __syncthreads();
    // Phase 6: first best over the cuts per phase, then inverters.
    if (tid < 2) {
        unsigned ph = tid;
        unsigned limit = has_req ? costs[n_nodes + 2 * node + ph] : INF_B;
        unsigned oarr = INF_B;
        unsigned oaf = AF_MAX, oimp = 0;
        for (unsigned ci = 0; ci < kept; ci++) {
            const unsigned* rr = res + (ci * 2 + ph) * 4;
            if (rr[0] == NONE) continue;
            if (better(rr[1], rr[2], oarr, oaf, limit, area)) {
                oarr = rr[1];
                oaf = rr[2];
                oimp = 4u | (ci << 3) | (rr[0] << 10);
            }
        }
        sarr[ph] = oarr;
        saf[ph] = oaf;
        simp[ph] = oimp;
    }
    __syncthreads();
    if (tid == 0) {
        for (unsigned p = 0; p < 2; p++) {
            unsigned limit = has_req ? costs[n_nodes + 2 * node + p] : INF_B;
            unsigned o = 1 - p;
            if (sarr[o] != INF_B && (simp[o] & 7u) != 3u) {
                unsigned carr = add_arr(sarr[o], d_inv);
                unsigned caf = af_add(saf[o], a_inv);
                if (better(carr, caf, sarr[p], saf[p], limit, area)) {
                    sarr[p] = carr;
                    saf[p] = caf;
                    simp[p] = 3u;
                }
            }
        }
        unsigned* bn = best + 6 * node;
        for (unsigned p = 0; p < 2; p++) {
            bn[3 * p] = simp[p];
            bn[3 * p + 1] = sarr[p];
            bn[3 * p + 2] = saf[p];
        }
    }
}

extern "C" __global__ void chosen_rows(const unsigned* cuts, const unsigned* best, unsigned* out,
                                       unsigned n_nodes, unsigned slots) {
    const unsigned i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < 2 * n_nodes) {
        const unsigned id = i >> 1;
        const unsigned code = best[3 * i];
        const bool cell = (code & 7u) == 4u;
        const unsigned c = (code >> 3) & 0x7Fu;
        for (unsigned w = 0; w < CW; ++w) {
            out[i * CW + w] = cell ? cuts[n_nodes + (id * slots + c) * CW + w] : 0u;
        }
    }
}
"#;

/// Largest device array: the kernels index with 32-bit words.
const MAX_BYTES: u64 = 16 << 30;

/// The CUDA backend.
#[derive(Debug)]
pub(super) struct CudaBackend;

pub(super) static CUDA: CudaBackend = CudaBackend;

/// Runs `f`, turning a panic inside the runtime into a device error (COMPUTE-REQ-0001).
fn guarded<T>(f: impl FnOnce() -> Result<T, ComputeError>) -> Result<T, ComputeError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| p.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            Err(ComputeError::Device(format!(
                "cuda runtime failed: {}",
                msg.trim()
            )))
        }
    }
}

impl CutPassBackend for CudaBackend {
    fn name(&self) -> &'static str {
        "cuda"
    }

    fn open<'s>(
        &'s self,
        p: &CutPassProblem<'_>,
    ) -> Result<Box<dyn CutPassSession + 's>, ComputeError> {
        super::validate(p)?;
        let pk = pack::pack(p, MAX_BYTES)?;
        guarded(move || {
            let dev = crate::cuda::device()?;
            let node_cuts = dev.function(SRC, "node_cuts")?;
            let chosen_rows = dev.function(SRC, "chosen_rows")?;
            let nodes = dev.upload(&pk.nodes)?;
            let cuts = dev.upload(&pk.cuts)?;
            let best = dev.upload(&pk.best)?;
            let lib = dev.upload(&pk.lib)?;
            let costs = dev.zeros::<u32>(pk.n * 3)?;
            let scratch = dev.zeros::<u32>(pk.scr_words * pk.chunk)?;
            // The device holds the arrays; the host keeps the shape only.
            let mut pk = pk;
            pk.nodes = Vec::new();
            pk.cuts = Vec::new();
            pk.best = Vec::new();
            pk.lib = Vec::new();
            let s = CudaSession {
                dev,
                node_cuts,
                chosen_rows,
                nodes,
                cuts,
                best,
                lib,
                costs,
                scratch,
                pk,
            };
            Ok(Box::new(s) as Box<dyn CutPassSession + 's>)
        })
    }
}

/// Device state of a session.
struct CudaSession {
    dev: &'static Device,
    node_cuts: CudaFunction,
    chosen_rows: CudaFunction,
    nodes: CudaSlice<u32>,
    cuts: CudaSlice<u32>,
    best: CudaSlice<u32>,
    lib: CudaSlice<u32>,
    costs: CudaSlice<u32>,
    scratch: CudaSlice<u32>,
    pk: Packed,
}

impl CudaSession {
    fn launch_chunk(&mut self, chunk_base: u32, count: u32, mode: u32) -> Result<(), ComputeError> {
        let l = &self.pk;
        let mut b = self.dev.stream().launch_builder(&self.node_cuts);
        b.arg(&self.nodes)
            .arg(&mut self.cuts)
            .arg(&mut self.best)
            .arg(&self.lib)
            .arg(&self.costs)
            .arg(&mut self.scratch)
            .arg(&l.ord_off)
            .arg(&chunk_base)
            .arg(&l.n32)
            .arg(&l.slots)
            .arg(&l.max_cuts)
            .arg(&l.kmax)
            .arg(&mode)
            .arg(&l.d_inv)
            .arg(&l.a_inv)
            .arg(&l.nkeys)
            .arg(&l.moff)
            .arg(&l.coff)
            .arg(&l.scr_words32);
        let cfg = LaunchConfig {
            grid_dim: (count, 1, 1),
            block_dim: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        // SAFETY: `pack` sized every array for the indices the kernel computes from a problem
        // that passed `validate`: node and fanin ids below `n`, cut rows of `slots` entries per
        // node, `scr_words` scratch words per block of the chunk (`count <= chunk`), match and
        // cell offsets inside `lib`. The chunk lies inside the level order list.
        unsafe { b.launch(cfg) }.map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        Ok(())
    }
}

impl CutPassSession for CudaSession {
    fn run(
        &mut self,
        c: &PassCosts<'_>,
        rb: Readback,
        out: &mut PassOutput,
    ) -> Result<PassStats, ComputeError> {
        super::check_costs(self.pk.n, c)?;
        guarded(|| {
            let t = Instant::now();
            let (costs, mode) = pack::costs(self.pk.n, c);
            self.costs = self.dev.upload(&costs)?;
            let mut launches = 0u32;
            let levels = self.pk.levels.clone();
            for (start, count) in levels {
                let mut done = 0u32;
                while done < count {
                    let k = (count - done).min(self.pk.chunk as u32);
                    self.launch_chunk(start + done, k, mode)?;
                    launches += 1;
                    done += k;
                }
            }
            self.dev.sync()?;
            let kernel_s = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let n = self.pk.n;
            match rb {
                Readback::All => {
                    let cuts = self.dev.read(&self.cuts)?;
                    let best = self.dev.read(&self.best)?;
                    pack::read_into(n, self.pk.slots, &best, &cuts[n..], Some(&cuts[..n]), out);
                }
                Readback::Chosen => {
                    let mut rows = self.dev.zeros::<u32>(2 * n * CW)?;
                    let mut b = self.dev.stream().launch_builder(&self.chosen_rows);
                    b.arg(&self.cuts)
                        .arg(&self.best)
                        .arg(&mut rows)
                        .arg(&self.pk.n32)
                        .arg(&self.pk.slots);
                    // SAFETY: one thread per (node, phase) below `2 n`; it reads the three `best`
                    // words of its phase and one cut row of its node (`slots` rows per node,
                    // `CW` words each) and writes its own `CW` words of `rows`, which holds
                    // `2 n CW` words.
                    unsafe { b.launch(launch_1d(2 * n, BLOCK)) }
                        .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
                    let rows = self.dev.read(&rows)?;
                    let best = self.dev.read(&self.best)?;
                    pack::read_into(n, self.pk.slots, &best, &rows, None, out);
                }
            }
            Ok(PassStats {
                launches,
                kernel_s,
                readback_s: t.elapsed().as_secs_f64(),
            })
        })
    }
}
