// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Netlists written by later tools: supply ports (CHECK-REQ-0022), port order (CHECK-REQ-0030),
//! vector-bit state names (CHECK-REQ-0031) and discovered correspondence (CHECK-REQ-0032).

use rustsys_check::{CheckOptions, CheckReport, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

/// `a` and `b` are always equal, `k` is 0 after reset.
const RTL: &str = "module m(input clk, rn, en, d, output logic q, r);
  logic a, b, k;
  always_ff @(posedge clk or negedge rn)
    if (!rn) begin a <= 1; b <= 1; k <= 0; end
    else begin
      if (en) begin a <= d; b <= d; end
      k <= k & d;
    end
  assign q = a ^ k;
  assign r = b & en;
endmodule";

fn run(netlist: &str, variant: Variant, supply: &[&str], pulse: bool) -> CheckReport {
    let lib = rusteda_formats::liberty::read_text(&library(variant), "fixture").expect("fixture");
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![("m.sv".into(), RTL.into())]),
            netlist: Input::Texts(vec![("m.v".into(), netlist.into())]),
            after_reset: if pulse {
                vec![("rn".into(), false)]
            } else {
                vec![]
            },
            supply_ports: supply.iter().map(|s| (*s).to_string()).collect(),
            cycles: 16,
            ..Default::default()
        },
        &[lib],
    )
}

/// A placed-and-routed form: ports reordered, supply ports, power connections, the shared
/// state `b` and the constant `k` without wires of their own, and the state output named `state`.
fn routed(flop_out: &str, data_pin: &str) -> String {
    format!(
        "module m(r, q, d, en, rn, clk, VDD, VSS);
  input clk; input rn; input en; input d; output q; output r; inout VDD; inout VSS;
  wire {flop_out}, nd, ndn, iq;
  wtf_mux2_1 u1 (.A0({flop_out}), .A1({data_pin}), .S(en), .X(nd), .VDD(VDD), .VSS(VSS));
  wtf_inv_1 u3 (.A(nd), .Y(ndn), .VDD(VDD), .VSS(VSS));
  wtf_dffr_1 u2 (.D(ndn), .CLK(clk), .RESET_B(rn), .Q(iq), .Q_N({flop_out}), .VDD(VDD), .VSS(VSS));
  wtf_buf_1 u4 (.A({flop_out}), .X(q), .VDD(VDD), .VSS(VSS));
  wtf_and2_1 u5 (.A({flop_out}), .B(en), .X(r), .VDD(VDD), .VSS(VSS));
endmodule"
    )
}

#[test]
fn routed_netlists_check_with_discovered_constants_and_shared_state() {
    let r = run(&routed("a", "d"), Variant::Full, &[], true);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.supply_ports, vec!["VDD".to_string(), "VSS".to_string()]);
    assert_eq!(
        r.discovered_states,
        vec![
            ("b".to_string(), "shares a".to_string()),
            ("k".to_string(), "constant 0".to_string()),
        ]
    );
    assert!(r.unmatched_rtl_state_bits.is_empty(), "{r:?}");
}

#[test]
fn renamed_state_outputs_are_discovered_and_defects_still_fail() {
    let r = run(&routed("state", "d"), Variant::Full, &[], true);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert!(
        r.discovered_states
            .contains(&("a".to_string(), "cell u2 inverted".to_string())),
        "{:?}",
        r.discovered_states
    );
    assert!(
        r.discovered_states
            .contains(&("b".to_string(), "shares a".to_string()))
    );
    // Planted defect: the element loads its own value instead of `d` (CHECK-REQ-0070).
    let bad = run(&routed("state", "state"), Variant::Full, &[], true);
    assert_ne!(bad.verdict, Verdict::Equivalent, "{bad:?}");
}

#[test]
fn discovery_respects_the_base_case_rules() {
    // Without the after-reset pulse neither the constant nor the shared state has a base case,
    // so the cut points stay and the check cannot prove equivalence (CHECK-REQ-0048, 0049).
    let r = run(&routed("a", "d"), Variant::Full, &[], false);
    assert_ne!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert!(r.discovered_states.is_empty(), "{r:?}");
}

#[test]
fn named_netlists_need_no_discovery() {
    let net = "module m(input clk, rn, en, d, output q, r);
  wire a, b, k, nd, ndn, iq;
  wtf_mux2_1 u1 (.A0(a), .A1(d), .S(en), .X(nd));
  wtf_inv_1 u3 (.A(nd), .Y(ndn));
  wtf_dffr_1 u2 (.D(ndn), .CLK(clk), .RESET_B(rn), .Q(iq), .Q_N(a));
  wtf_buf_1 u6 (.A(a), .X(b));
  assign k = 1'b0;
  wtf_buf_1 u4 (.A(a), .X(q));
  wtf_and2_1 u5 (.A(b), .B(en), .X(r));
endmodule";
    let r = run(net, Variant::Full, &[], true);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert!(r.discovered_states.is_empty(), "{r:?}");
    assert_eq!(r.constant_states_checked, vec![("k".to_string(), false)]);
}

#[test]
fn port_order_is_not_compared() {
    // The same netlist with its ports in another order (CHECK-REQ-0030).
    let net = "module m(output r, output q, input d, input en, input rn, input clk);
  wire a, b, k, nd, ndn, iq;
  wtf_mux2_1 u1 (.A0(a), .A1(d), .S(en), .X(nd));
  wtf_inv_1 u3 (.A(nd), .Y(ndn));
  wtf_dffr_1 u2 (.D(ndn), .CLK(clk), .RESET_B(rn), .Q(iq), .Q_N(a));
  wtf_buf_1 u6 (.A(a), .X(b));
  assign k = 1'b0;
  wtf_buf_1 u4 (.A(a), .X(q));
  wtf_and2_1 u5 (.A(b), .B(en), .X(r));
endmodule";
    let r = run(net, Variant::Full, &[], true);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert!(r.mismatches.is_empty());
}

#[test]
fn supply_ports_come_from_the_libraries_or_the_option() {
    // The Rich library has no power pins: an `inout` port is unsupported unless named.
    let net = "module m(input clk, rn, en, d, output q, r, inout VDD);
  wire a, b, k, nd, iq;
  wt_mux2_x1 u1 (.A0(a), .A1(d), .S(en), .Y(nd));
  wt_dffs_x1 u2 (.D(nd), .CK(clk), .SN(rn), .Q(a));
  assign b = a;
  assign k = 1'b0;
  assign q = a;
  wt_and2_x1 u5 (.A(b), .B(en), .Y(r));
endmodule";
    let r = run(net, Variant::Rich, &[], true);
    assert_eq!(r.verdict, Verdict::Error, "{r:?}");
    let r = run(net, Variant::Rich, &["VDD"], true);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.supply_ports, vec!["VDD".to_string()]);
    // A supply read as a logic value is a structural error.
    let tied = net.replace(
        "wt_and2_x1 u5 (.A(b), .B(en)",
        "wt_and2_x1 u5 (.A(VDD), .B(en)",
    );
    let r = run(&tied, Variant::Rich, &["VDD"], true);
    assert_eq!(r.verdict, Verdict::Error, "{r:?}");
    assert!(r.errors[0].contains("CHECK-REQ-0022"), "{:?}", r.errors);
}

#[test]
fn state_names_may_be_bits_of_vector_nets() {
    // Later tools write the escaped names `\st[1] ` as bits of a vector `st` (CHECK-REQ-0031).
    let rtl = "module v(input clk, rn, d, output logic [1:0] s);
  logic [1:0] st;
  always_ff @(posedge clk or negedge rn) if (!rn) st <= 0; else st <= {st[0], d};
  assign s = st;
endmodule";
    let net = "module v(input clk, rn, d, output [1:0] s);
  wire [1:0] \\st ;
  wt_dffr_x1 u0 (.D(d), .CK(clk), .RN(rn), .Q(\\st [0]));
  wt_dffr_x1 u1 (.D(\\st [0]), .CK(clk), .RN(rn), .Q(\\st [1]));
  assign s[0] = \\st [0];
  assign s[1] = \\st [1];
endmodule";
    let lib =
        rusteda_formats::liberty::read_text(&library(Variant::Rich), "rich").expect("fixture");
    let r = check(
        &CheckOptions {
            rtl: Input::Texts(vec![("v.sv".into(), rtl.into())]),
            netlist: Input::Texts(vec![("v.v".into(), net.into())]),
            ..Default::default()
        },
        &[lib],
    );
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.state_pairs_checked, 2);
    assert!(r.discovered_states.is_empty(), "{r:?}");
}

/// `p[1]` always equals `p[0]` but changes only when all 32 bits of `key` are set, which random
/// simulation never sees; `a` reads `p[1]` under the same condition. The netlist keeps one element
/// for `p` and has lost the name of the element of `a`.
const RARE_RTL: &str = "module c(input clk, rn, d, e, input [31:0] key, output logic q, y);
  logic [1:0] p;
  logic a;
  always_ff @(posedge clk or negedge rn)
    if (!rn) begin p <= 2'b11; a <= 1'b0; end
    else begin
      if (&key) p <= {d, d};
      a <= &key ? p[1] : e;
    end
  assign q = p[0];
  assign y = a;
endmodule";

const RARE_NETLIST: &str = "module c(input clk, rn, d, e, input [31:0] key, output q, y);
  wire [7:0] k1;
  wire k2a, k2b, rare, p_d, pn_d, piq, a_d, n7, n8, \\p[0] ;
  wtf_and4_1 a0 (.A(key[0]), .B(key[1]), .C(key[2]), .D(key[3]), .X(k1[0]));
  wtf_and4_1 a1 (.A(key[4]), .B(key[5]), .C(key[6]), .D(key[7]), .X(k1[1]));
  wtf_and4_1 a2 (.A(key[8]), .B(key[9]), .C(key[10]), .D(key[11]), .X(k1[2]));
  wtf_and4_1 a3 (.A(key[12]), .B(key[13]), .C(key[14]), .D(key[15]), .X(k1[3]));
  wtf_and4_1 a4 (.A(key[16]), .B(key[17]), .C(key[18]), .D(key[19]), .X(k1[4]));
  wtf_and4_1 a5 (.A(key[20]), .B(key[21]), .C(key[22]), .D(key[23]), .X(k1[5]));
  wtf_and4_1 a6 (.A(key[24]), .B(key[25]), .C(key[26]), .D(key[27]), .X(k1[6]));
  wtf_and4_1 a7 (.A(key[28]), .B(key[29]), .C(key[30]), .D(key[31]), .X(k1[7]));
  wtf_and4_1 b0 (.A(k1[0]), .B(k1[1]), .C(k1[2]), .D(k1[3]), .X(k2a));
  wtf_and4_1 b1 (.A(k1[4]), .B(k1[5]), .C(k1[6]), .D(k1[7]), .X(k2b));
  wtf_and2_1 b2 (.A(k2a), .B(k2b), .X(rare));
  wtf_mux2_1 m0 (.A0(\\p[0] ), .A1(d), .S(rare), .X(p_d));
  wtf_inv_1 i0 (.A(p_d), .Y(pn_d));
  wtf_dffr_1 f0 (.D(pn_d), .CLK(clk), .RESET_B(rn), .Q(piq), .Q_N(\\p[0] ));
  wtf_mux2_1 m1 (.A0(e), .A1(\\p[0] ), .S(rare), .X(a_d));
  wtf_dffr_1 f1 (.D(a_d), .CLK(clk), .RESET_B(rn), .Q(n7), .Q_N(n8));
  wtf_buf_1 o0 (.A(\\p[0] ), .X(q));
  wtf_buf_1 o1 (.A(n7), .X(y));
endmodule";

fn check_rare(rtl: &str, netlist: &str, seed: u64) -> CheckReport {
    let lib =
        rusteda_formats::liberty::read_text(&library(Variant::Full), "fixture").expect("fixture");
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![("c.sv".into(), rtl.into())]),
            netlist: Input::Texts(vec![("c.v".into(), netlist.into())]),
            after_reset: vec![("rn".into(), false)],
            seed,
            ..Default::default()
        },
        &[lib],
    )
}

#[test]
fn a_rare_condition_constant_does_not_take_the_relations_that_read_it_along() {
    // Simulation keeps `p[1] = 1`; the proof rejects it and, through it, the cell relation of
    // `a`. Only the constant is excluded, so the second discovery finds both (CHECK-REQ-0032).
    let r = check_rare(RARE_RTL, RARE_NETLIST, 1);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert!(
        r.discovered_states
            .contains(&("a".to_string(), "cell f1".to_string())),
        "{:?}",
        r.discovered_states
    );
    assert!(
        r.discovered_states
            .contains(&("p[1]".to_string(), "shares p[0]".to_string())),
        "{:?}",
        r.discovered_states
    );
    // Planted defect: `a` loads `p` inverted under the rare condition (CHECK-REQ-0070).
    let bad = check_rare(
        RARE_RTL,
        &RARE_NETLIST.replace(
            ".A1(\\p[0] ), .S(rare), .X(a_d)",
            ".A1(piq), .S(rare), .X(a_d)",
        ),
        1,
    );
    assert_ne!(bad.verdict, Verdict::Equivalent, "{bad:?}");
}

#[test]
fn an_element_without_a_relation_is_cut_free_while_the_proof_rejects_a_constant() {
    // `a` reads `p[1]` in every cycle, so with `p[1] = 1` assumed, simulation rejects the cell
    // relation of `a` and discovery leaves its element without a counterpart. The proof still
    // runs with that element cut free, rejects the constant, and the second discovery finds both
    // relations (CHECK-REQ-0032).
    let rtl = RARE_RTL.replace("a <= &key ? p[1] : e;", "a <= p[1] ^ e;");
    let net = RARE_NETLIST.replace(
        "wtf_mux2_1 m1 (.A0(e), .A1(\\p[0] ), .S(rare), .X(a_d));",
        "wtf_xor2_1 x1 (.A(\\p[0] ), .B(e), .X(a_d));",
    );
    let r = check_rare(&rtl, &net, 1);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    for relation in [("a", "cell f1"), ("p[1]", "shares p[0]")] {
        assert!(
            r.discovered_states
                .contains(&(relation.0.to_string(), relation.1.to_string())),
            "{:?}",
            r.discovered_states
        );
    }
    // Planted defect: `a` loads `p` inverted (CHECK-REQ-0070).
    let bad = check_rare(
        &rtl,
        &net.replace(".A(\\p[0] ), .B(e), .X(a_d)", ".A(piq), .B(e), .X(a_d)"),
        1,
    );
    assert_ne!(bad.verdict, Verdict::Equivalent, "{bad:?}");
}
