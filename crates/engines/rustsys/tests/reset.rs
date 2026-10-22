// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

mod common;
use common::{Eval, design};
use rustsys::opt::reset::constants;

#[test]
fn simultaneous_pulse_establishes_joint_invariant_across_clocks() {
    let mut d = design(
        "module m(input c1,c2,rn,rp,a, output logic q,t,h); always_ff @(posedge c1 or negedge rn) if(!rn) q<=0; else q<=t; always_ff @(negedge c2 or posedge rp) if(rp) t<=0; else t<=q; always_ff @(posedge c1 or negedge rn) if(!rn) h<=1; else h<=h|a; endmodule",
    );
    let original = d.clone();
    assert!(constants(&mut d, &[]).unwrap().is_empty());
    assert_eq!(d.states.len(), 3);
    let found = constants(&mut d, &[("rn".into(), false)]).unwrap();
    assert_eq!(found.len(), 1); // q and t cannot rely on an unestablished t.
    assert_eq!(found[0].name, "h");
    let mut d = original;
    assert_eq!(
        constants(&mut d, &[("rn".into(), false), ("rp".into(), true)])
            .unwrap()
            .len(),
        3
    );
    assert!(d.states.is_empty());
    for bits in 0..32 {
        let e = Eval::new(
            &d,
            &[
                ("c1", bits & 1),
                ("c2", (bits >> 1) & 1),
                ("rn", (bits >> 2) & 1),
                ("rp", (bits >> 3) & 1),
                ("a", (bits >> 4) & 1),
            ],
            &[],
        );
        assert_eq!((e.out("q"), e.out("t"), e.out("h")), (0, 0, 1));
    }
}

#[test]
fn candidate_withdrawal_reaches_a_fixed_point() {
    let mut d = design(
        "module m(input clk,rn,a, output logic q,t); always_ff @(posedge clk or negedge rn) if(!rn) begin q<=0;t<=0;end else begin q<=t;t<=a;end endmodule",
    );
    assert!(
        constants(&mut d, &[("rn".into(), false)])
            .unwrap()
            .is_empty()
    );
    assert_eq!(d.states.len(), 2);
    assert!(constants(&mut d, &[("absent".into(), false)]).is_err());
    assert!(constants(&mut d, &[("rn".into(), false), ("rn".into(), true)]).is_err());
}

#[test]
fn constants_that_drive_sequential_controls_are_retained() {
    let mut d = design(
        "module m(input clk,rn,a,output logic q,t); always_ff @(posedge clk or negedge rn) if(!rn) q<=0; else q<=q; always_ff @(posedge q) t<=a; endmodule",
    );
    assert!(
        constants(&mut d, &[("rn".into(), false)])
            .unwrap()
            .is_empty()
    );
    assert_eq!(d.states.len(), 2);
}

#[test]
fn equivalent_state_partition_is_inductive_and_requires_reset() {
    let rtl = "module m(input clk,rn,en,a,b,output logic q,t,u,v); always_ff @(posedge clk or negedge rn) if(!rn) begin q<=0;t<=0;u<=0;v<=0;end else begin if(en) begin q<=a;t<=a;end u<=v;v<=b;end endmodule";
    let mut d = design(rtl);
    assert!(rustsys::opt::reset::merge(&mut d, &[]).unwrap().is_empty());
    let eq = rustsys::opt::reset::merge(&mut d, &[("rn".into(), false)]).unwrap();
    assert_eq!(eq.len(), 1);
    assert_eq!(
        (eq[0].name.as_str(), eq[0].representative.as_str()),
        ("t", "q")
    );
    assert_eq!(d.states.len(), 3);
    let mut counter = design(
        "module m(input clk,rn,output logic[7:0] q); always_ff @(posedge clk or negedge rn) if(!rn) q<=0;else q<=q+1; endmodule",
    );
    assert!(
        rustsys::opt::reset::merge(&mut counter, &[("rn".into(), false)])
            .unwrap()
            .is_empty()
    );
    assert_eq!(counter.states.len(), 8);
}

#[test]
fn nonstructural_invariants_require_proof_not_matching_samples() {
    let mut d = design(
        "module m(input clk,rn,a,b,input [31:0] rare,output logic q,t); always_ff @(posedge clk or negedge rn) if(!rn) begin q<=0;t<=0;end else begin q<=((a&b)|(a&!b))^a; t<=(rare==32'h1234abcd);end endmodule",
    );
    assert!(constants(&mut d, &[]).unwrap().is_empty());
    let found = constants(&mut d, &[("rn".into(), false)]).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "q");
    assert_eq!(d.states.len(), 1);
    assert_eq!(d.states[0].name, "t");
    let e = Eval::new(
        &d,
        &[
            ("clk", 0),
            ("rn", 1),
            ("a", 1),
            ("b", 0),
            ("rare", 0x1234abcd),
        ],
        &[("t", false)],
    );
    assert_eq!(e.out("q"), 0);
}

#[test]
fn complementary_reset_states_share_one_inductive_representative() {
    let mut d = design(
        "module m(input clk,rn,en,a,output logic q,t); always_ff @(posedge clk or negedge rn) if(!rn) begin q<=0;t<=1;end else if(en) begin q<=a;t<=!a;end endmodule",
    );
    let eq = rustsys::opt::reset::merge(&mut d, &[("rn".into(), false)]).unwrap();
    assert_eq!(eq.len(), 1);
    assert!(eq[0].inverted);
    assert_eq!(d.states.len(), 1);
    for value in [false, true] {
        let e = Eval::new(
            &d,
            &[("clk", 0), ("rn", 1), ("en", 0), ("a", 0)],
            &[("q", value)],
        );
        assert_eq!(e.out("q"), u64::from(value));
        assert_eq!(e.out("t"), u64::from(!value));
    }
}

#[test]
fn uninitialized_choices_are_opt_in_and_jointly_inductive() {
    let rtl = "module m(input clk,rn,en,a,output logic q,t,h); always_ff @(posedge clk) if(en) q<=t; always_ff @(posedge clk or negedge rn) if(!rn) t<=0; else t<=q; always_ff @(posedge clk) h<=a; endmodule";
    let mut d = design(rtl);
    assert!(
        constants(&mut d, &[("rn".into(), false)])
            .unwrap()
            .is_empty()
    );
    assert!(
        rustsys::opt::reset::constants_with_initial(&mut d, &[], true)
            .unwrap()
            .is_empty()
    );
    let chosen =
        rustsys::opt::reset::constants_with_initial(&mut d, &[("rn".into(), false)], true).unwrap();
    assert_eq!(chosen.len(), 2);
    assert_eq!(chosen.iter().filter(|s| s.reset_input.is_none()).count(), 1);
    assert_eq!(d.states.len(), 1);
    assert_eq!(d.states[0].name, "h");
}
