// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Text results of the command line and of `rustsys show` (PROTO-REQ-0120).

use std::fmt::Write as _;

use rustsys::view as synth;
use rustsys_check::CheckReport;

use crate::proto::format::{Unit, name, number};

/// The synthesis summary (SYNTH-REQ-0704).
pub fn synth_summary(r: &synth::Report) -> String {
    let unit = match r.time_unit_ns {
        Some(t) => format!("time unit {} ns", crate::proto::format::exact(t)),
        None => "library time units".into(),
    };
    format!(
        "top: {}\ncells: {} (sequential {}, combinational {}, inverters {})\narea: {}\n\
         logic depth: {}\nestimated delay: {} ({unit})\nAIG AND nodes: {} -> {}\n\
         state bits: {} in RTL, {} retained, {} latches\nwarnings: {}\n",
        name(&r.top),
        r.cells.values().sum::<usize>(),
        r.sequential_cells,
        r.combinational_cells,
        r.inverters,
        number(r.area, Unit::Area),
        r.logic_depth,
        number(r.estimated_delay, Unit::LibTime),
        r.aig_nodes_before,
        r.aig_nodes_after,
        r.state_bits_rtl,
        r.state_bits_retained,
        r.latches,
        r.warnings
    )
}

/// The mismatch lines of a synthesis check (CHECK-REQ-0081).
pub fn check_synth_mismatches(r: &CheckReport) -> String {
    let mut out = String::new();
    for m in &r.mismatches {
        let _ = writeln!(out, "mismatch[{}]: {}", name(&m.kind), name(&m.name));
    }
    out
}

/// The verdict line of a synthesis check, the verdict spelled as in the JSON report
/// (CHECK-REQ-0081).
pub fn check_synth_verdict(r: &CheckReport) -> String {
    format!(
        "verdict: {} (outputs {}, state pairs {}, unmatched RTL state bits {})\n",
        r.verdict.as_str(),
        r.outputs_checked,
        r.state_pairs_checked,
        r.unmatched_rtl_state_bits.len()
    )
}

/// The result of a synthesis check: mismatches and verdict.
pub fn check_synth(r: &CheckReport) -> String {
    check_synth_mismatches(r) + &check_synth_verdict(r)
}

#[cfg(test)]
mod tests {
    use rustsys_check::{CheckReport, Verdict};
    use serde_json::json;

    // CHECK-REQ-0081: the verdict line spells every verdict as the JSON report does; reports of
    // format version 1 spell the initial-state refinement `refinement`.
    #[test]
    fn verdict_line_spells_verdicts_as_the_report() {
        let mut r: CheckReport = serde_json::from_value(json!({
            "verdict": "refinement",
            "outputs_checked": 2,
            "state_pairs_checked": 3,
            "rtl_state_bits": 3,
            "netlist_state_elements": 3,
            "unmatched_rtl_state_bits": ["k"],
            "mismatches": [],
            "errors": [],
            "sat_calls": 0,
            "seed": 1,
            "runtime_s": 0.0
        }))
        .unwrap();
        assert_eq!(r.verdict, Verdict::InitialStateRefinement);
        let words = [
            "equivalent",
            "initial-state-refinement",
            "not-equivalent",
            "undecided",
            "error",
        ];
        for (v, word) in Verdict::ALL.into_iter().zip(words) {
            r.verdict = v;
            assert_eq!(serde_json::to_value(v).unwrap(), word);
            assert_eq!(
                super::check_synth(&r),
                format!("verdict: {word} (outputs 2, state pairs 3, unmatched RTL state bits 1)\n")
            );
        }
    }
}
