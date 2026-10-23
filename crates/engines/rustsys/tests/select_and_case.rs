// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Exhaustive checks of the structures chosen for case statements (SYNTH-REQ-0220: parallel merge
//! of mutually exclusive items, priority otherwise) and array reads (SYNTH-REQ-0403: decoded or
//! multiplexer-tree selection, SYNTH-REQ-0206: zero out of range).

mod common;

use common::{Eval, design};

// Narrow, non-power-of-two tables must retain exact bounds whichever lowering
// is selected. 14 input bits, 16K vectors, <100 AIG nodes and <1 MiB scratch.
#[test]
fn narrow_partial_table_has_exact_data_and_zero_padding() {
    use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
    use rustsys::elab::{ElabOptions, elaborate};
    let source = r"module partial_table(input [9:0] data, input [3:0] index, output [1:0] y);
           logic [1:0] words [5];
           always_comb for (int j=0; j<5; j++) words[j]=data[j*2 +: 2];
           assign y=words[index];
           endmodule";
    let mut sm = SourceMap::new();
    let (units, diagnostics) = read_texts(&mut sm, &[("partial.sv", source)], PpOptions::default());
    assert!(!diagnostics.iter().any(|d| d.is_error()));
    for prefer_narrow_muxes in [false, true] {
        let opts = ElabOptions {
            prefer_narrow_muxes,
            ..Default::default()
        };
        let (d, diagnostics) = elaborate(&sm, &units, &opts);
        assert!(!diagnostics.iter().any(|d| d.is_error()));
        let d = d.unwrap();
        for data in 0..1024u64 {
            for index in 0..16u64 {
                let expected = if index < 5 {
                    (data >> (index * 2)) & 3
                } else {
                    0
                };
                let got = Eval::new(&d, &[("data", data), ("index", index)], &[]).out("y");
                assert_eq!(
                    got, expected,
                    "muxes={prefer_narrow_muxes} data={data} index={index}"
                );
            }
        }
    }
}

// SYNTH-REQ-0201: an independently captured Icarus truth table is the oracle.
// 8 inputs, 256 bounded vectors, <100 gates and <1 MiB test scratch.
#[test]
fn structured_wildcards_match_independent_icarus_truth() {
    let source =
        include_str!("../../../../verification/fixtures/synthetic/synth/rtl/wildcard_structures.v");
    let truth: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../verification/fixtures/synthetic/synth/wildcard-icarus-truth.json"
    ))
    .unwrap();
    let d = design(source);
    for (s, expected) in truth["values"].as_array().unwrap().iter().enumerate() {
        let e = Eval::new(&d, &[("s", s as u64)], &[]);
        assert_eq!(
            (e.out("y") << 1) | e.out("eq"),
            expected.as_u64().unwrap(),
            "input {s}"
        );
    }
}

// Constant labels (exclusive, merged in parallel), a repeated label (never taken), overlapping
// casez patterns (priority) and a default.
#[test]
fn case_items_keep_first_match_semantics() {
    let d = design(
        r#"
module cs (input logic [2:0] s, input logic [7:0] a, output logic [7:0] y, output logic [3:0] z);
  always_comb begin
    y = a;
    unique case (s)
      3'd0: y = {a[3:0], a[7:4]};
      3'd2, 3'd5: y = 8'h5a;
      3'd2: y = 8'hff;          // repeated label: never taken
      3'd6: y = ~a;
      default: ;
    endcase
    casez (s)
      3'b1??: z = 4'd1;
      3'b11?: z = 4'd2;         // overlaps the first item: never taken
      3'b0?1: z = 4'd3;
      default: z = 4'd4;
    endcase
  end
endmodule"#,
    );
    for s in 0..8u64 {
        for a in 0..256u64 {
            let e = Eval::new(&d, &[("s", s), ("a", a)], &[]);
            let y = match s {
                0 => ((a & 0xf) << 4) | (a >> 4),
                2 | 5 => 0x5a,
                6 => !a & 0xff,
                _ => a,
            };
            let z = if s & 4 != 0 {
                1
            } else if s & 1 == 1 {
                3
            } else {
                4
            };
            assert_eq!(e.out("y"), y, "s={s} a={a}");
            assert_eq!(e.out("z"), z, "s={s}");
        }
    }
}

// Reads of 3, 5 and 6 elements of 1 and 8 bits (multiplexer tree and decoded selection), with
// positions beyond the last element.
#[test]
fn array_reads_all_widths() {
    let d = design(
        r#"
module ar (input logic [2:0] i, input logic [47:0] w, input logic [5:0] b,
           output logic [7:0] y5, output logic y3, output logic [7:0] y6);
  logic [7:0] m5 [5];
  logic m3 [3];
  logic [7:0] m6 [6];
  always_comb begin
    for (int k = 0; k < 5; k++) m5[k] = w[k*8 +: 8];
    for (int k = 0; k < 3; k++) m3[k] = b[k];
    for (int k = 0; k < 6; k++) m6[k] = w[k*8 +: 8] ^ {8{b[k]}};
  end
  assign y5 = m5[i];
  assign y3 = m3[i];
  assign y6 = m6[i];
endmodule"#,
    );
    let words: [u64; 3] = [0x0123_4567_89ab, 0xfedc_ba98_7654, 0x5a5a_a5a5_3c3c];
    for &w in &words {
        for b in 0..64u64 {
            for i in 0..8u64 {
                let e = Eval::new(&d, &[("i", i), ("w", w), ("b", b)], &[]);
                let byte = |k: u64| (w >> (8 * k)) & 0xff;
                let y5 = if i < 5 { byte(i) } else { 0 };
                let y3 = if i < 3 { (b >> i) & 1 } else { 0 };
                let y6 = if i < 6 {
                    byte(i) ^ if (b >> i) & 1 == 1 { 0xff } else { 0 }
                } else {
                    0
                };
                assert_eq!(e.out("y5"), y5, "i={i}");
                assert_eq!(e.out("y3"), y3, "i={i} b={b}");
                assert_eq!(e.out("y6"), y6, "i={i} b={b}");
            }
        }
    }
}

// Constant drivers can occur after the read and behind aliases. Array direction, missing
// entries, high address bits and nonconstant entries retain the normal selection semantics.
#[test]
fn resolved_rom_reads_keep_exact_values_and_bounds() {
    for ascending in [false, true] {
        let range = if ascending { "0:36" } else { "36:0" };
        let mut src = format!(
            "module rom(input [8:0] address, input [7:0] live, output [7:0] y,z);\n\
             wire [7:0] lookup_table [{range}]; wire [7:0] mixed [{range}];\n\
             assign y=lookup_table[address]; assign z=mixed[address];\n"
        );
        for i in 0..37 {
            let value = ((i * 17) ^ (i << 1)) & 255;
            src.push_str(&format!(
                "wire [7:0] v{i}; assign lookup_table[{i}]=v{i}; assign v{i}=8'd{value};\n"
            ));
            src.push_str(&format!(
                "assign mixed[{i}]={};\n",
                if i == 19 {
                    "live".into()
                } else {
                    format!("8'd{value}")
                }
            ));
        }
        src.push_str("endmodule");
        let d = design(&src);
        assert!(d.states.is_empty());
        for address in 0..512u64 {
            for live in [0u64, 0x53, 0xff] {
                let e = Eval::new(&d, &[("address", address), ("live", live)], &[]);
                let value = if address < 37 {
                    ((address * 17) ^ (address << 1)) & 255
                } else {
                    0
                };
                assert_eq!(e.out("y"), value, "address={address} ascending={ascending}");
                assert_eq!(e.out("z"), if address == 19 { live } else { value });
            }
        }
    }
}

/// Elaborates with the complete-case assumption on or off (SYNTH-REQ-0224).
fn design_with(src: &str, assume_full_case: bool) -> rustsys::elab::Design {
    design_cases(src, assume_full_case, false)
}

/// Elaborates with the complete-case (SYNTH-REQ-0224) and parallel-case (SYNTH-REQ-0225)
/// assumptions on or off.
fn design_cases(
    src: &str,
    assume_full_case: bool,
    assume_parallel_case: bool,
) -> rustsys::elab::Design {
    use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
    use rustsys::elab::{ElabOptions, elaborate};
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("m.sv", src)], PpOptions::default());
    assert!(!diags.iter().any(|d| d.is_error()));
    let opts = ElabOptions {
        prefer_narrow_muxes: false,
        parallel_carry: false,
        carry_cost_weight: None,
        xor_carry: false,
        carry_plan: Vec::new(),
        assume_full_case,
        assume_parallel_case,
        top: None,
        params: Vec::new(),
    };
    elaborate(&sm, &units, &opts).0.expect("elaborates")
}

// SYNTH-REQ-0224: a case marked complete without a default item takes its last item as the
// default under the option; without it the unlisted selector value holds (a latch). `unique0`
// and `parallel_case` have no effect, and a default item is kept.
#[test]
fn complete_case_assumption_uses_the_last_item() {
    let src = |mark: &str, dflt: &str| {
        format!(
            "module m (input [1:0] s, input [3:0] a, b, c, output reg [3:0] y);
  always @* begin
    {mark} case (s) 2'd0: y = a; 2'd1: y = b; 2'd2: y = c; {dflt} endcase
  end
endmodule"
        )
    };
    let latches = |d: &rustsys::elab::Design| {
        d.states
            .iter()
            .filter(|s| s.kind == rustsys::elab::StateKind::Latch)
            .count()
    };
    for mark in [
        "(* full_case *)",
        "unique",
        "priority",
        "(* parallel_case, full_case *)",
    ] {
        let off = design_with(&src(mark, ""), false);
        assert_eq!(latches(&off), 4, "{mark}");
        let on = design_with(&src(mark, ""), true);
        assert_eq!(latches(&on), 0, "{mark}");
        for s in 0..4u64 {
            for (a, b, c) in [(1u64, 2u64, 12u64), (5, 9, 3)] {
                let e = Eval::new(&on, &[("s", s), ("a", a), ("b", b), ("c", c)], &[]);
                assert_eq!(e.out("y"), [a, b, c, c][s as usize], "{mark} s={s}");
            }
        }
    }
    for mark in ["(* parallel_case *)", "unique0", ""] {
        assert_eq!(latches(&design_with(&src(mark, ""), true)), 4, "{mark}");
    }
    let d = design_with(&src("(* full_case *)", "default: y = 4'd7;"), true);
    let e = Eval::new(&d, &[("s", 3), ("a", 1), ("b", 2), ("c", 3)], &[]);
    assert_eq!(e.out("y"), 7);
}

// SYNTH-REQ-0225: a case marked parallel over a one-hot selector written as `case (1'b1)` with
// signal labels needs no priority logic under the option; one matching item (or none) gives the
// first-match result, two matching items give the OR of their results. `unique` is both complete
// and parallel; `priority` and unmarked cases keep first-match semantics.
#[test]
fn parallel_case_assumption_merges_items_without_priority() {
    let src = |mark: &str, items: &str| {
        format!(
            "module m (input a, b, c, input [3:0] x, y, z, output reg [3:0] o);
  always @* begin
    o = 4'd5;
    {mark} case (1'b1) {items} endcase
  end
endmodule"
        )
    };
    let three = "a: o = x; b: o = y; c: o = z;";
    let size = |d: &mut rustsys::elab::Design| {
        rustsys::opt::sweep(d);
        d.aig.num_ands()
    };
    for mark in ["(* parallel_case *)", "unique0"] {
        let mut on = design_cases(&src(mark, three), false, true);
        let mut off = design_cases(&src(mark, three), false, false);
        for bits in 0..8u64 {
            let (a, b, c) = (bits & 1, bits >> 1 & 1, bits >> 2 & 1);
            let (x, y, z) = (0b0011u64, 0b0101u64, 0b1000u64);
            let ins = [("a", a), ("b", b), ("c", c), ("x", x), ("y", y), ("z", z)];
            let first = if a == 1 {
                x
            } else if b == 1 {
                y
            } else if c == 1 {
                z
            } else {
                5
            };
            let any = a | b | c;
            let or = (if a == 1 { x } else { 0 })
                | (if b == 1 { y } else { 0 })
                | (if c == 1 { z } else { 0 })
                | (if any == 0 { 5 } else { 0 });
            assert_eq!(
                Eval::new(&off, &ins, &[]).out("o"),
                first,
                "{mark} {bits:03b}"
            );
            assert_eq!(Eval::new(&on, &ins, &[]).out("o"), or, "{mark} {bits:03b}");
            if a + b + c <= 1 {
                assert_eq!(or, first);
            }
        }
        assert!(size(&mut on) < size(&mut off), "{mark}");
    }
    // `unique` without a default: complete and parallel, the last item covers no other match.
    let d = design_cases(&src("unique", three), true, true);
    for bits in 0..8u64 {
        let (a, b, c) = (bits & 1, bits >> 1 & 1, bits >> 2 & 1);
        let (x, y, z) = (0b0011u64, 0b0101u64, 0b1000u64);
        let ins = [("a", a), ("b", b), ("c", c), ("x", x), ("y", y), ("z", z)];
        let want = (if a == 1 { x } else { 0 })
            | (if b == 1 { y } else { 0 })
            | (if a | b == 0 { z } else { 0 });
        assert_eq!(Eval::new(&d, &ins, &[]).out("o"), want, "unique {bits:03b}");
    }
    // Neither `priority` nor an unmarked case is parallel.
    for mark in ["priority", ""] {
        let d = design_cases(&src(mark, three), false, true);
        let ins = [("a", 1), ("b", 1), ("c", 0), ("x", 3), ("y", 5), ("z", 8)];
        assert_eq!(Eval::new(&d, &ins, &[]).out("o"), 3, "{mark}");
    }
}
