// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded diagnostic driver with live checker phases on stderr.
//! Usage: check_trace TOP RTL NETLIST LIBERTY REPORT
//! Run under an OS RAM/wall guard; large-design memory is dominated by the
//! checker's existing bounded SAT circuit, not this observer.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rusteda_core::job::{Ctx, Event, Observer};
use rustsys_check::{CheckOptions, Input};

struct Trace {
    started: Instant,
    count: AtomicUsize,
}

impl Observer for Trace {
    fn event(&self, event: &Event) {
        if matches!(event, Event::Progress { .. }) {
            return;
        }
        if self.count.fetch_add(1, Ordering::Relaxed) < 10_000 {
            eprintln!("{:.3}s {event:?}", self.started.elapsed().as_secs_f64());
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(6..=8).contains(&args.len()) {
        return Err(
            "usage: check_trace TOP RTL NETLIST LIBERTY REPORT [CONFLICTS [SWEEP_CANDIDATES]]"
                .into(),
        );
    }
    let mut bytes = 0;
    for name in &args[2..5] {
        bytes += std::fs::metadata(name)?.len();
    }
    if bytes > 256 * 1024 * 1024 {
        return Err("diagnostic input byte budget reached".into());
    }
    let libpath = PathBuf::from(&args[4]);
    let library = rusteda_formats::liberty::read_file(&libpath)?;
    let text = std::fs::read_to_string(&libpath)?;
    let unread = rustsys_check::liberty::unread(&text, &args[4])?;
    let opts = CheckOptions {
        rtl: Input::Files(vec![PathBuf::from(&args[2])]),
        top: Some(args[1].clone()),
        netlist: Input::Files(vec![PathBuf::from(&args[3])]),
        verbose: true,
        budget: args.get(6).map_or(Ok(2_000_000), |s| s.parse())?,
        model_budget: 2_000_000,
        sweep_max_candidates: args.get(7).map_or(Ok(0), |s| s.parse())?,
        unread_liberty: unread,
        ..CheckOptions::default()
    };
    let trace = Trace {
        started: Instant::now(),
        count: AtomicUsize::new(0),
    };
    let report = rustsys_check::check_with(&opts, &[library], Ctx::new(None, Some(&trace)))?;
    std::fs::write(&args[5], serde_json::to_vec_pretty(&report)?)?;
    eprintln!("verdict: {:?}", report.verdict);
    Ok(())
}
