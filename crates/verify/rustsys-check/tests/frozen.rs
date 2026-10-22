// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Direct table ingestion must retain complete state and port obligations.
//! Each graph has <20 nodes; tests share one immutable small Liberty library.

use rusteda_core::netlist::{Conn, GateNetlist, PinDir, PortDir};
use rusteda_formats::liberty::Library;
use rustsys_check::netlist::frozen::Limits;
use rustsys_check::{CheckOptions, Input, Verdict, check_frozen};
use std::sync::OnceLock;

fn library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        rusteda_formats::liberty::read_text(
            &rustsys_fixtures::liberty::library(rustsys_fixtures::liberty::Variant::Full),
            "fixture",
        )
        .unwrap()
    })
}

fn sequential(bad: bool) -> GateNetlist {
    let mut g = GateNetlist::new("m").unwrap();
    for (name, dir) in [
        ("clk", PortDir::Input),
        ("rn", PortDir::Input),
        ("d", PortDir::Input),
        ("q", PortDir::Output),
    ] {
        g.add_port(name, dir, None).unwrap();
    }
    let cell = g
        .add_cell_type(
            "wtf_dffr_1",
            &[
                ("D", PinDir::Input),
                ("CLK", PinDir::Input),
                ("RESET_B", PinDir::Input),
                ("Q", PinDir::Output),
            ],
        )
        .unwrap();
    let inst = g.add_instance("state", cell).unwrap();
    for (pin, net) in [("D", "d"), ("CLK", "clk"), ("RESET_B", "rn"), ("Q", "q")] {
        let value = if pin == "D" && bad {
            Conn::Const(false)
        } else {
            Conn::Net(g.find_net(net, None).unwrap())
        };
        g.connect(inst, pin, value).unwrap();
    }
    g
}

#[test]
fn arbitrary_state_and_wrong_next_state_are_both_checked() {
    let options = CheckOptions { rtl:Input::Texts(vec![("m.v".into(),"module m(input clk,rn,d, output reg q); always @(posedge clk or negedge rn) if(!rn) q<=0; else q<=d; endmodule".into())]),..CheckOptions::default() };
    for bad in [false, true] {
        let g = sequential(bad);
        let report = check_frozen(
            &options,
            std::slice::from_ref(library()),
            &g.freeze(),
            Limits::default(),
        );
        assert_eq!(
            report.verdict,
            if bad {
                Verdict::NotEquivalent
            } else {
                Verdict::Equivalent
            },
            "{report:?}"
        );
        assert!(report.after_reset.is_empty());
        assert!(!report.uninitialized_dont_care);
        assert_eq!(report.state_pairs_checked, 1);
    }
}

#[test]
fn ascending_bus_aliases_and_budget_failures() {
    let mut g = GateNetlist::new("m").unwrap();
    g.add_port("a", PortDir::Input, Some((4, 7))).unwrap();
    g.add_port("y", PortDir::Output, Some((4, 7))).unwrap();
    for bit in 4..=7 {
        g.add_assign(
            g.find_net("y", Some(bit)).unwrap(),
            Conn::Net(g.find_net("a", Some(bit)).unwrap()),
        )
        .unwrap();
    }
    let options = CheckOptions {
        rtl: Input::Texts(vec![(
            "m.v".into(),
            "module m(input [4:7] a, output [4:7] y); assign y=a; endmodule".into(),
        )]),
        ..CheckOptions::default()
    };
    let frozen = g.freeze();
    let report = check_frozen(&options, &[], &frozen, Limits::default());
    assert_eq!(report.verdict, Verdict::Equivalent, "{report:?}");
    let report = check_frozen(
        &options,
        &[],
        &frozen,
        Limits {
            max_nets: 1,
            ..Limits::default()
        },
    );
    assert_eq!(report.verdict, Verdict::Undecided, "{report:?}");
    assert!(report.undecided.iter().any(|s| s.contains("C0005")));
}

fn anonymous_graph(fault: usize, names: &[&str], exact_alias: bool) -> GateNetlist {
    let mut g = GateNetlist::new("m").unwrap();
    for (name, dir) in [
        ("clk", PortDir::Input),
        ("rn", PortDir::Input),
        ("d", PortDir::Input),
        ("q", PortDir::Output),
    ] {
        g.add_port(name, dir, None).unwrap();
    }
    let cell = g
        .add_cell_type(
            "wtf_dffr_1",
            &[
                ("D", PinDir::Input),
                ("CLK", PinDir::Input),
                ("RESET_B", PinDir::Input),
                ("Q", PinDir::Output),
            ],
        )
        .unwrap();
    for (index, name) in names.iter().enumerate() {
        let state = g.add_net(Some(name), None).unwrap();
        let inst = g.add_instance(&format!("foreign{index}"), cell).unwrap();
        for (pin, wire) in [
            ("D", "d"),
            ("CLK", if fault == 2 { "rn" } else { "clk" }),
            ("RESET_B", "rn"),
        ] {
            let value = if pin == "D" && fault == 1 {
                Conn::Const(false)
            } else {
                Conn::Net(g.find_net(wire, None).unwrap())
            };
            g.connect(inst, pin, value).unwrap();
        }
        g.connect(inst, "Q", Conn::Net(state)).unwrap();
        if index == 0 {
            g.add_assign(g.find_net("q", None).unwrap(), Conn::Net(state))
                .unwrap();
            if exact_alias {
                let alias = g.add_net(Some("bank.state"), None).unwrap();
                g.add_assign(alias, Conn::Net(state)).unwrap();
            }
        }
    }
    g
}

const ANONYMOUS_RTL: &str = "module m(input clk,rn,d,output q); generate if(1) begin:bank
    reg state; always @(posedge clk or negedge rn) if(!rn) state<=0;
    else state<=d; assign q=state; end endgenerate endmodule";

/// CHECK-REQ-0031: names only propose a bijection. Data and clock differences
/// remain obligations with arbitrary initial state and no reset pulse.
#[test]
fn anonymous_generate_probes_keep_data_and_clock_checks() {
    let options = CheckOptions {
        rtl: Input::Texts(vec![("m.v".into(), ANONYMOUS_RTL.into())]),
        ..CheckOptions::default()
    };
    for fault in 0..3 {
        let g = anonymous_graph(fault, &["bank.genblk17.state"], false);
        let report = check_frozen(
            &options,
            std::slice::from_ref(library()),
            &g.freeze(),
            Limits::default(),
        );
        assert_eq!(
            report.verdict,
            if fault == 0 {
                Verdict::Equivalent
            } else {
                Verdict::NotEquivalent
            },
            "{report:?}"
        );
        assert_eq!(report.state_pairs_checked, 1, "{report:?}");
        assert!(report.after_reset.is_empty());
        assert!(!report.uninitialized_dont_care);
    }
}

/// Two independent implementation states must never collapse onto one RTL name.
#[test]
fn ambiguous_anonymous_scopes_fail_closed() {
    let g = anonymous_graph(0, &["bank.genblk17.state", "bank.genblk18.state"], false);
    let report = check_frozen(
        &CheckOptions {
            rtl: Input::Texts(vec![("m.v".into(), ANONYMOUS_RTL.into())]),
            ..CheckOptions::default()
        },
        std::slice::from_ref(library()),
        &g.freeze(),
        Limits::default(),
    );
    assert_eq!(report.verdict, Verdict::Undecided, "{report:?}");
    assert!(
        report.undecided.iter().any(|s| s.contains("ambiguous")),
        "{report:?}"
    );
    assert!(report.after_reset.is_empty());
}

#[test]
fn exact_state_probe_wins_over_anonymous_alias() {
    let g = anonymous_graph(0, &["bank.genblk17.state"], true);
    let report = check_frozen(
        &CheckOptions {
            rtl: Input::Texts(vec![("m.v".into(), ANONYMOUS_RTL.into())]),
            ..CheckOptions::default()
        },
        std::slice::from_ref(library()),
        &g.freeze(),
        Limits::default(),
    );
    assert_eq!(report.verdict, Verdict::Equivalent, "{report:?}");
}
