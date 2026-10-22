// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded wiring subset required by independent Verilog netlist exporters.
//! Tiny combinational examples (<100 variables, one worker, <1 MiB circuit).

use rustsys_check::{CheckOptions, Input, Verdict, check};

fn run(net: &str) -> rustsys_check::CheckReport {
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![(
                "rtl.v".into(),
                "module m(input [0:3] a, input [3:0] b, output [7:0] y); assign y={a,b}; endmodule"
                    .into(),
            )]),
            netlist: Input::Texts(vec![("net.v".into(), net.into())]),
            ..CheckOptions::default()
        },
        &[],
    )
}

#[test]
fn whole_vectors_slices_and_concatenated_targets_preserve_order() {
    let net = "module m(input [0:3] a, input [3:0] b, output [7:0] y);
      wire [7:0] t; assign t={a,b};
      assign {y[7:6],y[5],y[4]}={a[0:1],t[5:4]};
      assign y[3:0]=b;
    endmodule";
    assert_eq!(run(net).verdict, Verdict::Equivalent);
    let bad = net.replace("assign y[3:0]=b;", "assign y[3:0]=4'b0000;");
    assert_eq!(run(&bad).verdict, Verdict::NotEquivalent);
}

#[test]
fn unknown_bits_and_invalid_slices_are_rejected() {
    for assignment in [
        "assign y={a,4'b0x01};",
        "assign y={a[3:0],b};",
        "assign y={a[0:4],b};",
        "assign y=a;",
        "assign y={a,a[0] & a[1],3'b000};",
    ] {
        let r = run(&format!(
            "module m(input [0:3] a, input [3:0] b, output [7:0] y); {assignment} endmodule"
        ));
        assert_eq!(r.verdict, Verdict::Error, "{assignment}: {r:?}");
    }
}
