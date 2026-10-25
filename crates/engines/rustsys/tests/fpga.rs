// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Budget/contract tests; behavioral correctness is independently exercised by
//! verification/fpga/run.py through Icarus and Vivado/XSim, including bad controls.

use rustsys::{Sources, SynthOptions, fpga};

fn source() -> Sources {
    Sources::Texts(vec![(
        "probe.sv".into(),
        r#"
module probe(input clk, rst_n, set_n, gate, input [5:0] a,
             input rs_n1, output [5:0] y);
  reg q, p, r, l;
  always @(posedge clk or negedge rst_n) if (!rst_n) q <= 0; else q <= a[0];
  always @(negedge clk or negedge set_n) if (!set_n) p <= 1; else p <= a[1];
  always @(posedge clk) r <= a[2];
  always @* if (gate) l = a[3];
  assign y = {q,p,r,l, ((a[0]^a[1]) & (a[2]|a[3])) ^ (a[4]&a[5]), ~rs_n1};
endmodule
"#
        .into(),
    )])
}

#[test]
fn primitive_contract_and_scope() {
    let out = fpga::synthesize(
        &source(),
        &SynthOptions::default(),
        &fpga::Options::default(),
    )
    .unwrap();
    assert_eq!(out.report.flip_flops, 3);
    assert_eq!(out.report.latches, 1);
    for c in ["FDRE", "FDCE", "FDPE", "LDCE", "BUFG"] {
        assert_eq!(out.report.cells[c], 1, "{c}");
    }
    assert!(out.text.contains("IS_C_INVERTED(1'b1)"));
    assert!(!out.text.contains("IS_PRE_INVERTED"));
    assert!(!out.text.contains("IS_CLR_INVERTED"));
    assert!(out.text.contains(".CLR(rs__i"));
    assert!(out.text.contains(".PRE(rs__i"));
    assert!(out.text.contains("LUT6 #"));
    assert!(out.text.contains("rs__n")); // source port rs_n1 cannot collide
    assert_eq!(out.lut_table.len() % 40, 0);
    assert_eq!(out.report.equivalence_status, "unchecked");
    assert_eq!(out.report.drc_status, "unchecked");
    assert_eq!(out.report.corner, None);
    assert_eq!(out.report.unconstrained_endpoints, None);
}

// <500 objects, one corner/mapper worker, <1 MiB working storage. The reset
// inverter must be shared across bits, so this also bounds added LUT resources.
#[test]
fn asynchronous_control_inversion_is_explicit_and_shared() {
    let src = Sources::Texts(vec![(
        "controls.sv".into(),
        r#"
module controls(input clk, rst_n, set_n, input [7:0] d,
                output reg [7:0] clear_q, set_q);
  always @(posedge clk or negedge rst_n)
    if (!rst_n) clear_q <= 0; else clear_q <= d;
  always @(posedge clk or negedge set_n)
    if (!set_n) set_q <= 8'hff; else set_q <= d;
endmodule
"#
        .into(),
    )]);
    let out = fpga::synthesize(&src, &Default::default(), &Default::default()).unwrap();
    assert_eq!(out.report.flip_flops, 16);
    assert_eq!(out.report.cells["FDCE"], 8);
    assert_eq!(out.report.cells["FDPE"], 8);
    assert_eq!(out.report.cells["LUT1"], 2);
    assert_eq!(out.report.lut_depth, 1);
    assert_eq!(out.text.matches(".CLR(rs_i").count(), 8);
    assert_eq!(out.text.matches(".PRE(rs_i").count(), 8);
    assert!(!out.text.contains("IS_CLR_INVERTED"));
    assert!(!out.text.contains("IS_PRE_INVERTED"));
}

#[test]
fn deterministic_output_and_hard_budgets() {
    let src = source();
    let front = SynthOptions::default();
    let opts = fpga::Options::default();
    let a = fpga::synthesize(&src, &front, &opts).unwrap();
    let b = fpga::synthesize(&src, &front, &opts).unwrap();
    assert_eq!(a.text, b.text);
    assert_eq!(a.lut_table, b.lut_table);
    for (limit, diagnostic) in [
        (
            fpga::Options {
                max_nodes: 1,
                ..opts.clone()
            },
            "FPGA_NODE_BUDGET",
        ),
        (
            fpga::Options {
                max_mapping_bytes: 1,
                ..opts.clone()
            },
            "FPGA_MEMORY_BUDGET",
        ),
        (
            fpga::Options {
                max_cut_merges: 1,
                ..opts.clone()
            },
            "FPGA_WORK_BUDGET",
        ),
        (
            fpga::Options {
                max_output_bytes: 1,
                ..opts.clone()
            },
            "FPGA_OUTPUT_BUDGET",
        ),
    ] {
        let e = fpga::synthesize(&src, &front, &limit).unwrap_err();
        assert!(e.messages.join("\n").contains(diagnostic), "{e:?}");
    }
}

#[test]
fn unsupported_targets_and_semantics_fail_explicitly() {
    for part in ["xczu3eg-sbva484-1-e", "xc7a35tcpg236-1", "xc7z020;exit"] {
        assert!(
            fpga::Options {
                part: part.into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    let front = SynthOptions {
        after_reset: vec![("rst_n".into(), false)],
        ..Default::default()
    };
    assert!(fpga::synthesize(&source(), &front, &Default::default()).is_err());
    let src = Sources::Texts(vec![(
        "bad.sv".into(),
        "module bad(input a, output y); assign y = unsupported(a); endmodule".into(),
    )]);
    assert!(fpga::synthesize(&src, &Default::default(), &Default::default()).is_err());
    let initialized = Sources::Texts(vec![(
        "init.sv".into(),
        "module init(input clk, output reg q); initial q=1; always @(posedge clk) q<=~q; endmodule"
            .into(),
    )]);
    let out = fpga::synthesize(&initialized, &Default::default(), &Default::default()).unwrap();
    assert!(
        out.messages
            .iter()
            .any(|m| m.starts_with("warning[S0120]") && m.contains("initial"))
    );
}
