// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use super::*;

// SYNTH-REQ-0549: pin reassignment may exchange complete equal-sized symmetry groups.
#[test]
fn compound_pin_groups_improve_timing_with_repeated_inputs_and_preserve_truth() {
    let mut ls = libs();
    let cell = ls[0]
        .cells
        .iter_mut()
        .find(|c| c.name == "wt_aoi22_x1")
        .unwrap();
    for pin in &mut cell.pins {
        for arc in &mut pin.arcs {
            if ["A1", "A2"].contains(&arc.related_pin.as_str()) {
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
    for repeated in [false, true] {
        let mut m = NlModule {
            name: "compound_pins".into(),
            ports: ["a", "b", "c", "d"]
                .iter()
                .map(|n| port(n, NlDir::Input))
                .chain([port("y", NlDir::Output)])
                .collect(),
            instances: vec![
                inst("late_a", "wt_buf_x1", &[("A", "a"), ("Y", "la")]),
                inst("late_b", "wt_buf_x1", &[("A", "b"), ("Y", "lb")]),
                inst(
                    "gate",
                    "wt_aoi22_x1",
                    &[
                        ("A1", "la"),
                        ("A2", if repeated { "la" } else { "lb" }),
                        ("B1", "c"),
                        ("B2", "d"),
                        ("Y", "y"),
                    ],
                ),
            ],
            ..Default::default()
        };
        let old = m.clone();
        let mut net = network::Network::new(&m, &model).unwrap();
        let opts = Options {
            period_ns: 0.1,
            ..Default::default()
        };
        let before = timing::Timing::new(&net, &model, &opts).worst_slack;
        let area = net.area(&model);
        let mut report = Report::default();
        pins::run(&mut net, &model, &[], &opts, &mut report, Ctx::NONE).unwrap();
        assert!(report.pin_swaps > 0);
        assert_eq!(area, net.area(&model));
        assert!(timing::Timing::new(&net, &model, &opts).worst_slack > before + 0.01);
        net.write(&mut m, &model);
        for bits in 0..16 {
            let values: Vec<_> = ["a", "b", "c", "d"]
                .iter()
                .enumerate()
                .map(|(i, n)| (*n, bits & (1 << i) != 0))
                .collect();
            assert_eq!(
                evaluate(&old, &ls[0], &values)[&r("y")],
                evaluate(&m, &ls[0], &values)[&r("y")]
            );
        }
    }
}
