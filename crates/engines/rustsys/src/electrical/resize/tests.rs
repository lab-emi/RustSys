// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use super::*;
use crate::electrical::tests::{inst, libs, port};
use crate::map::library::CellLib;
use rusteda_formats::verilog::write::{NlDir, NlModule};

// SYNTH-REQ-0556: accumulated path delay and constant cells survive area recovery.
#[test]
fn recovery_rechecks_accumulated_path_delay() {
    let mut ls = libs();
    let slow = ls[0]
        .cells
        .iter_mut()
        .find(|c| c.name == "wt_buf_x1")
        .unwrap();
    slow.area = 1.0;
    for pin in &mut slow.pins {
        for arc in &mut pin.arcs {
            for table in [&mut arc.cell_rise, &mut arc.cell_fall]
                .into_iter()
                .flatten()
            {
                table.values.fill(0.2);
            }
        }
    }
    let mut fast = slow.clone();
    fast.name = "fast_buffer".into();
    fast.area = 2.0;
    for pin in &mut fast.pins {
        for arc in &mut pin.arcs {
            for table in [&mut arc.cell_rise, &mut arc.cell_fall]
                .into_iter()
                .flatten()
            {
                table.values.fill(0.01);
            }
        }
    }
    ls[0].cells.push(fast);
    let mut constant = ls[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_buf_x1")
        .unwrap()
        .clone();
    constant.name = "constant_one".into();
    constant.area = 0.5;
    constant.pins.retain(|p| p.function.is_some());
    constant.pins[0].function = Some(rusteda_formats::liberty::Func::Const(true));
    constant.pins[0].arcs.clear();
    ls[0].cells.insert(0, constant);
    let eligible = CellLib::build(&ls, &[]).unwrap();
    let lib = Model::new(&ls, &eligible);
    let module = NlModule {
        name: "chain".into(),
        ports: vec![port("a", NlDir::Input), port("y", NlDir::Output)],
        instances: vec![
            inst("tie", "constant_one", &[("Y", "one")]),
            inst("a_buf", "fast_buffer", &[("A", "a"), ("Y", "n")]),
            inst("y_buf", "fast_buffer", &[("A", "n"), ("Y", "y")]),
        ],
        ..Default::default()
    };
    for limit in [2.5, 3.5] {
        let mut net = Network::new(&module, &lib).unwrap();
        let opts = Options {
            period_ns: 0.3,
            margin_ns: 0.0,
            area_limit: Some(limit),
            ..Default::default()
        };
        recover_area(&mut net, &lib, &[], &opts, Ctx::NONE).unwrap();
        assert_eq!(net.area(&lib), 3.5);
        assert!(Timing::new(&net, &lib, &opts).worst_slack >= 0.0);
    }
}
