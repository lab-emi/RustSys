// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Guarded unrolling must preserve the value of a block-local counter after the loop.

use std::collections::HashMap;

use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
use rustsys_check::circ::{B, Circuit};
use rustsys_check::rtl::{RtlOptions, build};

#[test]
fn guarded_outer_local_loop_keeps_constant_bounds_and_guarded_final_value() {
    let src = r#"
module m(input en, input [3:0] a, output reg [7:0] y);
  always @* begin : combinational
    integer i;
    i = 9;
    y = 0;
    if(en) begin
      for(i=0; i<4; i=i+1) y[i] = a[i];
    end
    y[7:4] = i;
  end
endmodule
"#;
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("m.v", src)], PpOptions::default());
    assert!(diags.iter().all(|d| !d.is_error()));
    let mut c = Circuit::new();
    let m = build(&sm, &units, &mut c, &RtlOptions::default()).unwrap();
    assert!(m.states.is_empty());
    let y = &m.outputs[0].1;
    let order = c.order(y).unwrap();
    for en in 0..2u64 {
        for a in 0..16u64 {
            let mut input = HashMap::new();
            input.insert(m.inputs[0].1[0], en);
            for (bit, v) in m.inputs[1].1.iter().enumerate() {
                input.insert(*v, (a >> bit) & 1);
            }
            let mut values = HashMap::new();
            c.eval(
                &order,
                &|v| input.get(&v).copied().unwrap_or(0),
                &mut values,
            );
            let got = y
                .iter()
                .enumerate()
                .fold(0, |n, (i, b)| n | ((Circuit::value(*b, &values) & 1) << i));
            assert_eq!(got, if en == 1 { 0x40 | a } else { 0x90 });
        }
    }
}

#[test]
fn guarded_module_counter_preserves_both_branches() {
    // <=150 definitions, 64 exhaustive input valuations, <1 MiB scratch.
    let src = include_str!(
        "../../../../verification/fixtures/synthetic/synth/rtl/module_loop_counters.v"
    );
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("m.v", src)], PpOptions::default());
    assert!(diags.iter().all(|d| !d.is_error()));
    let mut c = Circuit::new();
    let model = build(&sm, &units, &mut c, &RtlOptions::default()).unwrap();
    assert!(model.states.is_empty());
    let y = &model.outputs[0].1;
    let order = c.order(y).unwrap();
    for en in 0..2u64 {
        for a in 0..16u64 {
            let mut inputs = HashMap::new();
            inputs.insert(model.inputs[0].1[0], en);
            for (bit, var) in model.inputs[1].1.iter().enumerate() {
                inputs.insert(*var, (a >> bit) & 1);
            }
            let mut values = HashMap::new();
            c.eval(
                &order,
                &|v| inputs.get(&v).copied().unwrap_or(0),
                &mut values,
            );
            let got = y.iter().enumerate().fold(0, |n, (bit, value)| {
                n | ((Circuit::value(*value, &values) & 1) << bit)
            });
            assert_eq!(got, if en == 1 { 0x40 | a } else { 0x90 });
        }
    }
}

#[test]
fn asynchronous_reset_array_loop_is_independently_modelled() {
    let src = r#"
module m(input clk,rn,en, input [15:0] a, output wire [15:0] y);
  reg [3:0] mem[0:3];
  always @(posedge clk or negedge rn) begin : registers
    integer i;
    if(!rn) begin
      for(i=0; i<=3; i=i+1) mem[i] <= 0;
    end else if(en) begin
      for(i=0; i<4; i=i+1) mem[i] <= a[i*4+:4];
    end
  end
  assign y={mem[3],mem[2],mem[1],mem[0]};
endmodule
"#;
    let mut sm = SourceMap::new();
    let (units, _) = read_texts(&mut sm, &[("m.v", src)], PpOptions::default());
    let mut c = Circuit::new();
    let m = build(&sm, &units, &mut c, &RtlOptions::default()).unwrap();
    assert_eq!(m.states.len(), 16);
    assert!(
        m.states
            .iter()
            .all(|s| !s.latch && s.arst.is_some_and(|(_, value)| !value))
    );
    // Reset is recognized independently of the synthesizer's model, including every word.
    assert!(m.states.iter().any(|s| s.name == "mem[3][3]"));
    assert_ne!(m.states[0].next, B::F);
}

#[test]
fn visible_loop_counter_with_conditional_break_is_still_rejected() {
    let src = r#"
module m(input stop, output reg [7:0] y);
  integer i;
  always @* begin
    y=0;
    for(i=0;i<4;i=i+1) begin
      if(stop) break;
      y[i]=1;
    end
    y[7:4]=i;
  end
endmodule
"#;
    let mut sm = SourceMap::new();
    let (units, _) = read_texts(&mut sm, &[("m.v", src)], PpOptions::default());
    let mut c = Circuit::new();
    let error = build(&sm, &units, &mut c, &RtlOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("break") && error.contains("outside"),
        "{error}"
    );
}
