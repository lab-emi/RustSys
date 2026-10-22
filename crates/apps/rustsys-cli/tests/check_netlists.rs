// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `check-netlists` (CHECK-REQ-0020): the checker reads netlists whose ports are declared in
//! the module header (ANSI) or in the body (non-ANSI, optionally repeated as nets).

mod common;

use rustsys::{Effort, Goal};
use rustsys_check::Verdict;
use rustsys_fixtures::liberty::Variant;
use rustsys_fixtures::rtl::CORPUS;

/// Rewrites the ANSI header `module m(\n  input wire [..] a,\n ...\n);` of a RustSys netlist into
/// non-ANSI form; `repeat_as_wire` adds a `wire` declaration for every port.
fn non_ansi(text: &str, repeat_as_wire: bool) -> String {
    let start = text.find('(').expect("header");
    let end = text.find("\n);").expect("header end");
    let header = &text[start + 1..end];
    let mut names = Vec::new();
    let mut decls = String::new();
    for line in header.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let line = line.trim_end_matches(',');
        let (dir, rest) = line.split_once(" wire ").expect("port line");
        let name = rest.rsplit(' ').next().expect("name");
        let range = rest[..rest.len() - name.len()].trim();
        names.push(name.to_string());
        decls += &format!("  {dir} {range} {name};\n");
        if repeat_as_wire {
            decls += &format!("  wire {range} {name};\n");
        }
    }
    format!(
        "{}({});\n{}{}",
        &text[..start],
        names.join(", "),
        decls,
        &text[end + 3..]
    )
}

#[test]
fn ansi_and_non_ansi_ports() {
    let lib = common::lib(Variant::Rich);
    for name in ["counter", "alu", "fifo"] {
        let d = CORPUS.iter().find(|d| d.name == name).unwrap();
        let files: Vec<(String, String)> = d
            .files
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        let out = common::synth(&files, d.top, &lib, Effort::Medium, Goal::Area).unwrap();
        for (text, what) in [
            (out.text.clone(), "ansi"),
            (non_ansi(&out.text, false), "non-ansi"),
            (non_ansi(&out.text, true), "non-ansi with nets"),
        ] {
            let rep = common::check_texts(&files, d.top, &text, &lib);
            assert_eq!(
                rep.verdict,
                Verdict::Equivalent,
                "{name} {what}: {:?} {:?}",
                rep.errors,
                rep.mismatches
            );
        }
        // A port redeclared with another range is rejected.
        let bad = non_ansi(&out.text, true).replacen("  wire [", "  wire [99:", 1);
        if bad != non_ansi(&out.text, true) {
            let rep = common::check_texts(&files, d.top, &bad, &lib);
            assert_eq!(rep.verdict, Verdict::Error, "{name}");
        }
    }
}
