// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Default quality budget/area contract, <1000 nodes and <128MiB per test worker.
//! Real Sky130/GF180 RTL-to-netlist LEC and OpenSTA are additionally required by the benchmark.

use rustsys::{Goal, Sources, SynthOptions, electrical::default_quality::Limits, synthesize};
use rustsys_fixtures::liberty::{Variant, library};

#[test]
fn default_quality_exhaustion_keeps_the_original_cover() {
    let raw = rusteda_formats::liberty::read_text(&library(Variant::Rich), "fixture.lib").unwrap();
    let source = Sources::Texts(vec![("budget.sv".into(),
        "module m(input clk,en,input [15:0] a,b,output reg [15:0] q); always @(posedge clk) if(en) q<=a^b; endmodule".into())]);
    let opts = SynthOptions {
        goal: Goal::Area,
        default_quality: None,
        ..Default::default()
    };
    let gold = synthesize(&source, std::slice::from_ref(&raw), &opts).unwrap();
    for limits in [
        Limits {
            max_gates: 0,
            ..Default::default()
        },
        Limits {
            max_pins: 0,
            ..Default::default()
        },
        Limits {
            max_name_bytes: 0,
            ..Default::default()
        },
    ] {
        let mut options = opts.clone();
        options.default_quality = Some(limits);
        let result = synthesize(&source, std::slice::from_ref(&raw), &options).unwrap();
        assert_eq!(result.text, gold.text);
        assert_eq!(result.report.area, gold.report.area);
        let quality = result.report.default_quality.unwrap();
        assert_eq!(quality.status, "budget_reached");
        assert_eq!(quality.estimated_path_after_ns, None);
    }
    let mut options = opts;
    options.default_quality = Some(Default::default());
    let result = synthesize(&source, std::slice::from_ref(&raw), &options).unwrap();
    assert!(result.report.area <= gold.report.area + 1e-8);
    assert!(result.report.after_reset.is_empty());
    assert!(!result.report.uninitialized_dont_care);
}
