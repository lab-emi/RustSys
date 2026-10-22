// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded pre/post-pass AIGER export for independent external ABC checking.
//! At most 100K nodes (~5 MiB graph/copy), 16 bounded worker caches (<1 GiB),
//! 1024 files and 256 MiB output. One fixture at a time; use a 2 GiB RAM guard.
//! This does not establish a proof: every emitted pair needs an external CEC.
//! Usage: parallel_opt_check OUTPUT_DIRECTORY

#[path = "../tests/common/mod.rs"]
mod common;

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use rustsys::aig::Lit;
use rustsys::elab::Design;
use rustsys::opt;
use rustsys_fixtures::rtl::CORPUS;

fn put(w: &mut impl Write, mut value: u32) -> std::io::Result<()> {
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        w.write_all(&[if value == 0 { byte } else { byte | 128 }])?;
        if value == 0 {
            return Ok(());
        }
    }
}

fn export(d: &Design, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if d.aig.num_nodes() > 100_000 {
        return Err("export node budget".into());
    }
    let roots = opt::design_roots(d);
    if roots.len() > 20_000 {
        return Err("export root budget".into());
    }
    let inputs = d.aig.num_inputs();
    let mut image = vec![0u32; d.aig.num_nodes()];
    for (i, &node) in d.aig.inputs().iter().enumerate() {
        image[node as usize] = 2 * (i as u32 + 1);
    }
    let mut next = inputs as u32;
    for node in 1..d.aig.num_nodes() as u32 {
        if d.aig.is_and(node) {
            next += 1;
            image[node as usize] = 2 * next;
        }
    }
    let lit = |value: Lit| image[value.node() as usize] ^ u32::from(value.is_compl());
    let mut out = BufWriter::new(File::create_new(path)?);
    writeln!(
        out,
        "aig {next} {inputs} 0 {} {}",
        roots.len(),
        d.aig.num_ands()
    )?;
    for root in roots {
        writeln!(out, "{}", lit(root))?;
    }
    for node in 1..d.aig.num_nodes() as u32 {
        if !d.aig.is_and(node) {
            continue;
        }
        let (a, b) = d.aig.fanins(node);
        let (a, b) = (lit(a), lit(b));
        let (high, low) = (a.max(b), a.min(b));
        let lhs = image[node as usize];
        if high >= lhs {
            return Err("non-topological export".into());
        }
        put(&mut out, lhs - high)?;
        put(&mut out, high - low)?;
    }
    out.flush()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        return Err("usage: parallel_opt_check OUTPUT_DIRECTORY".into());
    }
    let directory = Path::new(&args[1]);
    fs::create_dir(directory)?;
    let serial = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let parallel = rayon::ThreadPoolBuilder::new().num_threads(16).build()?;
    type Pass = fn(&mut Design);
    let passes: [(&str, Pass); 5] = [
        ("balance", opt::balance::balance),
        ("rewrite", opt::rewrite::rewrite),
        ("rewrite_zero", opt::rewrite::rewrite_zero),
        ("refactor", opt::refactor::refactor),
        ("resub", opt::resub::resub),
    ];
    let mut manifest = Vec::new();
    let mut bytes = 0u64;
    let mut count = 0usize;
    for (index, fixture) in CORPUS.iter().enumerate() {
        let src: Vec<_> = fixture.files.iter().map(|(_, text)| *text).collect();
        if src.iter().map(|s| s.len()).sum::<usize>() > 1024 * 1024 {
            return Err("fixture source budget".into());
        }
        let before = common::elab(&src, Some(fixture.top))?;
        if before.aig.num_nodes() > 100_000 {
            return Err("fixture graph budget".into());
        }
        let gold = directory.join(format!("case{index}_gold.aig"));
        export(&before, &gold)?;
        count += 1;
        bytes += fs::metadata(&gold)?.len();
        for (name, pass) in passes {
            let mut one = before.clone();
            serial.install(|| pass(&mut one));
            let mut sixteen = before.clone();
            parallel.install(|| pass(&mut sixteen));
            if format!("{one:?}") != format!("{sixteen:?}") {
                return Err(format!("thread-dependent {name} on {}", fixture.name).into());
            }
            if one.aig.num_inputs() != before.aig.num_inputs()
                || opt::design_roots(&one).len() != opt::design_roots(&before).len()
            {
                return Err(
                    "pass changed fixture interface; requires explicit correspondence".into(),
                );
            }
            let gate = directory.join(format!("case{index}_{name}.aig"));
            export(&sixteen, &gate)?;
            count += 1;
            bytes += fs::metadata(&gate)?.len();
            if count > 1024 || bytes > 256 * 1024 * 1024 {
                return Err("fixture output budget".into());
            }
            manifest.push(
                serde_json::json!({"fixture":fixture.name,"pass":name,"gold":gold,"gate":gate,
                "threads":[1,16],"graph_identity":true,"inputs":before.aig.num_inputs(),
                "roots":opt::design_roots(&before).len(),"proof":"not_run"}),
            );
        }
    }
    serde_json::to_writer_pretty(
        BufWriter::new(File::create_new(directory.join("manifest.json"))?),
        &manifest,
    )?;
    writeln!(
        std::io::stdout().lock(),
        "exported {} independently checkable pass pairs",
        manifest.len()
    )?;
    Ok(())
}
