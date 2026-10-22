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
