// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Read-only RTL-model feasibility probe. Does not claim netlist equivalence.
//! Plan: <=128 MiB input, ~13M definitions * 16 B plus signal/state strings,
//! vectors and proof scratch: <=4 GiB initial probe. Run with OS memory/time
//! guards; a larger follow-up must use the exclusive benchmark scale lock.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::time::Instant;

use rusteda_formats::verilog::{PpOptions, SourceMap, read_sources};
use rustsys_check::circ::Circuit;
use rustsys_check::rtl::{RtlOptions, build};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: model_config CONFIG.json REPORT.json".into());
    }
    let cfg: serde_json::Value = serde_json::from_reader(File::open(&args[0])?)?;
    let paths: Vec<PathBuf> = cfg["design"]["files"]
        .as_array()
        .ok_or("missing source list")?
        .iter()
        .map(|p| p.as_str().map(PathBuf::from).ok_or("invalid path"))
        .collect::<Result<_, _>>()?;
    if paths.len() > 600
        || paths
            .iter()
            .map(std::fs::metadata)
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .map(std::fs::Metadata::len)
            .sum::<u64>()
            > 128 * 1024 * 1024
    {
        return Err("source budget exceeded".into());
    }
    let t = Instant::now();
    let mut sm = SourceMap::new();
    let (units, diagnostics) = read_sources(&mut sm, &paths, PpOptions::default());
    if diagnostics.iter().any(|d| d.is_error()) {
        return Err(diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.render(&sm))
            .collect::<Vec<_>>()
            .join("\n")
            .into());
    }
    eprintln!("Parsed RTL in {:.2} seconds", t.elapsed().as_secs_f64());
    let mut circuit = Circuit::new();
    let options = RtlOptions {
        top: cfg["design"]["top"].as_str().map(String::from),
        ..RtlOptions::default()
    };
    let result = match build(&sm, &units, &mut circuit, &options) {
        Ok(model) => {
            let mut result = serde_json::json!({"status":"built", "rtl_state_bits":model.states.len(),
                "inputs":model.inputs.len(), "outputs":model.outputs.len(), "definitions":circuit.defs.len(),
                "wall_s":t.elapsed().as_secs_f64(), "equivalence":"not_run"});
            if let Some(workspace) = cfg["gate_probe_workspace"].as_str() {
                drop(units);
                drop(sm);
                // Model/lookup feasibility only, never induction or equivalence.
                // Plan: 1.3 GiB observed RTL peak + 0.7 GiB mapped gate tables,
                // <=0.7 GiB additional definitions and <=1 GiB state/name scratch.
                // One worker, 4 GiB physical guard; no whole-design SAT/sweep.
                let inputs = std::collections::HashMap::from_iter(
                    model.inputs.iter().map(|(n, b, _)| (n.clone(), b.clone())),
                );
                let mut wanted = std::collections::HashSet::new();
                for s in &model.states {
                    wanted.insert(s.name.as_str());
                    if let Some((base, index)) =
                        s.name.strip_suffix(']').and_then(|x| x.rsplit_once('['))
                    {
                        if index.parse::<usize>().is_ok() {
                            wanted.insert(base);
                        }
                    }
                }
                let library_path = cfg["library"].as_str().ok_or("missing library")?;
                let text = std::fs::read_to_string(library_path)?;
                let lib = rusteda_formats::liberty::read_text(&text, library_path)?;
                let unread = rustsys_check::liberty::unread(&text, library_path)?;
                let ws = rusteda_workspace::Workspace::open(workspace)?;
                let (_, rbin) = rusteda_workspace::netlist::open_head(&ws)?;
                let frozen = rusteda_workspace::netlist::decode(&rbin)?;
                eprintln!("RTL built; reading {} mapped cells", frozen.instances.len());
                let nm = rustsys_check::netlist::frozen::build(
                    &mut circuit,
                    &frozen,
                    &[lib],
                    &model.top,
                    Some(&inputs),
                    &[],
                    &unread,
                    &wanted,
                    rustsys_check::netlist::frozen::Limits::default(),
                )?;
                let cells = std::collections::HashMap::<u32, usize>::from_iter(
                    nm.seq.iter().enumerate().map(|(i, s)| (s.iq, i)),
                );
                let mut owners = vec![false; nm.seq.len()];
                let mut matched = 0;
                let mut constant = 0;
                let mut other = 0;
                let mut missing = 0;
                let mut examples = Vec::new();
                for state in &model.states {
                    let Some(net) = nm.named_bit(&state.name)? else {
                        missing += 1;
                        continue;
                    };
                    if let Some((iq, _)) = rustsys_check::netlist::trace(&circuit, net)
                        .and_then(|(v, p)| cells.get(&v).map(|&i| (i, p)))
                    {
                        owners[iq] = true;
                        matched += 1;
                    } else {
                        let mut value = rustsys_check::circ::B::V(net);
                        for _ in 0..1024 {
                            let rustsys_check::circ::B::V(v) = value else {
                                break;
                            };
                            if let rustsys_check::circ::Def::Alias(b) = circuit.defs[v as usize] {
                                value = b;
                            } else {
                                break;
                            }
                        }
                        if value.konst().is_some() {
                            constant += 1;
                        } else {
                            other += 1;
                        }
                        if examples.len() < 16 {
                            examples.push(state.name.clone());
                        }
                    }
                }
                result["gate_probe"] = serde_json::json!({"state_elements":nm.seq.len(),"definitions":circuit.defs.len(),
                    "named_states_reaching_elements":matched,"constant_state_probes":constant,
                    "other_state_probes":other,"absent_state_probes":missing,
                    "unclaimed_elements":owners.iter().filter(|x|!**x).count(),"examples":examples,
                    "equivalence":"not_run"});
                result["wall_s"] = serde_json::json!(t.elapsed().as_secs_f64());
            }
            result
        }
        Err(e) => {
            serde_json::json!({"status":"unsupported_or_undecided", "diagnostic":format!("{e:?}"),
            "definitions":circuit.defs.len(), "wall_s":t.elapsed().as_secs_f64(), "equivalence":"unverified"})
        }
    };
    serde_json::to_writer_pretty(BufWriter::new(File::create(&args[1])?), &result)?;
    eprintln!("{result}");
    Ok(())
}
