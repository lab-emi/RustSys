// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The engine run of each product within `execute` (PROTO-REQ-0030 to PROTO-REQ-0033): inputs,
//! engine, outputs and key figures.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusteda_core::job::{Ctx, Diag, Event, JobError};
use rusteda_formats::liberty::Library;
use rustsys::view as synth;
use rustsys_check::liberty::{Unread, unread};
use rustsys_check::{CheckOptions, CheckReport, Input, Verdict};

use crate::proto::ExitClass;
use crate::proto::exec::{ExecOptions, emit_text};
use crate::proto::job::{CheckSynthJob, SynthJob, split_kv};
use crate::proto::load::Report;
use crate::proto::record::OutputRecord;
use crate::proto::write::{write_json_atomic, write_text_atomic};

pub(crate) fn libraries(paths: &[PathBuf], code: &str, ctx: Ctx<'_>) -> Option<Vec<Library>> {
    ctx.emit(|| Event::PhaseStarted { phase: "libraries" });
    let t = Instant::now();
    let mut out = Vec::with_capacity(paths.len());
    let mut ok = true;
    for p in paths {
        match rusteda_formats::liberty::read_file(p) {
            Ok(l) => out.push(l),
            Err(e) => {
                ok = false;
                ctx.diag(|| Diag::error(code, format!("{}: {e}", p.display())));
            }
        }
    }
    ctx.emit(|| Event::PhaseFinished {
        phase: "libraries",
        seconds: t.elapsed().as_secs_f64(),
    });
    ok.then_some(out)
}

/// The libraries of a check, with the constructs of their texts that the library model does not
/// keep (CHECK-REQ-0023): each file is read once for both.
fn check_libraries(paths: &[PathBuf], ctx: Ctx<'_>) -> Option<(Vec<Library>, Vec<Unread>)> {
    ctx.emit(|| Event::PhaseStarted { phase: "libraries" });
    let t = Instant::now();
    let mut libs = Vec::with_capacity(paths.len());
    let mut constructs = Vec::new();
    let mut ok = true;
    for p in paths {
        let file = p.display().to_string();
        let read = std::fs::read(p)
            .map_err(|e| {
                rusteda_formats::liberty::Error::Io {
                    path: file.clone(),
                    msg: e.to_string(),
                }
                .to_string()
            })
            .and_then(|bytes| {
                let text = String::from_utf8_lossy(&bytes);
                let lib =
                    rusteda_formats::liberty::read_text(&text, &file).map_err(|e| e.to_string())?;
                Ok((lib, unread(&text, &file)?))
            });
        match read {
            Ok((l, u)) => {
                libs.push(l);
                constructs.extend(u);
            }
            Err(e) => {
                ok = false;
                ctx.diag(|| Diag::error("C0003", format!("{file}: {e}")));
            }
        }
    }
    ctx.emit(|| Event::PhaseFinished {
        phase: "libraries",
        seconds: t.elapsed().as_secs_f64(),
    });
    ok.then_some((libs, constructs))
}

/// What a product run produced.
pub(crate) struct Produced {
    pub(crate) class: ExitClass,
    pub(crate) report: Option<Report>,
    pub(crate) report_path: Option<PathBuf>,
    pub(crate) outputs: Vec<OutputRecord>,
    pub(crate) text: String,
    pub(crate) summary: BTreeMap<String, f64>,
    pub(crate) load_s: f64,
    pub(crate) engine_s: f64,
    pub(crate) write_s: f64,
}

impl Produced {
    pub(crate) fn failed(class: ExitClass) -> Produced {
        Produced {
            class,
            report: None,
            report_path: None,
            outputs: Vec::new(),
            text: String::new(),
            summary: BTreeMap::new(),
            load_s: 0.0,
            engine_s: 0.0,
            write_s: 0.0,
        }
    }
}

pub(crate) fn class_of<E>(e: &JobError<E>, internal: impl Fn(&E) -> bool) -> ExitClass {
    match e {
        JobError::Cancelled => ExitClass::Cancelled,
        JobError::Failed(x) if internal(x) => ExitClass::Internal,
        JobError::Failed(_) => ExitClass::Rejected,
    }
}

pub(crate) fn write_or(ctx: Ctx<'_>, code: &str, path: &Path, r: std::io::Result<()>) -> bool {
    match r {
        Ok(()) => true,
        Err(e) => {
            ctx.diag(|| Diag::error(code, format!("cannot write {}: {e}", path.display())));
            false
        }
    }
}

pub(crate) fn run_synth(j: &SynthJob, dir: Option<&Path>, ctx: Ctx<'_>) -> Produced {
    let Some(out) = j.out.clone().or_else(|| dir.map(|d| d.join("netlist.v"))) else {
        ctx.diag(|| {
            Diag::error("S0703", "no output netlist")
                .with_help(crate::proto::codes::help_for("S0703").unwrap_or_default())
        });
        return Produced::failed(ExitClass::Rejected);
    };
    let report_path = j
        .report
        .clone()
        .or_else(|| dir.map(|d| d.join("report.json")));
    let t = Instant::now();
    let Some(libs) = libraries(&j.fe.libs, "S0500", ctx) else {
        return Produced::failed(ExitClass::Rejected);
    };
    let Some(after_reset) = reset_values(&j.fe.after_reset, ctx) else {
        return Produced::failed(ExitClass::Rejected);
    };
    let electrical_corners = if j.electrical_corners.is_empty() {
        Vec::new()
    } else {
        let Some(corners) = libraries(&j.electrical_corners, "S0500", ctx) else {
            return Produced::failed(ExitClass::Rejected);
        };
        corners
    };
    let Some(backend) = j.backend.map_backend() else {
        ctx.diag(|| {
            Diag::error(
                "S0530",
                format!("the {} runtime no longer exists", j.backend.name()),
            )
            .with_help("use --backend cpu, gpu or cuda")
        });
        return Produced::failed(ExitClass::Rejected);
    };
    // Libraries of a gate-level netlist and of BLIF cells (CLI-REQ-0221).
    let Some(gate_libs) = libraries(&j.fe.gate_libs, "S0500", ctx) else {
        return Produced::failed(ExitClass::Rejected);
    };
    let load_s = t.elapsed().as_secs_f64();
    let opts = synth::SynthOptions {
        top: j.fe.top.clone(),
        defines: split_kv(&j.fe.defines),
        include_dirs: j.fe.include.clone(),
        params: split_kv(&j.fe.params),
        no_predefined_macros: j.fe.no_predefined_macros,
        after_reset,
        uninitialized_dont_care: j.fe.uninitialized_dont_care,
        assume_full_case: j.fe.assume_full_case,
        assume_parallel_case: j.fe.assume_parallel_case,
        dont_use: j.dont_use.clone(),
        effort: j.effort.engine(),
        goal: j.goal.engine(),
        delay_relax: j.delay_relax,
        electrical: j.electrical.clone(),
        default_quality: Some(Default::default()),
        electrical_corners,
        backend,
        gate_libs,
        gate_netlist: j.fe.gate_netlist.is_some(),
        initial_values: j.fe.initial_values.unwrap_or_default().engine(),
    };
    let t = Instant::now();
    let res = synth::synthesize_with(
        &synth::Sources::Files(j.fe.source_files()),
        &libs,
        &opts,
        ctx,
    );
    let engine_s = t.elapsed().as_secs_f64();
    let out_mod = match res {
        Ok(x) => x,
        Err(e) => return Produced::failed(class_of(&e, |x: &synth::SynthError| x.internal)),
    };
    let t = Instant::now();
    let mut outputs = vec![OutputRecord {
        role: "netlist".into(),
        path: out.clone(),
    }];
    if !write_or(ctx, "S0702", &out, write_text_atomic(&out, &out_mod.text)) {
        return Produced::failed(ExitClass::Rejected);
    }
    if let Some(p) = &report_path {
        if !write_or(ctx, "S0702", p, write_json_atomic(p, &out_mod.report)) {
            return Produced::failed(ExitClass::Rejected);
        }
        outputs.push(OutputRecord {
            role: "report".into(),
            path: p.clone(),
        });
    }
    let write_s = t.elapsed().as_secs_f64();
    let r = &out_mod.report;
    let summary = BTreeMap::from([
        ("area".to_string(), r.area),
        ("cells".to_string(), r.cells.values().sum::<usize>() as f64),
        ("logic_depth".to_string(), f64::from(r.logic_depth)),
    ]);
    Produced {
        class: ExitClass::Ok,
        text: crate::proto::text::synth_summary(r),
        report: Some(Report::Synth(out_mod.report)),
        report_path,
        outputs,
        summary,
        load_s,
        engine_s,
        write_s,
    }
}

pub(crate) fn run_check_synth(
    j: &CheckSynthJob,
    dir: Option<&Path>,
    o: &ExecOptions,
    ctx: Ctx<'_>,
) -> Produced {
    let report_path = j
        .report
        .clone()
        .or_else(|| dir.map(|d| d.join("check.json")));
    let t = Instant::now();
    let Some((libs, unread_liberty)) = check_libraries(&j.fe.libs, ctx) else {
        return Produced::failed(ExitClass::Rejected);
    };
    // Libraries of a gate-level reference and of BLIF cells (CLI-REQ-0221).
    let Some((gate_libs, unread_gate_liberty)) = check_libraries(&j.fe.gate_libs, ctx) else {
        return Produced::failed(ExitClass::Rejected);
    };
    let load_s = t.elapsed().as_secs_f64();
    let Some(after_reset) = reset_values(&j.fe.after_reset, ctx) else {
        return Produced::failed(ExitClass::Rejected);
    };
    let opts = CheckOptions {
        rtl: Input::Files(j.fe.source_files()),
        gate_netlist: j.fe.gate_netlist.is_some(),
        gate_libs,
        unread_gate_liberty,
        initial_values: j.fe.initial_values.unwrap_or_default().checker(),
        top: j.fe.top.clone(),
        defines: split_kv(&j.fe.defines),
        include_dirs: j.fe.include.clone(),
        params: split_kv(&j.fe.params),
        no_predefined_macros: j.fe.no_predefined_macros,
        after_reset,
        uninitialized_dont_care: j.fe.uninitialized_dont_care,
        assume_full_case: j.fe.assume_full_case,
        assume_parallel_case: j.fe.assume_parallel_case,
        netlist: Input::Files(j.netlists.clone()),
        seed: j.seed,
        budget: j.budget,
        model_budget: j.model_budget,
        sweep_max_candidates: j.sweep_max_candidates,
        verbose: o.verbose,
        cycles: j.cycles,
        supply_ports: j.supply_ports.clone(),
        unread_liberty,
        ..CheckOptions::default()
    };
    let t = Instant::now();
    let Ok(rep) = rustsys_check::check_with(&opts, &libs, ctx) else {
        return Produced::failed(ExitClass::Cancelled);
    };
    let engine_s = t.elapsed().as_secs_f64();
    // An internal failure of the checker is a defect, not an input error (CHECK-REQ-0001).
    let code = if rep.internal_error { "A0002" } else { "C0001" };
    for e in &rep.errors {
        emit_text(ctx, code, e);
    }
    // What a budget left undecided is not an input error (CHECK-REQ-0002).
    for u in &rep.undecided {
        ctx.diag(|| Diag::warning("C0005", u.clone()));
    }
    let class = check_class(&rep);
    let t = Instant::now();
    let mut outputs = Vec::new();
    if let Some(p) = &report_path {
        if !write_or(ctx, "C0004", p, write_json_atomic(p, &rep)) {
            return Produced::failed(ExitClass::Rejected);
        }
        outputs.push(OutputRecord {
            role: "check".into(),
            path: p.clone(),
        });
    }
    Produced {
        class,
        text: crate::proto::text::check_synth(&rep),
        summary: BTreeMap::from([("mismatches".to_string(), rep.mismatches.len() as f64)]),
        report: Some(Report::CheckSynth(rep)),
        report_path,
        outputs,
        load_s,
        engine_s,
        write_s: t.elapsed().as_secs_f64(),
    }
}

/// The exit class of a check report (CHECK-REQ-0001, PROTO-REQ-0031).
fn check_class(rep: &CheckReport) -> ExitClass {
    match rep.verdict {
        Verdict::Equivalent | Verdict::InitialStateRefinement => ExitClass::Ok,
        Verdict::NotEquivalent => ExitClass::Negative,
        Verdict::Undecided => ExitClass::Undecided,
        Verdict::Error if rep.internal_error => ExitClass::Internal,
        Verdict::Error => ExitClass::Rejected,
    }
}

fn reset_values(values: &[String], ctx: Ctx<'_>) -> Option<Vec<(String, bool)>> {
    values
        .iter()
        .map(|text| {
            if let Some((name, value)) = text.split_once('=') {
                if !name.is_empty() && matches!(value, "0" | "1") {
                    return Some((name.to_owned(), value == "1"));
                }
            }
            ctx.diag(|| {
                Diag::error(
                    "P0001",
                    format!("invalid --after-reset {text:?}: expected NAME=0 or NAME=1"),
                )
            });
            None
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rustsys_check::{CheckReport, Verdict};
    use serde_json::json;

    use crate::proto::ExitClass;

    // CHECK-REQ-0001, CHECK-REQ-0002, PROTO-REQ-0031: each verdict has its exit class; `error`
    // is an input error (1) unless the checker failed internally (2).
    #[test]
    fn check_verdicts_have_their_exit_classes() {
        let mut r: CheckReport = serde_json::from_value(json!({
            "verdict": "equivalent",
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
        }))
        .unwrap();
        assert!(!r.internal_error);
        let classes = [
            ExitClass::Ok,
            ExitClass::Ok,
            ExitClass::Negative,
            ExitClass::Undecided,
            ExitClass::Rejected,
        ];
        for (v, class) in Verdict::ALL.into_iter().zip(classes) {
            r.verdict = v;
            assert_eq!(super::check_class(&r), class, "{v:?}");
        }
        r.internal_error = true;
        assert_eq!(super::check_class(&r), ExitClass::Internal);
        assert_eq!(
            [
                ExitClass::Rejected,
                ExitClass::Internal,
                ExitClass::Undecided
            ]
            .map(ExitClass::status),
            [1, 2, 5]
        );
    }
}
