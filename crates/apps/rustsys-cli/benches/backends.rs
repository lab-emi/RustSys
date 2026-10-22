// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Backend benchmark: synthesizes a generated datapath of `K` multiply-accumulate lanes with every
//! mapping backend of the build, checks that the backends write the same netlist
//! (SYNTH-REQ-0516) and prints the time of each phase.
//!
//! `cargo bench -p rustsys-cli --features cuda --bench backends`
//! (`RUSTSYS_BENCH_K` sets the number of lanes, default 32; `RUSTSYS_BENCH_REPS` the
//! repetitions, default 3, of which the median is shown).

// The benchmark reports on standard output.
#![allow(clippy::print_stdout)]

use std::time::Instant;

use rusteda_formats::liberty;
use rustsys::map::MapBackend;
use rustsys::{Sources, SynthOptions, synthesize};
use rustsys_fixtures::liberty::{Variant, library};

const RTL: &str = r#"
module lanes #(parameter int K = 4) (
  input  logic            clk,
  input  logic [K*16-1:0] a, b, c,
  output logic [K*32-1:0] y,
  output logic [K-1:0]    gt
);
  for (genvar i = 0; i < K; i++) begin : g
    logic [31:0] acc_q;
    always_ff @(posedge clk) acc_q <= (a[i*16 +: 16] * b[i*16 +: 16] + acc_q) ^ {16'b0, c[i*16 +: 16]};
    assign y[i*32 +: 32] = acc_q;
    assign gt[i] = acc_q[31:16] > c[i*16 +: 16];
  end
endmodule
"#;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn in_pool<T: Send>(threads: usize, f: impl FnOnce() -> T + Send) -> T {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool")
        .install(f)
}

fn phase(p: &[(String, f64)], name: &str) -> f64 {
    p.iter().filter(|x| x.0 == name).map(|x| x.1).sum()
}

fn main() {
    let k = env("RUSTSYS_BENCH_K", 32);
    let reps = env("RUSTSYS_BENCH_REPS", 3).max(1);
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let lib = liberty::read_text(&library(Variant::Full), "full").expect("library");
    let mut synth_runs: Vec<(String, usize, MapBackend)> = vec![
        ("cpu".into(), 1, MapBackend::Cpu),
        ("cpu".into(), cpus, MapBackend::Cpu),
        ("cpu-task".into(), cpus, MapBackend::CpuTask),
    ];
    if cfg!(feature = "cuda") {
        synth_runs.push(("cuda".into(), cpus, MapBackend::Cuda));
    }
    println!("lanes {k}, {reps} repetitions (median), {cpus} CPUs");
    println!("synthesis           threads     total       map  map steps");
    let mut netlist: Option<String> = None;
    for (name, threads, b) in &synth_runs {
        let mut totals = Vec::new();
        let mut maps = Vec::new();
        let mut steps = Vec::new();
        for _ in 0..reps {
            let opts = SynthOptions {
                top: Some("lanes".into()),
                params: vec![("K".into(), k.to_string())],
                backend: *b,
                ..SynthOptions::default()
            };
            let t = Instant::now();
            let out = in_pool(*threads, || {
                synthesize(
                    &Sources::Texts(vec![("lanes.sv".into(), RTL.into())]),
                    std::slice::from_ref(&lib),
                    &opts,
                )
            })
            .unwrap_or_else(|e| panic!("{name}: {}", e.messages.join("\n")));
            totals.push(t.elapsed().as_secs_f64());
            maps.push(phase(&out.report.phase_s, "map"));
            steps = out.report.map_phase_s.clone();
            match &netlist {
                None => netlist = Some(out.text),
                Some(n) => assert!(*n == out.text, "{name}: netlist differs"),
            }
        }
        let st: Vec<String> = steps.iter().map(|(s, v)| format!("{s}={v:.3}")).collect();
        println!(
            "  {name:<16} {threads:>8} {:>8.3}s {:>8.3}s  {}",
            median(totals),
            median(maps),
            st.join(" ")
        );
    }
}
