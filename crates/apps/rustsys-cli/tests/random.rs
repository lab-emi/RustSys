// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `random` (SYNTH-REQ-0802): seeded random RTL, synthesized with every synthetic library
//! and proven equivalent by the checker.

mod common;

use common::{check_texts, lib, synth};
use rustsys::{Effort, Goal};
use rustsys_check::Verdict;
use rustsys_fixtures::liberty::Variant;
use rustsys_fixtures::random::{random_comb, random_seq};

fn seeds(base: u64, n: u64) -> impl Iterator<Item = u64> {
    let n = std::env::var("WT_RANDOM_N")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(n);
    (0..n).map(move |i| base + i)
}

#[test]
fn random_combinational() {
    let mut failures = Vec::new();
    for v in Variant::ALL {
        let l = lib(v);
        for seed in seeds(1000, 60) {
            let m = random_comb(seed, 20);
            let files = vec![(format!("{}.sv", m.name), m.text.clone())];
            let effort = [Effort::Low, Effort::Medium, Effort::High][(seed % 3) as usize];
            match synth(&files, &m.name, &l, effort, Goal::Delay) {
                Err(e) => failures.push(format!(
                    "seed {seed} {v:?}: synthesis failed: {e}\n{}",
                    m.text
                )),
                Ok(out) => {
                    let rep = check_texts(&files, &m.name, &out.text, &l);
                    if rep.verdict != Verdict::Equivalent {
                        failures.push(format!(
                            "seed {seed} {v:?}: {:?} {:?} {:?}\n{}",
                            rep.verdict,
                            rep.errors,
                            rep.mismatches.first(),
                            m.text
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn random_sequential() {
    let mut failures = Vec::new();
    for v in Variant::ALL {
        let l = lib(v);
        for seed in seeds(5000, 25) {
            let text = random_seq(seed);
            let top = format!("seq_{seed}");
            let files = vec![(format!("{top}.sv"), text.clone())];
            match synth(&files, &top, &l, Effort::Medium, Goal::Area) {
                Err(e) => {
                    // The minimal library has no latch cell.
                    if !(v == Variant::Minimal && e.contains("latch")) {
                        failures.push(format!("seed {seed} {v:?}: synthesis failed: {e}\n{text}"));
                    }
                }
                Ok(out) => {
                    let rep = check_texts(&files, &top, &out.text, &l);
                    if rep.verdict != Verdict::Equivalent {
                        failures.push(format!(
                            "seed {seed} {v:?}: {:?} {:?} {:?}\n{text}",
                            rep.verdict,
                            rep.errors,
                            rep.mismatches.first()
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
