// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded diagnostic export before technology mapping, for external LEC.
//! Usage: dump_transitions TOP INPUT.v OUTPUT.v [sweep]

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use rusteda_formats::verilog::{PpOptions, SourceMap, read_sources};
use rustsys::aig::Lit;
use rustsys::elab::{ElabOptions, StateKind, elaborate};

fn ident(name: &str) -> String {
    assert!(!name.chars().any(char::is_whitespace));
    format!("\\{name} ")
}

fn lit(l: Lit) -> String {
    match l.const_value() {
        Some(v) => format!("1'b{}", u8::from(v)),
        None => format!("{}_diag_n{}", if l.is_compl() { "~" } else { "" }, l.node()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(4..=5).contains(&args.len())
        || (args.len() == 5 && !["sweep", "trace", "trace-cells"].contains(&args[4].as_str()))
    {
        return Err(
            "usage: dump_transitions TOP INPUT.v OUTPUT.v [sweep|trace|trace-cells]".into(),
        );
    }
    if std::fs::metadata(&args[2])?.len() > 32 * 1024 * 1024 {
        return Err("diagnostic source size budget (32 MiB) reached".into());
    }
    let mut sm = SourceMap::default();
    let (units, diagnostics) =
        read_sources(&mut sm, &[PathBuf::from(&args[2])], PpOptions::default());
    if diagnostics.iter().any(|d| d.is_error()) {
        return Err(format!("parse failed: {diagnostics:?}").into());
    }
    let opts = ElabOptions {
        top: Some(args[1].clone()),
        ..ElabOptions::default()
    };
    let tracing = args.get(4).is_some_and(|s| s.starts_with("trace"));
    let structural_cells = args.get(4).is_some_and(|s| s == "trace-cells");
    let (design, diagnostics) = if tracing {
        rustsys::elab::elaborate_with_multiplier_trace(&sm, &units, &opts)
    } else {
        elaborate(&sm, &units, &opts)
    };
    let mut d = design.ok_or_else(|| format!("elaboration failed: {diagnostics:?}"))?;
    if args.get(4).is_some_and(|s| s == "sweep") {
        rustsys::opt::sweep(&mut d);
    }
    if d.aig.num_nodes() > 2_000_000 || d.states.len() > 200_000 {
        return Err("diagnostic graph size budget reached".into());
    }
    if tracing {
        let literal = |xs: &[Lit]| xs.iter().map(|l| l.0).collect::<Vec<_>>();
        let data = serde_json::json!({
            "top": d.top,
            "nodes": (0..d.aig.num_nodes() as u32).map(|n| if d.aig.is_and(n) {
                let (a,b)=d.aig.fanins(n);Some([a.0,b.0])
            } else {None}).collect::<Vec<_>>(),
            "inputs": d.aig.inputs(),
            "ports": d.ports.iter().map(|p| serde_json::json!({"name":p.name,"input":p.input,"bits":literal(&p.bits)})).collect::<Vec<_>>(),
            "states":d.states.iter().map(|s| serde_json::json!({"name":s.name,"q":s.q.0,"d":s.d.0,"clock":s.clk.0,"reset":s.arst.map(|(l,v)|(l.0,v))})).collect::<Vec<_>>(),
            "multipliers": d.aig.multiplier_sites().iter().map(|s| serde_json::json!({"a":literal(&s.a),"b":literal(&s.b),"product":literal(&s.product),"signed":s.signed})).collect::<Vec<_>>()
        });
        let path = PathBuf::from(&args[3]).with_extension("trace.json");
        serde_json::to_writer(BufWriter::new(File::create(path)?), &data)?;
    }
    let mut out = BufWriter::new(File::create(&args[3])?);
    writeln!(
        out,
        "module {}({});",
        ident(&d.top),
        d.ports
            .iter()
            .map(|p| ident(&p.name))
            .collect::<Vec<_>>()
            .join(",")
    )?;
    for p in &d.ports {
        writeln!(
            out,
            "{} {}{};",
            if p.input { "input" } else { "output" },
            if p.scalar {
                String::new()
            } else {
                format!("[{}:0] ", p.width - 1)
            },
            ident(&p.name)
        )?;
    }
    for n in 1..d.aig.num_nodes() {
        writeln!(out, "wire _diag_n{n};")?;
        if d.aig.is_and(n as u32) {
            let (a, b) = d.aig.fanins(n as u32);
            writeln!(out, "assign _diag_n{n} = {} & {};", lit(a), lit(b))?;
        }
    }
    for p in &d.ports {
        for (i, &b) in p.bits.iter().enumerate() {
            let name = format!(
                "{}{}",
                ident(&p.name),
                if p.scalar {
                    String::new()
                } else {
                    format!("[{i}]")
                }
            );
            writeln!(
                out,
                "assign {} = {};",
                if p.input { lit(b) } else { name.clone() },
                if p.input { name } else { lit(b) }
            )?;
        }
    }
    for (i, s) in d.states.iter().enumerate() {
        writeln!(
            out,
            "{} _diag_q{i};\nassign {} = _diag_q{i};",
            if structural_cells { "wire" } else { "reg" },
            lit(s.q)
        )?;
        if !d.ports.iter().any(|p| p.name == s.name) {
            writeln!(
                out,
                "wire {};\nassign {} = _diag_q{i};",
                ident(&s.name),
                ident(&s.name)
            )?;
        }
        if structural_cells {
            match (s.kind, s.arst) {
                (StateKind::Latch, None) => writeln!(
                    out,
                    "\\$dlatch #(.WIDTH(1),.EN_POLARITY(1)) r{i} (.EN({}),.D({}),.Q(_diag_q{i}));",
                    lit(s.clk),
                    lit(s.d)
                )?,
                (StateKind::Flop, None) => writeln!(
                    out,
                    "\\$dff #(.WIDTH(1),.CLK_POLARITY(1)) r{i} (.CLK({}),.D({}),.Q(_diag_q{i}));",
                    lit(s.clk),
                    lit(s.d)
                )?,
                (StateKind::Flop, Some((r, v))) => writeln!(
                    out,
                    "\\$adff #(.WIDTH(1),.CLK_POLARITY(1),.ARST_POLARITY(1),.ARST_VALUE(1'b{})) r{i} (.CLK({}),.ARST({}),.D({}),.Q(_diag_q{i}));",
                    u8::from(v),
                    lit(s.clk),
                    lit(r),
                    lit(s.d)
                )?,
                _ => return Err("unsupported diagnostic storage cell".into()),
            }
            continue;
        }
        match s.kind {
            StateKind::Latch => {
                if s.arst.is_some() {
                    return Err("unsupported diagnostic async latch".into());
                }
                writeln!(
                    out,
                    "always @* if ({}) _diag_q{i} <= {};",
                    lit(s.clk),
                    lit(s.d)
                )?;
            }
            StateKind::Flop => {
                writeln!(out, "wire _diag_clk{i} = {};", lit(s.clk))?;
                if let Some((r, v)) = s.arst {
                    writeln!(out, "wire _diag_rst{i} = {};", lit(r))?;
                    writeln!(
                        out,
                        "always @(posedge _diag_clk{i} or posedge _diag_rst{i}) if (_diag_rst{i}) _diag_q{i} <= 1'b{}; else _diag_q{i} <= {};",
                        u8::from(v),
                        lit(s.d)
                    )?;
                } else {
                    writeln!(
                        out,
                        "always @(posedge _diag_clk{i}) _diag_q{i} <= {};",
                        lit(s.d)
                    )?;
                }
            }
        }
    }
    writeln!(out, "endmodule")?;
    out.flush()?;
    eprintln!(
        "{} nodes, {} states; no reset or initial-state assumptions",
        d.aig.num_nodes(),
        d.states.len()
    );
    Ok(())
}
