// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Observation and cancellation of synthesis runs (JOB-REQ-0005, JOB-REQ-0006, SYNTH-REQ-0707,
//! SYNTH-REQ-0710).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusteda_core::job::{CancelToken, Ctx, Event, JobError, Observer};
use rusteda_formats::liberty::Library;
use rustsys::view::{PHASES, Report};
use rustsys::{Sources, SynthOptions, synthesize, synthesize_with};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;

fn lib() -> Library {
    rusteda_formats::liberty::read_text(&library(Variant::Full), "full.lib").expect("library")
}

fn design(name: &str) -> (Sources, SynthOptions) {
    let d = CORPUS.iter().find(|d| d.name == name).expect("design");
    let files = d
        .files
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    (
        Sources::Texts(files),
        SynthOptions {
            top: Some(d.top.to_string()),
            ..SynthOptions::default()
        },
    )
}

/// The report without its run times.
fn timeless(r: &Report) -> serde_json::Value {
    let mut v = serde_json::to_value(r).expect("json");
    for k in [
        "runtime_s",
        "phase_s",
        "map_phase_s",
        "opt_phase_s",
        "opt_work",
    ] {
        v.as_object_mut().expect("object").remove(k);
    }
    v
}

/// Records events, with the seconds of finished phases left out.
#[derive(Default)]
struct Recorder(Mutex<Vec<Event>>);

impl Observer for Recorder {
    fn event(&self, e: &Event) {
        let e = match e {
            Event::PhaseFinished { phase, .. } => Event::PhaseFinished {
                phase,
                seconds: 0.0,
            },
            e => e.clone(),
        };
        self.0.lock().expect("lock").push(e);
    }
}

// JOB-REQ-0005, SYNTH-REQ-0710: an observed run with a cancel token that is never set gives the
// results of an unobserved run, bit for bit, and the same events every time.
#[test]
fn observation_changes_nothing() {
    let lib = lib();
    for name in ["counter", "alu", "fifo", "rv_core"] {
        let (src, opts) = design(name);
        let plain = synthesize(&src, std::slice::from_ref(&lib), &opts).expect("synthesis");
        let mut seqs = Vec::new();
        for _ in 0..2 {
            let rec = Recorder::default();
            let token = CancelToken::new();
            let out = synthesize_with(
                &src,
                std::slice::from_ref(&lib),
                &opts,
                Ctx::new(Some(&token), Some(&rec)),
            )
            .unwrap_or_else(|_| panic!("{name}: observed synthesis failed"));
            assert_eq!(out.text, plain.text, "{name}: netlist differs");
            assert_eq!(timeless(&out.report), timeless(&plain.report), "{name}");
            seqs.push(rec.0.into_inner().expect("lock"));
        }
        assert_eq!(seqs[0], seqs[1], "{name}: events differ between runs");
        // Every phase starts and finishes, in order.
        let started: Vec<&str> = seqs[0]
            .iter()
            .filter_map(|e| match e {
                Event::PhaseStarted { phase } => Some(*phase),
                _ => None,
            })
            .collect();
        assert_eq!(started, PHASES.to_vec(), "{name}");
        let finished = seqs[0]
            .iter()
            .filter(|e| matches!(e, Event::PhaseFinished { .. }))
            .count();
        assert_eq!(finished, PHASES.len(), "{name}");
    }
}

/// Sets the token at the `k`-th event.
struct CancelAt<'a> {
    token: &'a CancelToken,
    k: usize,
    seen: AtomicUsize,
}

impl Observer for CancelAt<'_> {
    fn event(&self, _e: &Event) {
        if self.seen.fetch_add(1, Ordering::Relaxed) + 1 == self.k {
            self.token.cancel();
        }
    }
}

// JOB-REQ-0006: a run cancelled at any point returns `Cancelled` and no result; cancelled
// before the start it does no work.
#[test]
fn cancel_at_every_point() {
    let lib = lib();
    let (src, opts) = design("alu");
    let rec = Recorder::default();
    synthesize_with(
        &src,
        std::slice::from_ref(&lib),
        &opts,
        Ctx::new(None, Some(&rec)),
    )
    .expect("synthesis");
    let n = rec.0.into_inner().expect("lock").len();
    assert!(n > 10, "{n} events");
    let points: Vec<usize> = (1..n).step_by((n / 12).max(1)).collect();
    for k in points {
        let token = CancelToken::new();
        let obs = CancelAt {
            token: &token,
            k,
            seen: AtomicUsize::new(0),
        };
        let r = synthesize_with(
            &src,
            std::slice::from_ref(&lib),
            &opts,
            Ctx::new(Some(&token), Some(&obs)),
        );
        assert!(
            matches!(r, Err(JobError::Cancelled)),
            "cancelled at event {k} of {n}"
        );
    }
    let token = CancelToken::new();
    token.cancel();
    let r = synthesize_with(
        &src,
        std::slice::from_ref(&lib),
        &opts,
        Ctx::new(Some(&token), None),
    );
    assert!(matches!(r, Err(JobError::Cancelled)));
}

// SYNTH-REQ-0707: the report's `cell_area` times the cell counts sums to the reported area, tie
// cells included; `time_unit_ns` is the library's time unit.
#[test]
fn cell_areas_sum_to_the_area() {
    let lib = lib();
    for name in ["counter", "alu", "decoder", "rv_core"] {
        let (src, opts) = design(name);
        let out = synthesize(&src, std::slice::from_ref(&lib), &opts).expect("synthesis");
        let r = &out.report;
        assert_eq!(r.format, "rustsys-synth-report");
        assert_eq!(r.format_version, 1);
        let sum: f64 = r
            .cells
            .iter()
            .map(|(c, n)| r.cell_area.get(c).copied().expect("area of a used cell") * *n as f64)
            .sum();
        assert!(
            (sum - r.area).abs() <= 1e-9 * r.area.abs().max(1.0),
            "{name}: {sum} vs {}",
            r.area
        );
        assert!(r.time_unit_ns.is_some_and(|t| t > 0.0), "{name}");
    }
}
