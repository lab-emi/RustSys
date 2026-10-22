// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

// Level-synchronous arrival, transition and predecessor propagation kernels of
// `rusteda_compute::timing`, compiled at run time by NVRTC (COMPUTE-REQ-0014 to COMPUTE-REQ-0019,
// COMPUTE-REQ-0021, COMPUTE-REQ-0022, COMPUTE-REQ-0028).
// `-DREAL_F64` with `--fmad=false` selects IEEE 754 double precision bit-identical to the CPU;
// the default is single precision.

#ifdef REAL_F64
typedef double real;
typedef unsigned long long bits_t;
__device__ __forceinline__ real r_inf() { return __longlong_as_double(0x7ff0000000000000ULL); }
__device__ __forceinline__ bits_t to_bits(real x) { return (bits_t)__double_as_longlong(x); }
__device__ __forceinline__ real r_sqrt(real x) { return sqrt(x); }
#else
typedef float real;
typedef unsigned int bits_t;
__device__ __forceinline__ real r_inf() { return __int_as_float(0x7f800000); }
__device__ __forceinline__ bits_t to_bits(real x) { return __float_as_uint(x); }
__device__ __forceinline__ real r_sqrt(real x) { return sqrtf(x); }
#endif

#define NONE 0xFFFFFFFFu
#define NO_PRED 0xFFFFFFFFu
#define WIRE 1u

__device__ __forceinline__ real absent_val(unsigned an) {
    return an == 0u ? r_inf() : -r_inf();
}

__device__ __forceinline__ bool better(unsigned an, real x, real y) {
    return an == 0u ? (x < y) : (x > y);
}

__device__ __forceinline__ real canon(real x) {
    return x == (real)0 ? (real)0 : x;
}

__device__ __forceinline__ void interval(const real* p, unsigned m, real x,
                                         unsigned* i0, unsigned* i1, real* w) {
    if (m <= 1u) {
        *i0 = 0u;
        *i1 = 0u;
        *w = (real)0;
        return;
    }
    unsigned k = 0u;
    while (k + 2u < m && x > p[k + 1u]) {
        k += 1u;
    }
    *i0 = k;
    *i1 = k + 1u;
    *w = (x - p[k]) / (p[k + 1u] - p[k]);
}

// Multilinear interpolation and linear extrapolation on 0 to 3 axes (COMPUTE-REQ-0014), in the
// exact operation order of `timing::lut::lookup`.
__device__ real lookup(const unsigned* luts, const real* data, unsigned t,
                       real x0, real x1, real x2) {
    if (t == NONE) {
        return (real)0;
    }
    const unsigned* l = luts + t * 10u;
    unsigned n0 = l[0], n1 = l[1], n2 = l[2];
    unsigned off0 = l[3], off1 = l[4], off2 = l[5];
    unsigned arg0 = l[6], arg1 = l[7], arg2 = l[8];
    const real* v = data + l[9];
    if (n0 == 0u) {
        return v[0];
    }
    real args[3] = { x0, x1, x2 };
    unsigned i0, i1;
    real w0;
    interval(data + off0, n0, args[arg0], &i0, &i1, &w0);
    if (n1 == 0u) {
        return v[i0] + (v[i1] - v[i0]) * w0;
    }
    unsigned j0, j1;
    real w1;
    interval(data + off1, n1, args[arg1], &j0, &j1, &w1);
    real c00, c01, c10, c11;
    if (n2 == 0u) {
        c00 = v[i0 * n1 + j0];
        c01 = v[i0 * n1 + j1];
        c10 = v[i1 * n1 + j0];
        c11 = v[i1 * n1 + j1];
    } else {
        unsigned k0, k1;
        real w2;
        interval(data + off2, n2, args[arg2], &k0, &k1, &w2);
        unsigned b00 = (i0 * n1 + j0) * n2;
        unsigned b01 = (i0 * n1 + j1) * n2;
        unsigned b10 = (i1 * n1 + j0) * n2;
        unsigned b11 = (i1 * n1 + j1) * n2;
        c00 = v[b00 + k0] + (v[b00 + k1] - v[b00 + k0]) * w2;
        c01 = v[b01 + k0] + (v[b01 + k1] - v[b01 + k0]) * w2;
        c10 = v[b10 + k0] + (v[b10 + k1] - v[b10 + k0]) * w2;
        c11 = v[b11 + k0] + (v[b11 + k1] - v[b11 + k0]) * w2;
    }
    real lo = c00 + (c01 - c00) * w1;
    real hi = c10 + (c11 - c10) * w1;
    return lo + (hi - lo) * w0;
}

// Evaluates node `v` (COMPUTE-REQ-0017, COMPUTE-REQ-0018) into `da` (`ntags * 4` arrivals),
// `ds` (4 transitions) and `dp` (`ntags * 4` predecessors) from the values of lower levels in
// `low_arr` and `low_slew`. Returns the IEEE 754 `f64` bit pattern of the largest magnitude among
// the present values of the row.
__device__ unsigned long long eval_node(
    unsigned v,
    unsigned ntags,
    const real* low_arr,
    const real* low_slew,
    const unsigned* in_ptr,
    const unsigned* in_arcs,
    const unsigned* map_ptr,
    const unsigned* map_tags,
    const real* map_vals,
    const unsigned* classes,
    const unsigned* luts,
    const real* data,
    const real* load,
    const real* wires,
    unsigned has_wires,
    const unsigned* start_row,
    const real* start_arr,
    const real* start_slew,
    real* da,
    real* ds,
    unsigned* dp
) {
    unsigned w = ntags * 4u;
    unsigned r = start_row[v];
    if (r == NONE) {
        for (unsigned i = 0u; i < w; i++) {
            da[i] = absent_val(i & 1u);
        }
        for (unsigned i = 0u; i < 4u; i++) {
            ds[i] = absent_val(i & 1u);
        }
    } else {
        const real* sa = start_arr + (unsigned long long)r * w;
        const real* ss = start_slew + (unsigned long long)r * 4u;
        for (unsigned i = 0u; i < w; i++) {
            da[i] = sa[i];
        }
        for (unsigned i = 0u; i < 4u; i++) {
            ds[i] = ss[i];
        }
    }
    for (unsigned i = 0u; i < w; i++) {
        dp[i] = NO_PRED;
    }
    for (unsigned k = in_ptr[v]; k < in_ptr[v + 1u]; k++) {
        unsigned u = in_arcs[k * 3u];
        unsigned cls_idx = in_arcs[k * 3u + 1u];
        unsigned map_idx = in_arcs[k * 3u + 2u];
        const unsigned* c = classes + cls_idx * 7u;
        unsigned flags = c[0];
        unsigned m_begin = 0u, m_end = 0u;
        if (map_idx != NONE) {
            m_begin = map_ptr[map_idx];
            m_end = map_ptr[map_idx + 1u];
        }
        for (unsigned ti = 0u; ti < 2u; ti++) {
            for (unsigned to = 0u; to < 2u; to++) {
                if ((flags & (1u << (1u + 2u * ti + to))) == 0u) {
                    continue;
                }
                for (unsigned an = 0u; an < 2u; an++) {
                    real s = low_slew[(u * 2u + ti) * 2u + an];
                    if (s == absent_val(an)) {
                        continue;
                    }
                    real l = load[v * 4u + to * 2u + an];
                    real d, q;
                    if ((flags & WIRE) != 0u) {
                        if (has_wires != 0u) {
                            const real* wr = wires + (unsigned long long)k * 8ULL;
                            d = wr[to * 2u + an];
                            real wk = wr[4u + to * 2u + an];
                            q = (wk == (real)0) ? s : r_sqrt(s * s + wk * wk);
                        } else {
                            d = (real)0;
                            q = s;
                        }
                    } else {
                        d = lookup(luts, data, c[1u + to], s, l, l);
                        q = lookup(luts, data, c[3u + to], s, l, l);
                        if (q < (real)0) {
                            q = (real)0;
                        }
                    }
                    unsigned si = to * 2u + an;
                    if (better(an, q, ds[si])) {
                        ds[si] = q;
                    }
                    unsigned p = u | (ti << 31);
                    if (map_idx == NONE) {
                        for (unsigned t = 0u; t < ntags; t++) {
                            unsigned long long ui =
                                ((unsigned long long)(u * ntags + t) * 2ULL + ti) * 2ULL + an;
                            real x = low_arr[ui];
                            if (x == absent_val(an)) {
                                continue;
                            }
                            real cand = x + d;
                            unsigned i = (t * 2u + to) * 2u + an;
                            if (better(an, cand, da[i])) {
                                da[i] = cand;
                                dp[i] = p;
                            }
                        }
                        continue;
                    }
                    for (unsigned mr = m_begin; mr < m_end; mr++) {
                        unsigned src_tag = map_tags[mr * 2u];
                        unsigned dst_tag = map_tags[mr * 2u + 1u];
                        const real* mv = map_vals + mr * 9u;
                        unsigned long long ui =
                            ((unsigned long long)(u * ntags + src_tag) * 2ULL + ti) * 2ULL + an;
                        real x = low_arr[ui];
                        if (x == absent_val(an)) {
                            continue;
                        }
                        real m = x + mv[0];
                        if (m < mv[1u + an]) {
                            continue;
                        }
                        real h = mv[7u + an];
                        real close_an = mv[3u + an];
                        if (close_an < r_inf()) {
                            real cs = mv[5u + an];
                            real e = close_an - lookup(luts, data, c[5u + ti], cs, s, l);
                            if (e < h) {
                                h = e;
                            }
                        }
                        if (m > h) {
                            m = h;
                        }
                        real cand = m + d;
                        unsigned i = (dst_tag * 2u + to) * 2u + an;
                        if (better(an, cand, da[i])) {
                            da[i] = cand;
                            dp[i] = p;
                        }
                    }
                }
            }
        }
    }
    real mx = (real)0;
    for (unsigned i = 0u; i < w; i++) {
        real a = canon(da[i]);
        da[i] = a;
        real ab = a < (real)0 ? -a : a;
        if (ab < r_inf() && ab > mx) {
            mx = ab;
        }
    }
    for (unsigned i = 0u; i < 4u; i++) {
        real s = canon(ds[i]);
        ds[i] = s;
        real ab = s < (real)0 ? -s : s;
        if (ab < r_inf() && ab > mx) {
            mx = ab;
        }
    }
    return (unsigned long long)__double_as_longlong((double)mx);
}

// One thread per node of one level (`base .. base + count`): full or `FromLevel` propagation.
extern "C" __global__ void propagate_level(
    real* arr,
    real* slew,
    unsigned* pred,
    const unsigned* in_ptr,
    const unsigned* in_arcs,
    const unsigned* map_ptr,
    const unsigned* map_tags,
    const real* map_vals,
    const unsigned* classes,
    const unsigned* luts,
    const real* data,
    const real* load,
    const real* wires,
    unsigned has_wires,
    const unsigned* start_row,
    const real* start_arr,
    const real* start_slew,
    unsigned long long* max_bits,
    unsigned base,
    unsigned count,
    unsigned ntags
) {
    unsigned pos = blockIdx.x * blockDim.x + threadIdx.x;
    if (pos >= count) {
        return;
    }
    unsigned v = base + pos;
    unsigned long long w = (unsigned long long)ntags * 4ULL;
    unsigned long long mx = eval_node(
        v, ntags, arr, slew, in_ptr, in_arcs, map_ptr, map_tags, map_vals,
        classes, luts, data, load, wires, has_wires, start_row, start_arr, start_slew,
        arr + (unsigned long long)v * w,
        slew + (unsigned long long)v * 4ULL,
        pred + (unsigned long long)v * w
    );
    if (mx != 0ULL) {
        atomicMax(max_bits, mx);
    }
}

// One thread per node of one level (`base .. base + count`): cone propagation with bitwise change
// detection (COMPUTE-REQ-0021, COMPUTE-REQ-0028).
extern "C" __global__ void propagate_cone(
    real* arr,
    real* slew,
    unsigned* pred,
    real* scratch_arr,
    unsigned* scratch_pred,
    const unsigned* in_ptr,
    const unsigned* in_arcs,
    const unsigned* map_ptr,
    const unsigned* map_tags,
    const real* map_vals,
    const unsigned* classes,
    const unsigned* luts,
    const real* data,
    const real* load,
    const real* wires,
    unsigned has_wires,
    const unsigned* start_row,
    const real* start_arr,
    const real* start_slew,
    const unsigned* out_ptr,
    const unsigned* out_dst,
    unsigned char* dirty,
    unsigned char* changed_flags,
    unsigned long long* stats,
    unsigned base,
    unsigned count,
    unsigned ntags
) {
    unsigned pos = blockIdx.x * blockDim.x + threadIdx.x;
    if (pos >= count) {
        return;
    }
    unsigned v = base + pos;
    if (!dirty[v]) {
        return;
    }
    dirty[v] = 0;
    unsigned w = ntags * 4u;
    real* da = scratch_arr + (unsigned long long)pos * w;
    unsigned* dp = scratch_pred + (unsigned long long)pos * w;
    real ds[4];
    unsigned long long mx = eval_node(
        v, ntags, arr, slew, in_ptr, in_arcs, map_ptr, map_tags, map_vals,
        classes, luts, data, load, wires, has_wires, start_row, start_arr, start_slew,
        da, ds, dp
    );
    atomicAdd(&stats[0], 1ULL);
    if (mx != 0ULL) {
        atomicMax(&stats[2], mx);
    }
    real* oa = arr + (unsigned long long)v * w;
    real* os = slew + (unsigned long long)v * 4ULL;
    unsigned* op = pred + (unsigned long long)v * w;
    bool diff = false;
    for (unsigned i = 0u; i < 4u; i++) {
        if (to_bits(ds[i]) != to_bits(os[i])) {
            diff = true;
        }
        os[i] = ds[i];
    }
    for (unsigned i = 0u; i < w; i++) {
        if (to_bits(da[i]) != to_bits(oa[i]) || dp[i] != op[i]) {
            diff = true;
        }
        oa[i] = da[i];
        op[i] = dp[i];
    }
    if (diff) {
        changed_flags[v] = 1;
        atomicAdd(&stats[1], 1ULL);
        for (unsigned k = out_ptr[v]; k < out_ptr[v + 1u]; k++) {
            dirty[out_dst[k]] = 1;
        }
    }
}

// Gathers the rows of `rows[0..row_count]` into compact arrays for `Readback::Rows` and
// `Readback::Changed` (COMPUTE-REQ-0022).
extern "C" __global__ void gather_rows(
    const real* arr,
    const real* slew,
    const unsigned* pred,
    const unsigned* rows,
    real* out_arr,
    real* out_slew,
    unsigned* out_pred,
    unsigned row_count,
    unsigned w
) {
    unsigned i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= row_count) {
        return;
    }
    unsigned v = rows[i];
    const real* sa = arr + (unsigned long long)v * w;
    const real* ss = slew + (unsigned long long)v * 4ULL;
    const unsigned* sp = pred + (unsigned long long)v * w;
    real* da = out_arr + (unsigned long long)i * w;
    real* ds = out_slew + (unsigned long long)i * 4ULL;
    unsigned* dp = out_pred + (unsigned long long)i * w;
    for (unsigned j = 0u; j < w; j++) {
        da[j] = sa[j];
        dp[j] = sp[j];
    }
    for (unsigned j = 0u; j < 4u; j++) {
        ds[j] = ss[j];
    }
}
