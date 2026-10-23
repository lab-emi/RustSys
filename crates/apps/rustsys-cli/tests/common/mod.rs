// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Shared helpers of the verification suites.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt::Write as _;

use rusteda_formats::liberty::{Direction, Library, SeqKind};
use rusteda_formats::verilog::write::{NlDir, NlModule, NlRef};
use rustsys::{Effort, Goal, Sources, SynthOptions, SynthOutput, synthesize};
use rustsys_check::{CheckOptions, CheckReport, Input, check};
use rustsys_fixtures::liberty::{Variant, library};

pub fn lib(v: Variant) -> Library {
    rusteda_formats::liberty::read_text(&library(v), v.name()).expect("synthetic library")
}

pub fn synth(
    files: &[(String, String)],
    top: &str,
    lib: &Library,
    effort: Effort,
    goal: Goal,
) -> Result<SynthOutput, String> {
    let opts = SynthOptions {
        top: Some(top.to_string()),
        effort,
        goal,
        ..SynthOptions::default()
    };
    synthesize(
        &Sources::Texts(files.to_vec()),
        std::slice::from_ref(lib),
        &opts,
    )
    .map_err(|e| e.messages.join("\n"))
}

pub fn check_texts(
    rtl: &[(String, String)],
    top: &str,
    netlist: &str,
    lib: &Library,
) -> CheckReport {
    let opts = CheckOptions {
        rtl: Input::Texts(rtl.to_vec()),
        top: Some(top.to_string()),
        netlist: Input::Texts(vec![("netlist.v".into(), netlist.to_string())]),
        ..CheckOptions::default()
    };
    check(&opts, std::slice::from_ref(lib))
}

pub fn key(r: &NlRef) -> String {
    match r {
        NlRef::Net(n) => n.clone(),
        NlRef::Bit(n, k) => format!("{n}[{k}]"),
        NlRef::Const(v) => format!("#{}", u8::from(*v)),
    }
}

/// Evaluates a netlist from primary inputs and the internal state of every sequential instance.
pub fn eval_netlist(
    nl: &NlModule,
    lib: &Library,
    given: &HashMap<String, bool>,
    iq: &HashMap<String, bool>,
) -> HashMap<String, bool> {
    let mut val = given.clone();
    val.insert("#0".into(), false);
    val.insert("#1".into(), true);
    for _ in 0..100_000 {
        let mut changed = false;
        for inst in &nl.instances {
            let cell = lib
                .cells
                .iter()
                .find(|c| c.name == inst.cell)
                .expect("cell");
            let state = cell.sequentials.first().map(|s| {
                (
                    s.state.clone(),
                    s.state_n.clone(),
                    iq.get(&inst.name).copied().unwrap_or(false),
                )
            });
            for o in cell
                .pins
                .iter()
                .filter(|p| p.direction == Direction::Output)
            {
                let Some((_, r)) = inst.conns.iter().find(|(p, _)| *p == o.name) else {
                    continue;
                };
                let k = key(r);
                if val.contains_key(&k) {
                    continue;
                }
                let get = |n: &str| -> Option<bool> {
                    if let Some((s, sn, q)) = &state {
                        if n == s {
                            return Some(*q);
                        }
                        if n == sn {
                            return Some(!*q);
                        }
                    }
                    inst.conns
                        .iter()
                        .find(|(p, _)| p == n)
                        .and_then(|(_, r)| val.get(&key(r)).copied())
                };
                if let Some(v) = o.function.as_ref().expect("function").eval(&get) {
                    val.insert(k, v);
                    changed = true;
                }
            }
        }
        for (l, r) in &nl.assigns {
            if !val.contains_key(&key(l)) {
                if let Some(v) = val.get(&key(r)).copied() {
                    val.insert(key(l), v);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    val
}

/// Output port value (least significant bit first) from evaluated nets.
pub fn port_value(nl: &NlModule, vals: &HashMap<String, bool>, name: &str) -> u128 {
    let p = nl
        .ports
        .iter()
        .find(|p| p.name == name && p.dir == NlDir::Output)
        .expect("port");
    match p.width {
        None => u128::from(vals[name]),
        Some(w) => (0..w).fold(0u128, |a, k| {
            a | (u128::from(vals[&format!("{name}[{k}]")]) << k)
        }),
    }
}

/// SystemVerilog behavioral models of the library cells (for resynthesizing netlists).
pub fn cell_models(lib: &Library) -> String {
    let mut all = String::new();
    for c in &lib.cells {
        if let Some(m) = cell_model(c) {
            all.push_str(&m);
        }
    }
    all
}

fn cell_model(c: &rusteda_formats::liberty::Cell) -> Option<String> {
    let mut out = String::new();
    {
        let ins: Vec<&str> = c
            .pins
            .iter()
            .filter(|p| p.direction == Direction::Input)
            .map(|p| p.name.as_str())
            .collect();
        let outs: Vec<&rusteda_formats::liberty::Pin> = c
            .pins
            .iter()
            .filter(|p| p.direction == Direction::Output)
            .collect();
        if outs.is_empty()
            || outs.iter().any(|p| p.function.is_none())
            || !c.problems.is_empty()
            || c.has_test_cell
        {
            return None;
        }
        let mut ports: Vec<String> = ins.iter().map(|i| format!("input logic {i}")).collect();
        ports.extend(outs.iter().map(|o| format!("output logic {}", o.name)));
        let _ = writeln!(out, "module {} ({});", c.name, ports.join(", "));
        match c.sequentials.first() {
            None => {
                for o in &outs {
                    let _ = writeln!(
                        out,
                        "  assign {} = {};",
                        o.name,
                        o.function.as_ref().expect("function")
                    );
                }
            }
            Some(s) => {
                let single = |f: &rusteda_formats::liberty::Func| -> Option<(String, bool)> {
                    match f {
                        rusteda_formats::liberty::Func::Var(v) => Some((v.clone(), false)),
                        rusteda_formats::liberty::Func::Not(a) => match &**a {
                            rusteda_formats::liberty::Func::Var(v) => Some((v.clone(), true)),
                            _ => None,
                        },
                        _ => None,
                    }
                };
                let (clk, clk_low) = single(&s.clock)?;
                let _ = writeln!(out, "  logic {};", s.state);
                let _ = writeln!(out, "  wire {} = !{};", s.state_n, s.state);
                let ctrl = [(&s.clear, false), (&s.preset, true)];
                let asyncs: Vec<(String, bool, bool)> = ctrl
                    .iter()
                    .filter_map(|(f, v)| f.as_ref().and_then(single).map(|(p, low)| (p, low, *v)))
                    .collect();
                if asyncs.len() > 1 {
                    // Two asynchronous controls on one bit are outside the synthesizable subset.
                    return None;
                }
                // next_state is a Boolean function, including enabled-FF
                // feedback; requiring one data pin silently omitted those cells.
                let d = s.data.to_string();
                if s.kind == SeqKind::Latch {
                    let en = if clk_low {
                        format!("!{clk}")
                    } else {
                        clk.clone()
                    };
                    let _ = writeln!(out, "  always_latch if ({en}) {} = {d};", s.state);
                } else {
                    let mut ev = format!("{} {clk}", if clk_low { "negedge" } else { "posedge" });
                    let mut body = String::new();
                    for (p, low, v) in &asyncs {
                        let _ = write!(ev, " or {} {p}", if *low { "negedge" } else { "posedge" });
                        let _ = write!(
                            body,
                            "if ({}{p}) {} <= 1'b{}; else ",
                            if *low { "!" } else { "" },
                            s.state,
                            u8::from(*v)
                        );
                    }
                    let _ = writeln!(out, "  always_ff @({ev}) {body}{} <= {d};", s.state);
                }
                for o in &outs {
                    let f = o
                        .function
                        .as_ref()
                        .expect("function")
                        .to_string()
                        .replace(&s.state_n, &format!("(!{})", s.state));
                    let _ = writeln!(out, "  assign {} = {f};", o.name);
                }
            }
        }
        let _ = writeln!(out, "endmodule");
    }
    Some(out)
}

pub fn all_variants() -> [Variant; 4] {
    Variant::ALL
}
