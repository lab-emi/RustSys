// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Word-level operators lowered to AIG literals (SYNTH-REQ-0403). Vectors are least
//! significant bit first.

use crate::aig::{Aig, Lit};
mod carry_plan;
pub(crate) use carry_plan::plan as carry_plan;

/// Bit vector of literals, least significant bit first.
pub type Bv = Vec<Lit>;

/// Constant vector of width `w` from the low bits of `v` (bits above 64 are zero).
pub fn constant(v: u64, w: usize) -> Bv {
    (0..w)
        .map(|i| Lit::from_bool(i < 64 && (v >> i) & 1 == 1))
        .collect()
}

/// Zero- or sign-extends (or truncates) to width `w`.
pub fn resize(v: &[Lit], w: usize, signed: bool) -> Bv {
    let mut out: Bv = v.iter().copied().take(w).collect();
    let fill = if signed {
        v.last().copied().unwrap_or(Lit::FALSE)
    } else {
        Lit::FALSE
    };
    out.resize(w, fill);
    out
}

/// Constant value of a vector if all bits are constant and it fits in 64 bits after dropping
/// leading zeros.
pub fn const_u64(v: &[Lit]) -> Option<u64> {
    let mut r = 0u64;
    for (i, l) in v.iter().enumerate() {
        match l.const_value()? {
            true if i >= 64 => return None,
            true => r |= 1 << i,
            false => {}
        }
    }
    Some(r)
}

/// Constant value interpreted as signed (two's complement of `v.len()` bits), if it fits i64.
pub fn const_i64(v: &[Lit], signed: bool) -> Option<i64> {
    let bits: Vec<bool> = v.iter().map(|l| l.const_value()).collect::<Option<_>>()?;
    let n = bits.len();
    if n == 0 {
        return Some(0);
    }
    let sign = signed && bits[n - 1];
    // Every bit from position 63 up must equal the sign for the value to fit in an i64.
    if bits.iter().skip(63).any(|&b| b != sign) {
        return None;
    }
    let mut r: u64 = 0;
    for (i, &b) in bits.iter().enumerate().take(64) {
        if b {
            r |= 1 << i;
        }
    }
    if n < 64 && sign {
        r |= !0u64 << n;
    }
    Some(r as i64)
}

pub fn not(v: &[Lit]) -> Bv {
    v.iter().map(|l| !*l).collect()
}

pub fn and(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Bv {
    a.iter().zip(b).map(|(x, y)| g.and(*x, *y)).collect()
}

pub fn or(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Bv {
    a.iter().zip(b).map(|(x, y)| g.or(*x, *y)).collect()
}

pub fn xor(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Bv {
    a.iter().zip(b).map(|(x, y)| g.xor(*x, *y)).collect()
}

pub fn mux(g: &mut Aig, s: Lit, t: &[Lit], e: &[Lit]) -> Bv {
    t.iter().zip(e).map(|(x, y)| g.mux(s, *x, *y)).collect()
}

/// Balanced reduction tree.
fn reduce(g: &mut Aig, v: &[Lit], op: fn(&mut Aig, Lit, Lit) -> Lit, empty: Lit) -> Lit {
    if v.is_empty() {
        return empty;
    }
    let mut layer: Vec<Lit> = v.to_vec();
    while layer.len() > 1 {
        let mut next = Vec::with_capacity(layer.len().div_ceil(2));
        for pair in layer.chunks(2) {
            next.push(if pair.len() == 2 {
                op(g, pair[0], pair[1])
            } else {
                pair[0]
            });
        }
        layer = next;
    }
    layer[0]
}

pub fn red_and(g: &mut Aig, v: &[Lit]) -> Lit {
    reduce(g, v, Aig::and, Lit::TRUE)
}

pub fn red_or(g: &mut Aig, v: &[Lit]) -> Lit {
    reduce(g, v, Aig::or, Lit::FALSE)
}

pub fn red_xor(g: &mut Aig, v: &[Lit]) -> Lit {
    reduce(g, v, Aig::xor, Lit::FALSE)
}

fn majority(g: &mut Aig, a: Lit, b: Lit, c: Lit) -> Lit {
    let ab = g.and(a, b);
    let o = g.or(a, b);
    let co = g.and(c, o);
    g.or(ab, co)
}

/// Full adder: (sum, carry).
pub fn full_add(g: &mut Aig, a: Lit, b: Lit, c: Lit) -> (Lit, Lit) {
    let x = g.xor(a, b);
    let s = g.xor(x, c);
    let carry = if g.xor_carry {
        let ab = g.and(a, b);
        let xc = g.and(x, c);
        g.or(ab, xc)
    } else {
        majority(g, a, b, c)
    };
    (s, carry)
}

/// Addition of equal-width vectors: (sum, carry-out).
pub fn add(g: &mut Aig, a: &[Lit], b: &[Lit], cin: Lit) -> (Bv, Lit) {
    if a.len() == b.len() && a.len() <= 64 {
        if let (Some(x), Some(y), Some(c)) = (const_u64(a), const_u64(b), cin.const_value()) {
            let sum = u128::from(x) + u128::from(y) + u128::from(c);
            return (
                constant(sum as u64, a.len()),
                Lit::from_bool(sum >> a.len() != 0),
            );
        }
    }
    g.has_parallel_arithmetic |= a.len() >= 8;
    let record = g.parallel_carry && a.len() >= 8;
    let planned = g.carry_plan.get(g.add_sites.len()).copied().unwrap_or(0);
    let result = if record && planned != 0 {
        if planned >= a.len() {
            ripple_add(g, a, b, cin)
        } else {
            sparse_add(g, a, b, cin, planned)
        }
    } else if let Some(weight) = g.carry_cost_weight.filter(|_| a.len() >= 8) {
        adaptive_add(g, a, b, cin, weight)
    } else if record {
        sparse_add(g, a, b, cin, 4)
    } else {
        ripple_add(g, a, b, cin)
    };
    if record {
        g.add_sites.push(Some(crate::aig::AddSite {
            a: a.to_vec(),
            b: b.to_vec(),
            cin,
            sum: result.0.clone(),
            carry: result.1,
        }));
    }
    result
}

fn ripple_add(g: &mut Aig, a: &[Lit], b: &[Lit], cin: Lit) -> (Bv, Lit) {
    let mut c = cin;
    let mut sum = Vec::with_capacity(a.len());
    for (&x, &y) in a.iter().zip(b) {
        let (s, next) = full_add(g, x, y, c);
        sum.push(s);
        c = next;
    }
    (sum, c)
}

/// Compose adjacent carry-transfer functions, low interval first:
/// c_out = G | (P & c_in). Composition is associative (SYNTH-REQ-0405).
fn join_carry(g: &mut Aig, lo: (Lit, Lit), hi: (Lit, Lit)) -> (Lit, Lit) {
    let passed = g.and(hi.1, lo.0);
    (g.or(hi.0, passed), g.and(hi.1, lo.1))
}

fn apply_carry(g: &mut Aig, pair: (Lit, Lit), cin: Lit) -> Lit {
    let passed = g.and(pair.1, cin);
    g.or(pair.0, passed)
}

/// Pair reduction followed by expansion gives all carry prefixes in linear work.
fn carry_prefix(g: &mut Aig, pairs: &[(Lit, Lit)], cin: Lit) -> Bv {
    if pairs.len() == 1 {
        return vec![apply_carry(g, pairs[0], cin)];
    }
    let upper: Vec<_> = pairs
        .chunks(2)
        .map(|p| {
            if p.len() == 1 {
                p[0]
            } else {
                join_carry(g, p[0], p[1])
            }
        })
        .collect();
    let carries = carry_prefix(g, &upper, cin);
    (0..pairs.len())
        .map(|i| {
            if i % 2 == 1 {
                carries[i / 2]
            } else {
                apply_carry(g, pairs[i], if i == 0 { cin } else { carries[i / 2 - 1] })
            }
        })
        .collect()
}

/// Four-bit local chains and a prefix over their carry-transfer functions.
fn sparse_add(g: &mut Aig, a: &[Lit], b: &[Lit], cin: Lit, width: usize) -> (Bv, Lit) {
    let blocks: Vec<_> = a
        .chunks(width)
        .zip(b.chunks(width))
        .map(|(a, b)| {
            let mut gp = (Lit::FALSE, Lit::TRUE);
            for (&x, &y) in a.iter().zip(b) {
                let generate = g.and(x, y);
                let propagate = if g.xor_carry { g.xor(x, y) } else { g.or(x, y) };
                let bit = (generate, propagate);
                gp = join_carry(g, gp, bit);
            }
            gp
        })
        .collect();
    let carries = carry_prefix(g, &blocks, cin);
    let mut sum = Vec::with_capacity(a.len());
    for (k, (a, b)) in a.chunks(width).zip(b.chunks(width)).enumerate() {
        let mut c = if k == 0 { cin } else { carries[k - 1] };
        for (&x, &y) in a.iter().zip(b) {
            let (s, next) = full_add(g, x, y, c);
            sum.push(s);
            c = next;
        }
    }
    (sum, *carries.last().expect("nonempty addition"))
}

/// Select an addition's local carry block from its actual operand arrival levels and
/// reachable new logic. Unselected candidates are swept with other unreachable logic.
fn adaptive_add(g: &mut Aig, a: &[Lit], b: &[Lit], cin: Lit, weight: f64) -> (Bv, Lit) {
    let start = g.num_nodes() as u32;
    let mut best = None;
    for width in [1, 2, 4, 8, a.len()] {
        let (sum, carry) = sparse_add(g, a, b, cin, width);
        let depth = sum
            .iter()
            .chain(std::iter::once(&carry))
            .map(|&l| g.level(l))
            .max()
            .unwrap_or(0);
        let mut seen = rustc_hash::FxHashSet::default();
        let mut pending: Vec<_> = sum.iter().copied().chain(std::iter::once(carry)).collect();
        while let Some(l) = pending.pop() {
            if l.node() >= start && g.is_and(l.node()) && seen.insert(l.node()) {
                let (x, y) = g.fanins(l.node());
                pending.extend([x, y]);
            }
        }
        let score = depth as f64 + weight * seen.len() as f64;
        if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
            best = Some((score, sum, carry));
        }
    }
    let (_, sum, carry) = best.expect("nonempty carry choices");
    (sum, carry)
}

pub fn sub(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Bv {
    add(g, a, &not(b), Lit::TRUE).0
}

pub fn neg(g: &mut Aig, a: &[Lit]) -> Bv {
    let zero = constant(0, a.len());
    sub(g, &zero, a)
}

/// Adds a constant.
pub fn add_const(g: &mut Aig, a: &[Lit], k: i64) -> Bv {
    let kb = constant(k as u64, a.len());
    add(g, a, &kb, Lit::FALSE).0
}

pub fn eq(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Lit {
    let d: Bv = a.iter().zip(b).map(|(x, y)| g.xnor(*x, *y)).collect();
    red_and(g, &d)
}

/// Unsigned `a < b` by a borrow chain (no sum bits).
pub fn ult(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Lit {
    if a.len() == b.len() && a.len() <= 64 {
        if let (Some(x), Some(y)) = (const_u64(a), const_u64(b)) {
            return Lit::from_bool(x < y);
        }
    }
    g.has_parallel_arithmetic |= a.len() >= 32;
    if g.parallel_carry && a.len() >= 32 {
        // A sparse reduction bounds comparison depth without building every bit prefix.
        let mut groups: Vec<_> = a
            .chunks(16)
            .zip(b.chunks(16))
            .enumerate()
            .map(|(i, (a, b))| {
                let mut generate = if i == 0 { Lit::TRUE } else { Lit::FALSE };
                let mut propagate = if i == 0 { Lit::FALSE } else { Lit::TRUE };
                for (&x, &y) in a.iter().zip(b) {
                    generate = majority(g, x, !y, generate);
                    if i != 0 {
                        let p = g.or(x, !y);
                        propagate = g.and(propagate, p);
                    }
                }
                (generate, propagate)
            })
            .collect();
        while groups.len() > 1 {
            groups = groups
                .chunks(2)
                .map(|p| {
                    if p.len() == 1 {
                        p[0]
                    } else {
                        join_carry(g, p[0], p[1])
                    }
                })
                .collect();
        }
        return !groups[0].0;
    }
    // a - b = a + !b + 1; a < b iff no carry out.
    let mut c = Lit::TRUE;
    for (x, y) in a.iter().zip(b) {
        c = majority(g, *x, !*y, c);
    }
    !c
}

/// Signed `a < b`.
pub fn slt(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Lit {
    if a.is_empty() {
        return Lit::FALSE;
    }
    let mut a2 = a.to_vec();
    let mut b2 = b.to_vec();
    let n = a2.len() - 1;
    a2[n] = !a2[n];
    b2[n] = !b2[n];
    ult(g, &a2, &b2)
}

pub fn lt(g: &mut Aig, a: &[Lit], b: &[Lit], signed: bool) -> Lit {
    if signed { slt(g, a, b) } else { ult(g, a, b) }
}

/// Logical or arithmetic shift of `a` by the unsigned amount `amt`; `left` selects direction and
/// `fill` the bit shifted in (right shifts only; left shifts fill with 0).
pub fn shift(g: &mut Aig, a: &[Lit], amt: &[Lit], left: bool, fill: Lit) -> Bv {
    let w = a.len();
    let fill = if left { Lit::FALSE } else { fill };
    let mut r: Bv = a.to_vec();
    let mut over = Lit::FALSE;
    for (k, &s) in amt.iter().enumerate() {
        if s == Lit::FALSE {
            continue;
        }
        let sh = if k >= 63 { usize::MAX } else { 1usize << k };
        if sh >= w {
            over = g.or(over, s);
            continue;
        }
        let shifted: Bv = (0..w)
            .map(|i| {
                if left {
                    if i >= sh { r[i - sh] } else { Lit::FALSE }
                } else if i + sh < w {
                    r[i + sh]
                } else {
                    fill
                }
            })
            .collect();
        r = mux(g, s, &shifted, &r);
    }
    if over != Lit::FALSE {
        let f = vec![fill; w];
        r = mux(g, over, &f, &r);
    }
    r
}

/// Low `a.len()` bits of `a * b` (equal widths) using carry-save reduction and a final
/// ripple-carry adder [@wallace1964suggestion] (SYNTH-REQ-0404).
pub fn mul(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Bv {
    let product = mul_plain(g, a, b);
    g.note_multiplier(a, b, &product, false);
    product
}

fn mul_plain(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Bv {
    let w = a.len();
    let mut cols: Vec<Vec<Lit>> = vec![Vec::new(); w];
    for (i, &bi) in b.iter().enumerate().take(w) {
        if bi == Lit::FALSE {
            continue;
        }
        for (j, &aj) in a.iter().enumerate().take(w - i) {
            let p = g.and(aj, bi);
            if p != Lit::FALSE {
                cols[i + j].push(p);
            }
        }
    }
    reduce_columns(g, cols)
}

/// Significant width of a sign-extended vector: the smallest `n >= 1` such that every bit at or
/// above position `n - 1` is the same literal.
pub fn sign_width(v: &[Lit]) -> usize {
    let mut n = v.len();
    while n > 1 && v[n - 2] == v[n - 1] {
        n -= 1;
    }
    n
}

/// Low `a.len()` bits of the signed product of equal-width, sign-extended operands
/// (SYNTH-REQ-0404): over the significant widths `n` and `m`, Baugh–Wooley partial products
/// [@baugh1973twos] and the constants `2^(n-1) + 2^(m-1) + 2^(n+m-1)` give the `n + m`-bit product,
/// which is sign-extended. Operands without extension use the unsigned structure.
pub fn mul_signed(g: &mut Aig, a: &[Lit], b: &[Lit]) -> Bv {
    let w = a.len();
    let (n, m) = (sign_width(a), sign_width(b));
    if n == w && m == w {
        let product = mul_plain(g, a, b);
        g.note_multiplier(a, b, &product, true);
        return product;
    }
    let wp = w.min(n + m);
    let mut cols: Vec<Vec<Lit>> = vec![Vec::new(); wp];
    for (k, col) in cols.iter_mut().enumerate() {
        for i in 0..m.min(k + 1) {
            let j = k - i;
            if j >= n {
                continue;
            }
            let p = g.and(a[j], b[i]);
            let p = if (j == n - 1) != (i == m - 1) { !p } else { p };
            if p != Lit::FALSE {
                col.push(p);
            }
        }
    }
    for c in [n - 1, m - 1, n + m - 1] {
        if c < wp {
            cols[c].push(Lit::TRUE);
        }
    }
    let product = resize(&reduce_columns(g, cols), w, true);
    g.note_multiplier(a, b, &product, true);
    product
}

/// Sum of weighted bit columns: carry-save rounds over groups of three bits, then a
/// ripple-carry adder of the two remaining rows (SYNTH-REQ-0404).
fn reduce_columns(g: &mut Aig, mut cols: Vec<Vec<Lit>>) -> Bv {
    let w = cols.len();
    loop {
        if cols.iter().all(|c| c.len() <= 2) {
            break;
        }
        let mut next: Vec<Vec<Lit>> = vec![Vec::new(); w];
        for i in 0..w {
            let col = std::mem::take(&mut cols[i]);
            let mut k = 0;
            while col.len() - k >= 3 {
                let (s, c) = full_add(g, col[k], col[k + 1], col[k + 2]);
                next[i].push(s);
                if i + 1 < w {
                    next[i + 1].push(c);
                }
                k += 3;
            }
            next[i].extend_from_slice(&col[k..]);
        }
        cols = next;
    }
    let x: Bv = cols
        .iter()
        .map(|c| c.first().copied().unwrap_or(Lit::FALSE))
        .collect();
    let y: Bv = cols
        .iter()
        .map(|c| c.get(1).copied().unwrap_or(Lit::FALSE))
        .collect();
    add(g, &x, &y, Lit::FALSE).0
}

/// Unsigned restoring division of equal-width vectors: (quotient, remainder). Division by zero
/// yields zero for both (SYNTH-REQ-0203).
pub fn udivmod(g: &mut Aig, a: &[Lit], b: &[Lit]) -> (Bv, Bv) {
    let w = a.len();
    let mut q = vec![Lit::FALSE; w];
    // Remainder kept at w + 1 bits.
    let mut r: Bv = vec![Lit::FALSE; w + 1];
    let bx = resize(b, w + 1, false);
    for i in (0..w).rev() {
        // r = (r << 1) | a[i]
        let mut shifted = Vec::with_capacity(w + 1);
        shifted.push(a[i]);
        shifted.extend_from_slice(&r[..w]);
        let ge = !ult(g, &shifted, &bx);
        let diff = sub(g, &shifted, &bx);
        q[i] = ge;
        r = mux(g, ge, &diff, &shifted);
    }
    let rem: Bv = r[..w].to_vec();
    let zero_div = !red_or(g, b);
    let z = vec![Lit::FALSE; w];
    (mux(g, zero_div, &z, &q), mux(g, zero_div, &z, &rem))
}

/// Division and modulus with the standard's truncating definition for signed operands.
pub fn divmod(g: &mut Aig, a: &[Lit], b: &[Lit], signed: bool) -> (Bv, Bv) {
    if !signed || a.is_empty() {
        return udivmod(g, a, b);
    }
    let sa = *a.last().expect("non-empty");
    let sb = *b.last().expect("non-empty");
    let na = neg(g, a);
    let nb = neg(g, b);
    let abs_a = mux(g, sa, &na, a);
    let abs_b = mux(g, sb, &nb, b);
    let (q, r) = udivmod(g, &abs_a, &abs_b);
    let qn = neg(g, &q);
    let rn = neg(g, &r);
    let qs = g.xor(sa, sb);
    (mux(g, qs, &qn, &q), mux(g, sa, &rn, &r))
}

/// Number of one bits as a vector of width `w`.
pub fn popcount(g: &mut Aig, v: &[Lit], w: usize) -> Bv {
    if w == 0 {
        return Vec::new();
    }
    let mut cols = vec![Vec::new(); w];
    cols[0] = v.to_vec();
    add_columns(g, cols)
}

/// A sum of equal-width rows and a constant, modulo `2^width` (SYNTH-REQ-0407).
#[derive(Debug, Clone, PartialEq)]
pub struct Sum {
    pub rows: Vec<Bv>,
    /// Bits of the constant term, least significant first.
    pub konst: Vec<bool>,
}

impl Sum {
    /// The sum of one row.
    pub fn row(v: &[Lit]) -> Sum {
        Sum {
            rows: vec![v.to_vec()],
            konst: vec![false; v.len()],
        }
    }

    /// Number of row bits.
    pub fn size(&self) -> usize {
        self.rows.iter().map(Vec::len).sum()
    }

    /// Adds a constant given by its bits (same width).
    pub fn add_konst(&mut self, k: &[bool]) {
        let mut c = false;
        for (a, &b) in self.konst.iter_mut().zip(k) {
            let t = u8::from(*a) + u8::from(b) + u8::from(c);
            *a = t & 1 == 1;
            c = t >= 2;
        }
    }

    /// The negated sum: `-(r1 + ... + rn + k) = (!r1 + 1) + ... + (!rn + 1) - k`.
    pub fn negate(&self) -> Sum {
        let w = self.konst.len();
        let mut out = Sum {
            rows: self.rows.iter().map(|r| not(r)).collect(),
            konst: vec![false; w],
        };
        let mut one = vec![false; w];
        if w > 0 {
            one[0] = true;
        }
        for _ in &self.rows {
            out.add_konst(&one);
        }
        // Two's complement of the constant: !k + 1.
        let nk: Vec<bool> = self.konst.iter().map(|b| !b).collect();
        out.add_konst(&nk);
        out.add_konst(&one);
        out
    }

    /// Weighted columns: the non-constant row bits in row order, then the bits of the constant
    /// term plus every constant 1 among the rows.
    pub fn columns(&self) -> Vec<Vec<Lit>> {
        let w = self.konst.len();
        let mut cols: Vec<Vec<Lit>> = vec![Vec::new(); w];
        let mut ones = vec![0usize; w];
        for r in &self.rows {
            for (k, &l) in r.iter().enumerate().take(w) {
                match l.const_value() {
                    Some(true) => ones[k] += 1,
                    Some(false) => {}
                    None => cols[k].push(l),
                }
            }
        }
        let mut carry = 0usize;
        for k in 0..w {
            let t = ones[k] + usize::from(self.konst[k]) + carry;
            if t & 1 == 1 {
                cols[k].push(Lit::TRUE);
            }
            carry = t >> 1;
        }
        cols
    }
}

/// Sum of weighted bit columns modulo `2^cols.len()` (SYNTH-REQ-0407): carry-save rounds over
/// groups of three bits while a column holds more than two bits (the lowest more than three),
/// then one adder of the first and second bits with the third bit of the lowest column as its
/// carry-in.
pub fn add_columns(g: &mut Aig, mut cols: Vec<Vec<Lit>>) -> Bv {
    let w = cols.len();
    loop {
        if cols
            .iter()
            .enumerate()
            .all(|(i, c)| c.len() <= if i == 0 { 3 } else { 2 })
        {
            break;
        }
        let mut next: Vec<Vec<Lit>> = vec![Vec::new(); w];
        for i in 0..w {
            let col = std::mem::take(&mut cols[i]);
            let mut k = 0;
            while col.len() - k >= 3 {
                let (s, c) = full_add(g, col[k], col[k + 1], col[k + 2]);
                next[i].push(s);
                if i + 1 < w {
                    next[i + 1].push(c);
                }
                k += 3;
            }
            next[i].extend_from_slice(&col[k..]);
        }
        cols = next;
    }
    let x: Bv = cols
        .iter()
        .map(|c| c.first().copied().unwrap_or(Lit::FALSE))
        .collect();
    let y: Bv = cols
        .iter()
        .map(|c| c.get(1).copied().unwrap_or(Lit::FALSE))
        .collect();
    let cin = cols
        .first()
        .and_then(|c| c.get(2).copied())
        .unwrap_or(Lit::FALSE);
    add(g, &x, &y, cin).0
}

#[cfg(test)]
mod tests;
