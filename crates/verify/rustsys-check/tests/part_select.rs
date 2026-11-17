// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Dynamic indexed part-selects in the checker's RTL model (SYNTH-REQ-0206). The oracle was
//! recorded by Icarus Verilog, separately from synthesis and from this model: 280 vectors of 203
//! output bits, fewer than 10,000 circuit entries and 1 MiB of scratch.

use std::collections::HashMap;

use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
use rustsys_check::circ::{B, Circuit};
use rustsys_check::rtl::{RtlOptions, build};

/// Bits `lo .. lo + n` of a hexadecimal number.
fn field(hex: &str, lo: usize, n: usize) -> u64 {
    (0..n).fold(0, |a, b| {
        let k = lo + b;
        let digit = hex
            .len()
            .checked_sub(1 + k / 4)
            .map_or(0, |p| (hex.as_bytes()[p] as char).to_digit(16).unwrap());
        a | (u64::from(digit >> (k % 4)) & 1) << b
    })
}

#[test]
fn part_selects_match_icarus_not_synthesis() {
    let source =
        include_str!("../../../../verification/fixtures/synthetic/synth/rtl/part_selects.v");
    let truth: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../verification/fixtures/synthetic/synth/part-select-icarus-truth.json"
    ))
    .unwrap();
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("sel.v", source)], PpOptions::default());
    assert!(diags.iter().all(|d| !d.is_error()));
    let mut c = Circuit::new();
    let m = build(
        &sm,
        &units,
        &mut c,
        &RtlOptions {
            state_aliases: false,
            top: Some("part_selects".into()),
            assume_full_case: false,
            assume_parallel_case: false,
            params: vec![],
            observations: vec![],
        },
    )
    .unwrap();
    let roots: Vec<B> = m.outputs.iter().flat_map(|o| o.1.iter().copied()).collect();
    let order = c.order(&roots).unwrap();
    let names = truth["outputs"].as_array().unwrap();
    let widths = truth["widths"].as_array().unwrap();
    let mut compared = 0;
    for row in truth["vectors"].as_array().unwrap() {
        let mut inputs = HashMap::new();
        for (k, name) in ["d", "i", "w"].iter().enumerate() {
            let bits = &m.inputs.iter().find(|x| x.0 == *name).unwrap().1;
            for (b, v) in bits.iter().enumerate() {
                inputs.insert(*v, (row[k].as_u64().unwrap() >> b) & 1);
            }
        }
        let mut values = HashMap::new();
        c.eval(
            &order,
            &|v| inputs.get(&v).copied().unwrap_or(0),
            &mut values,
        );
        let (value, known) = (row[3].as_str().unwrap(), row[4].as_str().unwrap());
        let mut lo = 0;
        for (name, width) in names.iter().zip(widths) {
            let (name, width) = (name.as_str().unwrap(), width.as_u64().unwrap() as usize);
            let bits = &m.outputs.iter().find(|o| o.0 == name).unwrap().1;
            let got = bits
                .iter()
                .enumerate()
                .fold(0, |n, (i, b)| n | ((Circuit::value(*b, &values) & 1) << i));
            let (value, known) = (field(value, lo, width), field(known, lo, width));
            assert_eq!(got & known, value, "{name} of {row}");
            // What Icarus reports as x is an out-of-range read: zero, as in synthesis.
            assert_eq!(got & !known, 0, "{name} of {row} out of range");
            compared += known.count_ones();
            lo += width;
        }
    }
    assert_eq!(compared, 36_988);
}
