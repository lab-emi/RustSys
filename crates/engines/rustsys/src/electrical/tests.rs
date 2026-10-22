// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use super::*;
use rustc_hash::FxHashMap;
use rusteda_core::job::CancelToken;
use rusteda_formats::verilog::write::{NlDir, NlInstance, NlPort, NlRef};
use rustsys_fixtures::liberty::{Variant, library};

pub(super) fn libs() -> Vec<Library> {
    vec![rusteda_formats::liberty::read_text(&library(Variant::Rich), "test").unwrap()]
}
pub(super) fn r(name: &str) -> NlRef {
    NlRef::Net(name.into())
}
pub(super) fn port(name: &str, dir: NlDir) -> NlPort {
    NlPort {
        name: name.into(),
        dir,
        width: None,
    }
}
pub(super) fn inst(name: &str, cell: &str, conns: &[(&str, &str)]) -> NlInstance {
    NlInstance {
        name: name.into(),
        cell: cell.into(),
        conns: conns.iter().map(|&(p, n)| (p.into(), r(n))).collect(),
    }
}

// SYNTH-REQ-0523, 0530: selector data and load enable are synchronous endpoints,
// whereas clock/event controls retain the original protected cone.
#[test]
fn enabled_and_hold_inputs_are_timed_and_editable() {
    let ls = libs();
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    for (cell, pins, expected) in [
        (
            "wt_edff_x1",
            vec![("D", "d"), ("E", "enable"), ("CK", "clk"), ("Q", "q")],
            2,
        ),
        (
            "wt_sdff_x1",
            vec![
                ("D", "d"),
                ("SI", "q"),
                ("SE", "enable"),
                ("CK", "clk"),
                ("Q", "q"),
            ],
            3,
        ),
    ] {
        assert_eq!(model.cells.iter().filter(|c| c.raw.name == cell).count(), 1);
        let c = &model.cells[model.by_name[cell]];
        assert_eq!(c.data.len(), expected);
        let m = NlModule {
            name: "controlled".into(),
            ports: vec![
                port("clk", NlDir::Input),
                port("d", NlDir::Input),
                port("en", NlDir::Input),
                port("q", NlDir::Output),
            ],
            instances: vec![
                inst("logic", "wt_buf_x1", &[("A", "en"), ("Y", "enable")]),
                inst("state", cell, &pins),
            ],
            ..Default::default()
        };
        let n = network::Network::new(&m, &model).unwrap();
        let index = |name| n.refs.iter().position(|x| *x == r(name)).unwrap();
        assert!(!n.nodes[index("enable")].protected);
        assert!(!n.nodes[index("en")].protected);
        assert!(!n.nodes[index("q")].protected);
        assert!(n.nodes[index("clk")].protected);
        let t = timing::Timing::new(&n, &model, &Options::default());
        assert!(
            t.required[index("enable")]
                .iter()
                .all(|v| v.is_finite() && *v < 10.0)
        );
        assert!(
            t.required[index("d")]
                .iter()
                .all(|v| v.is_finite() && *v < 10.0)
        );
    }
}

/// Evaluate Liberty functions directly, resolving aliases in their Verilog direction.
pub(super) fn evaluate(
    m: &NlModule,
    lib: &Library,
    given: &[(&str, bool)],
) -> FxHashMap<NlRef, bool> {
    let mut vals: FxHashMap<_, _> = given.iter().map(|&(n, v)| (r(n), v)).collect();
    vals.insert(NlRef::Const(false), false);
    vals.insert(NlRef::Const(true), true);
    loop {
        let size = vals.len();
        for i in &m.instances {
            let c = lib.cells.iter().find(|c| c.name == i.cell).unwrap();
            if !c.sequentials.is_empty() {
                continue;
            }
            for p in c.outputs() {
                let Some((_, out)) = i.conns.iter().find(|(n, _)| n == &p.name) else {
                    continue;
                };
                let val = p.function.as_ref().unwrap().eval(&|pin| {
                    i.conns
                        .iter()
                        .find(|(p, _)| p == pin)
                        .and_then(|(_, n)| vals.get(n).copied())
                });
                if let Some(v) = val {
                    vals.insert(out.clone(), v);
                }
            }
        }
        for (lhs, rhs) in &m.assigns {
            if let Some(&v) = vals.get(rhs) {
                vals.insert(lhs.clone(), v);
            }
        }
        if vals.len() == size {
            break;
        }
    }
    vals
}

// SYNTH-REQ-0531, 0533: a reconvergent NAND cover of XOR is replaceable by one cell;
// retained external fanouts and scalar aliases must remain driven and equivalent.
#[test]
fn exact_collapse_preserves_reconvergence_and_aliases() {
    let ls = libs();
    let lib = CellLib::build(&ls, &[]).unwrap();
    for shared in [false, true] {
        let mut m = NlModule {
            name: "logic".into(),
            ports: vec![
                port("a", NlDir::Input),
                port("b", NlDir::Input),
                port("y", NlDir::Output),
            ],
            instances: vec![
                inst("p", "wt_nand2_x1", &[("A", "a"), ("B", "b"), ("Y", "n0")]),
                inst("q", "wt_nand2_x1", &[("A", "a"), ("B", "n0"), ("Y", "n1")]),
                inst("t", "wt_nand2_x1", &[("A", "b"), ("B", "n0"), ("Y", "n2")]),
                inst(
                    "u",
                    "wt_nand2_x1",
                    &[("A", "n1"), ("B", "n2"), ("Y", "raw")],
                ),
            ],
            assigns: vec![(r("alias"), r("raw")), (r("y"), r("alias"))],
            ..NlModule::default()
        };
        if shared {
            m.ports.push(port("shared", NlDir::Output));
            m.assigns.push((r("shared"), r("n0")));
        }
        let opts = Options {
            period_ns: 5.0,
            ..Options::default()
        };
        let old = m.clone();
        let report = refine(&mut m, &ls, &lib, &opts, Ctx::NONE).unwrap();
        assert!(report.gates_collapsed >= 2, "{report:?}");
        assert!(report.area_after < report.area_before);
        for a in [false, true] {
            for b in [false, true] {
                let got = evaluate(&m, &ls[0], &[("a", a), ("b", b)]);
                assert_eq!(got[&r("y")], a ^ b);
                if shared {
                    assert_eq!(got[&r("shared")], !(a & b));
                }
            }
        }
        let mut again = old;
        refine(&mut again, &ls, &lib, &opts, Ctx::NONE).unwrap();
        assert_eq!(m, again);
    }
}

pub(super) fn fanout_design() -> NlModule {
    let mut m = NlModule {
        name: "fanout".into(),
        ports: vec![
            port("a", NlDir::Input),
            port("b", NlDir::Input),
            port("clk", NlDir::Input),
            port("rst", NlDir::Input),
        ],
        instances: vec![
            inst("data", "wt_nand2_x1", &[("A", "a"), ("B", "b"), ("Y", "d")]),
            inst("clock", "wt_inv_x1", &[("A", "clk"), ("Y", "cn")]),
            inst("reset", "wt_inv_x1", &[("A", "rst"), ("Y", "rn")]),
        ],
        ..NlModule::default()
    };
    for i in 0..64 {
        let q = format!("state{i}");
        m.wires.push((q.clone(), None));
        m.instances.push(inst(
            &format!("ff{i}"),
            "wt_dffr_x1",
            &[("D", "d"), ("CK", "cn"), ("RN", "rn"), ("Q", &q)],
        ));
    }
    m
}

// SYNTH-REQ-0536: even a timing-loose net must satisfy pin limits; fanout uses sink weights.
#[test]
fn repair_noncritical_weighted_fanout_and_capacitance() {
    let mut ls = libs();
    ls[0].default_max_capacitance = Some(0.035);
    ls[0].default_max_fanout = Some(8.0);
    for c in &mut ls[0].cells {
        for p in &mut c.pins {
            if p.name == "D" {
                p.fanout_load = Some(2.0);
            }
        }
    }
    let lib = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &lib);
    let mut m = fanout_design();
    let before = network::Network::new(&m, &model).unwrap();
    let opts = Options {
        period_ns: 20.0,
        buffer_fanout: 16,
        area_limit: Some(before.area(&model) + 60.0),
        ..Options::default()
    };
    let t = timing::Timing::new(&before, &model, &opts);
    assert!(t.worst_slack > 10.0);
    let old = m.clone();
    let rr = refine(&mut m, &ls, &lib, &opts, Ctx::NONE).unwrap();
    assert!(rr.rule_buffers + rr.rule_clones > 0, "{rr:?}");
    assert!(rr.area_limit_met);
    assert!(rr.estimated_data_rules_met, "{rr:?}");
    for name in ["clock", "reset"] {
        assert_eq!(
            m.instances.iter().find(|i| i.name == name),
            old.instances.iter().find(|i| i.name == name)
        );
    }
    for a in [false, true] {
        for b in [false, true] {
            let v = evaluate(
                &m,
                &ls[0],
                &[("a", a), ("b", b), ("clk", false), ("rst", false)],
            );
            for ff in m.instances.iter().filter(|i| i.name.starts_with("ff")) {
                let n = &ff.conns.iter().find(|(p, _)| p == "D").unwrap().1;
                assert_eq!(v[n], !(a & b));
            }
        }
    }
}

// Repair candidates can feed each other. Splitting the earlier driver must not index the
// old timing arrays with a freshly allocated buffer node while sizing the later candidate.
#[test]
fn repair_connected_candidates_uses_original_timing_inputs() {
    let mut ls = libs();
    ls[0].default_max_capacitance = Some(0.035);
    let lib = CellLib::build(&ls, &[]).unwrap();
    let mut m = fanout_design();
    m.instances.push(inst(
        "second",
        "wt_nand2_x1",
        &[("A", "d"), ("B", "a"), ("Y", "e")],
    ));
    for i in 0..32 {
        let q = format!("extra_q{i}");
        m.wires.push((q.clone(), None));
        m.instances.push(inst(
            &format!("extra{i}"),
            "wt_dffr_x1",
            &[("D", "e"), ("CK", "cn"), ("RN", "rn"), ("Q", &q)],
        ));
    }
    let mut net = network::Network::new(&m, &model::Model::new(&ls, &lib)).unwrap();
    let model = model::Model::new(&ls, &lib);
    let opts = Options {
        period_ns: 100.0,
        area_limit: None,
        ..Options::default()
    };
    let mut report = Report::default();
    repair::run(&mut net, &model, &opts, &mut report, Ctx::NONE).unwrap();
    assert!(report.rule_buffers > 0);
    assert_eq!(
        rules::report(&net, &model, &timing::Timing::new(&net, &model, &opts)).capacitance_nets,
        0
    );
}

// SYNTH-REQ-0536: the late range value overrides the scalar capacitance, and pin limits
// override defaults with correct unit conversion.
#[test]
fn ranges_and_rule_units() {
    let mut ls = libs();
    ls[0].time_unit_ns = 0.001;
    ls[0].cap_unit_pf = 0.001;
    ls[0].default_max_transition = Some(5.0);
    ls[0].default_max_capacitance = Some(3.0);
    let c = ls[0]
        .cells
        .iter_mut()
        .find(|c| c.name == "wt_nand2_x1")
        .unwrap();
    let a = c.pins.iter_mut().find(|p| p.name == "A").unwrap();
    a.capacitance = 9.0;
    a.rise_capacitance_range = Some((1.0, 2.0));
    a.fall_capacitance = Some(1.5);
    let y = c.pins.iter_mut().find(|p| p.name == "Y").unwrap();
    y.max_capacitance = Some(4.0);
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let lib = model::Model::new(&ls, &eligible);
    let c = &lib.cells[lib.by_name["wt_nand2_x1"]];
    assert_eq!(c.input_cap(c.inputs[0]), 0.002);
    assert_eq!(c.max_cap(c.outputs[0]), 0.004);
    assert_eq!(c.max_slew(c.outputs[0]), 0.005);
}

// SYNTH-REQ-0532 to 0535: known linear load delay improves within a budget while control
// cones and state output names stay intact. Every FF sees the original data function.
#[test]
fn buffers_repair_load_without_changing_clock_or_state() {
    let ls = libs();
    let lib = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &lib);
    let mut m = fanout_design();
    let before = network::Network::new(&m, &model).unwrap();
    let opts = Options {
        period_ns: 0.35,
        buffer_fanout: 8,
        area_limit: Some(before.area(&model) + 30.0),
        ..Options::default()
    };
    let original = timing::Timing::new(&before, &model, &opts).worst_slack;
    let old = m.clone();
    let report = refine(&mut m, &ls, &lib, &opts, Ctx::NONE).unwrap();
    assert!(report.buffers_added > 0);
    // A deeper tree may have less electrical delay; validate delay rather than topology.
    assert!(report.estimated_slack_ns > original + 0.1);
    assert!(report.area_limit_met);
    for name in ["clock", "reset"] {
        assert_eq!(
            m.instances.iter().find(|i| i.name == name),
            old.instances.iter().find(|i| i.name == name)
        );
    }
    for a in [false, true] {
        for b in [false, true] {
            let vals = evaluate(
                &m,
                &ls[0],
                &[("a", a), ("b", b), ("clk", false), ("rst", false)],
            );
            for i in 0..64 {
                let ff = m
                    .instances
                    .iter()
                    .find(|x| x.name == format!("ff{i}"))
                    .unwrap();
                let pin = |p: &str| &ff.conns.iter().find(|(name, _)| name == p).unwrap().1;
                assert_eq!(vals[pin("D")], !(a & b));
                assert_eq!(pin("Q"), &r(&format!("state{i}")));
            }
        }
    }
    let mut bounded = old.clone();
    let opts = Options {
        area_limit: Some(before.area(&model)),
        ..opts
    };
    let rr = refine(&mut bounded, &ls, &lib, &opts, Ctx::NONE).unwrap();
    assert_eq!(rr.buffers_added, 0);
    assert!(rr.area_limit_met);
    assert!(!rr.estimated_timing_met);
    let mut excluded = old;
    let lib = CellLib::build(&ls, &["wt_buf*".into()]).unwrap();
    let rr = refine(
        &mut excluded,
        &ls,
        &lib,
        &Options {
            area_limit: None,
            ..opts
        },
        Ctx::NONE,
    )
    .unwrap();
    assert!(rr.estimated_data_rules_met);
    assert!(
        excluded
            .instances
            .iter()
            .all(|i| !i.cell.starts_with("wt_buf"))
    );
}

// SYNTH-REQ-0532: explicit units yield the fixture's analytic delay in ns and cap in pF.
#[test]
fn timing_units_and_invalid_inputs() {
    let mut ls = libs();
    ls[0].time_unit_ns = 0.001;
    ls[0].cap_unit_pf = 0.001;
    let lib = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &lib);
    let c = &model.cells[model.by_name["wt_nand2_x1"]];
    assert!((c.input_cap(c.inputs[0]) - 0.0000015).abs() < 1e-15);
    let arcs = c.arc(c.inputs[0], c.outputs[0], [0.0001; 2], 0.00001);
    assert!((arcs[0].2 - 0.000074).abs() < 1e-12);
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(
            Options {
                period_ns: value,
                ..Options::default()
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        Options {
            buffer_fanout: 1,
            ..Options::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Options {
            output_load_pf: -1.0,
            ..Options::default()
        }
        .validate()
        .is_err()
    );
}

// SYNTH-REQ-0534: cancellation leaves the caller's netlist unchanged.
#[test]
fn cancelled_refinement_is_transactional() {
    let ls = libs();
    let lib = CellLib::build(&ls, &[]).unwrap();
    let mut m = fanout_design();
    let old = m.clone();
    let token = CancelToken::new();
    token.cancel();
    assert!(matches!(
        refine(
            &mut m,
            &ls,
            &lib,
            &Options::default(),
            Ctx::new(Some(&token), None)
        ),
        Err(JobError::Cancelled)
    ));
    assert_eq!(m, old);
}

// SYNTH-REQ-0531, 0533: exact substitutions preserve every output on all six-input
// assignments, including reconvergence, repeated pins and non-symmetric cell pin orders.
#[test]
fn exhaustive_random_cell_networks() {
    for variant in Variant::ALL {
        let ls =
            vec![rusteda_formats::liberty::read_text(&library(variant), "random cells").unwrap()];
        let eligible = CellLib::build(&ls, &[]).unwrap();
        let mut seed = 0x1457319u64;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        let mut m = NlModule {
            name: "random".into(),
            ..NlModule::default()
        };
        let mut refs: Vec<_> = (0..6).map(|i| format!("p{i}")).collect();
        for s in &refs {
            m.ports.push(port(s, NlDir::Input));
        }
        for g in 0..160 {
            let c = &eligible.comb[random() % eligible.comb.len()];
            let mut conns: Vec<_> = c
                .inputs
                .iter()
                .map(|pin| (pin.clone(), r(&refs[random() % refs.len()])))
                .collect();
            let out = format!("n{g}");
            m.wires.push((out.clone(), None));
            conns.push((c.output.clone(), r(&out)));
            m.instances.push(NlInstance {
                cell: c.name.clone(),
                name: format!("cell{g}"),
                conns,
            });
            refs.push(out.clone());
            if g % 11 == 0 || g == 159 {
                let p = format!("out{g}");
                m.ports.push(port(&p, NlDir::Output));
                m.assigns.push((r(&p), r(&out)));
            }
        }
        let old = m.clone();
        refine(
            &mut m,
            &ls,
            &eligible,
            &Options {
                period_ns: 0.5,
                ..Options::default()
            },
            Ctx::NONE,
        )
        .unwrap();
        for pattern in 0..64 {
            let input: Vec<_> = refs[..6]
                .iter()
                .enumerate()
                .map(|(i, s)| (s.as_str(), pattern & (1 << i) != 0))
                .collect();
            let a = evaluate(&old, &ls[0], &input);
            let b = evaluate(&m, &ls[0], &input);
            for p in m.ports.iter().filter(|p| p.dir == NlDir::Output) {
                assert_eq!(
                    a[&r(&p.name)],
                    b[&r(&p.name)],
                    "{variant:?} {} pattern {pattern}",
                    p.name
                );
            }
        }
    }
}

// SYNTH-REQ-0539: additional electrical corners repair load and reject incompatible cells.
#[test]
fn extra_corner_repairs_real_load_and_rejects_incompatible_cells() {
    let ls = libs();
    let lib = CellLib::build(&ls, &[]).unwrap();
    let mut corner = ls[0].clone();
    corner.name = "high-cap".into();
    corner.default_max_capacitance = Some(0.04);
    for cell in &mut corner.cells {
        for pin in &mut cell.pins {
            if pin.direction == rusteda_formats::liberty::Direction::Input {
                pin.capacitance *= 2.0;
                pin.rise_capacitance = None;
                pin.fall_capacitance = None;
                pin.rise_capacitance_range = None;
                pin.fall_capacitance_range = None;
            }
        }
    }
    let original = fanout_design();
    let opts = Options {
        period_ns: 100.0,
        ..Options::default()
    };
    let mut m = original.clone();
    let r = refine_corners(
        &mut m,
        &ls,
        &lib,
        &opts,
        std::slice::from_ref(&corner),
        Ctx::NONE,
    )
    .unwrap();
    assert_eq!(r.corner_rules.len(), 1);
    assert_eq!(r.corner_rules[0].1.capacitance_nets, 0, "{r:?}");
    assert!(r.rule_buffers + r.rule_clones > 0);
    for a in [false, true] {
        for b in [false, true] {
            let values = evaluate(
                &m,
                &ls[0],
                &[("a", a), ("b", b), ("clk", false), ("rst", false)],
            );
            for ff in m.instances.iter().filter(|i| i.name.starts_with("ff")) {
                let d = &ff.conns.iter().find(|(p, _)| p == "D").unwrap().1;
                assert_eq!(values[d], !(a & b));
            }
        }
    }
    corner
        .cells
        .iter_mut()
        .find(|c| c.name == "wt_nand2_x1")
        .unwrap()
        .area += 1.0;
    let mut m = original.clone();
    assert!(refine_corners(&mut m, &ls, &lib, &opts, &[corner], Ctx::NONE).is_err());
    assert_eq!(m, original);
}

#[test]
fn ordinary_state_inversions_can_fold_but_named_state_probes_survive() {
    let mut ls = libs();
    let mut cell = ls[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_nand2_x1")
        .unwrap()
        .clone();
    cell.name = "negative_input_nand".into();
    cell.area += 0.1;
    cell.pins
        .iter_mut()
        .find(|p| p.name == "Y")
        .unwrap()
        .function = Some(rusteda_formats::liberty::parse_function("!(!A&B)").unwrap());
    ls[0].cells.push(cell);
    let lib = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &lib);
    let original = NlModule {
        name: "m".into(),
        ports: vec![
            port("clk", NlDir::Input),
            port("rn", NlDir::Input),
            port("a", NlDir::Input),
            port("b", NlDir::Input),
            port("y", NlDir::Output),
        ],
        instances: vec![
            inst(
                "ff",
                "wt_dffr_x1",
                &[("CK", "clk"), ("RN", "rn"), ("D", "a"), ("Q", "q")],
            ),
            inst("neg", "wt_inv_x1", &[("A", "q"), ("Y", "n")]),
            inst("gate", "wt_nand2_x1", &[("A", "n"), ("B", "b"), ("Y", "y")]),
        ],
        ..Default::default()
    };
    for protected in ["q", "n"] {
        let mut m = original.clone();
        let probes = [r(protected)];
        let mut net = network::Network::with_state_aliases(&m, &model, Some(&probes)).unwrap();
        let folded = polarity::fold(&mut net, &model);
        if protected == "q" {
            assert_eq!(folded, 1);
        } else {
            assert_eq!(folded, 0);
        }
        net.write(&mut m, &model);
        for q in [false, true] {
            for b in [false, true] {
                let val = evaluate(&m, &ls[0], &[("q", q), ("b", b)]);
                assert_eq!(val[&r("y")], q || !b);
                assert!(val.contains_key(&r(protected)));
            }
        }
    }
}

#[test]
fn fanout_trees_use_the_minimum_number_of_buffers() {
    let ls = libs();
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    for width in [2usize, 3, 4, 8, 16] {
        for count in 2usize..=130 {
            let mut m = fanout_design();
            m.instances.truncate(3);
            for i in 0..count {
                m.instances.push(inst(
                    &format!("f{i}"),
                    "wt_dffr_x1",
                    &[
                        ("D", "d"),
                        ("CK", "cn"),
                        ("RN", "rn"),
                        ("Q", &format!("q{i}")),
                    ],
                ));
            }
            let mut net = network::Network::new(&m, &model).unwrap();
            let n = net.refs.iter().position(|value| value == &r("d")).unwrap();
            let expected = count.saturating_sub(width).div_ceil(width - 1);
            assert_eq!(network::tree_size(count, width, false), expected);
            let added = net.split_fanout(n, model.buffers[0], width, false, &model);
            assert_eq!(added, expected, "sinks={count}, width={width}");
            net.rebuild(&model).unwrap();
            assert!(net.nodes[n].sinks.len() <= width);
            for g in &net.gates[m.instances.len()..] {
                assert!(net.nodes[g.outputs[0].1].sinks.len() <= width);
            }
            if count == 10 && width == 8 {
                net.write(&mut m, &model);
                for a in [false, true] {
                    for b in [false, true] {
                        let values = evaluate(&m, &ls[0], &[("a", a), ("b", b)]);
                        for ff in m.instances.iter().filter(|i| i.cell == "wt_dffr_x1") {
                            let d = &ff.conns.iter().find(|(p, _)| p == "D").unwrap().1;
                            assert_eq!(values[d], !(a & b));
                        }
                    }
                }
            }
        }
    }
}

// SYNTH-REQ-0542: phase absorption prices a shared inverter and all affected consumers together.
#[test]
fn shared_inverter_consumers_are_priced_together() {
    let mut ls = libs();
    let inv_area = ls[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_inv_x1")
        .unwrap()
        .area;
    let mut negative = ls[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_nand2_x1")
        .unwrap()
        .clone();
    negative.name = "shared_negative_nand".into();
    negative.area += inv_area * 0.4;
    negative
        .pins
        .iter_mut()
        .find(|p| p.name == "Y")
        .unwrap()
        .function = Some(rusteda_formats::liberty::parse_function("!(!A&B)").unwrap());
    ls[0].cells.push(negative);
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    let mut m = NlModule {
        name: "shared".into(),
        ports: vec![
            port("a", NlDir::Input),
            port("b", NlDir::Input),
            port("c", NlDir::Input),
            port("y", NlDir::Output),
            port("z", NlDir::Output),
        ],
        instances: vec![
            inst("neg", "wt_inv_x1", &[("A", "a"), ("Y", "an")]),
            inst(
                "left",
                "wt_nand2_x1",
                &[("A", "an"), ("B", "b"), ("Y", "y")],
            ),
            inst(
                "right",
                "wt_nand2_x1",
                &[("A", "an"), ("B", "c"), ("Y", "z")],
            ),
        ],
        ..Default::default()
    };
    let mut net = network::Network::new(&m, &model).unwrap();
    let before = net.area(&model);
    assert_eq!(polarity::fold(&mut net, &model), 1);
    assert!(net.area(&model) < before);
    net.write(&mut m, &model);
    for a in [false, true] {
        for b in [false, true] {
            for c in [false, true] {
                let v = evaluate(&m, &ls[0], &[("a", a), ("b", b), ("c", c)]);
                assert_eq!(v[&r("y")], a || !b);
                assert_eq!(v[&r("z")], a || !c);
            }
        }
    }
}

// SYNTH-REQ-0546: electrical models pre-resolve pin limits, fanout weights, arc senses, and table layouts.
#[test]
fn rule_timing_has_identical_forward_values() {
    let ls = libs();
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    let net = network::Network::new(&fanout_design(), &model).unwrap();
    let opts = Options::default();
    let full = timing::Timing::new(&net, &model, &opts);
    let rules = timing::Timing::rules(&net, &model, &opts);
    assert_eq!(full.load, rules.load);
    for (a, b) in full.value.iter().zip(&rules.value) {
        assert_eq!(a.arrival, b.arrival);
        assert_eq!(a.slew, b.slew);
    }
    assert_eq!(
        serde_json::to_value(rules::report(&net, &model, &full)).unwrap(),
        serde_json::to_value(rules::report(&net, &model, &rules)).unwrap()
    );
}

#[test]
fn cheap_replication_preserves_data_and_checks_upstream_headroom() {
    let mut ls = libs();
    ls[0].default_max_fanout = Some(8.0);
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    let opts = Options {
        period_ns: 100.0,
        ..Default::default()
    };
    for primary in [true, false] {
        let mut m = fanout_design();
        m.instances.truncate(13);
        m.instances[0] = inst(
            "data",
            "wt_inv_x1",
            &[("A", if primary { "a" } else { "source" }), ("Y", "d")],
        );
        if !primary {
            m.instances
                .push(inst("parent", "wt_inv_x1", &[("A", "a"), ("Y", "source")]));
            for i in 0..7 {
                m.instances.push(inst(
                    &format!("other{i}"),
                    "wt_inv_x1",
                    &[("A", "source"), ("Y", &format!("unused{i}"))],
                ));
            }
        }
        let mut net = network::Network::new(&m, &model).unwrap();
        let mut report = Report::default();
        replicate::run(&mut net, &model, &[], &opts, &mut report, Ctx::NONE).unwrap();
        assert_eq!(report.rule_clones, usize::from(primary));
        if primary {
            // Replication repairs the data net and leaves control nets alone: the clock net is
            // for clock-tree construction, the reset net for rule repair (SYNTH-REQ-0552).
            let t = timing::Timing::rules(&net, &model, &opts);
            for (n, node) in net.nodes.iter().enumerate() {
                let fanout = rules::Limits::for_node(&net, &model, n).violations(&t, n)[2];
                assert_eq!(
                    fanout,
                    node.protected && node.sinks.len() > 8,
                    "{:?}",
                    net.refs[n]
                );
            }
            net.write(&mut m, &model);
            for a in [false, true] {
                let values = evaluate(&m, &ls[0], &[("a", a)]);
                for ff in m.instances.iter().filter(|i| i.cell == "wt_dffr_x1") {
                    let d = &ff.conns.iter().find(|(p, _)| p == "D").unwrap().1;
                    assert_eq!(values[d], !a);
                }
            }
        }
    }
}

#[test]
fn sequential_drive_sizing_keeps_the_connected_output_polarity() {
    let mut ls = libs();
    let mut fast = ls[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_dffr_x1")
        .unwrap()
        .clone();
    fast.name = "faster_ff".into();
    fast.area += 1.0;
    for p in &mut fast.pins {
        for arc in &mut p.arcs {
            if arc.related_pin == "CK" {
                for table in [
                    &mut arc.cell_rise,
                    &mut arc.cell_fall,
                    &mut arc.rise_transition,
                    &mut arc.fall_transition,
                ]
                .into_iter()
                .flatten()
                {
                    for v in &mut table.values {
                        *v *= 0.1;
                    }
                }
            }
        }
    }
    ls[0].cells.push(fast);
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    for pin in ["Q", "QN"] {
        let mut m = NlModule {
            name: "sequential".into(),
            ports: vec![
                port("a", NlDir::Input),
                port("clk", NlDir::Input),
                port("rn", NlDir::Input),
                port("y", NlDir::Output),
            ],
            instances: vec![inst(
                "ff",
                "wt_dffr_x1",
                &[("D", "a"), ("CK", "clk"), ("RN", "rn"), (pin, "y")],
            )],
            ..Default::default()
        };
        let mut net = network::Network::new(&m, &model).unwrap();
        let opts = Options {
            period_ns: 0.1,
            margin_ns: 0.001,
            output_load_pf: 0.1,
            area_limit: Some(7.0),
            ..Default::default()
        };
        let before = timing::Timing::new(&net, &model, &opts).worst_slack;
        let mut report = Report::default();
        resize::run(&mut net, &model, &[], &opts, &mut report, Ctx::NONE).unwrap();
        assert_eq!(report.resized_cells, 1);
        assert!(report.estimated_slack_ns > before + 0.01);
        net.write(&mut m, &model);
        assert_eq!(m.instances[0].cell, "faster_ff");
        assert!(m.instances[0].conns.contains(&(pin.into(), r("y"))));
        assert!(
            !m.instances[0]
                .conns
                .iter()
                .any(|(p, _)| p == if pin == "Q" { "QN" } else { "Q" })
        );
    }
}

#[test]
fn shared_output_phase_can_change_with_its_consumers() {
    let mut ls = libs();
    for c in &mut ls[0].cells {
        if c.name == "wt_and2_x1" {
            c.area = 3.0;
        } else if c.name == "wt_nand2_x1" {
            c.area = 1.0;
        }
    }
    let mut cell = ls[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_nand2_x1")
        .unwrap()
        .clone();
    cell.name = "coupled_negative".into();
    cell.area = 1.4;
    cell.pins
        .iter_mut()
        .find(|p| p.name == "Y")
        .unwrap()
        .function = Some(rusteda_formats::liberty::parse_function("!(!A&B)").unwrap());
    ls[0].cells.push(cell);
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    let mut m = NlModule {
        name: "coupled".into(),
        ports: vec![
            port("a", NlDir::Input),
            port("b", NlDir::Input),
            port("c", NlDir::Input),
            port("d", NlDir::Input),
            port("yn", NlDir::Output),
            port("y", NlDir::Output),
            port("z", NlDir::Output),
        ],
        instances: vec![
            inst("root", "wt_and2_x1", &[("A", "a"), ("B", "b"), ("Y", "p")]),
            inst("neg", "wt_inv_x1", &[("A", "p"), ("Y", "yn")]),
            inst("left", "wt_nand2_x1", &[("A", "p"), ("B", "c"), ("Y", "y")]),
            inst(
                "right",
                "wt_nand2_x1",
                &[("A", "p"), ("B", "d"), ("Y", "z")],
            ),
        ],
        ..Default::default()
    };
    let old = m.clone();
    let mut net = network::Network::new(&m, &model).unwrap();
    let area = net.area(&model);
    assert_eq!(polarity::fold(&mut net, &model), 1);
    assert!(net.area(&model) < area - 1.0);
    net.write(&mut m, &model);
    for pattern in 0..16 {
        let values = [
            ("a", pattern & 1 != 0),
            ("b", pattern & 2 != 0),
            ("c", pattern & 4 != 0),
            ("d", pattern & 8 != 0),
        ];
        let a = evaluate(&old, &ls[0], &values);
        let b = evaluate(&m, &ls[0], &values);
        for p in ["yn", "y", "z"] {
            assert_eq!(a[&r(p)], b[&r(p)]);
        }
    }
}

// SYNTH-REQ-0543: sizing and symmetric-pin reassignment check pin limits and arrival.
#[test]
fn symmetric_pin_assignment_improves_actual_arrival_without_area() {
    let mut ls = libs();
    let c = ls[0]
        .cells
        .iter_mut()
        .find(|c| c.name == "wt_nand2_x1")
        .unwrap();
    for pin in &mut c.pins {
        for arc in &mut pin.arcs {
            if arc.related_pin == "A" {
                for table in [&mut arc.cell_rise, &mut arc.cell_fall]
                    .into_iter()
                    .flatten()
                {
                    for v in &mut table.values {
                        *v += 0.2;
                    }
                }
            }
        }
    }
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    let mut m = NlModule {
        name: "pins".into(),
        ports: vec![
            port("a", NlDir::Input),
            port("b", NlDir::Input),
            port("y", NlDir::Output),
        ],
        instances: vec![
            inst("late", "wt_buf_x1", &[("A", "a"), ("Y", "late")]),
            inst(
                "gate",
                "wt_nand2_x1",
                &[("A", "late"), ("B", "b"), ("Y", "y")],
            ),
        ],
        ..Default::default()
    };
    let mut net = network::Network::new(&m, &model).unwrap();
    let area = net.area(&model);
    let opts = Options {
        period_ns: 0.1,
        ..Default::default()
    };
    let before = timing::Timing::new(&net, &model, &opts).worst_slack;
    let mut report = Report::default();
    pins::run(&mut net, &model, &[], &opts, &mut report, Ctx::NONE).unwrap();
    assert!(report.pin_swaps > 0);
    assert_eq!(area, net.area(&model));
    assert!(timing::Timing::new(&net, &model, &opts).worst_slack > before + 0.01);
    net.write(&mut m, &model);
    for a in [false, true] {
        for b in [false, true] {
            assert_eq!(
                evaluate(&m, &ls[0], &[("a", a), ("b", b)])[&r("y")],
                !(a & b)
            );
        }
    }
}

// SYNTH-REQ-0545: small combinational windows may be replaced by an exact two-cell decomposition.
#[test]
fn two_cell_decompositions_are_exact_for_all_boundary_assignments() {
    let ls =
        vec![rusteda_formats::liberty::read_text(&library(Variant::Full), "two cells").unwrap()];
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    for n in 3usize..=6 {
        let bound = n.saturating_sub(3).max(2);
        let mut truth = 0u64;
        for x in 0..1usize << n {
            let inner = !(0..bound).all(|i| x & (1 << i) != 0);
            let value = !(inner && (bound..n).all(|i| x & (1 << i) != 0));
            truth |= u64::from(value) << x;
        }
        let recipe = decompose::find(&model, n, truth).expect("NAND composition is realizable");
        for x in 0..1usize << n {
            let bits = recipe
                .inner_inputs
                .iter()
                .enumerate()
                .fold(0, |v, (i, &p)| v | (((x >> p) & 1) << i));
            let inner = (model.cells[recipe.inner].truth.unwrap() >> bits) & 1;
            let bits = recipe.root_inputs.iter().enumerate().fold(0, |v, (i, &p)| {
                v | ((if p == 6 { inner } else { ((x >> p) & 1) as u64 }) << i)
            });
            let value = (model.cells[recipe.root].truth.unwrap() >> bits) & 1;
            assert_eq!(value, (truth >> x) & 1, "n={n}, x={x}, {recipe:?}");
        }
    }
}

// SYNTH-REQ-0548: timing-driven load isolation may insert a buffer even for a low-fanout net.
#[test]
fn a_timing_buffer_can_help_below_the_fanout_threshold() {
    let mut ls = libs();
    for cell in &mut ls[0].cells {
        for pin in &mut cell.pins {
            if pin.name == "D" {
                pin.capacitance = 0.1;
                pin.rise_capacitance = None;
                pin.fall_capacitance = None;
                pin.rise_capacitance_range = None;
                pin.fall_capacitance_range = None;
            }
        }
    }
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    for count in [1usize, 4] {
        let mut m = fanout_design();
        m.instances.truncate(3 + count);
        let mut net = network::Network::new(&m, &model).unwrap();
        let opts = Options {
            period_ns: 0.5,
            buffer_fanout: 8,
            area_limit: Some(net.area(&model) + 2.0),
            ..Default::default()
        };
        let before = timing::Timing::new(&net, &model, &opts).worst_slack;
        let mut report = Report::default();
        load_buffer::run(&mut net, &model, &[], &opts, &mut report, Ctx::NONE).unwrap();
        assert_eq!(report.load_buffers, 1);
        assert!(timing::Timing::new(&net, &model, &opts).worst_slack > before + 0.05);
        assert!(net.area(&model) <= opts.area_limit.unwrap());
        net.write(&mut m, &model);
        for a in [false, true] {
            for b in [false, true] {
                let values = evaluate(&m, &ls[0], &[("a", a), ("b", b)]);
                for ff in m.instances.iter().filter(|i| i.cell == "wt_dffr_x1") {
                    let d = &ff.conns.iter().find(|(p, _)| p == "D").unwrap().1;
                    assert_eq!(values[d], !(a & b));
                }
            }
        }
    }
}

mod pin_groups;
