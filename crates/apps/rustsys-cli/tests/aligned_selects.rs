// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `aligned_selects` (SYNTH-REQ-0419): netlists synthesized with `aligned_selects` are
//! checked against their RTL by the independent checker, which has its own RTL model and no
//! such option; a netlist cell replaced by a cell of another function is reported; and for a
//! buffer of packed words the decoded netlist is the smaller one.
//!
//! Memory: designs of fewer than 10,000 AIG nodes and 400 state bits; each check stays below
//! 200 MiB. The real-PDK tests share the two parsed libraries through a `OnceLock` (about
//! 200 MiB) and run with at most 4 test threads.

use std::path::PathBuf;
use std::sync::OnceLock;

use rusteda_formats::liberty::{Library, read_text};
use rustsys::{Sources, SynthOptions, SynthOutput, synthesize};
use rustsys_check::{CheckOptions, CheckReport, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

/// Indexed part-selects read and written, in range and out of it (the Icarus-oracle fixture).
const SELECTS: &str =
    include_str!("../../../../verification/fixtures/synthetic/synth/rtl/part_selects.v");

/// Eight packed words written and read at pointers, and a bit set at a dynamic position.
const BUFFER: &str = r"
module buffer(input clk, input rst_n, input we, input [2:0] wp, rp, input [31:0] din,
              input [5:0] k, output [31:0] dout, output reg [63:0] flags);
  reg [255:0] data;
  always @(posedge clk or negedge rst_n)
    if (!rst_n) begin
      data <= 256'd0;
      flags <= 64'd0;
    end else begin
      if (we) data[wp * 32 +: 32] <= din;
      flags[k] <= we;
    end
  assign dout = data[rp * 32 +: 32];
endmodule
";

fn fixture_lib() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| read_text(&library(Variant::Rich), "rich.lib").unwrap())
}

/// The pinned SKY130 HD and gf180mcu 7-track typical libraries of `RUSTSYS_PDK_ROOT`. A missing
/// library is an error, never a pass.
fn pdk_libs() -> &'static [Library; 2] {
    static LIBS: OnceLock<[Library; 2]> = OnceLock::new();
    LIBS.get_or_init(|| {
        let root = std::env::var_os("RUSTSYS_PDK_ROOT")
            .map(PathBuf::from)
            .expect("UNVERIFIED: RUSTSYS_PDK_ROOT is required");
        [
            "sky130_fd_sc_hd__tt_025C_1v80.lib",
            "gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib",
        ]
        .map(|name| {
            let p = root.join(name);
            let text = std::fs::read_to_string(&p).expect("UNVERIFIED: pinned PDK missing");
            assert!(text.len() < 64 * 1024 * 1024, "PDK source byte guard");
            read_text(&text, &p.to_string_lossy()).unwrap()
        })
    })
}

fn synth(lib: &Library, source: &str, top: &str, aligned_selects: bool) -> SynthOutput {
    synthesize(
        &Sources::Texts(vec![("top.v".into(), source.into())]),
        std::slice::from_ref(lib),
        &SynthOptions {
            top: Some(top.into()),
            aligned_selects,
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e.messages.join("\n")))
}

fn against_rtl(lib: &Library, source: &str, top: &str, netlist: &str) -> CheckReport {
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![("top.v".into(), source.into())]),
            top: Some(top.into()),
            netlist: Input::Texts(vec![("netlist.v".into(), netlist.into())]),
            cycles: 16,
            ..Default::default()
        },
        std::slice::from_ref(lib),
    )
}

/// At most `most` netlists, evenly spread, that differ from `netlist` in one instance: a cell
/// of `swaps` replaced by its partner, a cell with the same pin names and another function.
fn mutants(netlist: &str, swaps: &[(&str, &str)], most: usize) -> Vec<(usize, String)> {
    let lines: Vec<&str> = netlist.lines().collect();
    let mut out = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let cell = line.split_whitespace().next().unwrap_or_default();
        let Some(other) = swaps.iter().find_map(|(a, b)| {
            if cell == *a {
                Some(*b)
            } else if cell == *b {
                Some(*a)
            } else {
                None
            }
        }) else {
            continue;
        };
        let mut m = lines.clone();
        let changed = line.replacen(cell, other, 1);
        m[k] = &changed;
        out.push((k + 1, m.join("\n") + "\n"));
    }
    let step = out.len().div_ceil(most).max(1);
    out.into_iter().step_by(step).collect()
}

/// Both lowerings of both designs are equivalent to the RTL for the independent checker, the
/// decoded buffer is smaller, and replaced cells of the decoded netlists are reported, at
/// least `least` of them per design.
fn decoded_netlists_check(lib: &Library, swaps: &[(&str, &str)], least: usize) {
    for (source, top, outputs, pairs) in [
        (SELECTS, "part_selects", 203, 0),
        (BUFFER, "buffer", 96, 320),
    ] {
        let shifted = synth(lib, source, top, false);
        let decoded = synth(lib, source, top, true);
        assert!(decoded.report.aligned_selects && !shifted.report.aligned_selects);
        assert_ne!(
            shifted.text, decoded.text,
            "{top}: the option changes nothing"
        );
        for out in [&shifted, &decoded] {
            let r = against_rtl(lib, source, top, &out.text);
            assert_eq!(r.verdict, Verdict::Equivalent, "{top}: {r:?}");
            assert_eq!(r.outputs_checked, outputs, "{top}");
            assert_eq!(r.state_pairs_checked, pairs, "{top}");
            assert!(r.unmatched_rtl_state_bits.is_empty(), "{top}: {r:?}");
        }
        if top == "buffer" {
            assert!(
                decoded.report.area < shifted.report.area,
                "{top}: area {} decoded, {} shifted",
                decoded.report.area,
                shifted.report.area
            );
        }
        let all = mutants(&decoded.text, swaps, 24);
        assert!(all.len() >= least, "{top}: only {} mutants", all.len());
        for (line, m) in &all {
            let r = against_rtl(lib, source, top, m);
            assert_eq!(
                r.verdict,
                Verdict::NotEquivalent,
                "{top}: the cell of line {line} replaced: {:?}\n{}",
                r.errors,
                decoded.text.lines().nth(line - 1).unwrap_or_default()
            );
        }
    }
}

// SYNTH-REQ-0419: the decoded netlists are equivalent to the RTL, and replaced cells are found.
#[test]
fn decoded_netlists_check_against_the_rtl() {
    decoded_netlists_check(
        fixture_lib(),
        &[
            ("wt_and2_x1", "wt_or2_x1"),
            ("wt_nand2_x1", "wt_nor2_x1"),
            ("wt_xor2_x1", "wt_xnor2_x1"),
        ],
        8,
    );
}

#[test]
#[ignore = "needs the pinned SKY130 library in RUSTSYS_PDK_ROOT"]
fn sky130_decoded_netlists_check_against_the_rtl() {
    decoded_netlists_check(
        &pdk_libs()[0],
        &[
            ("sky130_fd_sc_hd__and2_1", "sky130_fd_sc_hd__or2_1"),
            ("sky130_fd_sc_hd__nand2_1", "sky130_fd_sc_hd__nor2_1"),
            ("sky130_fd_sc_hd__and3_1", "sky130_fd_sc_hd__or3_1"),
            ("sky130_fd_sc_hd__and4_1", "sky130_fd_sc_hd__or4_1"),
            ("sky130_fd_sc_hd__nand4_1", "sky130_fd_sc_hd__nor4_1"),
        ],
        4,
    );
}

#[test]
#[ignore = "needs the pinned gf180mcu library in RUSTSYS_PDK_ROOT"]
fn gf180mcu_decoded_netlists_check_against_the_rtl() {
    decoded_netlists_check(
        &pdk_libs()[1],
        &[
            (
                "gf180mcu_fd_sc_mcu7t5v0__and2_1",
                "gf180mcu_fd_sc_mcu7t5v0__or2_1",
            ),
            (
                "gf180mcu_fd_sc_mcu7t5v0__nand2_1",
                "gf180mcu_fd_sc_mcu7t5v0__nor2_1",
            ),
            (
                "gf180mcu_fd_sc_mcu7t5v0__nand2_2",
                "gf180mcu_fd_sc_mcu7t5v0__nor2_2",
            ),
            (
                "gf180mcu_fd_sc_mcu7t5v0__nand3_1",
                "gf180mcu_fd_sc_mcu7t5v0__nor3_1",
            ),
            (
                "gf180mcu_fd_sc_mcu7t5v0__nand4_2",
                "gf180mcu_fd_sc_mcu7t5v0__nor4_2",
            ),
            (
                "gf180mcu_fd_sc_mcu7t5v0__and4_1",
                "gf180mcu_fd_sc_mcu7t5v0__or4_1",
            ),
        ],
        4,
    );
}
