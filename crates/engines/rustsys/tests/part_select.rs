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
    let (units, diags) = read_texts(&mut sm, &[("m.sv", source)], PpOptions::default());
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

fn aligned() -> ElabOptions {
    ElabOptions {
        aligned_selects: true,
        ..Default::default()
    }
}

/// Bit-parallel comparison of every output of two elaborations of one source, for random
/// values of every input; returns the number of vectors.
fn same_function(a: &Design, b: &Design, rounds: usize) -> usize {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let n = a.num_input_bits() + a.states.len();
    assert_eq!(n, b.num_input_bits() + b.states.len());
    for round in 0..rounds {
        let words: Vec<u64> = (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            })
            .collect();
        let (sa, sb) = (a.simulate(&words), b.simulate(&words));
        for (pa, pb) in a.ports.iter().zip(&b.ports).filter(|(p, _)| !p.input) {
            assert_eq!(pa.name, pb.name);
            for (k, (la, lb)) in pa.bits.iter().zip(&pb.bits).enumerate() {
                assert_eq!(
                    rustsys::aig::Aig::lit_value(&sa, *la),
                    rustsys::aig::Aig::lit_value(&sb, *lb),
                    "{}[{k}] in round {round}",
                    pa.name
                );
            }
        }
        for (xa, xb) in a.states.iter().zip(&b.states) {
            assert_eq!(xa.name, xb.name);
            assert_eq!(
                rustsys::aig::Aig::lit_value(&sa, xa.d),
                rustsys::aig::Aig::lit_value(&sb, xb.d),
                "next state of {} in round {round}",
                xa.name
            );
        }
    }
    rounds * 64
}

/// AND nodes of the logic of `roots`.
fn ands(d: &Design, roots: &[rustsys::aig::Lit]) -> usize {
    let cone = d.aig.cone(roots);
    cone.iter().filter(|n| d.aig.is_and(**n)).count()
}

fn cone(d: &Design, port: &str) -> usize {
    ands(d, &d.port(port).unwrap().bits)
}

// SYNTH-REQ-0419: the decoded lowering has the function Icarus records and the function of the
// shifted lowering (64,000 random vectors of the 23 input bits). Windows that overlap keep the
// shifter.
#[test]
fn aligned_part_selects_match_independent_icarus_truth() {
    let shifted = elaborate_with(SOURCE, &ElabOptions::default());
    let decoded = elaborate_with(SOURCE, &aligned());
    assert_eq!(matches_icarus(&decoded, "decoded"), 36_988);
    assert_eq!(same_function(&shifted, &decoded, 1000), 64_000);
    for port in ["r_bits", "w_bits"] {
        assert_eq!(cone(&decoded, port), cone(&shifted, port), "{port}");
    }
    for port in [
        "r_up", "r_down", "r_low", "r_asc", "r_sum", "w_up", "w_low", "w_bit", "w_two",
    ] {
        assert!(cone(&decoded, port) < cone(&shifted, port), "{port}");
    }
}

// SYNTH-REQ-0419 in sequential logic: a buffer of eight packed words written and read at
// pointers, a bit set at a dynamic position, and an index of eleven bits, which is beyond the
// table. 512 state bits, the flags by an index of eight bits (256 table rows); 64,000 random
// vectors of inputs and states.
#[test]
fn aligned_selects_in_registers_keep_the_next_state_functions() {
    let source = r"module buffer(input clk, input we, input [2:0] wp, rp, input [31:0] din,
                         input [7:0] k, input [10:0] far, output [31:0] dout,
                         output reg [255:0] flags, output [1:0] pair);
           reg [255:0] data;
           wire [4095:0] wide = {16{data}};
           always @(posedge clk) begin
               if (we) data[wp * 32 +: 32] <= din;
               flags[k] <= we;
           end
           assign dout = data[rp * 32 +: 32];
           assign pair = wide[far * 2 +: 2];
           endmodule";
    let shifted = elaborate_with(source, &ElabOptions::default());
    let decoded = elaborate_with(source, &aligned());
    assert_eq!(shifted.states.len(), 512);
    assert_eq!(same_function(&shifted, &decoded, 1000), 64_000);
    let next = |d: &Design, name: &str| -> Vec<rustsys::aig::Lit> {
        let states = d.states.iter().filter(|s| s.name.starts_with(name));
        states.map(|s| s.d).collect()
    };
    for (what, d, s) in [
        (
            "data",
            ands(&decoded, &next(&decoded, "data")),
            ands(&shifted, &next(&shifted, "data")),
        ),
        (
            "flags",
            ands(&decoded, &next(&decoded, "flags")),
            ands(&shifted, &next(&shifted, "flags")),
        ),
        ("dout", cone(&decoded, "dout"), cone(&shifted, "dout")),
    ] {
        assert!(d < s, "{what}: {d} decoded, {s} shifted");
    }
    // The pointer of eleven bits is not tabulated: the same structure.
    assert_eq!(cone(&decoded, "pair"), cone(&shifted, "pair"));
}

// SYNTH-REQ-0419, SYNTH-REQ-0206 with values computed here, apart from both lowerings: an offset
// that counts packed elements of four bits, and windows of a plain vector with bits between
// them that start below it. All index values, 512 random data and write values each.
#[test]
fn aligned_selects_of_elements_and_of_spaced_windows_have_the_written_values() {
    let source = r"module elements(input logic [2:0] i, input logic [7:0][3:0] p,
                          input logic [31:0] v, input logic [15:0][3:0] big,
                          input logic [1:0][3:0] w, output logic [1:0][3:0] r,
                          output logic [7:0][3:0] q, output logic [3:0] rs,
                          output logic [31:0] qs, output logic [1:0][3:0] rb);
           wire signed [7:0] s = {5'b0, i};
           assign r = p[i * 2 +: 2];
           assign rs = v[s * 8 - 2 +: 4];
           assign rb = big[i[0] * 8 +: 2];
           always_comb begin
               q = p;
               q[i * 2 +: 2] = w;
               qs = v;
               qs[s * 8 - 2 +: 4] = w[0];
           end
           endmodule";
    for (what, opts) in [("shifted", ElabOptions::default()), ("decoded", aligned())] {
        let d = elaborate_with(source, &opts);
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..512 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let (p, w) = (seed & 0xFFFF_FFFF, (seed >> 32) & 0xFF);
            let v = (seed >> 24) & 0xFFFF_FFFF;
            for i in 0..8u64 {
                let big = seed.rotate_left(17);
                let inputs = [("i", i), ("p", p), ("v", v), ("big", big), ("w", w)];
                let e = Eval::new(&d, &inputs, &[]);
                // Two elements at element 0 or 8 of sixteen: a stride wider than the window.
                assert_eq!(
                    e.out("rb"),
                    (big >> (32 * (i & 1))) & 0xFF,
                    "{what}: rb i={i}"
                );
                // Two elements at element 2i: byte i, out of range from i = 4.
                let (r, q) = if i < 4 {
                    (
                        (p >> (8 * i)) & 0xFF,
                        (p & !(0xFF << (8 * i))) | (w << (8 * i)),
                    )
                } else {
                    (0, p)
                };
                assert_eq!(e.out("r"), r, "{what}: r p={p:#x} i={i}");
                assert_eq!(e.out("q"), q, "{what}: q p={p:#x} w={w:#x} i={i}");
                // Four bits at 8i - 2.
                let (mut rs, mut qs) = (0, v);
                for b in 0..4 {
                    let at = 8 * i as i64 - 2 + b;
                    if (0..32).contains(&at) {
                        rs |= ((v >> at) & 1) << b;
                        qs = (qs & !(1 << at)) | (((w >> b) & 1) << at);
                    }
                }
                assert_eq!(e.out("rs"), rs, "{what}: rs v={v:#x} i={i}");
                assert_eq!(e.out("qs"), qs, "{what}: qs v={v:#x} w={w:#x} i={i}");
            }
        }
    }
}
