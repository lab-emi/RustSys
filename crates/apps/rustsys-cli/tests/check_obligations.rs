// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `check-obligations` (CHECK-REQ-0040, CHECK-REQ-0041): a planted difference is reported
//! by the proof obligation that states the affected property: clock, asynchronous controls,
//! next-state function or output function.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use rusteda_formats::verilog::write::write_netlist;
use rustsys::{Effort, Goal};
use rustsys_check::{Verdict, check};
use rustsys_defects::{DefectClass, plant, read_netlist};
use rustsys_fixtures::liberty::Variant;
use rustsys_fixtures::rtl::CORPUS;

/// Mismatch kinds reported per defect class over the corpus.
fn kinds() -> BTreeMap<String, Vec<BTreeSet<String>>> {
    let lib = common::lib(Variant::Rich);
    let mut out: BTreeMap<String, Vec<BTreeSet<String>>> = BTreeMap::new();
    for d in CORPUS {
        let files: Vec<(String, String)> = d
            .files
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        let syn = common::synth(&files, d.top, &lib, Effort::Medium, Goal::Delay).unwrap();
        let nl = read_netlist(&syn.text).unwrap();
        for class in [
            DefectClass::ClockInversion,
            DefectClass::AsyncPolarity,
            DefectClass::AsyncValue,
            DefectClass::InvertedNet,
        ] {
            for seed in 0..2u64 {
                let Some(bad) = plant(&nl, &lib, class, seed + 3) else {
                    continue;
                };
                let opts = rustsys_check::CheckOptions {
                    rtl: rustsys_check::Input::Texts(files.clone()),
                    top: Some(d.top.to_string()),
                    netlist: rustsys_check::Input::Texts(vec![(
                        "bad.v".into(),
                        write_netlist(&bad),
                    )]),
                    ..Default::default()
                };
                let rep = check(&opts, std::slice::from_ref(&lib));
                if rep.verdict == Verdict::NotEquivalent {
                    out.entry(format!("{class:?}"))
                        .or_default()
                        .push(rep.mismatches.iter().map(|m| m.kind.clone()).collect());
                }
            }
        }
    }
    out
}

#[test]
fn defects_are_reported_by_their_obligation() {
    let k = kinds();
    let has = |class: &str, want: &[&str]| {
        let v = k
            .get(class)
            .unwrap_or_else(|| panic!("{class} never detected"));
        for set in v {
            assert!(
                want.iter().any(|w| set.contains(*w)),
                "{class}: kinds {set:?}, expected one of {want:?}"
            );
        }
    };
    // CHECK-REQ-0040: clock (flip-flops) or enable (latches) equality after polarity
    // normalization.
    has("ClockInversion", &["clock", "enable"]);
    // CHECK-REQ-0040: asynchronous condition and value.
    has("AsyncPolarity", &["async-clear", "async-preset"]);
    has("AsyncValue", &["async-clear", "async-preset"]);
    // CHECK-REQ-0040 and CHECK-REQ-0041: functional differences appear as next-state or output
    // mismatches.
    let all: BTreeSet<&String> = k["InvertedNet"].iter().flatten().collect();
    assert!(all.iter().any(|s| *s == "output"), "{all:?}");
    assert!(all.iter().any(|s| *s == "next-state"), "{all:?}");
}

// CHECK-REQ-0041: in a purely combinational design only output obligations exist.
#[test]
fn combinational_design_has_output_obligations_only() {
    let lib = common::lib(Variant::Rich);
    let d = CORPUS.iter().find(|d| d.name == "alu").unwrap();
    let files: Vec<(String, String)> = d
        .files
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    let syn = common::synth(&files, d.top, &lib, Effort::Medium, Goal::Delay).unwrap();
    let good = common::check_texts(&files, d.top, &syn.text, &lib);
    assert_eq!(good.verdict, Verdict::Equivalent);
    assert_eq!(good.state_pairs_checked, 0);
    assert!(good.outputs_checked > 0);
    let nl = read_netlist(&syn.text).unwrap();
    let bad = plant(&nl, &lib, DefectClass::InvertedNet, 5).expect("a net to invert");
    let rep = common::check_texts(&files, d.top, &write_netlist(&bad), &lib);
    assert_eq!(rep.verdict, Verdict::NotEquivalent);
    assert!(
        rep.mismatches.iter().all(|m| m.kind == "output"),
        "{:?}",
        rep.mismatches
    );
}
