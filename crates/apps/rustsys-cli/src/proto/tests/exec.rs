// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Execution of jobs (PROTO-REQ-0030 to PROTO-REQ-0033, PROTO-REQ-0053).

use std::path::PathBuf;
use std::sync::Mutex;

use crate::proto::argv::Placement;
use crate::proto::exec::{ExecOptions, execute};
use crate::proto::job::{FrontEnd, Job, SynthJob};
use crate::proto::{CancelToken, Event, ExitClass, Observer};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;

fn inputs(tag: &str) -> (PathBuf, Job) {
    let dir = std::env::temp_dir().join(format!("rustsys-exec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("cells.lib");
    std::fs::write(&lib, library(Variant::Full)).unwrap();
    let d = CORPUS.iter().find(|d| d.name == "alu").unwrap();
    let sources = d
        .files
        .iter()
        .map(|(n, t)| {
            let p = dir.join(n);
            std::fs::write(&p, t).unwrap();
            p
        })
        .collect();
    let job = Job::Synth(SynthJob {
        fe: FrontEnd {
            top: Some(d.top.into()),
            libs: vec![lib],
            sources,
            ..Default::default()
        },
        ..Default::default()
    });
    (dir, job)
}

/// At the start of each phase, what the run directory holds.
struct Watch {
    dir: PathBuf,
    seen: Mutex<Vec<(String, bool, bool)>>,
}

impl Observer for Watch {
    fn event(&self, e: &Event) {
        if let Event::PhaseStarted { phase } = e {
            let status = std::fs::read_to_string(self.dir.join("run.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .map(|v| v["status"].as_str().unwrap_or("").to_string())
                .unwrap_or_default();
            self.seen.lock().unwrap().push((
                format!("{phase}:{status}"),
                self.dir.join("netlist.v").exists(),
                self.dir.join("report.json").exists(),
            ));
        }
    }
}

// PROTO-REQ-0032: the run record says `running` before the engine starts; the outputs appear
// only after the engine; the final record comes last. PROTO-REQ-0053: every event is logged.
#[test]
fn order_of_effects() {
    let (dir, job) = inputs("order");
    let rd = dir.join("run");
    let w = Watch {
        dir: rd.clone(),
        seen: Mutex::new(Vec::new()),
    };
    let token = CancelToken::new();
    let out = execute(
        &job,
        &Placement {
            run_dir: Some(rd.clone()),
            record: false,
            label: None,
        },
        &ExecOptions::default(),
        Some(&token),
        Some(&w),
    );
    assert_eq!(out.class, ExitClass::Ok);
    let seen = w.seen.into_inner().unwrap();
    assert!(!seen.is_empty());
    for (what, netlist, report) in &seen {
        assert!(what.ends_with(":running"), "{what}");
        assert!(
            !netlist && !report,
            "{what}: outputs before the engine ended"
        );
    }
    let rec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(rd.join("run.json")).unwrap()).unwrap();
    assert_eq!(rec["status"], "ok");
    assert_eq!(rec["exit_status"], 0);
    assert!(rd.join("netlist.v").exists() && rd.join("report.json").exists());
    let log = std::fs::read_to_string(rd.join("events.jsonl")).unwrap();
    // Every line is JSON; every phase start the observer saw is logged.
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let started = lines
        .iter()
        .filter(|v| v["event"] == "phase-started")
        .count();
    assert_eq!(started, seen.len(), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

// PROTO-REQ-0031, PROTO-REQ-0032: a cancelled run leaves no output file and records
// `cancelled`.
#[test]
fn cancelled_run_leaves_no_outputs() {
    let (dir, job) = inputs("cancel");
    let rd = dir.join("run");
    let token = CancelToken::new();
    token.cancel();
    let out = execute(
        &job,
        &Placement {
            run_dir: Some(rd.clone()),
            record: false,
            label: None,
        },
        &ExecOptions::default(),
        Some(&token),
        None,
    );
    assert_eq!(out.class, ExitClass::Cancelled);
    assert!(!rd.join("netlist.v").exists() && !rd.join("report.json").exists());
    let rec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(rd.join("run.json")).unwrap()).unwrap();
    assert_eq!(rec["status"], "cancelled");
    assert_eq!(rec["exit_status"], 130);
    let _ = std::fs::remove_dir_all(&dir);
}
