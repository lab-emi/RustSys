// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `metamorphic` (SYNTH-REQ-0802).

mod common;

use common::{cell_models, check_texts, lib, synth};
use rustsys::{Effort, Goal};
use rustsys_check::Verdict;
use rustsys_fixtures::liberty::Variant;
use rustsys_fixtures::random::{random_comb, random_seq};
use rustsys_fixtures::rtl::CORPUS;

/// Resynthesizing a synthesized netlist (with behavioral models of its cells) yields a netlist
/// equivalent to it.
#[test]
fn resynthesis_is_equivalent() {
    let l = lib(Variant::Rich);
    let models = cell_models(&l);
    let mut failures = Vec::new();
    for d in CORPUS {
        let files: Vec<(String, String)> = d
            .files
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        let first = synth(&files, d.top, &l, Effort::Medium, Goal::Delay).unwrap();
        let rtl2 = vec![
            ("cells.sv".to_string(), models.clone()),
            ("first.v".to_string(), first.text.clone()),
        ];
        match synth(&rtl2, d.top, &l, Effort::Medium, Goal::Delay) {
            Err(e) => failures.push(format!("{}: resynthesis failed: {e}", d.name)),
            Ok(second) => {
                let rep = check_texts(&rtl2, d.top, &second.text, &l);
                if rep.verdict != Verdict::Equivalent {
                    failures.push(format!(
                        "{}: {:?} {:?} {:?}",
                        d.name,
                        rep.verdict,
                        rep.errors,
                        rep.mismatches.first()
                    ));
                }
                // Resynthesis never needs more sequential cells.
                if second.report.sequential_cells > first.report.sequential_cells {
                    failures.push(format!("{}: sequential cells grew", d.name));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Renaming signals changes neither the area nor the cell counts.
#[test]
fn renaming_is_invariant() {
    let l = lib(Variant::Rich);
    for seed in 0..20u64 {
        let m = random_comb(30_000 + seed, 16);
        let mut renamed = m.text.clone();
        for v in &m.vars {
            let new = format!("sig_{}_renamed", v.name);
            // Names are i<n> and o<n>; replace whole words only.
            let mut out = String::new();
            let b = renamed.as_bytes();
            let mut i = 0;
            while i < b.len() {
                let word_start = i == 0
                    || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'\'');
                if word_start && renamed[i..].starts_with(&v.name) {
                    let end = i + v.name.len();
                    if end >= b.len() || !(b[end].is_ascii_alphanumeric() || b[end] == b'_') {
                        out.push_str(&new);
                        i = end;
                        continue;
                    }
                }
                out.push(b[i] as char);
                i += 1;
            }
            renamed = out;
        }
        let a = synth(
            &[("a.sv".into(), m.text.clone())],
            &m.name,
            &l,
            Effort::Medium,
            Goal::Area,
        )
        .unwrap();
        let b = synth(
            &[("b.sv".into(), renamed)],
            &m.name,
            &l,
            Effort::Medium,
            Goal::Area,
        )
        .unwrap();
        assert_eq!(a.report.cells, b.report.cells, "seed {seed}");
    }
}

/// Reordering independent module items keeps the function: the netlist of the original module is
/// equivalent to the reordered RTL.
#[test]
fn reordering_keeps_function() {
    let l = lib(Variant::Rich);
    for seed in 0..15u64 {
        let text = random_seq(40_000 + seed);
        let top = format!("seq_{}", 40_000 + seed);
        // Reverse the order of the procedures of the wrapper module.
        let (head, tail) = text.split_at(text.find(&format!("module {top}")).unwrap());
        let mut lines: Vec<&str> = tail.lines().collect();
        let procs: Vec<usize> = (0..lines.len())
            .filter(|i| lines[*i].trim_start().starts_with("always"))
            .collect();
        let reordered: Vec<&str> = procs.iter().rev().map(|i| lines[*i]).collect();
        for (k, i) in procs.iter().enumerate() {
            lines[*i] = reordered[k];
        }
        let text2 = format!("{head}{}\n", lines.join("\n"));
        let files = vec![("orig.sv".to_string(), text)];
        let files2 = vec![("reordered.sv".to_string(), text2)];
        let out = synth(&files, &top, &l, Effort::Medium, Goal::Delay).unwrap();
        let rep = check_texts(&files2, &top, &out.text, &l);
        assert_eq!(
            rep.verdict,
            Verdict::Equivalent,
            "seed {seed}: {:?} {:?}",
            rep.errors,
            rep.mismatches.first()
        );
    }
}
