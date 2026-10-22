// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! A proof or search that stops at its budget gives the verdict `undecided` and names the budget,
//! never `error`; a replayed counterexample still decides (CHECK-REQ-0002).

use rusteda_formats::liberty::Library;
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

fn rich() -> Library {
    rusteda_formats::liberty::read_text(&library(Variant::Rich), "wt_rich").expect("fixture")
}

fn options(rtl: &str, netlist: &str) -> CheckOptions {
    CheckOptions {
        rtl: Input::Texts(vec![("m.sv".into(), rtl.into())]),
        netlist: Input::Texts(vec![("m.v".into(), netlist.into())]),
        ..Default::default()
    }
}

/// Parity of 17 inputs: the RTL folds from bit 0, the netlist from bit 16, so no internal point of
/// one equals one of the other and the output needs a SAT proof (more than 16 inputs).
const PARITY: &str = "module m(input logic [16:0] a, input logic b, output logic y, z);
  assign y = ^a;
  assign z = b;
endmodule";

fn parity_netlist(z: &str) -> String {
    let mut body = String::from("  wt_xor2_x1 x15 (.A(a[16]), .B(a[15]), .Y(s15));\n");
    let mut wires = vec!["s15".to_string()];
    for k in (1..15).rev() {
        body += &format!(
            "  wt_xor2_x1 x{k} (.A(s{}), .B(a[{k}]), .Y(s{k}));\n",
            k + 1
        );
        wires.push(format!("s{k}"));
    }
    body += "  wt_xor2_x1 x0 (.A(s1), .B(a[0]), .Y(y));\n";
    format!(
        "module m(a, b, y, z);\n  input [16:0] a; input b; output y; output z;\n  wire {};\n{body}  {z}\nendmodule\n",
        wires.join(", ")
    )
}

// A conflict budget too small for the output proof leaves it undecided; the message names the
// obligation and both budgets. With the default budgets the same check is decided.
#[test]
fn exhausted_proof_budget_is_undecided() {
    let lib = rich();
    let net = parity_netlist("assign z = b;");
    let decided = check(&options(PARITY, &net), std::slice::from_ref(&lib));
    assert_eq!(decided.verdict, Verdict::Equivalent, "{decided:?}");
    let small = CheckOptions {
        budget: 0,
        lemma_budget: 0,
        ..options(PARITY, &net)
    };
    let r = check(&small, std::slice::from_ref(&lib));
    assert_eq!(r.verdict, Verdict::Undecided, "{r:?}");
    assert!(r.errors.is_empty() && r.mismatches.is_empty(), "{r:?}");
    assert_eq!(r.undecided.len(), 1, "{r:?}");
    let u = &r.undecided[0];
    assert!(
        u.starts_with("output y: the conflict budget of 0 per proof ran out")
            && u.contains("sweeping left")
            && u.contains("at 0 conflicts each"),
        "{u}"
    );
    assert_eq!(r.outputs_checked, 2, "{r:?}");
    // The JSON verdict.
    let json = serde_json::to_value(&r).expect("json");
    assert_eq!(json["verdict"], "undecided");
    assert_eq!(json["format_version"], 2);
    assert_eq!(json["undecided"][0], serde_json::json!(u));
}

// A difference found by a replayed counterexample decides even when another proof runs out of
// its budget.
#[test]
fn a_counterexample_wins_over_undecided_proofs() {
    let lib = rich();
    let small = CheckOptions {
        budget: 0,
        lemma_budget: 0,
        ..options(PARITY, &parity_netlist("wt_inv_x1 u1 (.A(b), .Y(z));"))
    };
    let r = check(&small, std::slice::from_ref(&lib));
    assert_eq!(r.verdict, Verdict::NotEquivalent, "{r:?}");
    assert!(r.mismatches.iter().any(|m| m.name == "z"), "{r:?}");
    assert!(r.undecided[0].starts_with("output y:"), "{r:?}");
}

/// Each register is 1 only after the one before it was, under a condition that random simulation
/// never meets. The netlist keeps none of them and ties `y` to 0.
const CHAIN: &str = "module m(input logic clk, rn, input logic [19:0] a, output logic y);
  logic k1, k2, k3, k4, k5, k6;
  wire rare = &a;
  always_ff @(posedge clk or negedge rn)
    if (!rn) begin
      k1 <= 0; k2 <= 0; k3 <= 0; k4 <= 0; k5 <= 0; k6 <= 0;
    end else begin
      k1 <= rare; k2 <= k1 & rare; k3 <= k2 & rare;
      k4 <= k3 & rare; k5 <= k4 & rare; k6 <= k5 & rare;
    end
  assign y = k6;
endmodule";

const TIED: &str = "module m(clk, rn, a, y);
  input clk; input rn; input [19:0] a; output y;
  wt_tielo_x1 t0 (.L(y));
endmodule";

// With the reset pulse, discovery proposes every register as the constant 0; each proof rejects
// one more, and the search stops at its repetition limit with relations still rejected. Their
// failures are no evidence of a difference, so the check is undecided (CHECK-REQ-0032). Without
// the pulse no constant is proposed and the output mismatch decides.
#[test]
fn discovery_repetition_limit_is_undecided() {
    let lib = rich();
    let r = check(
        &CheckOptions {
            after_reset: vec![("rn".into(), false)],
            ..options(CHAIN, TIED)
        },
        std::slice::from_ref(&lib),
    );
    assert_eq!(r.verdict, Verdict::Undecided, "{r:?}");
    assert!(r.errors.is_empty() && r.mismatches.is_empty(), "{r:?}");
    assert!(
        r.undecided.len() == 1
            && r.undecided[0].contains("limit of 4 repetitions")
            && r.undecided[0].contains("k5"),
        "{r:?}"
    );
    let r = check(&options(CHAIN, TIED), std::slice::from_ref(&lib));
    assert_eq!(r.verdict, Verdict::NotEquivalent, "{r:?}");
}

// A port difference does not depend on the state correspondence, so it decides even when
// discovery stops at its repetition limit: the verdict is `not-equivalent` with the port mismatch,
// and the limit is still reported.
#[test]
fn a_port_difference_decides_at_the_discovery_limit() {
    let lib = rich();
    let extra = TIED
        .replace("output y;", "output y; output z;")
        .replace("module m(clk, rn, a, y);", "module m(clk, rn, a, y, z);")
        .replace(
            "wt_tielo_x1 t0 (.L(y));",
            "wt_tielo_x1 t0 (.L(y));\n  wt_tielo_x1 t1 (.L(z));",
        );
    let r = check(
        &CheckOptions {
            after_reset: vec![("rn".into(), false)],
            ..options(CHAIN, &extra)
        },
        std::slice::from_ref(&lib),
    );
    assert_eq!(r.verdict, Verdict::NotEquivalent, "{r:?}");
    assert!(
        r.mismatches.len() == 1
            && r.mismatches[0].kind == "ports"
            && r.mismatches[0].name.contains("\"z\""),
        "{r:?}"
    );
    assert!(
        r.undecided.len() == 1 && r.undecided[0].contains("limit of 4 repetitions"),
        "{r:?}"
    );
    assert_eq!(r.after_reset, vec![("rn".to_string(), false)]);
}

// A netlist register that no RTL state bit matches stays without a counterpart while the proof
// keeps rejecting the discovered constants: at the repetition limit the check is undecided and
// names the element.
#[test]
fn an_element_without_counterpart_at_the_discovery_limit_is_undecided() {
    let lib = rich();
    let extra = TIED.replace(
        "wt_tielo_x1 t0 (.L(y));",
        "wire j;\n  wt_tielo_x1 t0 (.L(y));\n  wt_dffr_x1 x0 (.D(a[0]), .CK(clk), .RN(rn), .Q(j));",
    );
    let r = check(
        &CheckOptions {
            after_reset: vec![("rn".into(), false)],
            ..options(CHAIN, &extra)
        },
        std::slice::from_ref(&lib),
    );
    assert_eq!(r.verdict, Verdict::Undecided, "{r:?}");
    assert!(r.errors.is_empty() && r.mismatches.is_empty(), "{r:?}");
    assert!(
        r.undecided.len() == 1
            && r.undecided[0].contains("limit of 4 repetitions")
            && r.undecided[0]
                .contains("netlist state element x0 (wt_dffr_x1) still without an RTL counterpart"),
        "{r:?}"
    );
}

/// A shift register of `n` stages whose netlist stores every other stage inverted and names no
/// state wire after the RTL: discovery relates each element to its stage only once the stage
/// before has its relation.
fn renamed_chain(n: usize) -> (String, String) {
    let rtl = format!(
        "module m(input logic clk, input logic d, output logic q);
  logic [{m}:0] s;
  always_ff @(posedge clk) s <= {{s[{p}:0], d}};
  assign q = s[{m}];
endmodule",
        m = n - 1,
        p = n - 2
    );
    let mut net = String::from("module m(clk, d, q);\n  input clk; input d; output q;\n");
    let mut prev = "d".to_string();
    for k in 0..n {
        net += &format!(
            "  wire n{k}, t{k};\n  wt_inv_x1 i{k} (.A({prev}), .Y(t{k}));\n  wt_dffq_x1 r{k} (.D(t{k}), .CK(clk), .Q(n{k}));\n"
        );
        prev = format!("n{k}");
    }
    // Stage k holds the inverse of s[k] when k is even.
    if n % 2 == 1 {
        net += &format!("  wt_inv_x1 io (.A({prev}), .Y(q));\n");
    } else {
        net += &format!("  assign q = {prev};\n");
    }
    net += "endmodule\n";
    (rtl, net)
}

/// Two equal reset shift registers of `n` stages; the netlist keeps one with its names and drives
/// both outputs from it, so every stage of the other shares an element.
fn shared_chains(n: usize) -> (String, String) {
    let rtl = format!(
        "module m(input logic clk, rn, d, output logic ya, yb);
  logic [{m}:0] a, b;
  always_ff @(posedge clk or negedge rn)
    if (!rn) begin a <= '0; b <= '0; end
    else begin a <= {{a[{p}:0], d}}; b <= {{b[{p}:0], d}}; end
  assign ya = a[{m}];
  assign yb = b[{m}];
endmodule",
        m = n - 1,
        p = n - 2
    );
    let mut net = String::from(
        "module m(clk, rn, d, ya, yb);\n  input clk; input rn; input d; output ya; output yb;\n",
    );
    let mut prev = "d".to_string();
    for k in 0..n {
        net += &format!(
            "  wire \\a[{k}] ;\n  wt_dffr_x1 r{k} (.D({prev}), .CK(clk), .RN(rn), .Q(\\a[{k}] ));\n"
        );
        prev = format!("\\a[{k}] ");
    }
    net += &format!("  assign ya = {prev};\n  assign yb = {prev};\nendmodule\n");
    (rtl, net)
}

// Discovery proposes and refines until no relation changes, without a bound on its rounds
// (CHECK-REQ-0032). A chain whose relations each need the one before takes a round per stage:
// at the former bound of eight rounds, the stages left over were elements without a counterpart
// (`error`) or state cut free with a counterexample that no run reaches (`not-equivalent`).
// Both netlists are equivalent.
#[test]
fn discovery_follows_a_chain_to_its_end() {
    let lib = rich();
    for n in [9, 16] {
        let (rtl, net) = renamed_chain(n);
        let r = check(&options(&rtl, &net), std::slice::from_ref(&lib));
        assert_eq!(r.verdict, Verdict::Equivalent, "{n}: {r:?}");
        assert_eq!(r.discovered_states.len(), n, "{r:?}");
        let (rtl, net) = shared_chains(n);
        let r = check(
            &CheckOptions {
                after_reset: vec![("rn".into(), false)],
                ..options(&rtl, &net)
            },
            std::slice::from_ref(&lib),
        );
        assert_eq!(r.verdict, Verdict::Equivalent, "{n}: {r:?}");
        assert!(
            r.unmatched_rtl_state_bits.is_empty() && r.equivalent_states_checked.len() == n,
            "{r:?}"
        );
    }
}

/// Whether `y` depends on its own value is a proof that the RTL model needs.
const OWN: &str = "module m(input logic [5:0] a, b, output logic y);
  always_comb if (a * b != b * a) y = y; else y = 1'b0;
endmodule";

fn own_netlist(body: &str) -> String {
    format!("module m(a, b, y);\n  input [5:0] a; input [5:0] b; output y;\n  {body}\nendmodule\n")
}

// The conflict budget of the proofs that the RTL model needs is an option; one that runs out
// leaves the check undecided and names the budget. An erroneous netlist is rejected all the
// same, as more budget would not change that verdict.
#[test]
fn exhausted_model_budget_is_undecided_unless_the_netlist_is_rejected() {
    let lib = rich();
    let tied = own_netlist("wt_tielo_x1 t0 (.L(y));");
    let r = check(&options(OWN, &tied), std::slice::from_ref(&lib));
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    let small = |net: &str| CheckOptions {
        model_budget: 0,
        ..options(OWN, net)
    };
    let r = check(&small(&tied), std::slice::from_ref(&lib));
    assert_eq!(r.verdict, Verdict::Undecided, "{r:?}");
    assert!(
        r.errors.is_empty()
            && r.undecided.len() == 1
            && r.undecided[0].contains("y depends on its own value")
            && r.undecided[0].contains("conflict budget of 0 per proof"),
        "{r:?}"
    );
    // Supply ports declared again as supply nets: whether they are supply ports depends on the
    // RTL ports, so the netlist alone is not rejected for them.
    let full =
        rusteda_formats::liberty::read_text(&library(Variant::Full), "wt_full").expect("fixture");
    let supplied = "module m(a, b, y, VDD, VSS);
  input [5:0] a; input [5:0] b; output y; input VDD; input VSS;
  supply1 VDD; supply0 VSS;
  wtf_tielo t0 (.L_LO(y), .VDD(VDD), .VSS(VSS));
endmodule
";
    for (budget, verdict) in [
        (rustsys_check::DEFAULT_MODEL_BUDGET, Verdict::Equivalent),
        (0, Verdict::Undecided),
    ] {
        let r = check(
            &CheckOptions {
                model_budget: budget,
                ..options(OWN, supplied)
            },
            std::slice::from_ref(&full),
        );
        assert_eq!(r.verdict, verdict, "{r:?}");
    }
    for (body, what) in [
        ("wt_tielo_x1 t0 (.L(y))", "m.v:4:"),
        ("wt_nosuch t0 (.L(y));", "unknown cell wt_nosuch"),
        ("assign y = 1'bx;", "error[C0103]"),
        (
            "wt_tielo_x1 t0 (.L(y));\n  wt_tielo_x1 t1 (.L(y));",
            "more than one driver",
        ),
    ] {
        let r = check(&small(&own_netlist(body)), std::slice::from_ref(&lib));
        assert_eq!(r.verdict, Verdict::Error, "{body}: {r:?}");
        assert!(
            r.undecided.is_empty() && r.errors.len() == 1 && r.errors[0].contains(what),
            "{body}: {r:?}"
        );
    }
}
