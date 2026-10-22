// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The netlist estimate of commands that change a netlist after synthesis against the synthesis
//! report of the netlists synthesis writes (SYNTH-REQ-0565): the cell statistics are equal, and
//! the logic depth and mapping delay estimate (SYNTH-REQ-0513) are equal when every cell of the
//! netlist belongs to the mapped cover, and otherwise at least as large, as the netlist estimate
//! also counts the cells emitted after covering.

use rustsys::map::library::CellLib;
use rustsys::view::netlist_estimate;
use rustsys::{Goal, Sources, SynthOptions, synthesize};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;

// SYNTH-REQ-0565
#[test]
fn estimate_of_synthesized_netlists_against_the_report() {
    for variant in [Variant::Full, Variant::Minimal] {
        let lib = rusteda_formats::liberty::read_text(&library(variant), variant.name()).unwrap();
        let cells = CellLib::build(std::slice::from_ref(&lib), &[]).unwrap();
        let (mut checked, mut equal) = (0, 0);
        for d in CORPUS {
            let files = d
                .files
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect();
            let opts = SynthOptions {
                top: Some(d.top.to_string()),
                goal: Goal::Delay,
                ..SynthOptions::default()
            };
            let out = match synthesize(&Sources::Texts(files), std::slice::from_ref(&lib), &opts) {
                Ok(o) => o,
                // A library without latches cannot implement the latch design.
                Err(_) if variant == Variant::Minimal && d.name == "latches" => continue,
                Err(e) => panic!("{} synthesis failed: {}", d.name, e.messages.join("\n")),
            };
            let e = netlist_estimate(&out.netlist, &cells);
            let r = &out.report;
            let tag = format!("{} [{variant:?}]", d.name);
            assert_eq!((e.unmodelled_cells, e.cyclic_cells), (0, 0), "{tag}");
            assert_eq!(
                (e.sequential_cells, e.combinational_cells, e.inverters),
                (r.sequential_cells, r.combinational_cells, r.inverters),
                "{tag}"
            );
            assert_eq!(e.cells, r.cells, "{tag}");
            assert!(e.logic_depth >= r.logic_depth, "{tag}");
            assert!(e.logic_depth <= r.logic_depth + 1, "{tag}");
            assert!(e.estimated_delay >= r.estimated_delay, "{tag}");
            if variant == Variant::Full {
                assert_eq!(e.logic_depth, r.logic_depth, "{tag}");
                assert_eq!(e.estimated_delay, r.estimated_delay, "{tag}");
            }
            equal += usize::from(
                e.logic_depth == r.logic_depth && e.estimated_delay == r.estimated_delay,
            );
            checked += 1;
        }
        assert!(checked >= 30, "{variant:?}: {checked}");
        assert!(
            equal * 4 >= checked * 3,
            "{variant:?}: {equal} of {checked} equal"
        );
    }
}
