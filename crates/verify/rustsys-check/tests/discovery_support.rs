// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! CHECK-REQ-0032: rare updates need input-support hints, not arbitrary simulation ties.

use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

#[test]
fn renamed_rarely_updated_registers_use_their_distinct_input_supports() {
    let rtl = "module m(input clk,rn,input [23:0] key,input [15:0] d,output [15:0] y);
      reg [15:0] q;
      always @(posedge clk or negedge rn) if (!rn) q <= 0; else if (&key) q <= d;
      assign y = q ^ d;
      endmodule";
    let lib = rusteda_formats::liberty::read_text(&library(Variant::Rich), "test").unwrap();
    for defect in [false, true] {
        let mut net = String::from(
            "module m(input clk,rn,input [23:0] key,input [15:0] d,output [15:0] y);\nwire [15:0] hidden,nx; wire [23:1] en;\n",
        );
        for i in 1..24 {
            let a = if i == 1 {
                "key[0]".into()
            } else {
                format!("en[{}]", i - 1)
            };
            net.push_str(&format!(
                "wt_and2_x1 a{i}(.A({a}),.B(key[{i}]),.Y(en[{i}]));\n"
            ));
        }
        // Deliberately reverse instance order. The reset/hold behavior is the same for every bit.
        for i in (0..16).rev() {
            let data = if defect && i == 0 { 1 } else { i };
            net.push_str(&format!("wt_mux2_x1 m{i}(.A0(hidden[{i}]),.A1(d[{data}]),.S(en[23]),.Y(nx[{i}]));\nwt_dffr_x1 r{i}(.D(nx[{i}]),.CK(clk),.RN(rn),.Q(hidden[{i}]));\nwt_xor2_x1 x{i}(.A(hidden[{i}]),.B(d[{i}]),.Y(y[{i}]));\n"));
        }
        net.push_str("endmodule\n");
        let report = check(
            &CheckOptions {
                rtl: Input::Texts(vec![("rtl.sv".into(), rtl.into())]),
                netlist: Input::Texts(vec![("gates.v".into(), net)]),
                seed: 91231,
                cycles: 16,
                ..CheckOptions::default()
            },
            std::slice::from_ref(&lib),
        );
        if defect {
            assert_ne!(report.verdict, Verdict::Equivalent, "{report:?}");
            assert_ne!(
                report.verdict,
                Verdict::InitialStateRefinement,
                "{report:?}"
            );
        } else {
            assert_eq!(report.verdict, Verdict::Equivalent, "{report:?}");
            assert_eq!(report.state_pairs_checked, 16);
        }
    }
}
