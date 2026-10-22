// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `check-seqsim` (CHECK-REQ-0045): sequential simulation of the RTL and the netlist over
//! clock cycles, on synthesized corpus designs and on planted defects.

mod common;

use rusteda_formats::verilog::write::write_netlist;
use rustsys::{Effort, Goal};
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_defects::{DefectClass, plant, read_netlist};
use rustsys_fixtures::liberty::Variant;
use rustsys_fixtures::rtl::CORPUS;

fn opts(rtl: &[(String, String)], top: &str, netlist: &str, cycles: usize) -> CheckOptions {
    CheckOptions {
        rtl: Input::Texts(rtl.to_vec()),
        top: Some(top.to_string()),
        netlist: Input::Texts(vec![("n.v".into(), netlist.to_string())]),
        cycles,
        ..CheckOptions::default()
    }
}

fn files(d: &rustsys_fixtures::rtl::Design) -> Vec<(String, String)> {
    d.files
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect()
}

// Equivalent designs simulate without a mismatch; designs outside the simulated class are
// skipped with a reason, never failed.
#[test]
fn corpus_simulates_without_mismatch() {
    let lib = common::lib(Variant::Rich);
    let mut simulated = 0;
    for d in CORPUS {
        let f = files(d);
        let out = common::synth(&f, d.top, &lib, Effort::Medium, Goal::Delay).unwrap();
        let rep = check(&opts(&f, d.top, &out.text, 40), std::slice::from_ref(&lib));
        assert_eq!(
            rep.verdict,
            Verdict::Equivalent,
            "{}: {:?} {:?}",
            d.name,
            rep.mismatches,
            rep.errors
        );
        match &rep.seq_sim.skipped {
            None => {
                assert_eq!(rep.seq_sim.cycles, 40, "{}", d.name);
                simulated += 1;
            }
            Some(why) => assert!(!why.is_empty()),
        }
        if d.name == "latches" {
            assert!(rep.seq_sim.skipped.is_some(), "latches must be skipped");
        }
    }
    assert!(
        simulated >= CORPUS.len() / 2,
        "only {simulated} designs simulated"
    );
}

// Without `--cycles` nothing is simulated.
#[test]
fn off_by_default() {
    let lib = common::lib(Variant::Rich);
    let d = CORPUS.iter().find(|d| d.name == "counter").unwrap();
    let f = files(d);
    let out = common::synth(&f, d.top, &lib, Effort::Medium, Goal::Delay).unwrap();
    let rep = check(&opts(&f, d.top, &out.text, 0), std::slice::from_ref(&lib));
    assert_eq!(rep.seq_sim.cycles, 0);
    assert_eq!(rep.seq_sim.skipped, None);
}

// Planted defects that change an output are also seen by the sequential simulation.
#[test]
fn planted_defects_show_in_simulation() {
    let lib = common::lib(Variant::Rich);
    let mut seen = 0;
    for name in [
        "counter",
        "shift_reg",
        "seq_det",
        "crc8",
        "lfsr",
        "pipeline",
    ] {
        let d = CORPUS.iter().find(|d| d.name == name).unwrap();
        let f = files(d);
        let out = common::synth(&f, d.top, &lib, Effort::Medium, Goal::Delay).unwrap();
        let nl = read_netlist(&out.text).unwrap();
        for (i, class) in [
            DefectClass::InvertedNet,
            DefectClass::StuckAt0,
            DefectClass::StuckAt1,
        ]
        .into_iter()
        .enumerate()
        {
            let Some(bad) = plant(&nl, &lib, class, 11 + i as u64) else {
                continue;
            };
            let rep = check(
                &opts(&f, d.top, &write_netlist(&bad), 64),
                std::slice::from_ref(&lib),
            );
            assert_ne!(rep.verdict, Verdict::Equivalent, "{name} {class:?}");
            if rep.mismatches.iter().any(|m| m.kind == "sequential") {
                seen += 1;
            }
        }
    }
    assert!(
        seen >= 6,
        "sequential simulation saw only {seen} planted defects"
    );
}

// Two clock domains are skipped.
#[test]
fn two_clock_domains_are_skipped() {
    let rtl = "module two (input logic ca, input logic cb, input logic d, output logic q);\n\
               logic m;\n always_ff @(posedge ca) m <= d;\n always_ff @(posedge cb) q <= m;\n\
               endmodule\n";
    let f = vec![("two.sv".to_string(), rtl.to_string())];
    let lib = common::lib(Variant::Rich);
    let out = common::synth(&f, "two", &lib, Effort::Medium, Goal::Delay).unwrap();
    let rep = check(&opts(&f, "two", &out.text, 16), std::slice::from_ref(&lib));
    assert_eq!(rep.verdict, Verdict::Equivalent);
    assert_eq!(rep.seq_sim.cycles, 0);
    assert!(
        rep.seq_sim
            .skipped
            .as_deref()
            .is_some_and(|w| w.contains("clock"))
    );
}
