// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! CHECK-REQ-0021: supplemental observations must not omit or assume original obligations.
//! <10 nodes per graph, <1 MiB export, two test workers; no PDK re-parsing.

use std::sync::atomic::{AtomicU64, Ordering};

use rusteda_core::netlist::{Conn, GateNetlist, PortDir};
use rustsys_check::netlist::frozen::Limits;
use rustsys_check::rtl::ObservationProbe;
use rustsys_check::{CheckOptions, Input, ObligationExport, Verdict, check_frozen};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn options(width: usize) -> (CheckOptions, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "rustsys-observation-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let options = CheckOptions {
        rtl: Input::Texts(vec![(
            "m.sv".into(),
            "module m(input [1:0] a,output y); wire [1:0] tap; assign tap=a; assign y=a[0]; endmodule".into(),
        )]),
        observation_probes: vec![ObservationProbe { name: "tap".into(), offset: 1, width }],
        obligation_export: Some(ObligationExport::new(path.clone())),
        ..CheckOptions::default()
    };
    (options, path)
}

fn gates(bad: bool, include_probe: bool) -> GateNetlist {
    let mut graph = GateNetlist::new("m").unwrap();
    graph.add_port("a", PortDir::Input, Some((1, 0))).unwrap();
    graph.add_port("y", PortDir::Output, None).unwrap();
    graph
        .add_assign(
            graph.find_net("y", None).unwrap(),
            Conn::Net(graph.find_net("a", Some(0)).unwrap()),
        )
        .unwrap();
    if include_probe {
        for bit in 0..2 {
            let net = graph.add_net(Some("tap"), Some(bit)).unwrap();
            graph
                .add_assign(
                    net,
                    if bad && bit == 1 {
                        Conn::Const(false)
                    } else {
                        Conn::Net(graph.find_net("a", Some(bit)).unwrap())
                    },
                )
                .unwrap();
        }
    }
    graph
}

#[test]
fn export_keeps_original_output_and_unconditional_observation() {
    for bad in [false, true] {
        let (options, path) = options(1);
        let report = check_frozen(&options, &[], &gates(bad, true).freeze(), Limits::default());
        assert_eq!(report.verdict, Verdict::Undecided, "{report:?}");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["output_bits"], 1);
        assert_eq!(manifest["obligation_kinds"]["output"], 1);
        assert_eq!(manifest["obligation_kinds"]["observation"], 1);
        assert_eq!(manifest["observations"][0]["first_output"], 1);
        assert_eq!(manifest["sides"][0]["inputs"], 2);
        assert_eq!(manifest["sides"][0]["outputs"], 2);
        assert_eq!(manifest["reset_assumptions"], serde_json::json!([]));
        let gold = std::fs::read_to_string(path.join("gold.aig")).unwrap();
        let gate = std::fs::read_to_string(path.join("gate.aig")).unwrap();
        assert_eq!(gold, "aig 2 2 0 2 0\n2\n4\n");
        assert_eq!(gate, if bad { "aig 2 2 0 2 0\n2\n0\n" } else { &gold });
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn unavailable_probe_and_invalid_slice_do_not_export() {
    for (width, present) in [(1, false), (2, true)] {
        let (options, path) = options(width);
        let report = check_frozen(
            &options,
            &[],
            &gates(false, present).freeze(),
            Limits::default(),
        );
        assert_ne!(report.verdict, Verdict::Equivalent);
        assert!(!path.join("manifest.json").exists());
    }
}

#[test]
fn observation_request_count_is_bounded() {
    let (mut options, path) = options(1);
    options.observation_probes = vec![options.observation_probes[0].clone(); 65];
    let report = check_frozen(
        &options,
        &[],
        &gates(false, true).freeze(),
        Limits::default(),
    );
    assert_ne!(report.verdict, Verdict::Equivalent);
    assert!(!path.join("manifest.json").exists());
}
