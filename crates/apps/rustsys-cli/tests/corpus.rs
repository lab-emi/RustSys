// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `corpus` (SYNTH-REQ-0801, SYNTH-REQ-0802): every corpus design is synthesized with every
//! synthetic library and proven equivalent by the independent checker.

use rustsys::{Goal, Sources, SynthOptions, synthesize};
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;

fn run(variant: Variant, goal: Goal, only: Option<&str>) -> Vec<String> {
    let lib = rusteda_formats::liberty::read_text(&library(variant), variant.name()).unwrap();
    let mut failures = Vec::new();
    for d in CORPUS {
        if only.is_some_and(|o| o != d.name) {
            continue;
        }
        let files: Vec<(String, String)> = d
            .files
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        let sopts = SynthOptions {
            top: Some(d.top.to_string()),
            goal,
            ..SynthOptions::default()
        };
        let out = match synthesize(
            &Sources::Texts(files.clone()),
            std::slice::from_ref(&lib),
            &sopts,
        ) {
            Ok(o) => o,
            Err(e) => {
                // A library without latches cannot implement the latch design.
                if variant == Variant::Minimal && d.name == "latches" {
                    continue;
                }
                failures.push(format!(
                    "{} [{variant:?}] synthesis failed:\n  {}",
                    d.name,
                    e.messages.join("\n  ")
                ));
                continue;
            }
        };
        let copts = CheckOptions {
            rtl: Input::Texts(files),
            top: Some(d.top.to_string()),
            netlist: Input::Texts(vec![(format!("{}_mapped.v", d.name), out.text.clone())]),
            ..CheckOptions::default()
        };
        let rep = check(&copts, std::slice::from_ref(&lib));
        if rep.verdict != Verdict::Equivalent {
            failures.push(format!(
                "{} [{variant:?} {goal:?}] verdict {:?}: errors {:?} mismatches {:?}",
                d.name,
                rep.verdict,
                rep.errors,
                rep.mismatches
                    .iter()
                    .take(3)
                    .map(|m| format!("{} {} {:?}", m.kind, m.name, m.counterexample))
                    .collect::<Vec<_>>()
            ));
        } else if rep.state_pairs_checked != out.report.state_bits_retained {
            failures.push(format!(
                "{}: {} state pairs checked, {} retained",
                d.name, rep.state_pairs_checked, out.report.state_bits_retained
            ));
        }
    }
    failures
}

#[test]
fn corpus_rich_delay() {
    let f = run(
        Variant::Rich,
        Goal::Delay,
        std::env::var("WT_ONLY").ok().as_deref(),
    );
    assert!(f.is_empty(), "{}", f.join("\n"));
}

#[test]
fn corpus_rich_area() {
    let f = run(
        Variant::Rich,
        Goal::Area,
        std::env::var("WT_ONLY").ok().as_deref(),
    );
    assert!(f.is_empty(), "{}", f.join("\n"));
}

#[test]
fn corpus_reset_only() {
    let f = run(
        Variant::ResetOnly,
        Goal::Delay,
        std::env::var("WT_ONLY").ok().as_deref(),
    );
    assert!(f.is_empty(), "{}", f.join("\n"));
}

#[test]
fn corpus_minimal() {
    let f = run(
        Variant::Minimal,
        Goal::Area,
        std::env::var("WT_ONLY").ok().as_deref(),
    );
    assert!(f.is_empty(), "{}", f.join("\n"));
}

#[test]
fn corpus_full() {
    let f = run(
        Variant::Full,
        Goal::Delay,
        std::env::var("WT_ONLY").ok().as_deref(),
    );
    assert!(f.is_empty(), "{}", f.join("\n"));
}
