// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Word-level operations of the checker's RTL model, built bit by bit on the circuit.

use super::Builder;
use crate::circ::B;

impl Builder<'_> {
    pub(crate) fn ext(&mut self, v: &[B], w: usize, signed: bool) -> Vec<B> {
        let fill = if signed {
            v.last().copied().unwrap_or(B::F)
        } else {
            B::F
        };
        (0..w).map(|i| v.get(i).copied().unwrap_or(fill)).collect()
    }

    pub(super) fn w_not(&mut self, v: &[B]) -> Vec<B> {
        v.iter().map(|b| self.c.not(*b)).collect()
    }

    pub(super) fn w_add(&mut self, a: &[B], b: &[B], mut carry: B) -> Vec<B> {
        let mut out = Vec::with_capacity(a.len());
        for (x, y) in a.iter().zip(b) {
            let p = self.c.xor(*x, *y);
            out.push(self.c.xor(p, carry));
            let g = self.c.and(*x, *y);
            let t = self.c.and(p, carry);
            carry = self.c.or(g, t);
        }
        out
    }

    pub(super) fn w_sub(&mut self, a: &[B], b: &[B]) -> Vec<B> {
        let nb = self.w_not(b);
        self.w_add(a, &nb, B::T)
    }

    /// Low `a.len()` bits of `a * b` in the canonical structure of CHECK-REQ-0047: partial
    /// products gathered in columns, reduced in rounds of three-bit groups, then one ripple add.
    pub(crate) fn w_mul(&mut self, a: &[B], b: &[B]) -> Vec<B> {
        let w = a.len();
        // Column k: products a[k - i] & b[i] by increasing i, constant zeros left out.
        let cols: Vec<Vec<B>> = (0..w)
            .map(|k| {
                (0..=k)
                    .map(|i| self.c.and(a[k - i], b[i]))
                    .filter(|p| *p != B::F)
                    .collect()
            })
            .collect();
        self.w_columns(cols)
    }

    /// Signed product of sign-extended operands (CHECK-REQ-0047, SYNTH-REQ-0404): Baugh–Wooley
    /// partial products over the significant widths, constants at columns n-1, m-1 and n+m-1,
    /// then sign extension of the (n+m)-bit result.
    pub(crate) fn w_mul_signed(&mut self, a: &[B], b: &[B]) -> Vec<B> {
        let w = a.len();
        let sig = |v: &[B]| {
            let mut n = v.len();
            while n > 1 && v[n - 2] == v[n - 1] {
                n -= 1;
            }
            n
        };
        let (n, m) = (sig(a), sig(b));
        if n == w && m == w {
            return self.w_mul(a, b);
        }
        let wp = w.min(n + m);
        let mut cols: Vec<Vec<B>> = Vec::with_capacity(wp);
        for k in 0..wp {
            let mut col = Vec::new();
            for i in 0..m.min(k + 1) {
                let j = k - i;
                if j >= n {
                    continue;
                }
                let p = self.c.and(a[j], b[i]);
                let p = if (j == n - 1) != (i == m - 1) {
                    self.c.not(p)
                } else {
                    p
                };
                if p != B::F {
                    col.push(p);
                }
            }
            cols.push(col);
        }
        for k in [n - 1, m - 1, n + m - 1] {
            if k < wp {
                cols[k].push(B::T);
            }
        }
        let r = self.w_columns(cols);
        let top = r[wp - 1];
        (0..w).map(|i| if i < wp { r[i] } else { top }).collect()
    }

    /// Carry-save reduction of weighted columns in groups of three, then the addition of the two
    /// remaining rows.
    fn w_columns(&mut self, mut cols: Vec<Vec<B>>) -> Vec<B> {
        let w = cols.len();
        while cols.iter().any(|c| c.len() > 2) {
            let mut next: Vec<Vec<B>> = vec![Vec::new(); w];
            for k in 0..w {
                let col = std::mem::take(&mut cols[k]);
                let groups = col.len() / 3;
                for g in 0..groups {
                    let (x, y, z) = (col[3 * g], col[3 * g + 1], col[3 * g + 2]);
                    let xy = self.c.xor(x, y);
                    next[k].push(self.c.xor(xy, z));
                    if k + 1 < w {
                        let a1 = self.c.and(x, y);
                        let a2 = self.c.and(xy, z);
                        let carry = self.c.or(a1, a2);
                        next[k + 1].push(carry);
                    }
                }
                next[k].extend_from_slice(&col[3 * groups..]);
            }
            cols = next;
        }
        let x: Vec<B> = cols
            .iter()
            .map(|c| c.first().copied().unwrap_or(B::F))
            .collect();
        let y: Vec<B> = cols
            .iter()
            .map(|c| c.get(1).copied().unwrap_or(B::F))
            .collect();
        self.w_add(&x, &y, B::F)
    }

    pub(super) fn w_eq(&mut self, a: &[B], b: &[B]) -> B {
        let mut r = B::T;
        for (x, y) in a.iter().zip(b) {
            let d = self.c.xor(*x, *y);
            let nd = self.c.not(d);
            r = self.c.and(r, nd);
        }
        r
    }

    /// `a < b`, scanning from the most significant bit.
    pub(super) fn w_lt(&mut self, a: &[B], b: &[B], signed: bool) -> B {
        let n = a.len();
        let mut lt = B::F;
        let mut eq = B::T;
        for i in (0..n).rev() {
            let (x, y) = (a[i], b[i]);
            let less = if signed && i == n - 1 {
                let ny = self.c.not(y);
                self.c.and(x, ny)
            } else {
                let nx = self.c.not(x);
                self.c.and(nx, y)
            };
            let t = self.c.and(eq, less);
            lt = self.c.or(lt, t);
            let d = self.c.xor(x, y);
            let nd = self.c.not(d);
            eq = self.c.and(eq, nd);
        }
        lt
    }

    /// Unsigned long division: (quotient, remainder); zero for division by zero.
    pub(super) fn w_udivmod(&mut self, a: &[B], b: &[B]) -> (Vec<B>, Vec<B>) {
        let w = a.len();
        let bb = self.ext(b, w + 1, false);
        let mut r = vec![B::F; w + 1];
        let mut q = vec![B::F; w];
        for i in (0..w).rev() {
            let mut sh = vec![a[i]];
            sh.extend_from_slice(&r[..w]);
            let lt = self.w_lt(&sh, &bb, false);
            let ge = self.c.not(lt);
            let d = self.w_sub(&sh, &bb);
            q[i] = ge;
            r = (0..=w).map(|k| self.c.mux(ge, d[k], sh[k])).collect();
        }
        let zero = vec![B::F; w];
        let bz = self.w_eq(b, &zero);
        let q2 = (0..w).map(|k| self.c.mux(bz, B::F, q[k])).collect();
        let r2 = (0..w).map(|k| self.c.mux(bz, B::F, r[k])).collect();
        (q2, r2)
    }

    pub(super) fn w_divmod(&mut self, a: &[B], b: &[B], signed: bool) -> (Vec<B>, Vec<B>) {
        if !signed {
            return self.w_udivmod(a, b);
        }
        let w = a.len();
        let (sa, sb) = (a[w - 1], b[w - 1]);
        let zero = vec![B::F; w];
        let na = self.w_sub(&zero, a);
        let nb = self.w_sub(&zero, b);
        let aa: Vec<B> = (0..w).map(|k| self.c.mux(sa, na[k], a[k])).collect();
        let ab: Vec<B> = (0..w).map(|k| self.c.mux(sb, nb[k], b[k])).collect();
        let (q, r) = self.w_udivmod(&aa, &ab);
        let nq = self.w_sub(&zero, &q);
        let nr = self.w_sub(&zero, &r);
        let qs = self.c.xor(sa, sb);
        let q2 = (0..w).map(|k| self.c.mux(qs, nq[k], q[k])).collect();
        let r2 = (0..w).map(|k| self.c.mux(sa, nr[k], r[k])).collect();
        (q2, r2)
    }

    /// Condition `v == k` for an index vector `v` (signedness `s`) and an integer constant.
    pub(crate) fn eq_const(&mut self, v: &[B], s: bool, k: i64) -> B {
        let w = v.len().max(66);
        let x = self.ext(v, w, s);
        let kb: Vec<B> = (0..w)
            .map(|i| B::from_bool(if i < 64 { (k >> i) & 1 == 1 } else { k < 0 }))
            .collect();
        self.w_eq(&x, &kb)
    }

    /// Shift by an unsigned amount: result bit i picks the operand bit selected by the decoded
    /// amount.
    pub(super) fn w_shift(&mut self, a: &[B], amt: &[B], left: bool, fill: B) -> Vec<B> {
        let w = a.len();
        let decoded: Vec<B> = (0..w)
            .map(|k| self.eq_const(amt, false, k as i64))
            .collect();
        let mut small = B::F;
        for d in &decoded {
            small = self.c.or(small, *d);
        }
        let big = self.c.not(small);
        let mut out = Vec::with_capacity(w);
        for i in 0..w {
            let mut r = B::F;
            for (k, d) in decoded.iter().enumerate() {
                let src = if left {
                    if i >= k { a[i - k] } else { B::F }
                } else if i + k < w {
                    a[i + k]
                } else {
                    fill
                };
                let t = self.c.and(*d, src);
                r = self.c.or(r, t);
            }
            let f = if left { B::F } else { fill };
            let t = self.c.and(big, f);
            r = self.c.or(r, t);
            out.push(r);
        }
        out
    }

    pub(super) fn reduce(&mut self, v: &[B], op: u8) -> B {
        let mut r = match op {
            0 => B::T,
            _ => B::F,
        };
        for b in v {
            r = match op {
                0 => self.c.and(r, *b),
                1 => self.c.or(r, *b),
                _ => self.c.xor(r, *b),
            };
        }
        r
    }
}
