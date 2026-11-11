// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Dynamic indexed part-selects, read and written (SYNTH-REQ-0206, SYNTH-REQ-0403). The oracle
//! is a truth table recorded by Icarus Verilog, separately from synthesis: 280 vectors of 203
//! output bits, fewer than 2,000 AIG nodes and 1 MiB of scratch.

mod common;

use common::Eval;
use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
use rustsys::elab::{Design, ElabOptions, elaborate};

const SOURCE: &str =
    include_str!("../../../../verification/fixtures/synthetic/synth/rtl/part_selects.v");
const TRUTH: &str =
    include_str!("../../../../verification/fixtures/synthetic/synth/part-select-icarus-truth.json");

fn elaborate_with(source: &str, opts: &ElabOptions) -> Design {
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("m.v", source)], PpOptions::default());
    assert!(!diags.iter().any(|d| d.is_error()));
    let (d, diags) = elaborate(&sm, &units, opts);
    assert!(!diags.iter().any(|d| d.is_error()));
    d.unwrap()
}

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

/// Every bit Icarus knows has its value; every bit it reports as x, an out-of-range read, is
/// zero. Returns the number of known bits compared.
fn matches_icarus(d: &Design, what: &str) -> usize {
    let truth: serde_json::Value = serde_json::from_str(TRUTH).unwrap();
    let outputs = truth["outputs"].as_array().unwrap();
    let widths = truth["widths"].as_array().unwrap();
    let mut compared = 0;
    for row in truth["vectors"].as_array().unwrap() {
        let input = |k: usize| row[k].as_u64().unwrap();
        let (value, known) = (row[3].as_str().unwrap(), row[4].as_str().unwrap());
        let e = Eval::new(d, &[("d", input(0)), ("i", input(1)), ("w", input(2))], &[]);
        let mut lo = 0;
        for (name, width) in outputs.iter().zip(widths) {
            let (name, width) = (name.as_str().unwrap(), width.as_u64().unwrap() as usize);
            let (value, known) = (field(value, lo, width), field(known, lo, width));
            let got = e.out(name);
            assert_eq!(
                got & known,
                value,
                "{what}: {name} d={:#x} i={} w={:#x}",
                input(0),
                input(1),
                input(2)
            );
            assert_eq!(got & !known, 0, "{what}: {name} out of range");
            compared += known.count_ones() as usize;
            lo += width;
        }
    }
    compared
}

#[test]
fn part_selects_match_independent_icarus_truth() {
    let d = elaborate_with(SOURCE, &ElabOptions::default());
    assert_eq!(matches_icarus(&d, "shifted"), 36_988);
}
