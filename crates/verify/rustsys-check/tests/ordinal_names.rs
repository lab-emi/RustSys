// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! CHECK-REQ-0031: canonical state bit numbers are independent of written vector bounds.
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

fn run(left: i32, right: i32, defect: bool) -> rustsys_check::CheckReport {
    let rtl = format!(
        "module m(input clk,rn,input [{left}:{right}] d,output reg [{left}:{right}] q); always @(posedge clk or negedge rn) if(!rn) q<=0; else q<=d; endmodule"
    );
    let mut net =
        format!("module m(input clk,rn,input [{left}:{right}] d,output [{left}:{right}] q);\n");
    for bit in 0..8 {
        let index = if left > right {
            right + bit
        } else {
            right - bit
        };
        let data = if defect && bit == 0 {
            if left > right { index + 1 } else { index - 1 }
        } else {
            index
        };
        net.push_str(&format!(
            "wt_dffr_x1 f{bit}(.CK(clk),.RN(rn),.D(d[{data}]),.Q(q[{index}]));\n"
        ));
    }
    net.push_str("endmodule\n");
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![("r.sv".into(), rtl)]),
            netlist: Input::Texts(vec![("n.v".into(), net)]),
            sim_rounds: 0,
            cycles: 0,
            ..Default::default()
        },
        &[rusteda_formats::liberty::read_text(&library(Variant::Rich), "test").unwrap()],
    )
}
#[test]
fn nonzero_and_ascending_ranges_pair_by_ordinal_without_discovery() {
    for (left, right) in [(15, 8), (8, 15)] {
        let r = run(left, right, false);
        assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
        assert_eq!(r.state_pairs_checked, 8);
        assert!(
            r.discovered_states.is_empty(),
            "direct name pairing is required: {r:?}"
        );
    }
}
#[test]
fn wrong_data_bit_remains_a_counterexample_with_nonzero_ranges() {
    for (left, right) in [(15, 8), (8, 15)] {
        let r = run(left, right, true);
        assert_eq!(r.verdict, Verdict::NotEquivalent, "{r:?}");
        assert!(!r.mismatches.is_empty());
    }
}
