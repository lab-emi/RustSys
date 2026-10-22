// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Execution of jobs (PROTO-REQ-0030 to PROTO-REQ-0033, PROTO-REQ-0050 to PROTO-REQ-0053,
//! PROTO-REQ-0061): the one path by which the command line runs engines. Nothing is printed;
//! every message is an event.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use rusteda_core::job::{CancelToken, Ctx, Diag, Event, Observer, Severity};

use crate::proto::argv::{Placement, canonical_command};
use crate::proto::job::Job;
use crate::proto::load::Report;
use crate::proto::progress::Tee;
use crate::proto::record::{
    MessageCounts, RUN_FORMAT, RunRecord, RunStatus, Timing, allocate, input_records, now_s,
    prepare_dir, rfc3339,
};
use crate::proto::write::write_json_atomic;
use crate::proto::{ExitClass, Product, VERSION};

/// Options of an execution that are not part of the job.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Worker threads the caller set up (for the record and the canonical command).
    pub threads: usize,
    /// Root of recorded runs.
    pub runs_root: Option<PathBuf>,
    /// The original arguments after `rustsys`, for the run record.
    pub arguments: Vec<String>,
    /// Verbose notes of the synthesis check.
    pub verbose: bool,
    /// GPU runtimes of this build, for the run record.
    pub runtimes: Vec<String>,
}

/// The result of an execution.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub class: ExitClass,
    /// The canonical command (PROTO-REQ-0020).
    pub command: String,
    pub dir: Option<PathBuf>,
    pub id: Option<String>,
    pub record: Option<RunRecord>,
    pub report: Option<Report>,
    pub report_path: Option<PathBuf>,
    /// The text result for standard output (empty unless the run produced results).
    pub text: String,
}

/// Counts messages and writes `events.jsonl` (PROTO-REQ-0053).
struct Recorder {
    counts: Mutex<MessageCounts>,
    log: Option<Mutex<std::io::BufWriter<std::fs::File>>>,
    t0: Instant,
}

impl Observer for Recorder {
    fn event(&self, e: &Event) {
        if let Event::Diagnostic(d) = e {
            if let Ok(mut c) = self.counts.lock() {
                match d.severity {
                    Severity::Error => c.error += 1,
                    Severity::Warning => c.warning += 1,
                    Severity::Note => c.note += 1,
                }
            }
        }
        if let Some(log) = &self.log {
            if let Ok(mut w) = log.lock() {
                let _ = serde_json::to_writer(&mut *w, &event_json(e, self.t0));
                let _ = w.write_all(b"\n");
            }
        }
    }
}

/// One event as JSON, with the seconds since the start of the run.
pub fn event_json(e: &Event, t0: Instant) -> serde_json::Value {
    use serde_json::json;
    let t = t0.elapsed().as_secs_f64();
    match e {
        Event::PhaseStarted { phase } => json!({"t": t, "event": "phase-started", "phase": phase}),
        Event::PhaseFinished { phase, seconds } => {
            json!({"t": t, "event": "phase-finished", "phase": phase, "seconds": seconds})
        }
        Event::Step {
            phase,
            step,
            ordinal,
            count,
        } => json!({"t": t, "event": "step", "phase": phase, "step": step,
                    "ordinal": ordinal, "count": count}),
        Event::Progress { phase, done, total } => {
            json!({"t": t, "event": "progress", "phase": phase, "done": done, "total": total})
        }
        Event::Metric { name, value } => {
            json!({"t": t, "event": "metric", "name": name, "value": value})
        }
        // The message's parts and its text.
        Event::Diagnostic(d) => {
            let mut v = crate::proto::message::to_json(d);
            if let Some(m) = v.as_object_mut() {
                m.insert("t".into(), json!(t));
                m.insert("event".into(), json!("message"));
                m.insert("text".into(), json!(d.render()));
            }
            v
        }
        _ => json!({"t": t, "event": "other"}),
    }
}

/// Emits an engine error text as coded messages: lines that are messages unchanged, other lines
/// with `code` (CLI-REQ-0004).
pub fn emit_text(ctx: Ctx<'_>, code: &str, text: &str) {
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        ctx.diag(|| Diag::parse(line).unwrap_or_else(|| Diag::error(code, line)));
    }
}

/// Runs `job` (PROTO-REQ-0030): places the run, runs the engine with the cancel token and the
/// observer, writes the outputs and the run record, and returns the outcome.
pub fn execute(
    job: &Job,
    placement: &Placement,
    o: &ExecOptions,
    cancel: Option<&CancelToken>,
    obs: Option<&dyn Observer>,
) -> Outcome {
    let t0 = Instant::now();
    let started = now_s();
    let product = job.product();
    let command = canonical_command(job, placement, o.threads);
    let mut out = Outcome {
        class: ExitClass::Rejected,
        command: command.clone(),
        dir: None,
        id: None,
        record: None,
        report: None,
        report_path: None,
        text: String::new(),
    };
    // Placement (PROTO-REQ-0050).
    let placed = if let Some(d) = &placement.run_dir {
        prepare_dir(d).map(|()| {
            let id = d
                .file_name()
                .map_or_else(|| "run".into(), |n| n.to_string_lossy().into_owned());
            Some((id, d.clone()))
        })
    } else if placement.record {
        match &o.runs_root {
            Some(root) => allocate(root, product, started).map(Some),
            None => Err(Diag::error("A0008", "--record needs a runs root")
                .with_help(crate::proto::codes::help_for("A0008").unwrap_or_default())),
        }
    } else {
        Ok(None)
    };
    let (id, dir) = match placed {
        Ok(Some((id, dir))) => (Some(id), Some(dir)),
        Ok(None) => (None, None),
        Err(d) => {
            if let Some(obs) = obs {
                obs.event(&Event::Diagnostic(d));
            }
            return out;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut record = id.as_ref().map(|id| RunRecord {
        format: RUN_FORMAT.into(),
        format_version: 1,
        id: id.clone(),
        product,
        label: placement.label.clone(),
        status: RunStatus::Running,
        exit_status: None,
        arguments: o.arguments.clone(),
        command: command.clone(),
        cwd: cwd.clone(),
        job: job.clone(),
        version: VERSION.into(),
        runtimes: o.runtimes.clone(),
        threads: o.threads,
        started: rfc3339(started),
        started_s: started,
        finished: None,
        timing: Timing::default(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        summary: BTreeMap::new(),
        diagnostics: MessageCounts::default(),
    });
    let log = dir.as_ref().and_then(|d| {
        std::fs::File::create(d.join("events.jsonl"))
            .ok()
            .map(|f| Mutex::new(std::io::BufWriter::new(f)))
    });
    let recorder = Recorder {
        counts: Mutex::new(MessageCounts::default()),
        log,
        t0,
    };
    let observers: Vec<&dyn Observer> = match obs {
        Some(o) => vec![&recorder, o],
        None => vec![&recorder],
    };
    let tee = Tee(&observers);
    let ctx = Ctx::new(cancel, Some(&tee));
    if let (Some(r), Some(d)) = (&record, &dir) {
        if let Err(e) = write_json_atomic(&d.join("run.json"), r) {
            ctx.diag(|| Diag::error("A0004", format!("{}: {e}", d.join("run.json").display())));
            return out;
        }
    }
    let (produced, inputs) = std::thread::scope(|s| {
        // Input hashes while the job runs (PROTO-REQ-0052).
        let hashes = record.is_some().then(|| s.spawn(|| input_records(job)));
        // A panic is an internal failure of the run, recorded like any other (PROTO-REQ-0031).
        use crate::proto::runners::{run_check_synth, run_synth};
        let p = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
            Job::Synth(j) => run_synth(j, dir.as_deref(), ctx),
            Job::CheckSynth(j) => run_check_synth(j, dir.as_deref(), o, ctx),
        }))
        .unwrap_or_else(|p| {
            let msg = panic_message(&*p);
            ctx.diag(|| Diag::error("A0002", format!("internal failure: {msg}")));
            crate::proto::runners::Produced::failed(ExitClass::Internal)
        });
        let inputs = hashes.and_then(|h| h.join().ok()).unwrap_or_default();
        (p, inputs)
    });
    if let Some(log) = &recorder.log {
        if let Ok(mut w) = log.lock() {
            let _ = w.flush();
        }
    }
    out.class = produced.class;
    if let (Some(r), Some(d)) = (&mut record, &dir) {
        r.status = RunStatus::from(produced.class);
        r.exit_status = Some(produced.class.status());
        r.finished = Some(rfc3339(now_s()));
        r.timing = Timing {
            load_s: produced.load_s,
            engine_s: produced.engine_s,
            write_s: produced.write_s,
            total_s: t0.elapsed().as_secs_f64(),
        };
        r.inputs = inputs;
        r.outputs = produced.outputs.clone();
        r.summary = produced.summary.clone();
        r.diagnostics = recorder.counts.lock().map(|c| *c).unwrap_or_default();
        if let Err(e) = write_json_atomic(&d.join("run.json"), r) {
            ctx.diag(|| Diag::error("A0004", format!("{}: {e}", d.join("run.json").display())));
            if out.class.has_result() {
                out.class = ExitClass::Rejected;
            }
        } else if produced.class.has_result() {
            ctx.diag(|| Diag::note("A0110", format!("recorded run {} in {}", r.id, d.display())));
        }
    }
    out.dir = dir;
    out.id = id;
    out.record = record;
    out.report_path = produced.report_path;
    // The report and the text of a finished run go to the caller (PROTO-REQ-0031).
    if out.class.has_result() {
        out.text = produced.text;
        out.report = produced.report;
    }
    out
}

/// Where the last panic happened, recorded by the quiet hook. Global rather than per thread: a
/// panic in a worker thread is caught on the thread that waits for it.
static PANIC_AT: Mutex<Option<String>> = Mutex::new(None);

/// Installs a panic hook that prints nothing and records where the panic happened, for the
/// `A0002` message built after the panic is caught (CLI-REQ-0002, CLI-REQ-0012).
pub fn install_quiet_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let at = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        if let Ok(mut p) = PANIC_AT.lock() {
            *p = at;
        }
    }));
}

/// The text of a panic payload, with where it happened when the quiet hook recorded it.
pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    let msg = p
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| "unknown panic".into());
    match PANIC_AT.lock().ok().and_then(|mut a| a.take()) {
        Some(at) => format!("{msg} (at {at})"),
        None => msg,
    }
}

/// Completes a job from a finished run (PROTO-REQ-0061); explicit options win.
pub fn complete_from_run(job: &mut Job, rec: &RunRecord, dir: &Path) -> Result<(), Diag> {
    let wrong = |need: &str| {
        Diag::error(
            "A0009",
            format!(
                "--from-run needs a {need} run; {} is a {} run",
                dir.display(),
                rec.product.name()
            ),
        )
    };
    let res = |ps: &[PathBuf]| ps.iter().map(|p| rec.resolve(p)).collect::<Vec<_>>();
    match job {
        Job::CheckSynth(j) => {
            let Job::Synth(s) = &rec.job else {
                return Err(wrong("synth"));
            };
            let netlist = rec
                .output("netlist")
                .ok_or_else(|| Diag::error("A0009", "the synth run has no netlist output"))?;
            // Field by field: what the command line gives wins (PROTO-REQ-0061).
            let f = &mut j.fe;
            if f.sources.is_empty() {
                f.sources = res(&s.fe.sources);
            }
            if f.libs.is_empty() {
                f.libs = res(&s.fe.libs);
            }
            if f.top.is_none() {
                f.top = synth_top(rec, dir).or_else(|| s.fe.top.clone());
            }
            if f.defines.is_empty() {
                f.defines = s.fe.defines.clone();
            }
            if f.params.is_empty() {
                f.params = s.fe.params.clone();
            }
            if f.include.is_empty() {
                f.include = res(&s.fe.include);
            }
            f.no_predefined_macros |= s.fe.no_predefined_macros;
            f.uninitialized_dont_care |= s.fe.uninitialized_dont_care;
            f.assume_full_case |= s.fe.assume_full_case;
            f.assume_parallel_case |= s.fe.assume_parallel_case;
            if f.after_reset.is_empty() {
                f.after_reset = s.fe.after_reset.clone();
            }
            // Sources other than RTL (CLI-REQ-0223).
            if f.gate_netlist.is_none() && f.sources.is_empty() {
                f.gate_netlist = s.fe.gate_netlist.as_ref().map(|g| rec.resolve(g));
            }
            if f.gate_libs.is_empty() {
                f.gate_libs = res(&s.fe.gate_libs);
            }
            if f.initial_values.is_none() {
                f.initial_values = s.fe.initial_values;
            }
            if j.netlists.is_empty() {
                j.netlists = vec![netlist];
            }
        }
        Job::Synth(_) => return Err(Diag::error("A0009", "synth takes no --from-run")),
    }
    Ok(())
}

/// The top module a synthesis run synthesized, from its report.
fn synth_top(rec: &RunRecord, dir: &Path) -> Option<String> {
    let p = crate::proto::load::report_path_of(dir, rec);
    let text = std::fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("top")?.as_str().map(String::from)
}

impl Product {
    /// The product of a job.
    pub fn of(job: &Job) -> Product {
        job.product()
    }
}
