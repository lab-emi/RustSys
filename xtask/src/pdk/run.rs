// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `cargo xtask verify pdk`: every fixture design of
//! `verification/fixtures/synthetic/synth/rtl/index.toml` synthesized with every library of
//! `verification/pdks.toml` by `rustsys synth`, and its netlist checked by `rustsys check synth`
//! (SYNTH-REQ-0801). Each run must exit 0 and the check must prove the netlist `equivalent`;
//! [`super::scan`] hazards and the cells and area of every netlist are reported. Outputs go to
//! `target/verify-pdk/<library>/<design>/`, with a summary and a table of runs beside them; each
//! run replaces that directory, so a library directory at or below it is refused.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::VerifyOptions;
use super::designs::{self, Design};
use super::fetch::{self, Tools};
use super::json::Json;
use super::manifest::{self, Library};
use super::scan::{self, Hazards, Mark};

/// Run outputs, relative to the repository root; each run deletes and writes them again.
pub(crate) const OUT: &str = "target/verify-pdk";

/// How a child process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exit {
    Code(i32),
    /// Ended by a signal.
    Signal,
    /// Killed when the time limit passed.
    TimedOut,
}

impl Exit {
    fn describe(self, limit: Duration) -> String {
        match self {
            Exit::Code(c) => format!("exit status {c}"),
            Exit::Signal => "ended by a signal".into(),
            Exit::TimedOut => format!("timed out after {} s", limit.as_secs()),
        }
    }
}

/// Runs `cmd`, killing it when `limit` passes; returns how it ended and its wall time (to within
/// the 5 ms polling interval).
pub(crate) fn run_timed(cmd: &mut Command, limit: Duration) -> Result<(Exit, Duration), String> {
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))?;
    let mut nap = Duration::from_millis(1);
    loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok((st.code().map_or(Exit::Signal, Exit::Code), start.elapsed()));
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return Ok((Exit::TimedOut, start.elapsed()));
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(5));
    }
}

/// File names inside a run directory.
const NETLIST: &str = "netlist.v";
const SYNTH_REPORT: &str = "report.json";
const CHECK_REPORT: &str = "check.json";

fn os(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

/// The arguments of `rustsys synth` for top module `top` with library file `lib`, writing the
/// netlist and the report into `run_dir`.
pub(crate) fn synth_args(
    top: &str,
    lib: &Path,
    run_dir: &Path,
    sources: &[PathBuf],
) -> Vec<OsString> {
    let mut a = os(&["--progress", "off", "synth", "--top", top, "--lib"]);
    a.push(lib.into());
    a.push("--out".into());
    a.push(run_dir.join(NETLIST).into());
    a.push("--report".into());
    a.push(run_dir.join(SYNTH_REPORT).into());
    a.extend(sources.iter().map(|s| s.clone().into_os_string()));
    a
}

/// The arguments of `rustsys check synth` for the netlist of [`synth_args`].
pub(crate) fn check_args(
    top: &str,
    lib: &Path,
    run_dir: &Path,
    sources: &[PathBuf],
) -> Vec<OsString> {
    let mut a = os(&["--progress", "off", "check", "synth", "--top", top, "--lib"]);
    a.push(lib.into());
    a.push("--netlist".into());
    a.push(run_dir.join(NETLIST).into());
    a.push("--report".into());
    a.push(run_dir.join(CHECK_REPORT).into());
    a.extend(sources.iter().map(|s| s.clone().into_os_string()));
    a
}

/// What the suite reads from a synthesis report.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SynthReport {
    /// Instances per cell.
    pub(crate) counts: Vec<(String, u64)>,
    pub(crate) area: f64,
    pub(crate) warnings: u64,
}

/// Reads the cell counts, the area and the warning count of a `rustsys synth` report.
pub(crate) fn synth_report(text: &str) -> Result<SynthReport, String> {
    let j = Json::parse(text)?;
    let counts = j
        .get("cells")
        .and_then(Json::as_obj)
        .ok_or("the report has no 'cells' object")?
        .iter()
        .map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
        .collect::<Option<Vec<_>>>()
        .ok_or("a cell count is not a count")?;
    let area = j
        .get("area")
        .and_then(Json::as_f64)
        .ok_or("the report has no 'area'")?;
    let warnings = j.get("warnings").and_then(Json::as_u64).unwrap_or(0);
    Ok(SynthReport {
        counts,
        area,
        warnings,
    })
}

/// Reads the verdict of a `rustsys check synth` report.
pub(crate) fn check_verdict(text: &str) -> Result<String, String> {
    Json::parse(text)?
        .get("verdict")
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the report has no 'verdict'".into())
}

/// The executable of the `rustsys` binary in cargo's JSON build messages.
pub(crate) fn executable(messages: &str) -> Option<PathBuf> {
    messages
        .lines()
        .filter_map(|l| Json::parse(l).ok())
        .filter(|m| m.get("reason").and_then(Json::as_str) == Some("compiler-artifact"))
        .filter(|m| {
            m.get("target")
                .and_then(|t| t.get("name"))
                .and_then(Json::as_str)
                == Some("rustsys")
        })
        .filter_map(|m| {
            m.get("executable")
                .and_then(Json::as_str)
                .map(PathBuf::from)
        })
        .next_back()
}

/// Builds the command line once (release, CPU runtime only) into cargo's target directory.
fn build(root: &Path) -> Result<PathBuf, String> {
    let args = [
        "build",
        "--release",
        "-p",
        "rustsys-cli",
        "--no-default-features",
        "--message-format=json-render-diagnostics",
    ];
    let out = Command::new(env!("CARGO"))
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cargo build: {e}"))?;
    if !out.status.success() {
        return Err("cargo build --release -p rustsys-cli --no-default-features failed".into());
    }
    executable(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "cargo build did not report the rustsys executable".into())
}

/// One synthesis-and-check run of a design with a library.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Run {
    pub(crate) library: String,
    pub(crate) design: String,
    /// The failed assertion; `None` when the run passed.
    pub(crate) failure: Option<String>,
    pub(crate) cells: Option<u64>,
    pub(crate) area: Option<f64>,
    pub(crate) warnings: Option<u64>,
    pub(crate) hazards: Hazards,
    pub(crate) synth_s: f64,
    pub(crate) check_s: f64,
}

/// The first error line of a standard-error file, else its last line, shortened.
fn first_error(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let line = text
        .lines()
        .find(|l| l.starts_with("error"))
        .or_else(|| text.lines().rfind(|l| !l.trim().is_empty()))
        .unwrap_or("no message");
    line.chars().take(240).collect()
}

/// What one design run needs besides the design.
struct Setup<'a> {
    bin: &'a Path,
    guard: &'a crate::guard::Guard,
    lib: &'a Library,
    lib_file: &'a Path,
    marks: &'a BTreeMap<String, Vec<Mark>>,
    rtl: &'a Path,
    out: &'a Path,
    limit: Duration,
}

/// Runs `rustsys` with `args`, standard output and error going to `<step>.stdout` and
/// `<step>.stderr` in `dir`.
fn step(s: &Setup, dir: &Path, name: &str, args: &[OsString]) -> Result<(Exit, Duration), String> {
    let file = |ext: &str| {
        let p = dir.join(format!("{name}.{ext}"));
        File::create(&p).map_err(|e| format!("{}: {e}", p.display()))
    };
    let mut cmd = s.guard.wrap(s.bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(file("stdout")?)
        .stderr(file("stderr")?);
    run_timed(&mut cmd, s.limit)
}

/// The command lines of a run, for `command.txt` in its directory.
fn command_text(bin: &Path, lists: &[&[OsString]]) -> String {
    let quote = |a: &OsString| {
        let a = a.to_string_lossy();
        if a.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-./=:".contains(&c))
        {
            a.into_owned()
        } else {
            format!("'{}'", a.replace('\'', "'\\''"))
        }
    };
    lists
        .iter()
        .map(|l| {
            let words: Vec<String> = l.iter().map(quote).collect();
            format!(
                "{} {}\n",
                quote(&bin.as_os_str().to_os_string()),
                words.join(" ")
            )
        })
        .collect()
}

/// Synthesizes and checks design `d`; a failed step or assertion becomes the run's failure.
fn design_run(s: &Setup, d: &Design) -> Run {
    let mut run = Run {
        library: s.lib.name.clone(),
        design: d.name.clone(),
        ..Run::default()
    };
    if let Err(e) = design_steps(s, d, &mut run) {
        run.failure = Some(e);
    }
    run
}

/// The steps of [`design_run`], recording figures and hazards in `run` as they become known.
fn design_steps(s: &Setup, d: &Design, run: &mut Run) -> Result<(), String> {
    let dir = s.out.join(&s.lib.name).join(&d.name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let sources: Vec<PathBuf> = d
        .files
        .iter()
        .map(|f| s.rtl.join(&d.name).join(f))
        .collect();
    let synth = synth_args(&d.top, s.lib_file, &dir, &sources);
    let check = check_args(&d.top, s.lib_file, &dir, &sources);
    crate::write(
        &dir.join("command.txt"),
        &command_text(s.bin, &[&synth, &check]),
    )?;
    let read = |name: &str| {
        let p = dir.join(name);
        std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))
    };

    let (exit, t) = step(s, &dir, "synth", &synth)?;
    run.synth_s = t.as_secs_f64();
    if exit != Exit::Code(0) {
        let why = first_error(&dir.join("synth.stderr"));
        return Err(format!("synth {}: {why}", exit.describe(s.limit)));
    }
    let report = synth_report(&read(SYNTH_REPORT)?).map_err(|e| format!("synth report: {e}"))?;
    run.cells = Some(report.counts.iter().map(|(_, n)| n).sum());
    run.area = Some(report.area);
    run.warnings = Some(report.warnings);
    run.hazards = scan::hazards(&report.counts, s.marks, &read(NETLIST)?);

    let (exit, t) = step(s, &dir, "check", &check)?;
    run.check_s = t.as_secs_f64();
    if !matches!(exit, Exit::Code(0 | 3)) {
        let why = first_error(&dir.join("check.stderr"));
        return Err(format!("check {}: {why}", exit.describe(s.limit)));
    }
    let verdict = check_verdict(&read(CHECK_REPORT)?).map_err(|e| format!("check report: {e}"))?;
    if verdict != "equivalent" || exit != Exit::Code(0) {
        return Err(format!(
            "check verdict '{verdict}' ({})",
            exit.describe(s.limit)
        ));
    }
    Ok(())
}

/// The banner for libraries that are not in the library directory.
pub(crate) fn skipped_banner(
    missing: &[&Library],
    total: usize,
    dir: &Path,
    require: bool,
) -> String {
    let rule = "=".repeat(78);
    let mut b = format!(
        "{rule}\nSKIPPED: verify pdk: {} of {total} libraries missing from {}\n",
        missing.len(),
        dir.display()
    );
    for l in missing {
        b.push_str(&format!("  {} ({})\n", l.name, l.file_name()));
    }
    b.push_str(&format!(
        "Fetch them with `cargo xtask pdk fetch` (or point --dir or {} at them).\n",
        fetch::DIR_VAR
    ));
    if require {
        b.push_str("--require: missing libraries fail the suite.\n");
    } else {
        b.push_str("Without them this suite does not run; `--require` makes that a failure.\n");
    }
    b.push_str(&rule);
    b.push('\n');
    b
}

/// Text table: the first column left-aligned, the others right-aligned.
pub(crate) fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let width: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .map(|r| r.get(c).map_or(0, |x| x.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for r in rows {
        let mut line = String::new();
        for (c, cell) in r.iter().enumerate() {
            let w = width.get(c).copied().unwrap_or(0);
            if c == 0 {
                line.push_str(&format!("{cell:<w$}"));
            } else {
                line.push_str(&format!("  {cell:>w$}"));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// One line per finished run.
fn progress(r: &Run, k: usize, n: usize) -> String {
    let head = format!("[{k:>3}/{n}] {:<14} {:<12}", r.library, r.design);
    match (&r.failure, r.cells, r.area) {
        (Some(f), _, _) => format!("{head} FAIL  {f}"),
        (None, Some(c), Some(a)) => format!(
            "{head} ok   {c:>6} cells {a:>12.1} area   synth {:.2} s, check {:.2} s",
            r.synth_s, r.check_s
        ),
        _ => format!("{head} ok"),
    }
}

/// Hazard text of one run, empty without hazards.
fn hazard_text(h: &Hazards) -> String {
    let mut parts = Vec::new();
    for m in Mark::ALL {
        let cells: Vec<String> = h
            .cells
            .iter()
            .filter(|(k, _, _)| *k == m)
            .map(|(_, c, n)| format!("{c} x{n}"))
            .collect();
        if !cells.is_empty() {
            parts.push(format!("{}: {}", m.label(), cells.join(", ")));
        }
    }
    if h.const_assigns + h.const_pins > 0 {
        parts.push(format!(
            "literal constants: {} assign(s), {} pin(s)",
            h.const_assigns, h.const_pins
        ));
    }
    parts.join("; ")
}

/// The pass and fail counts with every failure.
fn assertions(runs: &[Run]) -> String {
    let failed: Vec<&Run> = runs.iter().filter(|r| r.failure.is_some()).collect();
    let mut s = format!(
        "\n== Assertions: synth exits 0, check verdict equivalent\n{} runs: {} passed, {} failed\n",
        runs.len(),
        runs.len() - failed.len(),
        failed.len()
    );
    for r in &failed {
        let why = r.failure.as_deref().unwrap_or_default();
        s.push_str(&format!("FAIL {}/{}: {why}\n", r.library, r.design));
    }
    s
}

/// The marked cells of each library, the hazard instances per library and the hazards per run.
fn hazards(libs: &[(String, BTreeMap<String, Vec<Mark>>)], runs: &[Run]) -> String {
    let mut s = String::from("\n== Physical-usability hazards (reported, not asserted)\n");
    s.push_str(
        "Liberty cells with is_isolation_cell, always_on or is_level_shifter true (set in the cell \
         or in a group inside it):\n",
    );
    let width = libs.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, marks) in libs {
        let counts: Vec<String> = Mark::ALL
            .iter()
            .filter_map(|m| {
                let n = marks.values().filter(|v| v.contains(m)).count();
                (n > 0).then(|| format!("{}: {n}", m.label()))
            })
            .collect();
        let text = if counts.is_empty() {
            "none".to_string()
        } else {
            counts.join(", ")
        };
        s.push_str(&format!("  {name:<width$}  {text}\n"));
    }
    let mut head = vec!["  library".to_string(), "runs with hazards".to_string()];
    head.extend(Mark::ALL.iter().map(|m| m.label().to_string()));
    head.extend(["const assigns".to_string(), "const pins".to_string()]);
    let mut rows = vec![head];
    for (name, _) in libs {
        let mine: Vec<&Hazards> = runs
            .iter()
            .filter(|r| r.library == *name)
            .map(|r| &r.hazards)
            .collect();
        let sum = |f: &dyn Fn(&Hazards) -> u64| mine.iter().map(|h| f(h)).sum::<u64>().to_string();
        let with = mine.iter().filter(|h| !h.is_empty()).count();
        let mut row = vec![format!("  {name}"), format!("{with}/{}", mine.len())];
        row.extend(Mark::ALL.iter().map(|m| sum(&|h| h.instances(*m))));
        row.push(sum(&|h| h.const_assigns));
        row.push(sum(&|h| h.const_pins));
        rows.push(row);
    }
    s.push_str("Instances in the netlists, by library:\n");
    s.push_str(&table(&rows));
    s.push_str("Runs with hazards:\n");
    let listed: Vec<String> = runs
        .iter()
        .filter(|r| !r.hazards.is_empty())
        .map(|r| {
            format!(
                "  {}/{}: {}\n",
                r.library,
                r.design,
                hazard_text(&r.hazards)
            )
        })
        .collect();
    if listed.is_empty() {
        s.push_str("  none\n");
    }
    s.extend(listed);
    s
}

/// Cells and area per design and library, with totals over the passed runs.
fn qor(libs: &[(String, BTreeMap<String, Vec<Mark>>)], designs: &[Design], runs: &[Run]) -> String {
    let mut head = vec!["design".to_string()];
    head.extend(libs.iter().map(|(n, _)| n.clone()));
    let mut rows = vec![head];
    let mut totals = vec![(0u64, 0f64); libs.len()];
    for d in designs {
        let mut row = vec![d.name.clone()];
        for ((name, _), total) in libs.iter().zip(totals.iter_mut()) {
            let r = runs
                .iter()
                .find(|r| r.library == *name && r.design == d.name);
            row.push(match r {
                Some(Run {
                    failure: None,
                    cells: Some(c),
                    area: Some(a),
                    ..
                }) => {
                    total.0 += c;
                    total.1 += a;
                    format!("{c} / {a:.1}")
                }
                Some(_) => "FAIL".into(),
                None => "-".into(),
            });
        }
        rows.push(row);
    }
    let mut last = vec!["total (passed)".to_string()];
    last.extend(totals.iter().map(|(c, a)| format!("{c} / {a:.1}")));
    rows.push(last);
    format!("\n== QoR: cells / area\n{}", table(&rows))
}

/// The summary: assertions, hazards and QoR of the runs; `libs` holds the marked cells of each
/// library that ran.
pub(crate) fn summary(
    libs: &[(String, BTreeMap<String, Vec<Mark>>)],
    designs: &[Design],
    runs: &[Run],
) -> String {
    format!(
        "{}{}{}",
        assertions(runs),
        hazards(libs, runs),
        qor(libs, designs, runs)
    )
}

/// The runs as tab-separated values with a header line.
pub(crate) fn runs_tsv(runs: &[Run]) -> String {
    let mut head = vec![
        "library", "design", "status", "cells", "area", "warnings", "synth_s", "check_s",
    ];
    head.extend(Mark::ALL.iter().map(|m| m.label()));
    head.extend(["const_assigns", "const_pins", "failure"]);
    let mut out = head.join("\t");
    out.push('\n');
    for r in runs {
        let opt = |v: Option<u64>| v.map_or(String::new(), |v| v.to_string());
        let mut row = vec![
            r.library.clone(),
            r.design.clone(),
            if r.failure.is_some() { "fail" } else { "pass" }.to_string(),
            opt(r.cells),
            r.area.map_or(String::new(), |a| format!("{a:.4}")),
            opt(r.warnings),
            format!("{:.3}", r.synth_s),
            format!("{:.3}", r.check_s),
        ];
        row.extend(
            Mark::ALL
                .iter()
                .map(|m| r.hazards.instances(*m).to_string()),
        );
        row.push(r.hazards.const_assigns.to_string());
        row.push(r.hazards.const_pins.to_string());
        row.push(
            r.failure
                .clone()
                .unwrap_or_default()
                .replace(['\t', '\n'], " "),
        );
        out.push_str(&row.join("\t"));
        out.push('\n');
    }
    out
}

/// The `verify pdk` command on the libraries `libs` in directory `dir`. A library directory at or
/// below [`OUT`] is refused before anything else happens: the run would delete it.
pub(crate) fn run(
    root: &Path,
    libs: &[Library],
    dir: &Path,
    o: &VerifyOptions,
) -> Result<(), String> {
    let out = root.join(OUT);
    if fetch::inside(dir, &out)? {
        return Err(format!(
            "verify pdk: refusing the library directory {}: it is at or below {OUT}, which \
             each run deletes and writes again; keep the libraries elsewhere (the default is \
             target/pdk)",
            dir.display()
        ));
    }
    let (present, missing): (Vec<&Library>, Vec<&Library>) =
        libs.iter().partition(|l| dir.join(l.file_name()).is_file());
    if !missing.is_empty() {
        eprint!("{}", skipped_banner(&missing, libs.len(), dir, o.require));
        if o.require {
            return Err(format!(
                "verify pdk: {} of {} libraries missing (--require)",
                missing.len(),
                libs.len()
            ));
        }
        if present.is_empty() {
            return Ok(());
        }
    }
    let tools = Tools::find(false)?;
    for l in &present {
        let f = dir.join(l.file_name());
        if let Some(why) = tools.mismatch(&f, l)? {
            return Err(format!(
                "{}: {} does not match {} ({why}); run `cargo xtask pdk fetch`",
                l.name,
                f.display(),
                manifest::MANIFEST
            ));
        }
    }
    let rtl = root.join(designs::RTL);
    let designs = designs::load(root)?;
    let bin = build(root)?;
    if out.exists() {
        std::fs::remove_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    let total = present.len() * designs.len();
    let guard = crate::guard::Guard::detect();
    println!("verify pdk: {}", guard.describe());
    println!(
        "verify pdk: {} libraries x {} designs with {} (time limit {} s per step); outputs in {OUT}",
        present.len(),
        designs.len(),
        bin.display(),
        o.timeout.as_secs()
    );
    let mut marked = Vec::new();
    let mut runs = Vec::new();
    for lib in &present {
        let lib_file = dir.join(lib.file_name());
        let text = std::fs::read(&lib_file).map_err(|e| format!("{}: {e}", lib_file.display()))?;
        let marks = scan::liberty_marks(&text);
        drop(text);
        let s = Setup {
            bin: &bin,
            guard: &guard,
            lib,
            lib_file: &lib_file,
            marks: &marks,
            rtl: &rtl,
            out: &out,
            limit: o.timeout,
        };
        for d in &designs {
            let r = design_run(&s, d);
            println!("{}", progress(&r, runs.len() + 1, total));
            runs.push(r);
        }
        marked.push((lib.name.clone(), marks));
    }
    let text = summary(&marked, &designs, &runs);
    print!("{text}");
    crate::write(&out.join("summary.txt"), &text)?;
    crate::write(&out.join("runs.tsv"), &runs_tsv(&runs))?;
    println!("\nsummary: {OUT}/summary.txt, runs: {OUT}/runs.tsv");
    if !missing.is_empty() {
        eprint!("{}", skipped_banner(&missing, libs.len(), dir, o.require));
    }
    let failed = runs.iter().filter(|r| r.failure.is_some()).count();
    if failed == 0 {
        Ok(())
    } else {
        Err(format!(
            "verify pdk: {failed} of {} runs failed",
            runs.len()
        ))
    }
}

#[cfg(test)]
mod tests;
