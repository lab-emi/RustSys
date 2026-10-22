// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `synth-backends` (SYNTH-REQ-0515 to SYNTH-REQ-0518): the emitted netlist does not depend
//! on the thread count, the cut-pass task's CPU backend emits the netlist of the mapper's own
//! passes byte for byte, and so does the CUDA backend.
//! The GPU test is ignored by default; run it on a GPU machine with
//! `cargo test -p rustsys-cli --features cuda --test synth_backends -- --ignored`
//! (the CUDA runtime loads the driver and NVRTC libraries at run time).

mod common;

use rusteda_formats::liberty::Library;
use rustsys::map::MapBackend;
use rustsys::{Effort, Goal, Sources, SynthOptions, synthesize};
use rustsys_fixtures::liberty::Variant;
use rustsys_fixtures::random::{random_comb, random_seq};
use rustsys_fixtures::rtl::CORPUS;

struct Case {
    name: String,
    top: String,
    files: Vec<(String, String)>,
}

/// Corpus designs, random combinational and random sequential designs.
fn cases() -> Vec<Case> {
    let mut v: Vec<Case> = CORPUS
        .iter()
        .take(8)
        .map(|d| Case {
            name: d.name.to_string(),
            top: d.top.to_string(),
            files: d
                .files
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect(),
        })
        .collect();
    for seed in [3, 17, 42] {
        let m = random_comb(seed, 20);
        v.push(Case {
            name: format!("random_comb {seed}"),
            top: m.name.clone(),
            files: vec![(format!("{}.sv", m.name), m.text)],
        });
    }
    for seed in [5, 9] {
        v.push(Case {
            name: format!("random_seq {seed}"),
            top: format!("seq_{seed}"),
            files: vec![(format!("seq_{seed}.sv"), random_seq(seed))],
        });
    }
    v
}

/// The netlist text and the backend that ran the cut passes.
fn run(
    c: &Case,
    lib: &Library,
    effort: Effort,
    goal: Goal,
    backend: MapBackend,
) -> Result<(String, String), String> {
    let opts = SynthOptions {
        top: Some(c.top.clone()),
        effort,
        goal,
        backend,
        ..SynthOptions::default()
    };
    synthesize(
        &Sources::Texts(c.files.clone()),
        std::slice::from_ref(lib),
        &opts,
    )
    .map(|o| (o.text, o.report.map_backend))
    .map_err(|e| e.messages.join("\n"))
}

const SETTINGS: [(Effort, Goal); 3] = [
    (Effort::Low, Goal::Delay),
    (Effort::Medium, Goal::Delay),
    (Effort::High, Goal::Area),
];

// SYNTH-REQ-0515, SYNTH-REQ-0517: level-parallel passes give the same netlist for any number of
// threads.
#[test]
fn thread_count_does_not_change_netlist() {
    let lib = common::lib(Variant::Rich);
    for c in cases() {
        for (effort, goal) in SETTINGS {
            let one = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .expect("thread pool")
                .install(|| run(&c, &lib, effort, goal, MapBackend::Cpu))
                .unwrap_or_else(|e| panic!("{}: {e}", c.name));
            for threads in [3, 8] {
                let many = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .expect("thread pool")
                    .install(|| run(&c, &lib, effort, goal, MapBackend::Cpu))
                    .unwrap_or_else(|e| panic!("{}: {e}", c.name));
                assert!(
                    one.0 == many.0,
                    "{} {effort:?} {goal:?}: {threads} threads change the netlist",
                    c.name
                );
            }
        }
    }
}

fn task_equals_cpu(b: MapBackend, name: &str) {
    for variant in [Variant::Rich, Variant::Full] {
        let lib = common::lib(variant);
        for c in cases() {
            for (effort, goal) in SETTINGS {
                let (cpu, ran) = run(&c, &lib, effort, goal, MapBackend::Cpu)
                    .unwrap_or_else(|e| panic!("{}: {e}", c.name));
                assert_eq!(ran, "cpu");
                let (task, ran) = run(&c, &lib, effort, goal, b)
                    .unwrap_or_else(|e| panic!("{} on {name}: {e}", c.name));
                assert_eq!(ran, name);
                assert!(
                    task == cpu,
                    "{} {variant:?} {effort:?} {goal:?}: {name} netlist differs",
                    c.name
                );
            }
        }
    }
}

// SYNTH-REQ-0516: the task's CPU backend emits the CPU netlist byte for byte.
#[test]
fn task_cpu_netlists_equal_cpu() {
    task_equals_cpu(MapBackend::CpuTask, "cpu");
}

// SYNTH-REQ-0516, SYNTH-REQ-0518: the CUDA backend emits the CPU netlist byte for byte.
#[test]
#[ignore]
fn gpu_netlists_equal_cpu() {
    task_equals_cpu(MapBackend::Cuda, "cuda");
    task_equals_cpu(MapBackend::Gpu, "cuda");
}

// SYNTH-REQ-0516: a runtime that this build lacks is an input error, not a silent fallback.
#[test]
fn missing_runtime_is_an_error() {
    let lib = common::lib(Variant::Minimal);
    let c = &cases()[0];
    if !cfg!(feature = "cuda") {
        for backend in [MapBackend::Cuda, MapBackend::Gpu] {
            let e = run(c, &lib, Effort::Low, Goal::Delay, backend)
                .expect_err("missing runtime must fail");
            assert!(e.contains("error[S0530]"), "{e}");
        }
    }
}
