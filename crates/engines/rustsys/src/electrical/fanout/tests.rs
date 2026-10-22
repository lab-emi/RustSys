// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use super::*;
use crate::electrical::tests::{evaluate, fanout_design, inst, libs, port, r};
use crate::map::library::CellLib;
use rusteda_formats::verilog::write::NlDir;

// SYNTH-REQ-0556: shared positive/negative distribution preserves every sink polarity.
#[test]
fn phase_trees_share_inversion_and_preserve_both_polarities() {
    let mut ls = libs();
    ls[0].default_max_fanout = Some(8.0);
    let cells = CellLib::build(&ls, &[]).unwrap();
    let lib = Model::new(&ls, &cells);
    let mut m = fanout_design();
    m.instances.push(inst(
        "source_inverse",
        "wt_inv_x1",
        &[("A", "d"), ("Y", "negative")],
    ));
    for gate in &mut m.instances {
        if gate
            .name
            .strip_prefix("ff")
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| n >= 32)
        {
            gate.conns.iter_mut().find(|(p, _)| p == "D").unwrap().1 = r("negative");
        }
    }
    let mut net = Network::new(&m, &lib).unwrap();
    let root = net.refs.iter().position(|n| *n == r("d")).unwrap();
    let neg = net.refs.iter().position(|n| *n == r("negative")).unwrap();
    net.split_fanout(root, lib.by_name["wt_buf_x1"], 8, false, &lib);
    net.rebuild(&lib).unwrap();
    net.split_fanout(neg, lib.by_name["wt_buf_x1"], 8, false, &lib);
    net.rebuild(&lib).unwrap();
    let opts = Options {
        period_ns: 20.0,
        area_limit: Some(net.area(&lib) - 0.5),
        ..Options::default()
    };
    run_phases(
        &mut net,
        &lib,
        &[],
        &opts,
        &mut Report::default(),
        Ctx::NONE,
    )
    .unwrap();
    assert!(net.area(&lib) <= opts.area_limit.unwrap());
    net.write(&mut m, &lib);
    for a in [false, true] {
        for b in [false, true] {
            let values = evaluate(
                &m,
                &ls[0],
                &[("a", a), ("b", b), ("clk", false), ("rst", false)],
            );
            for gate in &m.instances {
                if let Some(i) = gate
                    .name
                    .strip_prefix("ff")
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    let d = &gate.conns.iter().find(|(p, _)| p == "D").unwrap().1;
                    assert_eq!(values[d], if i < 32 { !(a & b) } else { a & b });
                }
            }
        }
    }
}

// SYNTH-REQ-0556: usable setup margin may be spent to satisfy a hard area limit.
#[test]
fn area_recovery_can_spend_slack_but_cannot_miss_the_period() {
    let mut ls = libs();
    ls[0].default_max_fanout = Some(8.0);
    for cell in &mut ls[0].cells {
        if !cell.name.starts_with("wt_inv") {
            continue;
        }
        for pin in &mut cell.pins {
            for arc in &mut pin.arcs {
                for table in [&mut arc.cell_rise, &mut arc.cell_fall]
                    .into_iter()
                    .flatten()
                {
                    for value in &mut table.values {
                        *value += 1.0;
                    }
                }
            }
        }
    }
    let cells = CellLib::build(&ls, &[]).unwrap();
    let lib = Model::new(&ls, &cells);
    let mut net = Network::new(&fanout_design(), &lib).unwrap();
    let root = net.refs.iter().position(|n| *n == r("d")).unwrap();
    net.split_fanout(root, lib.by_name["wt_buf_x1"], 8, false, &lib);
    net.rebuild(&lib).unwrap();
    let opts = Options {
        period_ns: 10.0,
        area_limit: Some(net.area(&lib) - 0.5),
        ..Options::default()
    };
    let original = net.clone();
    let before = Timing::new(&net, &lib, &opts).worst_slack;
    run(
        &mut net,
        &lib,
        &[],
        &opts,
        &mut Report::default(),
        Ctx::NONE,
    )
    .unwrap();
    let after = Timing::new(&net, &lib, &opts).worst_slack;
    assert!(net.area(&lib) <= opts.area_limit.unwrap());
    assert!(after >= 0.0 && after < before);
    let mut tight = original;
    let tight_opts = Options {
        period_ns: 1.0,
        ..opts
    };
    run(
        &mut tight,
        &lib,
        &[],
        &tight_opts,
        &mut Report::default(),
        Ctx::NONE,
    )
    .unwrap();
    assert!(tight.area(&lib) > tight_opts.area_limit.unwrap());
    assert!(Timing::new(&tight, &lib, &tight_opts).worst_slack >= 0.0);
}

// SYNTH-REQ-0556: every path has an even number of inversions, including partial groups.
#[test]
fn paired_inverters_preserve_every_sink_across_partial_groups() {
    let ls = libs();
    let cells = CellLib::build(&ls, &[]).unwrap();
    let lib = Model::new(&ls, &cells);
    let inv = lib.by_name["wt_inv_x1"];
    for width in [2, 3, 5, 8, 16] {
        let mut m = fanout_design();
        let mut net = Network::new(&m, &lib).unwrap();
        let root = net.refs.iter().position(|n| *n == r("d")).unwrap();
        assert!(paired_tree(&mut net, root, inv, width, &lib) > 0);
        net.rebuild(&lib).unwrap();
        assert!(net.nodes[root].sinks.len() <= width);
        net.write(&mut m, &lib);
        for a in [false, true] {
            for b in [false, true] {
                let values = evaluate(
                    &m,
                    &ls[0],
                    &[("a", a), ("b", b), ("clk", false), ("rst", false)],
                );
                for gate in m.instances.iter().filter(|g| g.name.starts_with("ff")) {
                    let d = &gate.conns.iter().find(|(p, _)| p == "D").unwrap().1;
                    assert_eq!(values[d], !(a & b));
                }
            }
        }
    }
}

#[test]
fn whole_tree_rebuild_preserves_observed_buffer_and_state_inputs() {
    let ls = libs();
    let cells = CellLib::build(&ls, &[]).unwrap();
    let lib = Model::new(&ls, &cells);
    let mut m = fanout_design();
    // Twelve redundant stages before 64 sinks; the second stage is externally observed.
    for i in 0..12 {
        let source = if i == 0 {
            "d".to_string()
        } else {
            format!("b{}", i - 1)
        };
        m.instances.push(inst(
            &format!("buf{i}"),
            "wt_buf_x1",
            &[("A", &source), ("Y", &format!("b{i}"))],
        ));
    }
    for gate in &mut m.instances {
        if gate.name.starts_with("ff") {
            gate.conns.iter_mut().find(|(p, _)| p == "D").unwrap().1 = r("b11");
        }
    }
    m.ports.push(port("probe", NlDir::Output));
    m.assigns.push((r("probe"), r("b1")));
    let mut net = Network::new(&m, &lib).unwrap();
    let mut opts = Options {
        period_ns: 0.35,
        ..Options::default()
    };
    let before = Timing::new(&net, &lib, &opts).worst_slack;
    let area = net.area(&lib);
    opts.area_limit = Some(area);
    run(
        &mut net,
        &lib,
        &[],
        &opts,
        &mut Report::default(),
        Ctx::NONE,
    )
    .unwrap();
    assert!(net.area(&lib) < area);
    assert!(Timing::new(&net, &lib, &opts).worst_slack > before);
    net.write(&mut m, &lib);
    for a in [false, true] {
        for b in [false, true] {
            let values = evaluate(
                &m,
                &ls[0],
                &[("a", a), ("b", b), ("clk", false), ("rst", false)],
            );
            assert_eq!(values[&r("probe")], !(a & b));
            for gate in m.instances.iter().filter(|g| g.name.starts_with("ff")) {
                let d = &gate.conns.iter().find(|(p, _)| p == "D").unwrap().1;
                assert_eq!(values[d], !(a & b));
            }
        }
    }
}
