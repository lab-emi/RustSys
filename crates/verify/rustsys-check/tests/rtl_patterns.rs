// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! SYNTH-REQ-0223 in the checker's RTL model: `default` pattern items descend into structures and
//! arrays of non-bit elements and set elements of the value's own type whole.

use std::collections::HashMap;

use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
use rustsys_check::circ::{B, Circuit};
use rustsys_check::rtl::{RtlOptions, build};

fn word(bits: &[B], values: &HashMap<u32, u64>) -> u64 {
    bits.iter()
        .enumerate()
        .fold(0, |n, (i, b)| n | ((Circuit::value(*b, values) & 1) << i))
}

// The oracle was recorded by Icarus, separately from synthesis and this model.
// 256 bounded inputs, <100 circuit entries; <1 MiB scratch.
#[test]
fn structured_wildcards_match_icarus_not_synthesis() {
    let source =
        include_str!("../../../../verification/fixtures/synthetic/synth/rtl/wildcard_structures.v");
    let truth: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../verification/fixtures/synthetic/synth/wildcard-icarus-truth.json"
    ))
    .unwrap();
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("wild.v", source)], PpOptions::default());
    assert!(diags.iter().all(|d| !d.is_error()));
    let mut c = Circuit::new();
    let m = build(
        &sm,
        &units,
        &mut c,
        &RtlOptions {
            top: Some("wildcard_structures".into()),
            assume_full_case: false,
            assume_parallel_case: false,
            params: vec![],
            observations: vec![],
        },
    )
    .unwrap();
    let y = &m.outputs.iter().find(|o| o.0 == "y").unwrap().1;
    let eq = &m.outputs.iter().find(|o| o.0 == "eq").unwrap().1;
    let roots: Vec<_> = y.iter().chain(eq).copied().collect();
    let order = c.order(&roots).unwrap();
    for (s, expected) in truth["values"].as_array().unwrap().iter().enumerate() {
        let inputs: HashMap<_, _> = m.inputs[0]
            .1
            .iter()
            .enumerate()
            .map(|(i, v)| (*v, ((s >> i) & 1) as u64))
            .collect();
        let mut values = HashMap::new();
        c.eval(
            &order,
            &|v| inputs.get(&v).copied().unwrap_or(0),
            &mut values,
        );
        assert_eq!(
            (word(y, &values) << 1) | word(eq, &values),
            expected.as_u64().unwrap(),
            "input {s}"
        );
    }
}

#[test]
fn default_items_fill_nested_aggregates() {
    let src = r#"
package q;
  typedef logic [0:2][7:0] row_t;
  typedef row_t [0:1] tab_t;
  typedef struct packed { logic [3:0] a; logic [1:0][1:0] b; } s_t;
  typedef struct packed { s_t s; logic [2:0] c; } outer_t;
  localparam tab_t T = '{default: 1};
  localparam s_t S = '{a: 4'h3, default: '0};
endpackage
module m import q::*; (input logic [1:0] i, output tab_t t, output outer_t o,
                       output s_t [1:0] arr, output logic [1:0][7:0] w, output logic [3:0] z);
  logic [3:0] un [2][2];
  assign t = T;
  assign o = '{default: 1};
  assign arr = '{default: S};
  assign w = '{default: 1};
  assign un = '{default: 2};
  assign z = un[i[1]][i[0]] + 4'd1;
endmodule
"#;
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("m.sv", src)], PpOptions::default());
    assert!(diags.iter().all(|d| !d.is_error()));
    let mut c = Circuit::new();
    let m = build(
        &sm,
        &units,
        &mut c,
        &RtlOptions {
            assume_full_case: false,
            assume_parallel_case: false,
            top: Some("m".into()),
            params: vec![],
            observations: vec![],
        },
    )
    .unwrap();
    let want = [
        ("t", 0x0101_0101_0101u64),
        ("o", (1 << 7) | (0b0101 << 3) | 1),
        ("arr", 0x3030),
        ("w", 0x0101),
        ("z", 3),
    ];
    for i in 0..4u64 {
        let mut input = HashMap::new();
        for (bit, v) in m.inputs[0].1.iter().enumerate() {
            input.insert(*v, (i >> bit) & 1);
        }
        for (name, expect) in want {
            let (_, bits, _) = m.outputs.iter().find(|(n, _, _)| n == name).unwrap();
            let order = c.order(bits).unwrap();
            let mut values = HashMap::new();
            c.eval(
                &order,
                &|v| input.get(&v).copied().unwrap_or(0),
                &mut values,
            );
            assert_eq!(word(bits, &values), expect, "{name}");
        }
    }
}

// CHECK-REQ-0012: the complete-case assumption is applied by the checker's own model and only
// with the option; a netlist that resolves the unlisted value to the last item is then accepted.
#[test]
fn complete_case_assumption_is_explicit() {
    use rustsys_check::{CheckOptions, Input, Verdict, check};
    let rtl = "module m (input [1:0] s, input [3:0] a, b, c, output reg [3:0] y);
  always @* begin
    (* full_case *) case (s) 2'd0: y = a; 2'd1: y = b; 2'd2: y = c; endcase
  end
endmodule";
    // s = 3 selects c, the last item.
    let net = "module m (input [1:0] s, input [3:0] a, b, c, output [3:0] y);
  wire [3:0] m;
  wt_mux2_x1 l0 (.A0(a[0]), .A1(b[0]), .S(s[0]), .Y(m[0]));
  wt_mux2_x1 h0 (.A0(m[0]), .A1(c[0]), .S(s[1]), .Y(y[0]));
  wt_mux2_x1 l1 (.A0(a[1]), .A1(b[1]), .S(s[0]), .Y(m[1]));
  wt_mux2_x1 h1 (.A0(m[1]), .A1(c[1]), .S(s[1]), .Y(y[1]));
  wt_mux2_x1 l2 (.A0(a[2]), .A1(b[2]), .S(s[0]), .Y(m[2]));
  wt_mux2_x1 h2 (.A0(m[2]), .A1(c[2]), .S(s[1]), .Y(y[2]));
  wt_mux2_x1 l3 (.A0(a[3]), .A1(b[3]), .S(s[0]), .Y(m[3]));
  wt_mux2_x1 h3 (.A0(m[3]), .A1(c[3]), .S(s[1]), .Y(y[3]));
endmodule";
    let lib = rusteda_formats::liberty::read_text(
        &rustsys_fixtures::liberty::library(rustsys_fixtures::liberty::Variant::Rich),
        "rich",
    )
    .unwrap();
    let run = |assume_full_case: bool| {
        check(
            &CheckOptions {
                rtl: Input::Texts(vec![("m.sv".into(), rtl.into())]),
                netlist: Input::Texts(vec![("m.v".into(), net.into())]),
                assume_full_case,
                ..Default::default()
            },
            std::slice::from_ref(&lib),
        )
    };
    let on = run(true);
    assert_eq!(on.verdict, Verdict::Equivalent, "{on:?}");
    assert!(on.assume_full_case);
    assert_ne!(run(false).verdict, Verdict::Equivalent);
}

// CHECK-REQ-0013: under the parallel-case assumption the checker ORs the results of the matching
// items of a case marked parallel; a netlist built that way is equivalent only with the option.
#[test]
fn parallel_case_assumption_is_explicit() {
    use rustsys_check::{CheckOptions, Input, Verdict, check};
    let rtl = |mark: &str| {
        format!(
            "module m (input a, b, c, x, y, z, output reg o);
  always @* begin
    o = 1'b1;
    {mark} case (1'b1) a: o = x; b: o = y; c: o = z; endcase
  end
endmodule"
        )
    };
    // o = a&x | b&y | c&z | !(a|b|c)
    let net = "module m (input a, b, c, x, y, z, output o);
  wire ax, by, cz, none, t0, t1;
  wt_and2_x1 g0 (.A(a), .B(x), .Y(ax));
  wt_and2_x1 g1 (.A(b), .B(y), .Y(by));
  wt_and2_x1 g2 (.A(c), .B(z), .Y(cz));
  wt_nor3_x1 g3 (.A(a), .B(b), .C(c), .Y(none));
  wt_or2_x1 g4 (.A(ax), .B(by), .Y(t0));
  wt_or2_x1 g5 (.A(cz), .B(none), .Y(t1));
  wt_or2_x1 g6 (.A(t0), .B(t1), .Y(o));
endmodule";
    let lib = rusteda_formats::liberty::read_text(
        &rustsys_fixtures::liberty::library(rustsys_fixtures::liberty::Variant::Rich),
        "rich",
    )
    .unwrap();
    let run = |mark: &str, assume_parallel_case: bool| {
        check(
            &CheckOptions {
                rtl: Input::Texts(vec![("m.sv".into(), rtl(mark))]),
                netlist: Input::Texts(vec![("m.v".into(), net.into())]),
                assume_parallel_case,
                ..Default::default()
            },
            std::slice::from_ref(&lib),
        )
    };
    for mark in [
        "(* parallel_case *)",
        "unique0",
        "(* parallel_case, full_case *)",
    ] {
        let on = run(mark, true);
        assert_eq!(on.verdict, Verdict::Equivalent, "{mark}: {on:?}");
        assert!(on.assume_parallel_case);
        assert_eq!(run(mark, false).verdict, Verdict::NotEquivalent, "{mark}");
    }
    // `priority` and unmarked cases keep first-match semantics with the option.
    for mark in ["priority", ""] {
        assert_eq!(run(mark, true).verdict, Verdict::NotEquivalent, "{mark}");
    }
}
