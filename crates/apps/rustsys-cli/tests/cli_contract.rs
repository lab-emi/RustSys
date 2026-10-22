// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The command-line contract (`docs/specs/cli/README.md`): streams, exit statuses, recorded
//! runs, run references, `show` and reproduction by the canonical command.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;

struct Env {
    dir: PathBuf,
    lib: PathBuf,
    sources: Vec<PathBuf>,
    top: String,
    root: PathBuf,
}

fn env(tag: &str) -> Env {
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("cells.lib");
    std::fs::write(&lib, library(Variant::Full)).unwrap();
    let d = CORPUS.iter().find(|d| d.name == "counter").unwrap();
    let sources = d
        .files
        .iter()
        .map(|(n, t)| {
            let p = dir.join(n);
            std::fs::write(&p, t).unwrap();
            p
        })
        .collect();
    Env {
        root: dir.join("runs"),
        dir,
        lib,
        sources,
        top: d.top.into(),
    }
}

impl Env {
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rustsys"));
        c.env("RUSTSYS_RUNS_ROOT", &self.root)
            .env_remove("NO_COLOR")
            .current_dir(&self.dir);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    fn synth_args(&self) -> Vec<String> {
        let mut a = vec![
            "synth".to_string(),
            "--top".into(),
            self.top.clone(),
            "--lib".into(),
            self.lib.display().to_string(),
        ];
        a.extend(self.sources.iter().map(|p| p.display().to_string()));
        a
    }

    fn synth(&self, extra: &[&str]) -> Output {
        let mut a = self.synth_args();
        a.extend(extra.iter().map(|s| s.to_string()));
        self.cmd().args(&a).output().unwrap()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn only_dir(root: &Path, product: &str) -> PathBuf {
    dirs(root, product).pop().unwrap()
}

/// Run directories of a product: `<time>Z-<product>-<suffix>`, so that `synth` does not match
/// `check-synth`.
fn dirs(root: &Path, product: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(&format!("Z-{product}-"))
        })
        .collect();
    v.sort();
    v
}

fn record(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("run.json")).unwrap()).unwrap()
}

fn timeless(p: &Path) -> serde_json::Value {
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    for k in [
        "runtime_s",
        "phase_s",
        "map_phase_s",
        "opt_phase_s",
        "opt_work",
    ] {
        v.as_object_mut().unwrap().remove(k);
    }
    v
}

// CLI-REQ-0002, CLI-REQ-0003, SYNTH-REQ-0705, SYNTH-REQ-0707: results on standard output,
// messages on standard error; outputs default to the run directory; `--print json` is the
// report file and `--print none` nothing.
#[test]
fn streams_and_run_directories() {
    let e = env("streams");
    let rd = e.dir.join("run1");
    let o = e.synth(&["--run-dir", rd.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("area"), "{out}");
    assert!(!out.contains("note[") && !out.contains("error["), "{out}");
    assert!(
        text(&o.stderr).contains("note[A0110]"),
        "{}",
        text(&o.stderr)
    );
    let rep = std::fs::read_to_string(rd.join("report.json")).unwrap();
    assert!(rep.starts_with("{\n  \"format\": \"rustsys-synth-report\",\n  \"format_version\": 1"));
    assert!(rd.join("netlist.v").exists() && rd.join("events.jsonl").exists());
    assert_eq!(record(&rd)["status"], "ok");
    let rd2 = e.dir.join("run2");
    let o = e.synth(&["--run-dir", rd2.to_str().unwrap(), "--print", "json"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(o.stdout, std::fs::read(rd2.join("report.json")).unwrap());
    let o = e.synth(&[
        "--run-dir",
        e.dir.join("run3").to_str().unwrap(),
        "--print",
        "none",
    ]);
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stdout.is_empty());
    // A run directory that holds a run already is refused.
    let o = e.synth(&["--run-dir", rd.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
}

// SYNTH-REQ-0705, SYNTH-REQ-0706, CLI-REQ-0010: no output netlist, write failures and bad
// relaxations are input errors with their codes.
#[test]
fn synth_errors() {
    let e = env("synth-errors");
    let o = e.synth(&[]);
    assert_eq!(o.status.code(), Some(1));
    let err = text(&o.stderr);
    assert!(
        err.contains("error[S0703]") && err.contains("  help: "),
        "{err}"
    );
    let blocker = e.dir.join("file");
    std::fs::write(&blocker, "x").unwrap();
    let bad = blocker.join("out.v");
    let o = e.synth(&["--out", bad.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("error[S0702]"),
        "{}",
        text(&o.stderr)
    );
    for v in ["-1", "nan", "inf"] {
        let o = e.synth(&["--out", "x.v", "--delay-relax", v]);
        assert_eq!(o.status.code(), Some(1), "--delay-relax {v}");
        assert!(
            text(&o.stderr).contains("error[A0003]"),
            "{}",
            text(&o.stderr)
        );
    }
}

// CHECK-REQ-0082, PROTO-REQ-0061: `check synth` completes its job from a synthesis run; library
// and write failures give their codes.
#[test]
fn from_run_and_codes() {
    let e = env("from-run");
    let o = e.synth(&["--record"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let synth_dir = only_dir(&e.root, "synth");
    // check synth from the synthesis run.
    let o = e.run(&["check", "synth", "--from-run", "@synth"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}{}",
        text(&o.stdout),
        text(&o.stderr)
    );
    assert!(
        text(&o.stdout).contains("verdict: equivalent"),
        "{}",
        text(&o.stdout)
    );
    assert!(dirs(&e.root, "check-synth").is_empty());
    // A recorded check names the netlist and the top module of the synthesis run.
    let o = e.run(&["check", "synth", "--from-run", "@synth", "--record"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let rec = record(&only_dir(&e.root, "check-synth"));
    let netlist = rec["job"]["netlists"][0].as_str().unwrap().to_string();
    assert_eq!(Path::new(&netlist), synth_dir.join("netlist.v"));
    assert_eq!(rec["job"]["top"], "counter");
    let blocker = e.dir.join("file");
    std::fs::write(&blocker, "x").unwrap();
    let bad = blocker.join("r.json");
    let badlib = e.dir.join("bad.lib");
    std::fs::write(&badlib, "library (x) { cell (").unwrap();
    let o = e.run(&[
        "check",
        "synth",
        "--from-run",
        "@synth",
        "--lib",
        badlib.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("error[C0003]"),
        "{}",
        text(&o.stderr)
    );
    let o = e.run(&[
        "check",
        "synth",
        "--from-run",
        "@synth",
        "--report",
        bad.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("error[C0004]"),
        "{}",
        text(&o.stderr)
    );
}

// PROTO-REQ-0021: the canonical command of a recorded run reproduces its outputs.
#[test]
fn canonical_command_reproduces_the_run() {
    let e = env("reproduce");
    let o = e.synth(&["--record", "--label", "first"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let before = dirs(&e.root, "synth");
    assert_eq!(before.len(), 1);
    let first = before[0].clone();
    let rec = record(&first);
    let words = rustsys_cli::proto::argv::split(rec["command"].as_str().unwrap()).unwrap();
    assert_eq!(words[0], "rustsys");
    let o = e
        .cmd()
        .current_dir(rec["cwd"].as_str().unwrap())
        .args(&words[1..])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let after = dirs(&e.root, "synth");
    assert_eq!(after.len(), 2);
    let second = after.into_iter().find(|d| *d != first).unwrap();
    assert_eq!(
        timeless(&first.join("report.json")),
        timeless(&second.join("report.json"))
    );
    assert_eq!(
        std::fs::read(first.join("netlist.v")).unwrap(),
        std::fs::read(second.join("netlist.v")).unwrap()
    );
    assert_eq!(record(&second)["command"], rec["command"]);
}

// CLI-REQ-0010, CLI-REQ-0011, PROTO-REQ-0032: with standard output closed by its reader, the
// run still writes its outputs and record and exits with the status of the computation.
#[test]
fn outputs_before_standard_output() {
    let e = env("pipe");
    let mut c = e.cmd();
    let mut a = e.synth_args();
    a.push("--record".into());
    let mut child = c
        .args(&a)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let st = child.wait().unwrap();
    assert_eq!(st.code(), Some(0));
    let d = only_dir(&e.root, "synth");
    assert_eq!(record(&d)["status"], "ok");
    assert!(d.join("netlist.v").exists() && d.join("report.json").exists());
}

// CLI-REQ-0100 to CLI-REQ-0102: sections, filters, TSV, the report file as JSON, and the
// sections that a report file cannot provide or that do not exist.
#[test]
fn show_sections() {
    let e = env("show");
    assert_eq!(e.synth(&["--record"]).status.code(), Some(0));
    let report = only_dir(&e.root, "synth").join("report.json");
    let rep: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let many = rep["cells"]
        .as_object()
        .unwrap()
        .values()
        .filter(|n| n.as_u64().unwrap() >= 2)
        .count();
    let o = e.run(&[
        "show",
        "@synth",
        "--section",
        "cells",
        "--filter",
        "count>=2",
        "--print",
        "tsv",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let t = text(&o.stdout);
    let mut lines = t.lines();
    assert_eq!(
        lines.next(),
        Some("cell\tcount\tarea_each\tarea_total\tshare")
    );
    assert_eq!(lines.count(), many.min(20));
    // Without sections, --print json is the report file itself.
    let o = e.run(&["show", "@synth", "--print", "json"]);
    assert_eq!(o.stdout, std::fs::read(&report).unwrap());
    // The command is in the run record, which a report file does not have.
    let o = e.run(&["show", "@synth", "--section", "command"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(
        text(&o.stdout).starts_with("rustsys synth "),
        "{}",
        text(&o.stdout)
    );
    let o = e.run(&["show", report.to_str().unwrap(), "--section", "command"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("error[A0010]"));
    let o = e.run(&["show", "@synth", "--section", "no-such"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("error[A0022]"));
}

// CLI-REQ-0150: completion scripts.
#[test]
fn completions() {
    let e = env("completions");
    for sh in ["bash", "zsh", "fish"] {
        let o = e.run(&["completions", sh]);
        assert_eq!(o.status.code(), Some(0), "{sh}");
        assert!(text(&o.stdout).contains("rustsys"), "{sh}");
    }
}

// PROTO-REQ-0061: options given with --from-run win field by field; the rest comes from the run.
#[test]
fn from_run_keeps_explicit_options() {
    let e = env("from-run-merge");
    assert_eq!(e.synth(&["--record"]).status.code(), Some(0));
    let o = e.run(&[
        "check",
        "synth",
        "--from-run",
        "@synth",
        "-D",
        "EXTRA=1",
        "--record",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let rec = record(&only_dir(&e.root, "check-synth"));
    let cmd = rec["command"].as_str().unwrap();
    assert!(cmd.contains("-D EXTRA=1"), "{cmd}");
    assert!(cmd.contains("--lib "), "{cmd}");
    assert_eq!(rec["job"]["top"], "counter");
}

// PROTO-REQ-0020, PROTO-REQ-0021: a canonical command with values and sources that start with
// `-` parses back to the same values.
#[test]
fn dash_values_parse_back() {
    use rustsys_cli::proto::argv::{Placement, canonical_args};
    use rustsys_cli::proto::job::{FrontEnd, Job, SynthJob};
    let job = Job::Synth(SynthJob {
        fe: FrontEnd {
            libs: vec!["-c.lib".into()],
            defines: vec!["-X".into()],
            sources: vec!["-top.sv".into(), "b.sv".into()],
            ..Default::default()
        },
        out: Some("-o.v".into()),
        ..Default::default()
    });
    let p = Placement {
        label: Some("-baseline".into()),
        ..Default::default()
    };
    let mut args = vec!["rustsys".to_string()];
    args.extend(canonical_args(&job, &p, 0));
    use clap::Parser;
    let cli = rustsys_cli::args::Cli::try_parse_from(&args).unwrap();
    let rustsys_cli::args::Cmd::Synth(a) = cli.cmd else {
        panic!("not a synthesis command");
    };
    assert_eq!(a.job(), job);
    assert_eq!(a.place.placement(), p);
}
