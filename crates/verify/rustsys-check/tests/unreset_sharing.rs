// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! CHECK-REQ-0051: opt-in initial correlations must be reported and inductive.
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

fn run(next: &str, inverted: bool, enabled: bool, cycles: usize) -> rustsys_check::CheckReport {
    let rtl = format!(
        "module m(input clk,d,output reg a,b); always @(posedge clk) begin a<=d; b<={next}; end endmodule"
    );
    let net = format!(
        "module m(input clk,d,output a,b); wt_dff_x1 f(.CK(clk),.D(d),.Q(a)); {} endmodule",
        if inverted {
            "wt_inv_x1 n(.A(a),.Y(b));"
        } else {
            "assign b=a;"
        }
    );
    let lib = rusteda_formats::liberty::read_text(&library(Variant::Rich), "test").unwrap();
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![("rtl.sv".into(), rtl)]),
            netlist: Input::Texts(vec![("gate.v".into(), net)]),
            uninitialized_dont_care: enabled,
            cycles,
            sim_rounds: 0,
            ..Default::default()
        },
        &[lib],
    )
}

#[test]
fn equal_unreset_states_require_opt_in_and_are_refinement() {
    for cycles in [0, 32] {
        let strict = run("d", false, false, cycles);
        assert!(!matches!(
            strict.verdict,
            Verdict::Equivalent | Verdict::InitialStateRefinement
        ));
        let allowed = run("d", false, true, cycles);
        assert_eq!(
            allowed.verdict,
            Verdict::InitialStateRefinement,
            "{allowed:?}"
        );
        assert_eq!(
            allowed.uninitialized_state_relations,
            vec![("b".into(), "a".into(), false)]
        );
        assert_eq!(allowed.seq_sim.cycles, cycles);
    }
}
#[test]
fn complemented_initial_relation_is_preserved_and_reported() {
    let r = run("!d", true, true, 32);
    assert_eq!(r.verdict, Verdict::InitialStateRefinement, "{r:?}");
    assert_eq!(
        r.uninitialized_state_relations,
        vec![("b".into(), "a".into(), true)]
    );
}
#[test]
fn a_valid_initial_relation_does_not_excuse_a_wrong_transition() {
    for cycles in [0, 32] {
        let r = run("!d", false, true, cycles);
        assert_eq!(r.verdict, Verdict::NotEquivalent, "{r:?}");
        assert!(!r.mismatches.is_empty());
    }
}

#[test]
fn sharing_does_not_choose_a_sequential_control_state() {
    let lib = rusteda_formats::liberty::read_text(&library(Variant::Rich), "test").unwrap();
    let r=check(&CheckOptions {
        rtl:Input::Texts(vec![("rtl.sv".into(),"module m(input clk,d,output reg a,b); always @(posedge clk) a<=d; always @(posedge a) b<=d; endmodule".into())]),
        netlist:Input::Texts(vec![("gate.v".into(),"module m(input clk,d,output a,b); wt_dff_x1 f(.CK(clk),.D(d),.Q(a)); assign b=a; endmodule".into())]),
        uninitialized_dont_care:true,
        ..Default::default()
    },&[lib]);
    assert!(
        !matches!(
            r.verdict,
            Verdict::Equivalent | Verdict::InitialStateRefinement
        ),
        "{r:?}"
    );
}
