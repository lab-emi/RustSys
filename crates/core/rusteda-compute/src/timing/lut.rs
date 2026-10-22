// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Table evaluation in `f64` (COMPUTE-REQ-0014): linear interpolation on up to three axes and
//! linear extrapolation outside them, in a fixed order of operations that the CUDA kernels repeat
//! exactly. Rust never contracts a multiplication and an addition into a fused multiply-add, so
//! every operation rounds once.

use super::abi::{LutDesc, NONE};

/// The interval `(i0, i1)` of axis points `p` around `x` and the weight of `x` in it
/// (COMPUTE-REQ-0014).
#[inline]
fn interval(p: &[f64], x: f64) -> (usize, usize, f64) {
    let m = p.len();
    if m <= 1 {
        return (0, 0, 0.0);
    }
    let mut k = 0;
    while k + 2 < m && x > p[k + 1] {
        k += 1;
    }
    (k, k + 1, (x - p[k]) / (p[k + 1] - p[k]))
}

/// The value of table `t` of `luts` over `data` at the canonical arguments `x`; 0 for [`NONE`]
/// (COMPUTE-REQ-0014). The table must be valid (COMPUTE-REQ-0020).
#[inline]
pub fn lookup(luts: &[LutDesc], data: &[f64], t: u32, x: [f64; 3]) -> f64 {
    if t == NONE {
        return 0.0;
    }
    let l = &luts[t as usize];
    let v = &data[l.voff as usize..];
    let axis = |k: usize| {
        let off = l.off[k] as usize;
        interval(&data[off..off + l.n[k] as usize], x[l.arg[k] as usize])
    };
    if l.n[0] == 0 {
        return v[0];
    }
    let (i0, i1, w0) = axis(0);
    if l.n[1] == 0 {
        return v[i0] + (v[i1] - v[i0]) * w0;
    }
    let (j0, j1, w1) = axis(1);
    let n1 = l.n[1] as usize;
    let (c00, c01, c10, c11) = if l.n[2] == 0 {
        let at = |i: usize, j: usize| v[i * n1 + j];
        (at(i0, j0), at(i0, j1), at(i1, j0), at(i1, j1))
    } else {
        let (k0, k1, w2) = axis(2);
        let n2 = l.n[2] as usize;
        let c = |i: usize, j: usize| {
            let b = (i * n1 + j) * n2;
            v[b + k0] + (v[b + k1] - v[b + k0]) * w2
        };
        (c(i0, j0), c(i0, j1), c(i1, j0), c(i1, j1))
    };
    let lo = c00 + (c01 - c00) * w1;
    let hi = c10 + (c11 - c10) * w1;
    lo + (hi - lo) * w0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of `f(x0, x1, x2)` on the given axes (arguments 0, 1, 2 in order).
    fn table(axes: &[&[f64]], f: impl Fn([f64; 3]) -> f64) -> (Vec<LutDesc>, Vec<f64>) {
        let mut data = Vec::new();
        let mut d = LutDesc::default();
        for (k, p) in axes.iter().enumerate() {
            d.n[k] = p.len() as u32;
            d.off[k] = data.len() as u32;
            d.arg[k] = k as u32;
            data.extend_from_slice(p);
        }
        d.voff = data.len() as u32;
        let n = |k: usize| axes.get(k).map_or(1, |p| p.len());
        for i in 0..n(0) {
            for j in 0..n(1) {
                for k in 0..n(2) {
                    let pt = |a: usize, idx: usize| axes.get(a).map_or(0.0, |p| p[idx]);
                    data.push(f([pt(0, i), pt(1, j), pt(2, k)]));
                }
            }
        }
        (vec![d], data)
    }

    // COMPUTE-REQ-0014: multilinear interpolation and extrapolation reproduce a function that is
    // linear in each argument exactly at points where every operation is exact, on 0 to 3 axes.
    #[test]
    fn linear_functions_are_reproduced() {
        let f = |x: [f64; 3]| 1.0 + 2.0 * x[0] + 4.0 * x[1] + 8.0 * x[2];
        let axes: [&[f64]; 3] = [&[0.0, 1.0, 2.0], &[0.0, 0.5, 1.0, 1.5], &[0.0, 2.0]];
        for dims in 0..=3 {
            let (luts, data) = table(&axes[..dims], f);
            for x in [
                [0.5, 0.25, 1.0],
                [1.5, 1.25, 0.5],
                [-1.0, 2.0, 4.0],
                [4.0, -0.5, -2.0],
            ] {
                let mut want = [x[0], x[1], x[2]];
                for w in want.iter_mut().skip(dims) {
                    *w = 0.0;
                }
                assert_eq!(lookup(&luts, &data, 0, x), f(want), "{dims} axes at {x:?}");
            }
        }
        assert_eq!(lookup(&luts_none(), &[], NONE, [1.0; 3]), 0.0);
    }

    fn luts_none() -> Vec<LutDesc> {
        Vec::new()
    }

    // COMPUTE-REQ-0014: the interval is the first whose upper point is not below the argument,
    // the last one beyond the points; a single point is constant along its axis; the canonical
    // argument of each axis is its `arg`.
    #[test]
    fn intervals_arguments_and_single_points() {
        let p = [1.0, 2.0, 4.0];
        assert_eq!(interval(&p, 0.0), (0, 1, -1.0));
        assert_eq!(interval(&p, 2.0), (0, 1, 1.0));
        assert_eq!(interval(&p, 3.0), (1, 2, 0.5));
        assert_eq!(interval(&p, 8.0), (1, 2, 3.0));
        assert_eq!(interval(&[5.0], 9.0), (0, 0, 0.0));
        // Axis 0 indexed by argument 2 and axis 1 by argument 0.
        let data = vec![0.0, 1.0, 0.0, 2.0, 10.0, 11.0, 20.0, 22.0];
        let d = LutDesc {
            n: [2, 2, 0],
            off: [0, 2, 0],
            arg: [2, 0, 0],
            voff: 4,
        };
        // value(i, j) = 10 + 10 i + j (1 + i): at x2 = 0.5, x0 = 1.0 (j weight 0.5).
        let got = lookup(&[d], &data, 0, [1.0, 99.0, 0.5]);
        let lo = 10.0 + (11.0 - 10.0) * 0.5;
        let hi = 20.0 + (22.0 - 20.0) * 0.5;
        assert_eq!(got, lo + (hi - lo) * 0.5);
        // A one-point axis followed by a second axis.
        let d1 = LutDesc {
            n: [1, 2, 0],
            off: [0, 2, 0],
            arg: [0, 1, 0],
            voff: 4,
        };
        assert_eq!(lookup(&[d1], &data, 0, [7.0, 2.0, 0.0]), 11.0);
        assert_eq!(lookup(&[d1], &data, 0, [7.0, 1.0, 0.0]), 10.5);
        assert_eq!(lookup(&[LutDesc::scalar(6)], &data, 0, [1.0; 3]), 20.0);
    }
}
