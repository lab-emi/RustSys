// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Diagnostic observations must not alter normal synthesis semantics.
//! Fixtures stay below 10,000 nodes and 1 MiB, even with two test workers.
use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
use rustsys::elab::{ElabOptions, elaborate, elaborate_with_multiplier_trace};

#[test]
fn traced_multiplier_preserves_output_and_input_layout() {
    let mut sm = SourceMap::new();
    let (units, diagnostics) = read_texts(
        &mut sm,
        &[(
            "t.v",
            "module t(input [7:0] a,b,output [15:0] y);assign y=a*b;endmodule",
        )],
        PpOptions::default(),
    );
    assert!(diagnostics.iter().all(|d| !d.is_error()));
    let opts = ElabOptions {
        top: Some("t".into()),
        ..ElabOptions::default()
    };
    let (plain, _) = elaborate(&sm, &units, &opts);
    let (traced, diagnostics) = elaborate_with_multiplier_trace(&sm, &units, &opts);
    assert!(diagnostics.iter().all(|d| !d.is_error()));
    let (plain, traced) = (plain.unwrap(), traced.unwrap());
    assert_eq!(plain.aig.inputs(), traced.aig.inputs());
    assert_eq!(plain.ports[2].bits, traced.ports[2].bits);
    assert!(plain.aig.multiplier_sites().is_empty());
    assert_eq!(traced.aig.multiplier_sites().len(), 1);
    let site = &traced.aig.multiplier_sites()[0];
    assert_eq!(site.product.len(), 16);
    for a in 0..256u64 {
        let b = 255 - a;
        let inputs: Vec<_> = (0..8)
            .map(|i| if a >> i & 1 != 0 { u64::MAX } else { 0 })
            .chain((0..8).map(|i| if b >> i & 1 != 0 { u64::MAX } else { 0 }))
            .collect();
        let values = traced.simulate(&inputs);
        let mut result = 0;
        for (i, bit) in site.product.iter().enumerate() {
            let value = values[bit.node() as usize] ^ if bit.is_compl() { u64::MAX } else { 0 };
            result |= (value & 1) << i;
        }
        assert_eq!(result, a * b);
    }
}

#[test]
fn arithmetic_observation_limit_is_a_diagnostic() {
    let mut source = String::from("module t(input [7:0] a,b,output [1039:0] y);\n");
    for i in 0..65 {
        source.push_str(&format!("assign y[{}+:16] = (a+8'd{i})*b;\n", 16 * i));
    }
    source.push_str("endmodule\n");
    let mut sm = SourceMap::new();
    let (units, diagnostics) = read_texts(&mut sm, &[("bounded.v", &source)], PpOptions::default());
    assert!(diagnostics.iter().all(|d| !d.is_error()));
    let opts = ElabOptions {
        top: Some("t".into()),
        ..ElabOptions::default()
    };
    let (design, diagnostics) = elaborate_with_multiplier_trace(&sm, &units, &opts);
    assert!(design.is_none());
    assert!(diagnostics.iter().any(|d| d.code == "S0398"));
}
