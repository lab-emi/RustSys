// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! CHECK-REQ-0047: the checker's multiplication structure computes integer products (exhaustively
//! for small widths, randomly for wide ones), signed and unsigned.

use std::collections::HashMap;

use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
use rustsys_check::circ::{B, Circuit};
use rustsys_check::rtl::{RtlOptions, build};

/// Builds `y = a * b` (optionally signed) at width `w` and returns the circuit, the input
/// variables of `a` and `b` and the output bits.
fn model(w: u32, signed: bool) -> (Circuit, Vec<u32>, Vec<u32>, Vec<B>) {
    model_widths(w, w, w, signed)
}

/// As `model` with operand widths `n` and `m` and product width `w`.
fn model_widths(n: u32, m: u32, w: u32, signed: bool) -> (Circuit, Vec<u32>, Vec<u32>, Vec<B>) {
    let s = if signed { "signed " } else { "" };
    let src = format!(
        "module m (input logic {s}[{n1}:0] a, input logic {s}[{m1}:0] b, output logic {s}[{w1}:0] y);\n\
         assign y = a * b;\nendmodule\n",
        n1 = n - 1,
        m1 = m - 1,
        w1 = w - 1
    );
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("m.sv", &src)], PpOptions::default());
    assert!(diags.iter().all(|d| !d.is_error()));
    let mut c = Circuit::new();
    let m = build(
        &sm,
        &units,
        &mut c,
        &RtlOptions {
            assume_full_case: false,
            assume_parallel_case: false,
            top: Some("m".into()),
            params: vec![],
            observations: vec![],
        },
    )
    .unwrap();
    let a = m.inputs[0].1.clone();
    let b = m.inputs[1].1.clone();
    let y = m.outputs[0].1.clone();
    (c, a, b, y)
}

/// Evaluates 64 operand pairs at once; lane `k` uses `xs[k]` and `ys[k]`.
fn eval(c: &Circuit, a: &[u32], b: &[u32], y: &[B], xs: &[u64], ys: &[u64]) -> Vec<u64> {
    let mut words: HashMap<u32, u64> = HashMap::new();
    for (i, v) in a.iter().enumerate() {
        let w = xs
            .iter()
            .enumerate()
            .fold(0u64, |acc, (k, x)| acc | (((x >> i) & 1) << k));
        words.insert(*v, w);
    }
    for (i, v) in b.iter().enumerate() {
        let w = ys
            .iter()
            .enumerate()
            .fold(0u64, |acc, (k, x)| acc | (((x >> i) & 1) << k));
        words.insert(*v, w);
    }
    let order = c.order(y).unwrap();
    let mut vals = HashMap::new();
    c.eval(&order, &|v| words.get(&v).copied().unwrap_or(0), &mut vals);
    (0..xs.len())
        .map(|k| {
            y.iter().enumerate().fold(0u64, |acc, (i, bit)| {
                acc | (((Circuit::value(*bit, &vals) >> k) & 1) << i)
            })
        })
        .collect()
}

fn mask(w: u32) -> u64 {
    if w == 64 { u64::MAX } else { (1u64 << w) - 1 }
}

#[test]
fn products_are_exact() {
    for signed in [false, true] {
        // Exhaustive up to 6 bits (the low `w` bits agree for signed and unsigned operands).
        for w in 1..=6u32 {
            let (c, a, b, y) = model(w, signed);
            let pairs: Vec<(u64, u64)> = (0..1u64 << w)
                .flat_map(|x| (0..1u64 << w).map(move |z| (x, z)))
                .collect();
            for chunk in pairs.chunks(64) {
                let xs: Vec<u64> = chunk.iter().map(|p| p.0).collect();
                let ys: Vec<u64> = chunk.iter().map(|p| p.1).collect();
                let got = eval(&c, &a, &b, &y, &xs, &ys);
                for (k, (x, z)) in chunk.iter().enumerate() {
                    assert_eq!(got[k], x.wrapping_mul(*z) & mask(w), "w={w} {x}*{z}");
                }
            }
        }
        // Random operands for wide multipliers.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for w in [17u32, 33, 64] {
            let (c, a, b, y) = model(w, signed);
            let xs: Vec<u64> = (0..64).map(|_| rnd() & mask(w)).collect();
            let ys: Vec<u64> = (0..64).map(|_| rnd() & mask(w)).collect();
            let got = eval(&c, &a, &b, &y, &xs, &ys);
            for k in 0..64 {
                assert_eq!(got[k], xs[k].wrapping_mul(ys[k]) & mask(w), "w={w}");
            }
        }
    }
}

// Signed operands narrower than the product (the Baugh-Wooley structure), exhaustively.
#[test]
fn signed_narrow_products_are_exact() {
    let sx = |v: u64, bits: u32| ((v << (64 - bits)) as i64) >> (64 - bits);
    for n in 1..=5u32 {
        for m in 1..=5u32 {
            for w in [n.max(m), n + m - 1, n + m, n + m + 2] {
                if w < n.max(m) {
                    continue;
                }
                let (c, a, b, y) = model_widths(n, m, w, true);
                let pairs: Vec<(u64, u64)> = (0..1u64 << n)
                    .flat_map(|x| (0..1u64 << m).map(move |z| (x, z)))
                    .collect();
                for chunk in pairs.chunks(64) {
                    let xs: Vec<u64> = chunk.iter().map(|p| p.0).collect();
                    let ys: Vec<u64> = chunk.iter().map(|p| p.1).collect();
                    let got = eval(&c, &a, &b, &y, &xs, &ys);
                    for (k, (x, z)) in chunk.iter().enumerate() {
                        let want = (sx(*x, n).wrapping_mul(sx(*z, m)) as u64) & mask(w);
                        assert_eq!(got[k], want, "{n}x{m}@{w}: {x}*{z}");
                    }
                }
            }
        }
    }
}
