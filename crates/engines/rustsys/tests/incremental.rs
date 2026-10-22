// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The incremental electrical mode of post-mapping commands (SYNTH-REQ-0561 to SYNTH-REQ-0564):
//! it starts from the cells of the netlist, changes only the focus instances and the nets they
//! drive, one change per iteration, and stops at its condition, its iteration bound, the area
//! limit or the first iteration without improvement. Equivalence of the results is proven by the
//! independent checker in the command-line tests.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusteda_core::job::Ctx;
use rusteda_formats::liberty::{Direction, Library};
use rusteda_formats::verilog::write::{NlDir, NlInstance, NlModule, NlPort, NlRef, write_netlist};
use rustsys::electrical::{
    Incremental, Options, Report, SearchEffort, StopReason, StopRule, refine,
};
use rustsys::map::library::CellLib;
use rustsys::{Goal, Sources, SynthOptions, synthesize};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;

fn lib() -> Library {
    rusteda_formats::liberty::read_text(&library(Variant::Full), "full.lib").unwrap()
}

/// The refined netlist of corpus design `name` at `period_ns`.
fn synthesized(lib: &Library, name: &str, period_ns: f64) -> NlModule {
    let d = CORPUS.iter().find(|d| d.name == name).unwrap();
    let files = d
        .files
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    let opts = SynthOptions {
        top: Some(d.top.to_string()),
        goal: Goal::Delay,
        electrical: Some(Options {
            period_ns,
            ..Options::default()
        }),
        ..SynthOptions::default()
    };
    synthesize(&Sources::Texts(files), std::slice::from_ref(lib), &opts)
        .unwrap()
        .netlist
}

fn options(
    period_ns: f64,
    focus: Option<Vec<String>>,
    max_iterations: usize,
    stop: StopRule,
    effort: SearchEffort,
) -> Options {
    Options {
        period_ns,
        incremental: Some(Incremental {
            focus,
            max_iterations,
            stop,
            effort,
        }),
        ..Options::default()
    }
}

fn run(nl: &NlModule, lib: &Library, opts: &Options) -> (NlModule, Report) {
    let eligible = CellLib::build(std::slice::from_ref(lib), &[]).unwrap();
    let mut out = nl.clone();
    let r = refine(
        &mut out,
        std::slice::from_ref(lib),
        &eligible,
        opts,
        Ctx::NONE,
    )
    .unwrap();
    (out, r)
}

fn cells(nl: &NlModule) -> BTreeMap<String, String> {
    nl.instances
        .iter()
        .map(|i| (i.name.clone(), i.cell.clone()))
        .collect()
}

/// The instances that drive each net, by the output pins of their cells.
fn drivers(nl: &NlModule, lib: &Library) -> HashMap<NlRef, String> {
    let mut out = HashMap::new();
    for inst in &nl.instances {
        let cell = lib.cells.iter().find(|c| c.name == inst.cell).unwrap();
        for (pin, r) in &inst.conns {
            let output = cell
                .pins
                .iter()
                .any(|p| &p.name == pin && p.direction == Direction::Output);
            if output {
                out.insert(r.clone(), inst.name.clone());
            }
        }
    }
    out
}

// SYNTH-REQ-0561: the mode starts from the cells of the netlist. An instance outside the focus
// keeps an upsized cell that the complete refinement of synthesis would reset to the smallest
// variant of its family first.
#[test]
fn cells_outside_the_focus_keep_their_size() {
    let lib = lib();
    let mut nl = synthesized(&lib, "alu", 2.0);
    let pos = nl
        .instances
        .iter()
        .position(|i| i.cell == "wtf_nand2_1")
        .expect("a nand2 instance");
    nl.instances[pos].cell = "wtf_nand2_4".into();
    let kept = nl.instances[pos].name.clone();
    let focus: Vec<String> = nl
        .instances
        .iter()
        .filter(|i| i.name != kept)
        .take(5)
        .map(|i| i.name.clone())
        .collect();
    let opts = options(
        0.4,
        Some(focus),
        6,
        StopRule::NoImprovement,
        SearchEffort::Medium,
    );
    let (out, r) = run(&nl, &lib, &opts);
    assert_eq!(cells(&out)[&kept], "wtf_nand2_4", "{r:?}");
}

// SYNTH-REQ-0561: only focus instances change cells, and inserted buffers hang on nets that a
// focus instance (or an inserted buffer) drives; every other instance keeps its cell.
#[test]
fn changes_stay_in_the_focus_neighbourhood() {
    let lib = lib();
    let nl = synthesized(&lib, "alu", 2.0);
    let before = cells(&nl);
    let focus: BTreeSet<String> = nl
        .instances
        .iter()
        .skip(10)
        .step_by(7)
        .take(12)
        .map(|i| i.name.clone())
        .collect();
    let opts = options(
        0.4,
        Some(focus.iter().cloned().collect()),
        12,
        StopRule::NoImprovement,
        SearchEffort::High,
    );
    let (out, r) = run(&nl, &lib, &opts);
    let inc = r.incremental.as_ref().unwrap();
    assert_eq!(inc.editable_instances, focus.len());
    assert!(inc.iterations_run > 0, "{inc:?}");
    let after = cells(&out);
    let added: BTreeSet<&String> = after.keys().filter(|n| !before.contains_key(*n)).collect();
    for (name, cell) in &before {
        if !focus.contains(name) {
            assert_eq!(&after[name], cell, "{name} is outside the focus");
        }
    }
    let driven = drivers(&out, &lib);
    for inst in out.instances.iter().filter(|i| added.contains(&i.name)) {
        assert!(inst.name.starts_with("rs_buf"), "{}", inst.name);
        let (_, input) = inst.conns.iter().find(|(p, _)| p == "A").unwrap();
        let driver = &driven[input];
        assert!(
            focus.contains(driver) || added.contains(driver),
            "{} is fed by {driver}",
            inst.name
        );
    }
    let resized = after
        .iter()
        .filter(|(n, c)| before.get(*n).is_some_and(|b| b != *c))
        .count();
    assert_eq!(resized, r.resized_cells);
    assert_eq!(added.len(), r.buffers_added);
    assert!(inc.wns_after_ns >= inc.wns_before_ns, "{inc:?}");
    assert!(
        inc.wns_after_ns > inc.wns_before_ns || inc.tns_after_ns > inc.tns_before_ns,
        "{inc:?}"
    );
    assert_eq!(inc.wns_after_ns, r.estimated_slack_ns);
}

/// An inverter driving a heavily loaded output: only upsizing it helps.
fn heavy_inverter() -> NlModule {
    let net = |n: &str| NlRef::Net(n.into());
    NlModule {
        name: "m".into(),
        ports: vec![
            NlPort {
                name: "a".into(),
                dir: NlDir::Input,
                width: None,
            },
            NlPort {
                name: "y".into(),
                dir: NlDir::Output,
                width: None,
            },
        ],
        wires: Vec::new(),
        instances: vec![NlInstance {
            cell: "wtf_inv_1".into(),
            name: "u1".into(),
            conns: vec![("A".into(), net("a")), ("Y".into(), net("y"))],
        }],
        assigns: Vec::new(),
    }
}

// SYNTH-REQ-0562: the stop condition is evaluated before the first iteration and after each,
// the iteration bound holds, and an improvement that only the area limit forbids is reported.
#[test]
fn stop_reasons_and_bounds() {
    let lib = lib();
    let nl = heavy_inverter();
    let heavy = |o: Options| Options {
        output_load_pf: 0.5,
        ..o
    };
    let opts = heavy(options(
        0.1,
        None,
        8,
        StopRule::WnsAtLeast(-1e6),
        SearchEffort::Low,
    ));
    let (out, r) = run(&nl, &lib, &opts);
    let inc = r.incremental.unwrap();
    assert_eq!(
        (inc.stop_reason, inc.iterations_run),
        (StopReason::ConditionMet, 0)
    );
    assert_eq!(write_netlist(&out), write_netlist(&nl));

    let opts = heavy(options(
        0.1,
        None,
        1,
        StopRule::NoImprovement,
        SearchEffort::Low,
    ));
    let (out, r) = run(&nl, &lib, &opts);
    let inc = r.incremental.unwrap();
    assert_eq!(
        (inc.stop_reason, inc.iterations_run),
        (StopReason::MaxIters, 1)
    );
    assert_eq!(cells(&out)["u1"], "wtf_inv_2");
    assert_eq!(inc.history[0].change, "resize");
    assert_eq!(inc.history[0].instance.as_deref(), Some("u1"));
    assert_eq!(
        (
            inc.history[0].from_cell.as_str(),
            inc.history[0].to_cell.as_str()
        ),
        ("wtf_inv_1", "wtf_inv_2")
    );
    assert!(inc.wns_after_ns > inc.wns_before_ns);

    let area = lib
        .cells
        .iter()
        .find(|c| c.name == "wtf_inv_1")
        .unwrap()
        .area;
    let opts = Options {
        area_limit: Some(area),
        ..heavy(options(
            0.1,
            None,
            8,
            StopRule::NoImprovement,
            SearchEffort::Low,
        ))
    };
    let (out, r) = run(&nl, &lib, &opts);
    let inc = r.incremental.unwrap();
    assert_eq!(
        (inc.stop_reason, inc.iterations_run),
        (StopReason::AreaLimit, 0)
    );
    assert_eq!(cells(&out)["u1"], "wtf_inv_1");

    let opts = heavy(options(
        0.1,
        None,
        50,
        StopRule::NoImprovement,
        SearchEffort::Medium,
    ));
    let (out, r) = run(&nl, &lib, &opts);
    let inc = r.incremental.unwrap();
    assert_eq!(inc.stop_reason, StopReason::NoImprovement);
    assert!(
        inc.iterations_run >= 1 && inc.iterations_run < 50,
        "{inc:?}"
    );
    // A second run from the fixed point changes nothing.
    let (again, r2) = run(&out, &lib, &opts);
    assert_eq!(r2.incremental.unwrap().iterations_run, 0);
    assert_eq!(write_netlist(&again), write_netlist(&out));

    let wns = inc.wns_before_ns + 1e-6;
    let opts = heavy(options(
        0.1,
        None,
        50,
        StopRule::WnsAtLeast(wns),
        SearchEffort::Medium,
    ));
    let r = run(&nl, &lib, &opts).1.incremental.unwrap();
    assert_eq!(
        (r.stop_reason, r.iterations_run),
        (StopReason::ConditionMet, 1)
    );
    let opts = heavy(options(
        0.1,
        None,
        50,
        StopRule::TnsAtLeast(0.0),
        SearchEffort::Medium,
    ));
    let r = run(&nl, &lib, &opts).1.incremental.unwrap();
    assert!(
        r.stop_reason != StopReason::ConditionMet || r.tns_after_ns >= 0.0,
        "{r:?}"
    );
}

// SYNTH-REQ-0563: the low effort examines the next smaller and larger variant and never
// buffers; the medium and high efforts may buffer. The mode is deterministic.
#[test]
fn effort_controls_the_search() {
    let lib = lib();
    let nl = synthesized(&lib, "barrel", 3.0);
    for effort in [SearchEffort::Low, SearchEffort::Medium, SearchEffort::High] {
        let opts = options(0.3, None, 6, StopRule::NoImprovement, effort);
        let (a, ra) = run(&nl, &lib, &opts);
        let (b, _) = run(&nl, &lib, &opts);
        assert_eq!(write_netlist(&a), write_netlist(&b), "{effort:?}");
        let inc = ra.incremental.unwrap();
        if effort == SearchEffort::Low {
            assert!(inc.history.iter().all(|h| h.change == "resize"), "{inc:?}");
            assert_eq!(ra.buffers_added, 0);
        }
        let mut last = inc.wns_before_ns;
        for h in &inc.history {
            assert!(h.estimated_wns_ns >= last - 1e-9, "{h:?}");
            last = h.estimated_wns_ns;
        }
    }
}

// SYNTH-REQ-0564: every resize stays in the cell's family (same function and pins) and every
// inserted cell is a non-inverting buffer; an unknown focus name or a bad option is rejected.
#[test]
fn changes_keep_functions_and_options_are_checked() {
    let lib = lib();
    let nl = synthesized(&lib, "alu", 2.0);
    let opts = options(0.4, None, 10, StopRule::NoImprovement, SearchEffort::High);
    let (out, _) = run(&nl, &lib, &opts);
    let before = cells(&nl);
    let function = |cell: &str| -> Vec<(String, Option<String>)> {
        let c = lib.cells.iter().find(|c| c.name == cell).unwrap();
        let mut f: Vec<_> = c
            .pins
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    p.function.as_ref().map(|f| format!("{f:?}")),
                )
            })
            .collect();
        f.sort();
        f
    };
    for inst in &out.instances {
        match before.get(&inst.name) {
            Some(old) => assert_eq!(function(old), function(&inst.cell), "{}", inst.name),
            None => assert!(inst.cell.starts_with("wtf_buf_"), "{}", inst.cell),
        }
    }
    let eligible = CellLib::build(std::slice::from_ref(&lib), &[]).unwrap();
    let mut copy = nl.clone();
    let bad = options(
        0.4,
        Some(vec!["no_such_instance".into()]),
        1,
        StopRule::NoImprovement,
        SearchEffort::Low,
    );
    assert!(
        refine(
            &mut copy,
            std::slice::from_ref(&lib),
            &eligible,
            &bad,
            Ctx::NONE
        )
        .is_err()
    );
    for opts in [
        options(0.4, None, 0, StopRule::NoImprovement, SearchEffort::Low),
        options(
            0.4,
            None,
            1,
            StopRule::WnsAtLeast(f64::NAN),
            SearchEffort::Low,
        ),
    ] {
        assert!(opts.validate().is_err());
    }
    // Synthesis runs the complete refinement only.
    let d = CORPUS.iter().find(|d| d.name == "alu").unwrap();
    let files = d
        .files
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    let synth = SynthOptions {
        top: Some(d.top.to_string()),
        electrical: Some(options(
            1.0,
            None,
            1,
            StopRule::NoImprovement,
            SearchEffort::Low,
        )),
        ..SynthOptions::default()
    };
    let err = synthesize(&Sources::Texts(files), std::slice::from_ref(&lib), &synth).unwrap_err();
    assert!(err.messages[0].contains("S0531"), "{:?}", err.messages);
}
