// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use rustsys::{Sources, SynthOptions, synthesize};
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

// SYNTH-REQ-0309: explicit reset mode validates constant state aliases and retained flops.
#[test]
fn independent_checker_validates_emitted_constant_aliases_and_remaining_flops() {
    let text = "module m(input clk,rn,a,output logic y,z); logic g0; always_ff @(posedge clk or negedge rn) if(!rn) begin g0<=0;y<=1;z<=0;end else begin g0<=g0&a;y<=y|g0;z<=a^g0;end endmodule";
    let libs = [rusteda_formats::liberty::read_text(&library(Variant::Full), "test.lib").unwrap()];
    for electrical in [
        None,
        Some(rustsys::electrical::Options {
            period_ns: 100.0,
            ..Default::default()
        }),
    ] {
        let options = SynthOptions {
            after_reset: vec![("rn".into(), false)],
            electrical,
            ..Default::default()
        };
        let out = synthesize(
            &Sources::Texts(vec![("m.sv".into(), text.into())]),
            &libs,
            &options,
        )
        .unwrap();
        assert_eq!(out.report.constant_states.len(), 2);
        assert_eq!(out.report.state_bits_retained, 1);
        let opts = CheckOptions {
            rtl: Input::Texts(vec![("m.sv".into(), text.into())]),
            netlist: Input::Texts(vec![("m.v".into(), out.text)]),
            after_reset: options.after_reset,
            cycles: 32,
            ..Default::default()
        };
        let proof = check(&opts, &libs);
        assert_eq!(proof.verdict, Verdict::Equivalent, "{proof:?}");
        assert_eq!(proof.constant_states_checked.len(), 2);
        assert_eq!(proof.state_pairs_checked, 1);
    }
}

#[test]
fn electrical_corners_survive_job_round_trips() {
    use crate::proto::{
        argv,
        job::{Job, SynthJob},
    };
    let j = Job::Synth(SynthJob {
        electrical: Some(Default::default()),
        electrical_corners: vec!["/test/fast.lib".into(), "/test/slow.lib".into()],
        ..Default::default()
    });
    let loaded: Job = serde_json::from_str(&serde_json::to_string(&j).unwrap()).unwrap();
    assert_eq!(loaded, j);
    let args = argv::canonical_args(&loaded, &Default::default(), 1);
    assert!(
        args.windows(2)
            .any(|a| a == ["--electrical-corner", "/test/fast.lib"])
    );
    assert!(
        args.windows(2)
            .any(|a| a == ["--electrical-corner", "/test/slow.lib"])
    );
    assert!(
        loaded
            .inputs()
            .iter()
            .any(|(_, p)| p == std::path::Path::new("/test/slow.lib"))
    );
}

// SYNTH-REQ-0310: in explicit reset mode, state bits with the same clock and reset literal may be merged.
#[test]
fn shared_state_needs_an_independent_base_and_step_proof() {
    let libs = [rusteda_formats::liberty::read_text(&library(Variant::Full), "test.lib").unwrap()];
    let rtl = "module m(input clk,rn,en,a,b,output logic q,t); always_ff @(posedge clk or negedge rn) if(!rn) begin q<=0;t<=0;end else if(en) begin q<=a;t<=a;end endmodule";
    let out = synthesize(
        &Sources::Texts(vec![("m.sv".into(), rtl.into())]),
        &libs,
        &SynthOptions {
            after_reset: vec![("rn".into(), false)],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out.report.merged_states.len(), 1);
    assert_eq!(out.report.state_bits_retained, 1);
    let opts = CheckOptions {
        rtl: Input::Texts(vec![("m.sv".into(), rtl.into())]),
        netlist: Input::Texts(vec![("m.v".into(), out.text)]),
        after_reset: vec![("rn".into(), false)],
        cycles: 32,
        ..Default::default()
    };
    let r = check(&opts, &libs);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.equivalent_states_checked.len(), 1);
    let r = check(
        &CheckOptions {
            after_reset: vec![],
            ..opts.clone()
        },
        &libs,
    );
    assert_eq!(r.verdict, Verdict::Error);
    let broken = rtl.replace("t<=a", "t<=b");
    let r = check(
        &CheckOptions {
            rtl: Input::Texts(vec![("m.sv".into(), broken)]),
            cycles: 0,
            ..opts
        },
        &libs,
    );
    assert_eq!(r.verdict, Verdict::NotEquivalent, "{r:?}");
}

#[test]
fn complementary_state_aliases_are_independently_proven() {
    let libs = [rusteda_formats::liberty::read_text(&library(Variant::Full), "test.lib").unwrap()];
    for initial in [false, true] {
        for timed in [false, true] {
            let rtl = format!(
                "module m(input clk,rn,en,a,b,output y,z); logic q,t; assign y=q; assign z=t^b; always_ff @(posedge clk or negedge rn) if(!rn) begin q<=1'b{};t<=1'b{};end else if(en) begin q<=a;t<=!a;end endmodule",
                u8::from(initial),
                u8::from(!initial)
            );
            let out = synthesize(
                &Sources::Texts(vec![("m.sv".into(), rtl.clone())]),
                &libs,
                &SynthOptions {
                    after_reset: vec![("rn".into(), false)],
                    electrical: timed.then(|| rustsys::electrical::Options {
                        period_ns: 10.0,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(out.report.merged_states.len(), 1);
            assert!(out.report.merged_states[0].inverted);
            let opts = CheckOptions {
                rtl: Input::Texts(vec![("m.sv".into(), rtl.clone())]),
                netlist: Input::Texts(vec![("m.v".into(), out.text)]),
                after_reset: vec![("rn".into(), false)],
                cycles: 32,
                ..Default::default()
            };
            let proof = check(&opts, &libs);
            assert_eq!(proof.verdict, Verdict::Equivalent, "{proof:?}");
            assert!(proof.equivalent_states_checked[0].2);
            let broken = CheckOptions {
                rtl: Input::Texts(vec![("m.sv".into(), rtl.replace("t<=!a", "t<=b"))]),
                cycles: 0,
                ..opts
            };
            assert_eq!(check(&broken, &libs).verdict, Verdict::NotEquivalent);
        }
    }
}

#[test]
fn dual_output_state_replaces_inverter_without_an_initial_state_assumption() {
    let rtl = "module m(input clk,a,output logic q,output n); always_ff @(posedge clk) q<=a; assign n=!q; endmodule";
    let mut libs =
        [rusteda_formats::liberty::read_text(&library(Variant::Rich), "test.lib").unwrap()];
    let dual = libs[0]
        .cells
        .iter_mut()
        .find(|c| c.name == "wt_dff_x1")
        .unwrap();
    dual.sequentials[0].state_n = "complement_of_iq".into();
    dual.pins
        .iter_mut()
        .find(|p| p.name == "QN")
        .unwrap()
        .function_text = Some("complement_of_iq".into());
    dual.pins
        .iter_mut()
        .find(|p| p.name == "QN")
        .unwrap()
        .function = Some(rusteda_formats::liberty::parse_function("complement_of_iq").unwrap());
    let out = synthesize(
        &Sources::Texts(vec![("m.sv".into(), rtl.into())]),
        &libs,
        &SynthOptions {
            electrical: Some(rustsys::electrical::Options {
                period_ns: 10.0,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        out.report
            .electrical
            .as_ref()
            .unwrap()
            .state_inverters_absorbed,
        1
    );
    assert_eq!(out.report.sequential_cells, 1);
    assert_eq!(out.report.inverters, 0);
    let proof = check(
        &CheckOptions {
            rtl: Input::Texts(vec![("m.sv".into(), rtl.into())]),
            netlist: Input::Texts(vec![("m.v".into(), out.text)]),
            cycles: 32,
            ..Default::default()
        },
        &libs,
    );
    assert_eq!(proof.verdict, Verdict::Equivalent, "{proof:?}");
}

// SYNTH-REQ-0311: --uninitialized-dont-care initial-state assumption and refinement verdict.
#[test]
fn uninitialized_mode_round_trips_and_checker_labels_refinement() {
    let text = "module m(input clk,rn,en,a,output logic q,t,y); always_ff @(posedge clk) if(en) q<=t; always_ff @(posedge clk or negedge rn) if(!rn) t<=0; else t<=q; always_ff @(posedge clk) y<=a^q; endmodule";
    let libs = [rusteda_formats::liberty::read_text(&library(Variant::Full), "test.lib").unwrap()];
    let options = SynthOptions {
        after_reset: vec![("rn".into(), false)],
        uninitialized_dont_care: true,
        ..Default::default()
    };
    let out = synthesize(
        &Sources::Texts(vec![("m.sv".into(), text.into())]),
        &libs,
        &options,
    )
    .unwrap();
    assert!(out.report.uninitialized_dont_care);
    assert_eq!(out.report.constant_states.len(), 2);
    let mut opts = CheckOptions {
        rtl: Input::Texts(vec![("m.sv".into(), text.into())]),
        netlist: Input::Texts(vec![("m.v".into(), out.text)]),
        after_reset: options.after_reset,
        cycles: 32,
        ..Default::default()
    };
    assert_eq!(check(&opts, &libs).verdict, Verdict::Error);
    opts.uninitialized_dont_care = true;
    let proof = check(&opts, &libs);
    assert_eq!(proof.verdict, Verdict::InitialStateRefinement, "{proof:?}");
    assert_eq!(proof.uninitialized_choices, vec![("q".into(), false)]);
    use crate::proto::{
        argv,
        job::{FrontEnd, Job, SynthJob},
    };
    let job = Job::Synth(SynthJob {
        fe: FrontEnd {
            uninitialized_dont_care: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let loaded: Job = serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
    assert_eq!(loaded, job);
    assert!(
        argv::canonical_args(&job, &Default::default(), 1)
            .iter()
            .any(|a| a == "--uninitialized-dont-care")
    );
}
