// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Rule precedence over the area limit and control nets without a clock (SYNTH-REQ-0552).

use super::tests::{evaluate, fanout_design, inst, libs, port, r};
use super::*;
use rustc_hash::FxHashMap;
use rusteda_formats::verilog::write::{NlDir, NlInstance, NlRef};

// SYNTH-REQ-0552: data-net rules take precedence over the area limit, and control nets that
// carry no clock follow them: an asynchronous reset net and a latch enable that comes from a
// flop are buffered, while the clock net stays as it is for clock-tree construction.
#[test]
fn rules_precede_the_area_limit_and_cover_control_nets_without_a_clock() {
    let mut ls = libs();
    ls[0].default_max_fanout = Some(8.0);
    let lib = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &lib);
    let mut m = fanout_design();
    m.ports.push(port("e", NlDir::Input));
    m.wires.push(("en_q".into(), None));
    m.instances.push(inst(
        "enable",
        "wt_dffr_x1",
        &[("D", "e"), ("CK", "cn"), ("RN", "rn"), ("Q", "en_q")],
    ));
    for i in 0..12 {
        let q = format!("held{i}");
        m.wires.push((q.clone(), None));
        m.instances.push(inst(
            &format!("lat{i}"),
            "wt_lat_x1",
            &[("D", "d"), ("G", "en_q"), ("Q", &q)],
        ));
    }
    let before = network::Network::new(&m, &model).unwrap();
    let opts = Options {
        period_ns: 20.0,
        area_limit: Some(before.area(&model)),
        ..Options::default()
    };
    let old = m.clone();
    let rr = refine(&mut m, &ls, &lib, &opts, Ctx::NONE).unwrap();
    assert!(rr.estimated_data_rules_met, "{rr:?}");
    assert!(!rr.area_limit_met && rr.area_after > rr.area_before);
    assert_eq!(rr.electrical_rules.fanout_nets, 0);
    assert_eq!(rr.electrical_rules.protected_nets, 1, "{rr:?}");
    assert_eq!(
        m.instances.iter().find(|i| i.name == "clock"),
        old.instances.iter().find(|i| i.name == "clock")
    );
    let pin = |i: &NlInstance, p: &str| i.conns.iter().find(|(n, _)| n == p).unwrap().1.clone();
    let mut per_net: FxHashMap<NlRef, usize> = FxHashMap::default();
    for i in &m.instances {
        if i.cell == "wt_dffr_x1" {
            assert_eq!(pin(i, "CK"), r("cn"));
            *per_net.entry(pin(i, "RN")).or_default() += 1;
        }
        if i.cell == "wt_lat_x1" {
            *per_net.entry(pin(i, "G")).or_default() += 1;
        }
    }
    assert!(per_net.values().all(|&n| n <= 8), "{per_net:?}");
    for (rst, en) in [(false, false), (false, true), (true, false), (true, true)] {
        let v = evaluate(
            &m,
            &ls[0],
            &[
                ("a", true),
                ("b", rst),
                ("clk", false),
                ("rst", rst),
                ("en_q", en),
            ],
        );
        for i in &m.instances {
            if i.cell == "wt_dffr_x1" {
                assert_eq!(v[&pin(i, "RN")], !rst);
            }
            if i.cell == "wt_lat_x1" {
                assert_eq!(v[&pin(i, "G")], en);
                assert_eq!(v[&pin(i, "D")], !rst);
            }
        }
    }
}

// SYNTH-REQ-0541, 0547: over-limit refinement compacts De Morgan two-cell muxes using dual
// state outputs or shared inversions and repacks AOI22 trees into AOI222 leaves.
#[test]
fn over_limit_repacks_demorgan_mux_and_sop_trees_preserving_equivalence() {
    use rusteda_formats::liberty::{Direction, Pin};

    let mut ls = libs();
    let template = ls[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_aoi22_x1")
        .unwrap()
        .clone();
    let make_comb = |name: &str, area: f64, inputs: &[&str], expr: &str| {
        let mut c = template.clone();
        c.name = name.into();
        c.area = area;
        let in_pin = c
            .pins
            .iter()
            .find(|p| p.direction == Direction::Input)
            .unwrap()
            .clone();
        let mut out_pin = c
            .pins
            .iter()
            .find(|p| p.direction == Direction::Output)
            .unwrap()
            .clone();
        out_pin.function = Some(rusteda_formats::liberty::parse_function(expr).unwrap());
        out_pin.arcs.truncate(inputs.len());
        for (arc, &pin_name) in out_pin.arcs.iter_mut().zip(inputs) {
            arc.related_pin = pin_name.into();
        }
        c.pins = inputs
            .iter()
            .map(|&n| Pin {
                name: n.into(),
                ..in_pin.clone()
            })
            .chain(std::iter::once(out_pin))
            .collect();
        c
    };
    ls[0].cells.push(make_comb(
        "wt_aoi222_x1",
        2.6,
        &["A1", "A2", "B1", "B2", "C1", "C2"],
        "!((A1&A2)|(B1&B2)|(C1&C2))",
    ));
    ls[0].cells.push(make_comb(
        "wt_nand4_x1",
        1.8,
        &["A", "B", "C", "D"],
        "!(A&B&C&D)",
    ));
    ls[0].cells.push(make_comb(
        "wt_oai31_x1",
        1.8,
        &["A1", "A2", "A3", "B1"],
        "!((A1|A2|A3)&B1)",
    ));
    ls[0].cells.push(make_comb(
        "wt_oai41_x1",
        2.2,
        &["A1", "A2", "A3", "A4", "B1"],
        "!((A1|A2|A3|A4)&B1)",
    ));
    ls[0].cells.push(make_comb(
        "wt_ao221_x1",
        2.6,
        &["A1", "A2", "B1", "B2", "C1"],
        "(A1&A2)|(B1&B2)|C1",
    ));
    let mut single_ff = ls[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_dffr_x1")
        .unwrap()
        .clone();
    single_ff.name = "wt_dffr_qonly_x1".into();
    single_ff.area = 5.0;
    single_ff.pins.retain(|p| p.name != "QN");
    ls[0].cells.push(single_ff);
    ls[0]
        .cells
        .iter_mut()
        .find(|c| c.name == "wt_dffr_x1")
        .unwrap()
        .area = 5.2;

    let eligible = CellLib::build(&ls, &[]).unwrap();
    let model = model::Model::new(&ls, &eligible);
    let mut m = NlModule {
        name: "repack_test".into(),
        ports: ["clk", "rn", "en", "x0", "x1", "x2", "x3", "b1"]
            .into_iter()
            .map(|n| port(n, NlDir::Input))
            .chain(["y_mux0", "y_mux1", "y_sop1", "y_sop2"].map(|n| port(n, NlDir::Output)))
            .collect(),
        instances: vec![inst("en_buf", "wt_buf_x1", &[("A", "en"), ("Y", "en_b")])],
        ..Default::default()
    };
    for i in 0..4 {
        let q = format!("q{i}");
        let n = format!("nor{i}");
        let d = format!("d{i}");
        m.instances.push(inst(
            &format!("ff{i}"),
            "wt_dffr_qonly_x1",
            &[("CK", "clk"), ("RN", "rn"), ("D", &d), ("Q", &q)],
        ));
        m.instances.push(inst(
            &format!("hold{i}"),
            "wt_nor2_x1",
            &[("A", "en_b"), ("B", &q), ("Y", &n)],
        ));
        m.instances.push(inst(
            &format!("mux{i}"),
            "wt_aoi21_x1",
            &[("A1", "x0"), ("A2", "x1"), ("B", &n), ("Y", &d)],
        ));
    }
    m.assigns.push((r("y_mux0"), r("d0")));
    m.assigns.push((r("y_mux1"), r("d1")));

    for i in 0..3 {
        let out = format!("l1_{i}");
        m.instances.push(inst(
            &format!("s1_a22_{i}"),
            "wt_aoi22_x1",
            &[
                ("A1", "x0"),
                ("A2", "x1"),
                ("B1", "x2"),
                ("B2", "x3"),
                ("Y", &out),
            ],
        ));
    }
    m.instances.push(inst(
        "s1_root",
        "wt_nand4_x1",
        &[
            ("A", "l1_0"),
            ("B", "l1_1"),
            ("C", "l1_2"),
            ("D", "x0"),
            ("Y", "y_sop1"),
        ],
    ));

    for b in 0..4 {
        for j in 0..4 {
            let out = format!("l2_{b}_{j}");
            m.instances.push(inst(
                &format!("s2_a22_{b}_{j}"),
                "wt_aoi22_x1",
                &[
                    ("A1", "x0"),
                    ("A2", "x1"),
                    ("B1", "x2"),
                    ("B2", "x3"),
                    ("Y", &out),
                ],
            ));
        }
        let mid = format!("m2_{b}");
        if b < 3 {
            m.instances.push(inst(
                &format!("s2_n4_{b}"),
                "wt_nand4_x1",
                &[
                    ("A", &format!("l2_{b}_0")),
                    ("B", &format!("l2_{b}_1")),
                    ("C", &format!("l2_{b}_2")),
                    ("D", &format!("l2_{b}_3")),
                    ("Y", &mid),
                ],
            ));
        } else {
            m.instances.push(inst(
                "s2_n4_3_inner",
                "wt_nand4_x1",
                &[
                    ("A", "l2_3_0"),
                    ("B", "l2_3_1"),
                    ("C", "l2_3_2"),
                    ("D", "l2_3_3"),
                    ("Y", "m2_3_inner"),
                ],
            ));
            m.instances.push(inst(
                "s2_ao221",
                "wt_ao221_x1",
                &[
                    ("A1", "x0"),
                    ("A2", "x1"),
                    ("B1", "x2"),
                    ("B2", "x3"),
                    ("C1", "m2_3_inner"),
                    ("Y", &mid),
                ],
            ));
        }
    }
    m.instances.push(inst(
        "s2_root",
        "wt_oai41_x1",
        &[
            ("A1", "m2_0"),
            ("A2", "m2_1"),
            ("A3", "m2_2"),
            ("A4", "m2_3"),
            ("B1", "b1"),
            ("Y", "y_sop2"),
        ],
    ));

    let old = m.clone();
    let mut net = network::Network::new(&m, &model).unwrap();
    let area_before = net.area(&model);
    let mut report = Report::default();
    let count = repack::recover(&mut net, &model, &mut report, Ctx::NONE).unwrap();
    assert!(count >= 6, "expected mux and SOP rewrites, got {count}");
    assert!(net.area(&model) < area_before - 1.0);
    net.write(&mut m, &model);
    for bits in 0..64usize {
        let given = [
            ("en", bits & 1 != 0),
            ("x0", bits & 2 != 0),
            ("x1", bits & 4 != 0),
            ("x2", bits & 8 != 0),
            ("x3", bits & 16 != 0),
            ("b1", bits & 32 != 0),
            ("q0", bits & 3 == 1),
            ("q1", bits & 5 == 0),
            ("q2", false),
            ("q3", true),
        ];
        let mut expected = evaluate(&old, &ls[0], &given);
        let mut extra = given.to_vec();
        for i in &m.instances {
            if i.cell == "wt_dffr_x1" {
                let q = i.conns.iter().find(|(p, _)| p == "Q").unwrap().1.clone();
                if let Some((_, NlRef::Net(qn_name))) = i.conns.iter().find(|(p, _)| p == "QN") {
                    let q_val = expected[&q];
                    extra.push((Box::leak(qn_name.clone().into_boxed_str()), !q_val));
                }
            }
        }
        let got = evaluate(&m, &ls[0], &extra);
        for out in ["y_mux0", "y_mux1", "y_sop1", "y_sop2"] {
            assert_eq!(
                got[&r(out)],
                expected.remove(&r(out)).unwrap(),
                "{out} bits={bits}"
            );
        }
    }
    let mut full = old;
    let rr = refine(
        &mut full,
        &ls,
        &eligible,
        &Options {
            period_ns: 20.0,
            area_limit: Some(area_before - 5.0),
            ..Options::default()
        },
        Ctx::NONE,
    )
    .unwrap();
    assert!(rr.area_after < area_before - 1.0, "{rr:?}");
}
