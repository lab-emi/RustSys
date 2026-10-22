// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Summary fields with their sources (PROTO-REQ-0110).

use rustsys::view as synth;
use rustsys_check::CheckReport;

use crate::proto::format::{Unit, number};
use crate::proto::load::Report;

/// Where a shown value comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A JSON pointer into the report.
    Report(String),
    /// A rule over report values.
    Derived(&'static str),
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Report(p) => write!(f, "report {p}"),
            Source::Derived(r) => write!(f, "derived: {r}"),
        }
    }
}

/// A labelled summary value.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub label: &'static str,
    pub text: String,
    pub unit: Unit,
    /// The numeric value, when there is one.
    pub value: Option<f64>,
    pub source: Source,
}

pub(crate) fn num(label: &'static str, v: f64, unit: Unit, pointer: &str) -> Shown {
    Shown {
        label,
        text: number(v, unit),
        unit,
        value: Some(v),
        source: Source::Report(pointer.into()),
    }
}

pub(crate) fn text(label: &'static str, t: String, pointer: &str) -> Shown {
    Shown {
        label,
        text: t,
        unit: Unit::Plain,
        value: None,
        source: Source::Report(pointer.into()),
    }
}

pub(crate) fn count(label: &'static str, n: usize, pointer: &str) -> Shown {
    num(label, n as f64, Unit::Count, pointer)
}

/// Fields of a synthesis report.
pub fn synth_fields(r: &synth::Report) -> Vec<Shown> {
    let cells: usize = r.cells.values().sum();
    let mut out = vec![
        text("Top module", r.top.clone(), "/top"),
        Shown {
            label: "Cells",
            text: cells.to_string(),
            unit: Unit::Count,
            value: Some(cells as f64),
            source: Source::Derived("sum of /cells"),
        },
        count("Sequential cells", r.sequential_cells, "/sequential_cells"),
        count(
            "Combinational cells",
            r.combinational_cells,
            "/combinational_cells",
        ),
        count("Inverters", r.inverters, "/inverters"),
        num("Area", r.area, Unit::Area, "/area"),
        count("Logic depth", r.logic_depth as usize, "/logic_depth"),
        num(
            if r.electrical.is_some() {
                "Cover delay estimate"
            } else {
                "Estimated delay"
            },
            r.estimated_delay,
            Unit::LibTime,
            "/estimated_delay",
        ),
        count(
            "AIG AND nodes before",
            r.aig_nodes_before,
            "/aig_nodes_before",
        ),
        count("AIG AND nodes after", r.aig_nodes_after, "/aig_nodes_after"),
        count("State bits in RTL", r.state_bits_rtl, "/state_bits_rtl"),
        count(
            "State bits retained",
            r.state_bits_retained,
            "/state_bits_retained",
        ),
        count("Latches", r.latches, "/latches"),
        count("Warnings", r.warnings, "/warnings"),
        text("Mapping backend", r.map_backend.clone(), "/map_backend"),
        num("Run time", r.runtime_s, Unit::Seconds, "/runtime_s"),
    ];
    out.push(count(
        "Proved constants removed",
        r.constant_states.len(),
        "/constant_states",
    ));
    out.push(text(
        "Uninitialized state don’t-care",
        r.uninitialized_dont_care.to_string(),
        "/uninitialized_dont_care",
    ));
    out.push(text(
        "Complete case statements",
        r.assume_full_case.to_string(),
        "/assume_full_case",
    ));
    out.push(text(
        "Parallel case statements",
        r.assume_parallel_case.to_string(),
        "/assume_parallel_case",
    ));
    if let Some(e) = &r.electrical {
        out.push(count(
            "Additional electrical corners",
            e.corner_rules.len(),
            "/electrical/corner_rules",
        ));
        out.push(text(
            "Area target met",
            e.area_limit_met.to_string(),
            "/electrical/area_limit_met",
        ));
        out.push(text(
            "Estimated timing target met",
            e.estimated_timing_met.to_string(),
            "/electrical/estimated_timing_met",
        ));
        out.push(text(
            "Estimated data rules met",
            e.estimated_data_rules_met.to_string(),
            "/electrical/estimated_data_rules_met",
        ));
        out.push(count(
            "Data nets exceeding max capacitance",
            e.electrical_rules.capacitance_nets,
            "/electrical/electrical_rules/capacitance_nets",
        ));
        out.push(count(
            "Data nets exceeding max transition",
            e.electrical_rules.transition_nets,
            "/electrical/electrical_rules/transition_nets",
        ));
        out.push(count(
            "Data nets exceeding max fanout",
            e.electrical_rules.fanout_nets,
            "/electrical/electrical_rules/fanout_nets",
        ));
        out.push(num(
            "Electrical estimated slack",
            e.estimated_slack_ns,
            Unit::Ns,
            "/electrical/estimated_slack_ns",
        ));
        out.push(count(
            "Buffers added",
            e.buffers_added,
            "/electrical/buffers_added",
        ));
        out.push(count(
            "Gates collapsed",
            e.gates_collapsed,
            "/electrical/gates_collapsed",
        ));
    }
    out
}

/// Fields of a synthesis check report.
pub fn check_synth_fields(r: &CheckReport) -> Vec<Shown> {
    let v = serde_json::to_value(r.verdict)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    vec![
        text("Verdict", v, "/verdict"),
        count("Outputs checked", r.outputs_checked, "/outputs_checked"),
        count(
            "State pairs checked",
            r.state_pairs_checked,
            "/state_pairs_checked",
        ),
        count("Mismatches", r.mismatches.len(), "/mismatches"),
        count("Errors", r.errors.len(), "/errors"),
        count("Undecided", r.undecided.len(), "/undecided"),
        num("Run time", r.runtime_s, Unit::Seconds, "/runtime_s"),
    ]
}

/// Fields of any report.
pub fn fields(r: &Report) -> Vec<Shown> {
    match r {
        Report::Synth(r) => synth_fields(r),
        Report::CheckSynth(r) => check_synth_fields(r),
    }
}

/// Checks that every report-sourced value shows the formatted value at its pointer; returns
/// the first mismatch (PROTO-REQ-0110).
pub fn trace_errors(r: &Report) -> Option<String> {
    let v = r.to_value();
    for f in fields(r) {
        if let Source::Report(p) = &f.source {
            let Some(x) = v.pointer(p) else {
                return Some(format!("{}: no {p} in the report", f.label));
            };
            let expect = match x {
                serde_json::Value::Number(n) => number(n.as_f64().unwrap_or(f64::NAN), f.unit),
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a.len().to_string(),
                other => other.to_string(),
            };
            if expect != f.text {
                return Some(format!("{}: shows {} but {p} is {expect}", f.label, f.text));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::load::parse_report;
    use serde_json::json;

    /// A synthesis report whose shown numbers all differ, so that a field reading another
    /// pointer shows another text.
    fn synth_report() -> serde_json::Value {
        let mut v = serde_json::to_value(synth::Report {
            format: synth::REPORT_FORMAT.into(),
            top: "top".into(),
            cells: [("inv".to_string(), 3), ("nand2".to_string(), 4)].into(),
            area: 10.5,
            sequential_cells: 2,
            combinational_cells: 5,
            inverters: 3,
            logic_depth: 6,
            estimated_delay: 0.75,
            aig_nodes_before: 17,
            aig_nodes_after: 13,
            state_bits_rtl: 9,
            state_bits_retained: 8,
            uninitialized_dont_care: true,
            assume_parallel_case: true,
            latches: 19,
            warnings: 11,
            runtime_s: 1.25,
            map_backend: "cuda".into(),
            ..Default::default()
        })
        .unwrap();
        v["constant_states"] = json!([
            {"name": "a", "value": true, "reset_input": "rst_ni"},
            {"name": "b", "value": false, "reset_input": "rst_ni"},
            {"name": "c", "value": true, "reset_input": "rst_ni"},
            {"name": "d", "value": false, "reset_input": "rst_ni"}
        ]);
        v
    }

    fn shown(r: &Report, label: &str) -> Option<String> {
        fields(r)
            .into_iter()
            .find(|f| f.label == label)
            .map(|f| f.text)
    }

    // PROTO-REQ-0110: every report-sourced summary field shows the formatted value at its
    // pointer, for synthesis reports with and without electrical results and for check reports.
    #[test]
    fn fields_trace_to_the_report() {
        let mut v = synth_report();
        let plain = parse_report(&v.to_string(), "plain.json").unwrap();
        assert_eq!(trace_errors(&plain), None);
        assert_eq!(shown(&plain, "Warnings").as_deref(), Some("11"));
        assert_eq!(
            shown(&plain, "Proved constants removed").as_deref(),
            Some("4")
        );

        v["electrical"] = json!({
            "area_limit_met": true,
            "estimated_timing_met": false,
            "estimated_data_rules_met": true,
            "electrical_rules": {"capacitance_nets": 21, "transition_nets": 22, "fanout_nets": 23},
            "estimated_slack_ns": -0.125,
            "buffers_added": 24,
            "gates_collapsed": 25,
            "corner_rules": [["slow", {"capacitance_nets": 26}]]
        });
        let electrical = parse_report(&v.to_string(), "electrical.json").unwrap();
        assert_eq!(trace_errors(&electrical), None);
        assert_eq!(fields(&electrical).len(), fields(&plain).len() + 10);
        assert_eq!(
            shown(&electrical, "Electrical estimated slack").as_deref(),
            Some("-0.1250")
        );

        let check = json!({
            "format": rustsys_check::REPORT_FORMAT,
            "verdict": "not-equivalent",
            "outputs_checked": 31,
            "state_pairs_checked": 32,
            "rtl_state_bits": 33,
            "netlist_state_elements": 34,
            "unmatched_rtl_state_bits": [],
            "mismatches": [
                {"kind": "output", "name": "y", "counterexample": [["a", true]]},
                {"kind": "next-state", "name": "q", "counterexample": []}
            ],
            "errors": ["e1", "e2", "e3"],
            "sat_calls": 35,
            "seed": 36,
            "runtime_s": 0.5
        });
        let check = parse_report(&check.to_string(), "check.json").unwrap();
        assert_eq!(trace_errors(&check), None);
        assert_eq!(shown(&check, "Verdict").as_deref(), Some("not-equivalent"));
        assert_eq!(shown(&check, "Mismatches").as_deref(), Some("2"));
    }
}
