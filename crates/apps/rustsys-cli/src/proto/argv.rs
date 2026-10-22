// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Canonical command lines and POSIX shell quoting (PROTO-REQ-0020).

use std::path::Path;

use crate::proto::job::{Format, FrontEnd, Job};

/// Where a run is recorded, as far as the command line says.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Placement {
    /// `--run-dir DIR`.
    pub run_dir: Option<std::path::PathBuf>,
    /// `--record`: a new directory under the runs root.
    pub record: bool,
    /// `--label TEXT`.
    pub label: Option<String>,
}

/// Quotes one argument for a POSIX shell: unchanged when it needs no quoting, else in single
/// quotes with each `'` written as `'\''`.
pub fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./-_^".contains(&b));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Splits a command line as a POSIX shell does for words without expansions: single quotes,
/// double quotes (with `\"`, `\\`, `\$` and `` \` ``) and backslashes.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match it.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => return Err("unterminated single quote".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match it.next() {
                        Some('"') => break,
                        Some('\\') => match it.next() {
                            Some(x @ ('"' | '\\' | '$' | '`')) => cur.push(x),
                            Some('\n') => {}
                            Some(x) => {
                                cur.push('\\');
                                cur.push(x);
                            }
                            None => return Err("unterminated double quote".into()),
                        },
                        Some(x) => cur.push(x),
                        None => return Err("unterminated double quote".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match it.next() {
                    Some('\n') => {}
                    Some(x) => cur.push(x),
                    None => return Err("backslash at the end".into()),
                }
            }
            x => {
                in_word = true;
                cur.push(x);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Ok(out)
}

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// An option and its value: two words, or one (`--flag=value`, `-Dvalue`) when the value starts
/// with `-`, which would otherwise read as an option.
fn opt(a: &mut Vec<String>, flag: &str, value: String) {
    if value.starts_with('-') {
        if flag.starts_with("--") {
            a.push(format!("{flag}={value}"));
        } else {
            a.push(format!("{flag}{value}"));
        }
    } else {
        a.push(flag.to_string());
        a.push(value);
    }
}

/// Shortest decimal that reads back to the same `f64`.
fn num(x: f64) -> String {
    format!("{x}")
}

fn front_end(a: &mut Vec<String>, fe: &FrontEnd) {
    if let Some(t) = &fe.top {
        opt(a, "--top", t.clone());
    }
    for d in &fe.defines {
        opt(a, "-D", d.clone());
    }
    for i in &fe.include {
        opt(a, "-I", path(i));
    }
    for p in &fe.params {
        opt(a, "-P", p.clone());
    }
    if fe.no_predefined_macros {
        a.push("--no-predefined-macros".into());
    }
    if fe.uninitialized_dont_care {
        a.push("--uninitialized-dont-care".into());
    }
    if fe.assume_full_case {
        a.push("--assume-full-case".into());
    }
    if fe.assume_parallel_case {
        a.push("--assume-parallel-case".into());
    }
    for r in &fe.after_reset {
        opt(a, "--after-reset", r.clone());
    }
    for l in &fe.libs {
        opt(a, "--lib", path(l));
    }
    // Sources other than RTL (CLI-REQ-0223).
    if let Some(g) = &fe.gate_netlist {
        opt(a, "--gate-netlist", path(g));
    }
    for l in &fe.gate_libs {
        opt(a, "--gate-lib", path(l));
    }
    if matches!(fe.format(), Ok(Format::Blif | Format::Aiger)) {
        let mode = fe.initial_values.unwrap_or_default();
        opt(a, "--initial-values", mode.name().into());
    } else if let Some(m) = fe.initial_values {
        opt(a, "--initial-values", m.name().into());
    }
}

/// The arguments of the canonical command, after `rustsys` (PROTO-REQ-0020): every
/// result-affecting option spelled out, defaults included; outputs; `--threads N` when not 0.
pub fn canonical_args(job: &Job, placement: &Placement, threads: usize) -> Vec<String> {
    let mut a = Vec::new();
    if threads > 0 {
        opt(&mut a, "--threads", threads.to_string());
    }
    match job {
        Job::Synth(j) => {
            a.push("synth".into());
            front_end(&mut a, &j.fe);
            for d in &j.dont_use {
                opt(&mut a, "--dont-use", d.clone());
            }
            opt(&mut a, "--effort", j.effort.name().into());
            opt(&mut a, "--goal", j.goal.name().into());
            opt(&mut a, "--delay-relax", num(j.delay_relax));
            for p in &j.electrical_corners {
                opt(&mut a, "--electrical-corner", path(p));
            }
            if let Some(e) = &j.electrical {
                opt(&mut a, "--timing-period", num(e.period_ns));
                if let Some(area) = e.area_limit {
                    opt(&mut a, "--area-limit", num(area));
                }
                for (flag, value) in [
                    ("--input-delay", e.input_delay_ns),
                    ("--output-delay", e.output_delay_ns),
                    ("--input-slew", e.input_slew_ns),
                    ("--clock-slew", e.clock_slew_ns),
                    ("--output-load", e.output_load_pf),
                    ("--timing-margin", e.margin_ns),
                ] {
                    opt(&mut a, flag, num(value));
                }
                opt(&mut a, "--buffer-fanout", e.buffer_fanout.to_string());
            }
            opt(&mut a, "--backend", j.backend.name().into());
            if let Some(o) = &j.out {
                opt(&mut a, "--out", path(o));
            }
            if let Some(r) = &j.report {
                opt(&mut a, "--report", path(r));
            }
        }
        Job::CheckSynth(j) => {
            a.extend(["check".into(), "synth".into()]);
            front_end(&mut a, &j.fe);
            for n in &j.netlists {
                opt(&mut a, "--netlist", path(n));
            }
            opt(&mut a, "--seed", j.seed.to_string());
            opt(&mut a, "--cycles", j.cycles.to_string());
            opt(&mut a, "--budget", j.budget.to_string());
            opt(&mut a, "--model-budget", j.model_budget.to_string());
            opt(
                &mut a,
                "--sweep-max-candidates",
                j.sweep_max_candidates.to_string(),
            );
            for p in &j.supply_ports {
                opt(&mut a, "--supply-port", p.clone());
            }
            if let Some(r) = &j.report {
                opt(&mut a, "--report", path(r));
            }
        }
    }
    if let Some(d) = &placement.run_dir {
        opt(&mut a, "--run-dir", path(d));
    } else if placement.record {
        a.push("--record".into());
    }
    if let Some(l) = &placement.label {
        opt(&mut a, "--label", l.clone());
    }
    // Positional sources last, after `--` when one starts with `-`.
    let sources = match job {
        Job::Synth(j) => &j.fe.sources[..],
        Job::CheckSynth(j) => &j.fe.sources[..],
    };
    if sources.iter().any(|p| path(p).starts_with('-')) {
        a.push("--".into());
    }
    a.extend(sources.iter().map(|p| path(p)));
    a
}

/// The canonical command as one shell line (PROTO-REQ-0020).
pub fn canonical_command(job: &Job, placement: &Placement, threads: usize) -> String {
    let mut s = String::from("rustsys");
    for a in canonical_args(job, placement, threads) {
        s.push(' ');
        s.push_str(&quote(&a));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // PROTO-REQ-0020: quoting round trip.
    #[test]
    fn quote_split_round_trip() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let alphabet: Vec<char> = "ab '\"\\$`\t\n*?;&|()<>~#=.-_/éΩ".chars().collect();
        for len in 0..40 {
            for _ in 0..50 {
                let s: String = (0..len)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        alphabet[(seed % alphabet.len() as u64) as usize]
                    })
                    .collect();
                let q = quote(&s);
                assert_eq!(split(&q).unwrap(), vec![s.clone()], "{s:?} -> {q}");
            }
        }
        assert_eq!(quote("cells.lib"), "cells.lib");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote(""), "''");
        assert_eq!(
            split(r#"rustsys synth --lib "a b.lib" 'c''d' e\ f"#).unwrap(),
            vec!["rustsys", "synth", "--lib", "a b.lib", "cd", "e f"]
        );
    }

    // PROTO-REQ-0020, PROTO-REQ-0021: values that start with `-` stay values.
    #[test]
    fn dash_values_stay_values() {
        let s = Job::Synth(crate::proto::job::SynthJob {
            fe: FrontEnd {
                libs: vec!["-c.lib".into()],
                defines: vec!["-X".into()],
                sources: vec!["-top.sv".into()],
                ..Default::default()
            },
            out: Some("o.v".into()),
            ..Default::default()
        });
        let p = Placement {
            label: Some("-baseline".into()),
            ..Default::default()
        };
        assert_eq!(
            canonical_command(&s, &p, 0),
            "rustsys synth -D-X --lib=-c.lib --effort medium --goal delay --delay-relax 0.1 \
             --backend cpu --out o.v --label=-baseline -- -top.sv"
        );
    }

    // PROTO-REQ-0020: defaults are spelled out.
    #[test]
    fn canonical_spells_out_defaults() {
        let j = Job::CheckSynth(crate::proto::job::CheckSynthJob {
            fe: FrontEnd {
                libs: vec!["cells.lib".into()],
                sources: vec!["top.sv".into()],
                ..Default::default()
            },
            netlists: vec!["top.v".into()],
            ..Default::default()
        });
        assert_eq!(
            canonical_command(&j, &Placement::default(), 0),
            "rustsys check synth --lib cells.lib --netlist top.v --seed 1 --cycles 0 \
             --budget 2000000 --model-budget 200000 --sweep-max-candidates 32768 top.sv"
        );
        // A job written before the budgets were options has their defaults.
        let old: Job = serde_json::from_value(serde_json::json!({
            "product": "check-synth",
            "libs": ["cells.lib"],
            "sources": ["top.sv"],
            "netlists": ["top.v"],
            "seed": 1,
            "cycles": 0,
            "report": null
        }))
        .expect("job");
        assert_eq!(old, j);
        let s = Job::Synth(crate::proto::job::SynthJob {
            fe: FrontEnd {
                libs: vec!["c.lib".into()],
                sources: vec!["my top.sv".into()],
                ..Default::default()
            },
            out: Some("o.v".into()),
            ..Default::default()
        });
        assert_eq!(
            canonical_command(
                &s,
                &Placement {
                    record: true,
                    ..Default::default()
                },
                4
            ),
            "rustsys --threads 4 synth --lib c.lib --effort medium --goal delay --delay-relax 0.1 \
             --backend cpu --out o.v --record 'my top.sv'"
        );
    }
}
