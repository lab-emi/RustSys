// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use super::*;

#[test]
fn native_constant_arithmetic_preserves_width_and_carry() {
    for w in [0usize, 1, 4, 8, 32, 63, 64] {
        let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
        for x in [0u64, 1, 7, u64::MAX, 0x8000_0000_0000_0000] {
            for y in [0u64, 1, 3, u64::MAX] {
                for cin in [false, true] {
                    let mut g = Aig::new();
                    let a = constant(x, w);
                    let b = constant(y, w);
                    let expected = u128::from(x & mask) + u128::from(y & mask) + u128::from(cin);
                    let (s, carry) = add(&mut g, &a, &b, Lit::from_bool(cin));
                    assert_eq!(const_u64(&s), Some(expected as u64 & mask));
                    assert_eq!(carry.const_value(), Some(expected >> w != 0));
                    assert_eq!(
                        ult(&mut g, &a, &b).const_value(),
                        Some((x & mask) < (y & mask))
                    );
                    assert_eq!(g.num_ands(), 0);
                }
            }
        }
    }
}

/// Evaluates vectors of an AIG whose inputs are the bits of `ins` (least significant first).
fn eval(g: &Aig, v: &[Lit], inputs: &[u64], widths: &[usize]) -> u64 {
    let mut words = Vec::new();
    for (x, w) in inputs.iter().zip(widths) {
        for i in 0..*w {
            words.push(if (x >> i) & 1 == 1 { u64::MAX } else { 0 });
        }
    }
    let sim = g.simulate(&words);
    v.iter().enumerate().fold(0u64, |acc, (i, l)| {
        acc | ((Aig::lit_value(&sim, *l) & 1) << i)
    })
}

fn inputs(g: &mut Aig, w: usize) -> Bv {
    (0..w).map(|_| g.input()).collect()
}

// SYNTH-REQ-0405, SYNTH-REQ-0559: arbitrary incoming carries and partial groups preserve integer arithmetic.
#[test]
fn parallel_carry_arithmetic() {
    for (weight, xor_carry) in [
        None,
        Some(0.0),
        Some(0.01),
        Some(0.025),
        Some(0.08),
        Some(0.2),
        Some(0.5),
    ]
    .into_iter()
    .flat_map(|weight| [false, true].map(move |xor| (weight, xor)))
    {
        for w in [1usize, 3, 7, 8, 9, 15, 16, 31, 32, 33, 63] {
            let mut g = Aig::new();
            g.parallel_carry = true;
            g.carry_cost_weight = weight;
            g.xor_carry = xor_carry;
            let a = inputs(&mut g, w);
            let b = inputs(&mut g, w);
            let cin = g.input();
            let (sum, carry) = add(&mut g, &a, &b, cin);
            let diff = sub(&mut g, &a, &b);
            let less = ult(&mut g, &a, &b);
            let mask = (1u64 << w) - 1;
            let mut seed = 0x5269_4851_7225_1937u64;
            let count = if w == 8 { 1 << 17 } else { 2048 };
            for i in 0..count {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let (x, y, c) = if w == 8 {
                    (i & 255, (i >> 8) & 255, i >> 16)
                } else if i < 2 * w as u64 {
                    (mask >> (i / 2), 1, i & 1)
                } else {
                    (seed & mask, seed.rotate_left(23) & mask, i & 1)
                };
                let ev = |v: &[Lit]| eval(&g, v, &[x, y, c], &[w, w, 1]);
                let expected = u128::from(x) + u128::from(y) + u128::from(c);
                assert_eq!(ev(&sum), expected as u64 & mask, "sum w={w} {x}+{y}+{c}");
                assert_eq!(ev(&[carry]), (expected >> w) as u64, "carry w={w}");
                assert_eq!(ev(&diff), x.wrapping_sub(y) & mask, "difference w={w}");
                assert_eq!(ev(&[less]), u64::from(x < y), "comparison w={w}");
            }
        }
    }
}

// SYNTH-REQ-0203, SYNTH-REQ-0204, SYNTH-REQ-0403: exhaustive 4-bit operator checks.
#[test]
fn exhaustive_four_bit_operators() {
    let mut g = Aig::new();
    let a = inputs(&mut g, 4);
    let b = inputs(&mut g, 4);
    let sum = add(&mut g, &a, &b, Lit::FALSE).0;
    let diff = sub(&mut g, &a, &b);
    let prod = mul(&mut g, &a, &b);
    let (uq, ur) = udivmod(&mut g, &a, &b);
    let (sq, sr) = divmod(&mut g, &a, &b, true);
    let lt_u = ult(&mut g, &a, &b);
    let lt_s = slt(&mut g, &a, &b);
    let e = eq(&mut g, &a, &b);
    let shl = shift(&mut g, &a, &b, true, Lit::FALSE);
    let sra = shift(&mut g, &a, &b, false, a[3]);
    let pc = popcount(&mut g, &a, 3);
    let sx = |v: u64| -> i64 { if v & 8 != 0 { v as i64 - 16 } else { v as i64 } };
    for x in 0..16u64 {
        for y in 0..16u64 {
            let ev = |v: &[Lit]| eval(&g, v, &[x, y], &[4, 4]);
            assert_eq!(ev(&sum), (x + y) & 15);
            assert_eq!(ev(&diff), x.wrapping_sub(y) & 15);
            assert_eq!(ev(&prod), (x * y) & 15);
            let (eq_, er_) = (x.checked_div(y).unwrap_or(0), x.checked_rem(y).unwrap_or(0));
            assert_eq!((ev(&uq), ev(&ur)), (eq_, er_), "{x}/{y}");
            let (xs, ys) = (sx(x), sx(y));
            let (q, r) = if ys == 0 {
                (0, 0)
            } else {
                (xs.wrapping_div(ys), xs.wrapping_rem(ys))
            };
            assert_eq!(
                (ev(&sq), ev(&sr)),
                ((q as u64) & 15, (r as u64) & 15),
                "{xs}/{ys}"
            );
            assert_eq!(ev(&[lt_u]), u64::from(x < y));
            assert_eq!(ev(&[lt_s]), u64::from(xs < ys));
            assert_eq!(ev(&[e]), u64::from(x == y));
            assert_eq!(ev(&shl), if y >= 4 { 0 } else { (x << y) & 15 });
            let expect_sra = if y >= 4 {
                if xs < 0 { 15 } else { 0 }
            } else {
                ((xs >> y) as u64) & 15
            };
            assert_eq!(ev(&sra), expect_sra);
            assert_eq!(ev(&pc), u64::from(x.count_ones()));
        }
    }
}

#[test]
fn constants_and_resizing() {
    assert_eq!(const_u64(&constant(0xA5, 8)), Some(0xA5));
    assert_eq!(const_i64(&constant(0xFE, 8), true), Some(-2));
    assert_eq!(const_i64(&constant(0xFE, 8), false), Some(254));
    let v = resize(&constant(0b10, 2), 4, true);
    assert_eq!(const_u64(&v), Some(0b1110));
    let v = resize(&constant(0b10, 2), 4, false);
    assert_eq!(const_u64(&v), Some(0b0010));
}

// SYNTH-REQ-0404: the canonical multiplier gives the low `w` bits of the product for every
// operand pair, leaves out constant-0 partial products, and is built identically every time.
#[test]
fn canonical_multiplier() {
    for w in 1..=6usize {
        let mut g = Aig::new();
        let a = inputs(&mut g, w);
        let b = inputs(&mut g, w);
        let p = mul(&mut g, &a, &b);
        let mask = (1u64 << w) - 1;
        for x in 0..=mask {
            for y in 0..=mask {
                assert_eq!(
                    eval(&g, &p, &[x, y], &[w, w]),
                    (x * y) & mask,
                    "{w}: {x}*{y}"
                );
            }
        }
        let mut h = Aig::new();
        let a2 = inputs(&mut h, w);
        let b2 = inputs(&mut h, w);
        let p2 = mul(&mut h, &a2, &b2);
        assert_eq!(p, p2, "{w}: output literals differ between builds");
        assert_eq!(g.num_ands(), h.num_ands());
        for n in 0..g.num_nodes() as u32 {
            if g.is_and(n) {
                assert_eq!(g.fanins(n), h.fanins(n));
            }
        }
    }
    // A constant operand with zero bits needs fewer gates than a variable one.
    let mut g = Aig::new();
    let a = inputs(&mut g, 6);
    let k = constant(0b000101, 6);
    let before = g.num_ands();
    let p = mul(&mut g, &a, &k);
    let with_const = g.num_ands() - before;
    let b = inputs(&mut g, 6);
    let before = g.num_ands();
    mul(&mut g, &a, &b);
    assert!(with_const < g.num_ands() - before);
    for x in 0..64u64 {
        let mut words: Vec<u64> = (0..6)
            .map(|i| if (x >> i) & 1 == 1 { u64::MAX } else { 0 })
            .collect();
        words.extend([0u64; 6]);
        let sim = g.simulate(&words);
        let got = p.iter().enumerate().fold(0u64, |acc, (i, l)| {
            acc | ((Aig::lit_value(&sim, *l) & 1) << i)
        });
        assert_eq!(got, (x * 5) & 63);
    }
}

// SYNTH-REQ-0404: Baugh-Wooley signed products of sign-extended operands, every width
// combination up to 5 + 5 bits and every context width up to one beyond the product.
#[test]
fn signed_multiplier() {
    for n in 1..=5usize {
        for m in 1..=5usize {
            for w in n.max(m)..=n + m + 1 {
                let mut g = Aig::new();
                let a = resize(&inputs(&mut g, n), w, true);
                let b = resize(&inputs(&mut g, m), w, true);
                assert_eq!((sign_width(&a), sign_width(&b)), (n, m));
                let p = mul_signed(&mut g, &a, &b);
                let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
                let sx = |v: u64, bits: usize| ((v << (64 - bits)) as i64) >> (64 - bits);
                for x in 0..1u64 << n {
                    for y in 0..1u64 << m {
                        let want = (sx(x, n).wrapping_mul(sx(y, m)) as u64) & mask;
                        assert_eq!(eval(&g, &p, &[x, y], &[n, m]), want, "{n}x{m}@{w}: {x}*{y}");
                    }
                }
            }
        }
    }
}

// SYNTH-REQ-0557: the local carry choice also preserves arithmetic with skewed operands.
#[test]
fn adaptive_carry_with_unequal_input_arrivals() {
    let mut g = Aig::new();
    g.parallel_carry = true;
    g.carry_cost_weight = Some(0.025);
    let mut a = inputs(&mut g, 24);
    let b = inputs(&mut g, 24);
    let enables = inputs(&mut g, 8);
    for enable in enables {
        a[0] = g.and(a[0], enable);
    }
    let (sum, carry) = add(&mut g, &a, &b, Lit::FALSE);
    let diff = sub(&mut g, &a, &b);
    let mask = (1 << 24) - 1;
    let mut seed = 0xd917_a2fe_6417_5231u64;
    for i in 0..2048 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let x = seed & mask;
        let y = seed.rotate_left(17) & mask;
        let en = if i % 2 == 0 { 255 } else { seed >> 32 & 127 };
        let value = if en == 255 { x } else { x & !1 };
        let got = |v: &[Lit]| eval(&g, v, &[x, y, en], &[24, 24, 8]);
        assert_eq!(got(&sum), (value + y) & mask);
        assert_eq!(got(&[carry]), (value + y) >> 24);
        assert_eq!(got(&diff), value.wrapping_sub(y) & mask);
    }
}
