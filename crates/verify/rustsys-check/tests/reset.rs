// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use rustsys_check::{CheckOptions, Input, Verdict, check};

fn run(next: &str, pulse: Vec<(String, bool)>, cycles: usize) -> rustsys_check::CheckReport {
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![(
                "m.sv".into(),
                format!(
                    "module m(input clk,rn,a, output logic q); always_ff @(posedge clk or negedge rn) if(!rn) q<=0; else q<={next}; endmodule"
                ),
            )]),
            netlist: Input::Texts(vec![(
                "m.v".into(),
                "module m(input clk,rn,a, output q); assign q=1'b0; endmodule".into(),
            )]),
            after_reset: pulse,
            cycles,
            ..Default::default()
        },
        &[],
    )
}

#[test]
fn reset_constants_need_both_base_case_and_inductive_step() {
    let pulse = vec![("rn".into(), false)];
    let r = run("q&a", pulse.clone(), 32);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.constant_states_checked, vec![("q".into(), false)]);
    assert_eq!(r.seq_sim.cycles, 32);
    assert_eq!(run("q&a", vec![], 0).verdict, Verdict::Error);
    assert_eq!(
        run("q&a", vec![("rn".into(), true)], 0).verdict,
        Verdict::Error
    );
    let bad = run("a", pulse, 0); // Disable random sequential simulation: proof must reject it.
    assert_eq!(bad.verdict, Verdict::NotEquivalent, "{bad:?}");
    assert!(bad.mismatches.iter().any(|m| m.kind == "reset-invariant"));
}

#[test]
fn reset_value_and_async_control_are_not_optional() {
    for body in [
        "always_ff @(posedge clk) q<=0;",
        "always_ff @(posedge clk or negedge rn) if(!rn) q<=1; else q<=0;",
    ] {
        let r = check(
            &CheckOptions {
                rtl: Input::Texts(vec![(
                    "m.sv".into(),
                    format!("module m(input clk,rn,output logic q); {body} endmodule"),
                )]),
                netlist: Input::Texts(vec![(
                    "m.v".into(),
                    "module m(input clk,rn,output q); assign q=1'b0; endmodule".into(),
                )]),
                after_reset: vec![("rn".into(), false)],
                ..Default::default()
            },
            &[],
        );
        assert_eq!(r.verdict, Verdict::Error, "{r:?}");
    }
}

#[test]
fn checker_rejects_removed_state_that_drives_a_clock() {
    let r=check(&CheckOptions {
        rtl:Input::Texts(vec![("m.sv".into(),"module m(input clk,rn,a,output logic q,t); always_ff @(posedge clk or negedge rn) if(!rn) q<=0; else q<=q; always_ff @(posedge q) t<=a; endmodule".into())]),
        netlist:Input::Texts(vec![("m.v".into(),"module m(input clk,rn,a,output q,t); assign q=1'b0; assign t=1'b0; endmodule".into())]),
        after_reset:vec![("rn".into(),false)], ..Default::default()
    },&[]);
    assert_eq!(r.verdict, Verdict::Error, "{r:?}");
}

#[test]
fn initial_refinement_is_explicit_and_proves_all_future_inputs() {
    let run = |body: &str, value: bool, enabled: bool, cycles: usize| {
        check(
            &CheckOptions {
                rtl: Input::Texts(vec![(
                    "m.sv".into(),
                    format!(
                        "module m(input clk,rn,en,a, input[31:0] rare, output logic q); {body} endmodule"
                    ),
                )]),
                netlist: Input::Texts(vec![(
                    "m.v".into(),
                    format!(
                        "module m(input clk,rn,en,a, input[31:0] rare, output q); assign q=1'b{}; endmodule",
                        u8::from(value)
                    ),
                )]),
                uninitialized_dont_care: enabled,
                sim_rounds: 0,
                cycles,
                ..Default::default()
            },
            &[],
        )
    };
    for value in [false, true] {
        let rtl = if value {
            "always_ff @(posedge clk) if(en) q<=1;"
        } else {
            "always_ff @(posedge clk) if(en) q<=0;"
        };
        assert_eq!(run(rtl, value, false, 0).verdict, Verdict::Error);
        let r = run(rtl, value, true, 32);
        assert_eq!(r.verdict, Verdict::InitialStateRefinement, "{r:?}");
        assert_eq!(r.uninitialized_choices, vec![("q".into(), value)]);
        assert_eq!(r.seq_sim.cycles, 32);
    }
    // Disable every simulation: SAT must refute a rare future input changing the constant.
    let r = run(
        "always_ff @(posedge clk) if(en) q<=q|(rare==32'h1234abcd);",
        false,
        true,
        0,
    );
    assert_eq!(r.verdict, Verdict::NotEquivalent, "{r:?}");
    assert!(r.mismatches.iter().any(|m| m.kind == "initial-invariant"));
    for body in [
        "always_ff @(posedge clk or negedge rn) if(!rn) q<=1; else q<=0;",
        "always_latch if(en) q<=0;",
    ] {
        assert_eq!(run(body, false, true, 0).verdict, Verdict::Error);
    }
}
