// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Export must never turn removed or merged arbitrary state into assumptions.
//! <100 circuit entries, one shared small library; <1 MiB scratch per test.
//! Independent positive/negative ABC controls are recorded by the benchmark.

use std::sync::OnceLock;

use rusteda_formats::liberty::Library;
use rustsys_check::{CheckOptions, Input, ObligationExport, Verdict, check};

fn library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        rusteda_formats::liberty::read_text(
            &rustsys_fixtures::liberty::library(rustsys_fixtures::liberty::Variant::Rich),
            "fixture",
        )
        .unwrap()
    })
}

#[test]
fn removed_and_aliased_rtl_state_stays_unconstrained_in_exports() {
    for (case, rhs, assignment) in [("constant", "0", "1'b0"), ("alias", "d", "q")] {
        let directory =
            std::env::temp_dir().join(format!("rustsys-export-free-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let rtl = format!(
            "module m(input clk,d,output reg q,output y); reg gone; always @(posedge clk) begin q<=d; gone<={rhs}; end assign y=gone; endmodule"
        );
        let net = format!(
            "module m(input clk,d,output q,y); wire gone; wt_dff_x1 f(.CK(clk),.D(d),.Q(q)); assign gone={assignment}; assign y=gone; endmodule"
        );
        let report = check(
            &CheckOptions {
                rtl: Input::Texts(vec![("rtl.v".into(), rtl)]),
                netlist: Input::Texts(vec![("net.v".into(), net)]),
                obligation_export: Some(ObligationExport::new(directory.clone())),
                ..CheckOptions::default()
            },
            std::slice::from_ref(library()),
        );
        assert_eq!(report.verdict, Verdict::Undecided, "{report:?}");
        assert!(report.after_reset.is_empty());
        assert!(report.uninitialized_choices.is_empty());
        assert!(report.uninitialized_state_relations.is_empty());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["all_rtl_inputs_and_states_quantified"], true);
        assert_eq!(manifest["all_gate_initial_states_covered"], true);
        assert_eq!(manifest["rtl_state_bits"], 2);
        assert_eq!(manifest["state_pairs"], 1);
        assert_eq!(manifest["sides"][0]["inputs"], 4);
        assert_eq!(manifest["sides"][1]["inputs"], 4);
        assert_eq!(manifest["initial_state_choices"], serde_json::json!([]));
        assert_eq!(manifest["reset_assumptions"], serde_json::json!([]));
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn unnamed_state_proposals_keep_arbitrary_state_and_never_decide_equivalence() {
    for bad_data in [false, true] {
        let directory = std::env::temp_dir().join(format!(
            "rustsys-export-unnamed-{}-{bad_data}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let report = check(&CheckOptions {
            rtl: Input::Texts(vec![("m.sv".into(),
                "module m(input clk,d,output y); reg q; always @(posedge clk) q<=d; assign y=q; endmodule".into())]),
            netlist: Input::Texts(vec![("m.v".into(), format!(
                "module m(input clk,d,output y); wire hidden,nd; {} wt_dff_x1 f(.CK(clk),.D(nd),.Q(hidden)); assign y=hidden; endmodule",
                if bad_data { "wt_inv_x1 inv(.A(d),.Y(nd));" } else { "assign nd=d;" }
            ))]),
            obligation_export: Some(ObligationExport::new(directory.clone())),
            ..CheckOptions::default()
        }, std::slice::from_ref(library()));
        assert_eq!(report.verdict, Verdict::Undecided, "{report:?}");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["proposed_state_bindings"], 1);
        assert_eq!(manifest["all_gate_initial_states_covered"], true);
        assert_eq!(manifest["all_rtl_inputs_and_states_quantified"], true);
        assert_eq!(manifest["state_pairs"], 1);
        assert_eq!(manifest["reset_assumptions"], serde_json::json!([]));
        assert_eq!(manifest["initial_state_choices"], serde_json::json!([]));
        let gold = std::fs::read(directory.join("gold.aig")).unwrap();
        let gate = std::fs::read(directory.join("gate.aig")).unwrap();
        assert_eq!(gold == gate, !bad_data);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn unnamed_state_discovery_budget_is_not_an_equivalence_verdict() {
    let directory =
        std::env::temp_dir().join(format!("rustsys-export-budget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut export = ObligationExport::new(directory.clone());
    export.max_discovery_work = 1;
    let report = check(&CheckOptions {
        rtl: Input::Texts(vec![("m.sv".into(),
            "module m(input clk,d,output y); reg q; always @(posedge clk) q<=d; assign y=q; endmodule".into())]),
        netlist: Input::Texts(vec![("m.v".into(),
            "module m(input clk,d,output y); wire hidden; wt_dff_x1 f(.CK(clk),.D(d),.Q(hidden)); assign y=hidden; endmodule".into())]),
        obligation_export: Some(export),
        ..CheckOptions::default()
    }, std::slice::from_ref(library()));
    assert_eq!(report.verdict, Verdict::Undecided, "{report:?}");
    assert!(!directory.join("manifest.json").exists());
    assert!(report.after_reset.is_empty());
    assert!(report.uninitialized_choices.is_empty());
}

#[test]
fn proposal_candidate_filter_keeps_every_unused_rtl_state_free() {
    let directory =
        std::env::temp_dir().join(format!("rustsys-export-relevant-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut export = ObligationExport::new(directory.clone());
    export.max_discovery_support_entries = 0; // Ranking hints are optional, never proof premises.
    let report = check(&CheckOptions {
        rtl: Input::Texts(vec![("m.sv".into(),
            "module m(input clk,d,output y); reg q; reg [255:0] unused; always @(posedge clk) begin q<=d; unused<=0; end assign y=q; endmodule".into())]),
        netlist: Input::Texts(vec![("m.v".into(),
            "module m(input clk,d,output y); wire hidden; wt_dff_x1 f(.CK(clk),.D(d),.Q(hidden)); assign y=hidden; endmodule".into())]),
        obligation_export: Some(export),
        ..CheckOptions::default()
    }, std::slice::from_ref(library()));
    assert_eq!(report.verdict, Verdict::Undecided, "{report:?}");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["rtl_state_bits"], 257);
    assert_eq!(manifest["state_pairs"], 1);
    assert_eq!(manifest["sides"][0]["inputs"], 259);
    assert_eq!(manifest["proposed_state_bindings"], 1);
    assert_eq!(manifest["all_rtl_inputs_and_states_quantified"], true);
    assert_eq!(manifest["initial_state_choices"], serde_json::json!([]));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn opposite_reset_encodings_are_only_untrusted_state_proposals() {
    let directory = std::env::temp_dir().join(format!(
        "rustsys-export-reset-encoding-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let report=check(&CheckOptions {
        rtl:Input::Texts(vec![("m.sv".into(),
            "module m(input clk,rn,d,output y,z); reg q,r; always @(posedge clk or negedge rn) if(!rn) begin q<=1; r<=0; end else begin q<=d^r; r<=d^q; end assign y=q; assign z=r; endmodule".into())]),
        netlist:Input::Texts(vec![("m.v".into(),
            "module m(input clk,rn,d,output y,z); wire a,b,da,db; wt_xnor2_x1 xa(.A(d),.B(b),.Y(da)); wt_xnor2_x1 xb(.A(d),.B(a),.Y(db)); wt_dffr_x1 fa(.CK(clk),.RN(rn),.D(da),.Q(a)); wt_dffr_x1 fb(.CK(clk),.RN(rn),.D(db),.Q(b)); wt_inv_x1 iy(.A(a),.Y(y)); assign z=b; endmodule".into())]),
        obligation_export:Some(ObligationExport::new(directory.clone())),
        ..CheckOptions::default()
    },std::slice::from_ref(library()));
    assert_eq!(report.verdict, Verdict::Undecided, "{report:?}");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["state_pairs"], 2);
    assert_eq!(manifest["proposed_state_bindings"], 2);
    assert_eq!(manifest["all_gate_initial_states_covered"], true);
    assert_eq!(manifest["initial_state_choices"], serde_json::json!([]));
    assert_eq!(manifest["reset_assumptions"], serde_json::json!([]));
    // Different AIG structures can encode the same reset/data function.
    // The tracked external-CEC controls, not byte identity, decide equivalence.
    let records = std::fs::read_to_string(directory.join("state-relation.jsonl")).unwrap();
    let relations: Vec<serde_json::Value> = records
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        relations.iter().filter(|r| r["inverted"] == true).count(),
        1
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn explicit_state_hints_cannot_override_names_or_share_variables() {
    for case in ["unknown", "duplicate", "conflict"] {
        let directory = std::env::temp_dir().join(format!(
            "rustsys-export-hints-{}-{case}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut export = ObligationExport::new(directory.clone());
        export.state_proposals.push(rustsys_check::StateProposal {
            rtl_state: if case == "unknown" { "absent" } else { "q" }.into(),
            gate_instance: "f".into(),
            inverted: case == "conflict",
        });
        if case == "duplicate" {
            export
                .state_proposals
                .push(export.state_proposals[0].clone());
        }
        let report = check(
            &CheckOptions {
                rtl: Input::Texts(vec![(
                    "m.sv".into(),
                    "module m(input clk,d,output reg q); always @(posedge clk) q<=d; endmodule"
                        .into(),
                )]),
                netlist: Input::Texts(vec![(
                    "m.v".into(),
                    "module m(input clk,d,output q); wt_dff_x1 f(.CK(clk),.D(d),.Q(q)); endmodule"
                        .into(),
                )]),
                obligation_export: Some(export),
                ..CheckOptions::default()
            },
            std::slice::from_ref(library()),
        );
        assert_eq!(report.verdict, Verdict::Error, "{case}: {report:?}");
        assert!(!directory.join("manifest.json").exists());
        assert!(!report.errors.is_empty(), "{case}: {report:?}");
    }
}
