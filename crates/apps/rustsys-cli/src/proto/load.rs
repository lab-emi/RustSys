// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Loading runs and reports (PROTO-REQ-0062).

use std::path::{Path, PathBuf};

use rusteda_core::job::Diag;
use rustsys::view as synth;
use rustsys_check::CheckReport;

use crate::proto::Product;
use crate::proto::record::{RunLocation, RunRecord, read_record};

/// A report of any product.
#[derive(Debug, Clone)]
pub enum Report {
    Synth(synth::Report),
    CheckSynth(CheckReport),
}

impl Report {
    pub fn product(&self) -> Product {
        match self {
            Report::Synth(_) => Product::Synth,
            Report::CheckSynth(_) => Product::CheckSynth,
        }
    }

    /// The report as pretty JSON, byte for byte what its report file holds (CLI-REQ-0003),
    /// without a final line feed.
    pub fn to_json_pretty(&self) -> String {
        match self {
            Report::Synth(r) => serde_json::to_string_pretty(r).unwrap_or_default(),
            Report::CheckSynth(r) => serde_json::to_string_pretty(r).unwrap_or_default(),
        }
    }

    /// The report as a JSON value, for field sources.
    pub fn to_value(&self) -> serde_json::Value {
        match self {
            Report::Synth(r) => serde_json::to_value(r),
            Report::CheckSynth(r) => serde_json::to_value(r),
        }
        .unwrap_or(serde_json::Value::Null)
    }
}

/// The product of a report's JSON: its `format`, or its fields for older reports.
pub fn kind_of(v: &serde_json::Value) -> Option<Product> {
    match v.get("format").and_then(serde_json::Value::as_str) {
        Some(synth::REPORT_FORMAT) => return Some(Product::Synth),
        Some(rustsys_check::REPORT_FORMAT) => return Some(Product::CheckSynth),
        _ => {}
    }
    if v.get("aig_nodes_before").is_some() {
        Some(Product::Synth)
    } else if v.get("outputs_checked").is_some() {
        Some(Product::CheckSynth)
    } else {
        None
    }
}

/// The newest `format_version` of each kind of report that this program reads.
fn newest_version(kind: Option<Product>) -> u64 {
    match kind {
        Some(Product::CheckSynth) => u64::from(rustsys_check::REPORT_FORMAT_VERSION),
        Some(Product::Synth) | None => 1,
    }
}

/// Parses a report's JSON text. A `format_version` newer than the program reads for the report's
/// kind is `A0021`; any other file that the reader of its kind refuses is `A0020`
/// (PROTO-REQ-0062).
pub fn parse_report(text: &str, name: &str) -> Result<Report, Diag> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| Diag::error("A0020", format!("{name}: not JSON: {e}")))?;
    let kind = kind_of(&v);
    if v.get("format_version")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|n| n > newest_version(kind))
    {
        return Err(Diag::error(
            "A0021",
            format!("{name}: report format is newer than this program"),
        ));
    }
    let bad = |e: serde_json::Error| Diag::error("A0020", format!("{name}: {e}"));
    match kind {
        Some(Product::Synth) => serde_json::from_value(v).map(Report::Synth).map_err(bad),
        Some(Product::CheckSynth) => serde_json::from_value(v)
            .map(Report::CheckSynth)
            .map_err(bad),
        None => Err(Diag::error(
            "A0020",
            format!("{name}: not a RustSys report"),
        )),
    }
}

/// A loaded run or report.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub record: Option<RunRecord>,
    pub dir: Option<PathBuf>,
    pub report_path: PathBuf,
    /// The report file's text, for byte-identical JSON output.
    pub report_text: String,
    pub report: Report,
}

/// The report of a run: `report.json` of synthesis, `check.json` of the check, or its recorded
/// report output.
pub fn report_path_of(dir: &Path, r: &RunRecord) -> PathBuf {
    let role = match r.product {
        Product::Synth => "report",
        Product::CheckSynth => "check",
    };
    let inside = dir.join(if role == "check" {
        "check.json"
    } else {
        "report.json"
    });
    // A moved or copied run directory still holds its implicit report.
    match r.output(role) {
        Some(p) if p.exists() || !inside.exists() => p,
        _ => inside,
    }
}

/// Loads a run directory or a report file.
pub fn load(loc: &RunLocation) -> Result<Loaded, Diag> {
    let (record, dir, path) = match loc {
        RunLocation::Dir(d) => {
            let r = read_record(d)?;
            let p = report_path_of(d, &r);
            (Some(r), Some(d.clone()), p)
        }
        RunLocation::Report(p) => (None, None, p.clone()),
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Diag::error("A0005", format!("{}: {e}", path.display())))?;
    let report = parse_report(&text, &path.display().to_string())?;
    Ok(Loaded {
        record,
        dir,
        report_path: path,
        report_text: text,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check_report(version: u64, verdict: &str) -> String {
        json!({
            "format": rustsys_check::REPORT_FORMAT,
            "format_version": version,
            "verdict": verdict,
            "outputs_checked": 1,
            "state_pairs_checked": 0,
            "rtl_state_bits": 0,
            "netlist_state_elements": 0,
            "unmatched_rtl_state_bits": [],
            "mismatches": [],
            "errors": [],
            "sat_calls": 0,
            "seed": 1,
            "runtime_s": 0.0
        })
        .to_string()
    }

    // PROTO-REQ-0062, CHECK-REQ-0085: check reports of format versions 1 and 2 are read, a newer
    // one is A0021; each kind of report has its own newest version.
    #[test]
    fn format_versions_per_kind() {
        use rustsys_check::Verdict;
        let verdict = |text: &str| match parse_report(text, "check.json") {
            Ok(Report::CheckSynth(r)) => r.verdict,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            verdict(&check_report(1, "refinement")),
            Verdict::InitialStateRefinement
        );
        assert_eq!(verdict(&check_report(2, "undecided")), Verdict::Undecided);
        let newer = parse_report(&check_report(3, "equivalent"), "check.json").unwrap_err();
        assert_eq!(newer.code, "A0021");
        let synth = json!({"format": synth::REPORT_FORMAT, "format_version": 2}).to_string();
        assert_eq!(
            parse_report(&synth, "report.json").unwrap_err().code,
            "A0021"
        );
    }
}
