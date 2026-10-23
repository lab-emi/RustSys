// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The netlist estimate of commands that change a netlist after synthesis against the synthesis
//! report of the netlists synthesis writes (SYNTH-REQ-0565): the cell statistics are equal, and
//! the logic depth and mapping delay estimate (SYNTH-REQ-0513) are equal when every cell of the
//! netlist belongs to the mapped cover, and otherwise at least as large, as the netlist estimate
//! also counts the cells emitted after covering.
//! The default load-aware cleanup is tested separately: it may replace the cover,
//! while the report intentionally retains the original mapping delay proxy.
//! <=1000 nodes/design and <128 MiB per test worker; no path enumeration.

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
                default_quality: None,
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

#[test]
fn default_cleanup_keeps_mapping_proxy_and_reports_final_netlist_statistics() {
    let d = CORPUS.iter().find(|d| d.name == "counter").unwrap();
    let lib = rusteda_formats::liberty::read_text(&library(Variant::Full), "full.lib").unwrap();
    let cells = CellLib::build(std::slice::from_ref(&lib), &[]).unwrap();
    let source = Sources::Texts(
        d.files
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect(),
    );
    let mut opts = SynthOptions {
        top: Some(d.top.to_string()),
        goal: Goal::Delay,
        default_quality: None,
        ..SynthOptions::default()
    };
    let original = synthesize(&source, std::slice::from_ref(&lib), &opts).unwrap();
    opts.default_quality = Some(Default::default());
    let cleaned = synthesize(&source, std::slice::from_ref(&lib), &opts).unwrap();
    let quality = cleaned.report.default_quality.as_ref().unwrap();
    assert_eq!(quality.status, "improved");
    assert_ne!(original.text, cleaned.text);
    assert_eq!(
        cleaned.report.estimated_delay,
        original.report.estimated_delay
    );
    let final_estimate = netlist_estimate(&cleaned.netlist, &cells);
    assert_eq!(
        (final_estimate.unmodelled_cells, final_estimate.cyclic_cells),
        (0, 0)
    );
    assert_eq!(final_estimate.cells, cleaned.report.cells);
    assert_eq!(
        (
            final_estimate.sequential_cells,
            final_estimate.combinational_cells,
            final_estimate.inverters
        ),
        (
            cleaned.report.sequential_cells,
            cleaned.report.combinational_cells,
            cleaned.report.inverters
        )
    );
    assert_eq!(final_estimate.logic_depth, cleaned.report.logic_depth);
    assert_eq!(Some(final_estimate.logic_depth), quality.logic_depth_after);
}
