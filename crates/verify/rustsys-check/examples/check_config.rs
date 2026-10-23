// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Development-only independent checker driver for pinned benchmark configs.
//! Reads existing artifacts; never mutates the design workspace. Reports are
//! streamed. Run under an explicit OS memory/time guard (8 GiB for large cases).

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;

use rusteda_core::job::{Ctx, Event, Observer};
use rustsys_check::{CheckOptions, Input};

struct Progress;
impl Observer for Progress {
    fn event(&self, event: &Event) {
        match event {
            Event::PhaseStarted { phase } => eprintln!("Start {phase}"),
            Event::PhaseFinished { phase, seconds } => {
                eprintln!("Finished {phase} in {seconds:.2} seconds")
            }
            Event::Diagnostic(diag) => eprintln!("{}: {}", diag.code, diag.message),
            _ => {}
        }
    }
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: check_config CONFIG.json REPORT.json".into());
    }
    if fs::metadata(&args[0])?.len() > 1024 * 1024 {
        return Err("config byte budget exceeded".into());
    }
    let config: serde_json::Value = serde_json::from_reader(File::open(&args[0])?)?;
    let lib_path = config["library"].as_str().ok_or("missing library")?;
    let text = fs::read_to_string(lib_path)?;
    let lib = rusteda_formats::liberty::read_text(&text, lib_path)?;
    let unread = rustsys_check::liberty::unread(&text, lib_path)?;
    let files = config["design"]["files"]
        .as_array()
        .ok_or("missing RTL files")?
        .iter()
        .map(|v| v.as_str().map(PathBuf::from).ok_or("invalid source path"))
        .collect::<Result<Vec<_>, _>>()?;
    let netlist = config["netlist"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(config["directory"].as_str().unwrap_or(".")).join("netlist.v")
        });
    let mut export = config["export_obligations"]
        .as_str()
        .map(|path| rustsys_check::ObligationExport::new(PathBuf::from(path)));
    if let Some(options) = &mut export {
        if let Some(path) = config["state_proposals_file"].as_str() {
            if fs::metadata(path)?.len() > 8 * 1024 * 1024 {
                return Err("state proposal file byte budget exceeded".into());
            }
            options.state_proposals = serde_json::from_reader(File::open(path)?)?;
        }
        if let Some(value) = config.get("export_discovery_work") {
            options.max_discovery_work = value.as_u64().ok_or("invalid export_discovery_work")?;
        }
        if let Some(value) = config.get("export_discovery_rounds") {
            options.max_discovery_rounds =
                usize::try_from(value.as_u64().ok_or("invalid export_discovery_rounds")?)?;
        }
    }
    let options = CheckOptions {
        rtl: Input::Files(files),
        top: config["design"]["top"].as_str().map(String::from),
        netlist: Input::Files(vec![netlist]),
        unread_liberty: unread,
        budget: 2_000_000,
        model_budget: 2_000_000,
        sweep_max_candidates: 0,
        obligation_export: export,
        observation_probes: config
            .get("observation_probes")
            .map(|p| serde_json::from_value(p.clone()))
            .transpose()?
            .unwrap_or_default(),
        verbose: true,
        ..CheckOptions::default()
    };
    let observer = Progress;
    let ctx = Ctx::new(None, Some(&observer));
    let report = if let Some(path) = config["frozen_workspace"].as_str() {
        let ws = rusteda_workspace::Workspace::open(path)?;
        let (_revision, rbin) = rusteda_workspace::netlist::open_head(&ws)?;
        let tables = rusteda_workspace::netlist::decode(&rbin)?;
        eprintln!(
            "Frozen gate table: {} nets, {} instances",
            tables.nets.len(),
            tables.instances.len()
        );
        rustsys_check::check_frozen_with(
            &options,
            &[lib],
            &tables,
            rustsys_check::netlist::frozen::Limits::default(),
            ctx,
        )?
    } else {
        rustsys_check::check_with(&options, &[lib], ctx)?
    };
    let output = BufWriter::new(File::create(&args[1])?);
    serde_json::to_writer_pretty(output, &report)?;
    eprintln!("{}", report.verdict.as_str());
    Ok(())
}
