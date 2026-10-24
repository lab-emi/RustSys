// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Tests of edit files and of the application of edits: each edit is compared with the netlist
//! that the operations of its specification give when a test performs them by hand.

use rusteda_core::netlist::{Conn, GateNetlist, PinDir, PortDir};

use super::*;

const BUF: [(&str, PinDir); 2] = [("A", PinDir::Input), ("Y", PinDir::Output)];
const NAND: [(&str, PinDir); 3] = [
    ("A", PinDir::Input),
    ("B", PinDir::Input),
    ("Y", PinDir::Output),
];

/// Pins of the cells the tests know, in library order.
fn cells(name: &str) -> Option<Vec<(String, PinDir)>> {
    let pins: &[(&str, PinDir)] = match name {
        "buf1" | "buf2" | "inv1" => &BUF,
        "nand2" | "nand2b" => &NAND,
        _ => return None,
    };
    Some(pins.iter().map(|(n, d)| ((*n).to_string(), *d)).collect())
}

/// `in0 -> u1 (nand2 A,B=in1) -> n1 -> {u2.A, u3.A, out0}`; `u2 -> n2 -> out1`; `u3 -> n3`.
fn netlist() -> GateNetlist {
    let mut g = GateNetlist::new("top").unwrap();
    let nand = g.add_cell_type("nand2", &NAND).unwrap();
    let buf = g.add_cell_type("buf1", &BUF).unwrap();
    for (p, d) in [
        ("in0", PortDir::Input),
        ("in1", PortDir::Input),
        ("out0", PortDir::Output),
        ("out1", PortDir::Output),
    ] {
        g.add_port(p, d, None).unwrap();
    }
    let net = |g: &GateNetlist, n: &str| g.find_net(n, None).unwrap();
    let u1 = g.add_instance("u1", nand).unwrap();
    g.connect(u1, "A", Conn::Net(net(&g, "in0"))).unwrap();
    g.connect(u1, "B", Conn::Net(net(&g, "in1"))).unwrap();
    g.connect(u1, "Y", Conn::Net(net(&g, "out0"))).unwrap();
    let u2 = g.add_instance("u2", buf).unwrap();
    g.connect(u2, "A", Conn::Net(net(&g, "out0"))).unwrap();
    g.connect(u2, "Y", Conn::Net(net(&g, "out1"))).unwrap();
    let n3 = g.add_net(Some("n3"), None).unwrap();
    let u3 = g.add_instance("u3", buf).unwrap();
    g.connect(u3, "A", Conn::Net(net(&g, "out0"))).unwrap();
    g.connect(u3, "Y", Conn::Net(n3)).unwrap();
    g
}

fn pin_sink(i: &str, p: &str) -> Sink {
    Sink::Pin {
        instance: i.into(),
        pin: p.into(),
    }
}

fn run(g: &mut GateNetlist, e: &Edit) -> Result<Applied, EditsError> {
    apply(g, e, &cells)
}

const FILE: &str = r#"{
  "schema": "rusteda.edits.v1",
  "base": [{"op": "resize", "instance": "u2", "cell": "buf2"}],
  "candidates": [
    {"id": "c-1", "edits": [
      {"op": "insert_buffer", "net": "out0", "cell": "buf1", "instance": "b1",
       "net_out": "nb1", "sinks": [{"instance": "u3", "pin": "A"}, {"port": "out0"}]},
      {"op": "swap_pins", "instance": "u1", "pin_a": "A", "pin_b": "B"}]},
    {"id": "c_2", "edits": [
      {"op": "clone", "instance": "u1", "new_instance": "u1c", "new_net": "n1c",
       "sinks": [{"instance": "u2", "pin": "A"}]},
      {"op": "remove_buffer", "instance": "u3"}]}
  ],
  "report": {"endpoints": ["out1", "ep3"], "paths": ["path1"]}
}"#;

// WS-REQ-0140, WS-REQ-0142, WS-REQ-0143, WS-REQ-0145: a file with every edit kind reads back
// from its canonical text unchanged, and the canonical text is a fixed point.
#[test]
fn edit_file_round_trips_through_canonical_json() {
    let f = parse(FILE, &Limits::default()).unwrap();
    assert_eq!(f.base.len(), 1);
    assert_eq!(f.candidates.len(), 2);
    assert_eq!(f.candidates[0].id, "c-1");
    assert_eq!(
        f.candidates[1]
            .edits
            .iter()
            .map(Edit::op)
            .collect::<Vec<_>>(),
        ["clone", "remove_buffer"]
    );
    assert_eq!(f.report.endpoints, ["out1", "ep3"]);
    assert_eq!(f.report.paths, ["path1"]);
    let text = f.to_json();
    assert!(text.starts_with("{\n  \"base\""));
    assert!(text.ends_with("}\n"));
    let again = parse(&text, &Limits::default()).unwrap();
    assert_eq!(again, f);
    assert_eq!(again.to_json(), text);
    let empty = parse(r#"{"schema": "rusteda.edits.v1"}"#, &Limits::default()).unwrap();
    assert_eq!(empty, EditFile::default());
}

fn code_of(text: &str, limits: &Limits) -> (String, String) {
    let e = parse(text, limits).unwrap_err();
    (e.code.as_str().to_string(), e.at)
}

// WS-REQ-0141, WS-REQ-0144: every malformed file is rejected with its code and the JSON path of
// the value, before any edit is applied; nothing unknown is ignored.
#[test]
fn malformed_files_are_rejected_with_codes() {
    let l = Limits::default();
    let wrap = |edits: &str| {
        format!(
            r#"{{"schema": "rusteda.edits.v1", "candidates": [{{"id": "c", "edits": [{edits}]}}]}}"#
        )
    };
    let cases: Vec<(String, &str, &str)> = vec![
        ("not json".into(), "W0300", ""),
        (r#"{"base": []}"#.into(), "W0301", "schema"),
        (r#"{"schema": "rusteda.edits.v2"}"#.into(), "W0301", "schema"),
        (
            r#"{"schema": "rusteda.edits.v1", "extra": 1}"#.into(),
            "W0300",
            "extra",
        ),
        (
            wrap(r#"{"op": "resize", "instance": "u1", "cell": "b", "x": 1}"#),
            "W0300",
            "candidates[0].edits[0].x",
        ),
        (
            wrap(r#"{"op": "grow", "instance": "u1"}"#),
            "W0300",
            "candidates[0].edits[0].op",
        ),
        (
            wrap(r#"{"op": "resize", "instance": "a//b", "cell": "b"}"#),
            "W0302",
            "candidates[0].edits[0].instance",
        ),
        (
            wrap(r#"{"op": "resize", "instance": "u1", "cell": "b c"}"#),
            "W0302",
            "candidates[0].edits[0].cell",
        ),
        (
            wrap(
                r#"{"op": "clone", "instance": "u1", "new_instance": "k", "new_net": "m", "sinks": []}"#,
            ),
            "W0310",
            "candidates[0].edits[0].sinks",
        ),
        (
            wrap(
                r#"{"op": "clone", "instance": "u1", "new_instance": "k", "new_net": "m", "sinks": [{"port": "o"}, {"port": "o"}]}"#,
            ),
            "W0310",
            "candidates[0].edits[0].sinks[1]",
        ),
        (
            r#"{"schema": "rusteda.edits.v1", "candidates": [{"id": "a b", "edits": []}]}"#
                .into(),
            "W0303",
            "candidates[0].id",
        ),
        (
            r#"{"schema": "rusteda.edits.v1", "candidates": [{"id": "a", "edits": []}, {"id": "a", "edits": []}]}"#
                .into(),
            "W0303",
            "candidates[1].id",
        ),
        (
            r#"{"schema": "rusteda.edits.v1", "report": {"paths": ["path0"]}}"#.into(),
            "W0302",
            "report.paths[0]",
        ),
        (
            r#"{"schema": "rusteda.edits.v1", "report": {"endpoints": [3]}}"#.into(),
            "W0300",
            "report.endpoints[0]",
        ),
    ];
    for (text, code, at) in &cases {
        assert_eq!(
            code_of(text, &l),
            (code.to_string(), at.to_string()),
            "{text}"
        );
    }
    let small = Limits {
        max_candidates: 1,
        max_edits: 1,
        max_report: 1,
        max_bytes: 4096,
        max_sinks: 1,
    };
    let two_candidates = r#"{"schema": "rusteda.edits.v1", "candidates": [{"id": "a", "edits": []}, {"id": "b", "edits": []}]}"#;
    assert_eq!(code_of(two_candidates, &small).0, "W0304");
    let two_edits = r#"{"schema": "rusteda.edits.v1", "base": [{"op": "remove_buffer", "instance": "a"}, {"op": "remove_buffer", "instance": "b"}]}"#;
    assert_eq!(code_of(two_edits, &small), ("W0304".into(), "base".into()));
    let long = format!(
        r#"{{"schema": "rusteda.edits.v1", "x": "{}"}}"#,
        "y".repeat(5000)
    );
    assert_eq!(code_of(&long, &small).0, "W0304");
    assert!(is_numbered_handle("path12", "path") && !is_numbered_handle("path012", "path"));
}

// WS-REQ-0141, WS-REQ-0146: `resize` is `swap_cell` to a pin-compatible cell, which is added from
// the cell supplier when the netlist lacks it; an unknown or incompatible cell changes nothing.
#[test]
fn resize_is_a_cell_swap() {
    let mut g = netlist();
    let applied = run(
        &mut g,
        &Edit::Resize {
            instance: "u2".into(),
            cell: "buf2".into(),
        },
    )
    .unwrap();
    let mut want = netlist();
    let buf2 = want.add_cell_type("buf2", &BUF).unwrap();
    let u2 = want.find_instance("u2").unwrap();
    want.swap_cell(u2, buf2).unwrap();
    assert_eq!(g.freeze(), want.freeze());
    assert_eq!(applied.instances, [u2]);
    let before = g.freeze();
    for (cell, code) in [("nand2", "W0314"), ("nope", "W0308")] {
        let e = run(
            &mut g,
            &Edit::Resize {
                instance: "u2".into(),
                cell: cell.into(),
            },
        )
        .unwrap_err();
        assert_eq!(e.code.as_str(), code);
        assert_eq!(g.freeze(), before);
    }
    let e = run(
        &mut g,
        &Edit::Resize {
            instance: "zz".into(),
            cell: "buf1".into(),
        },
    )
    .unwrap_err();
    assert_eq!(e.code.as_str(), "W0305");
}

// WS-REQ-0147: `insert_buffer` without a port sink is the netlist's buffer insertion; with a port
// sink the port's net keeps its name and its listed sinks, and the driver with the other pins
// moves to the new net, ahead of the buffer input.
#[test]
fn insert_buffer_with_pin_and_port_sinks() {
    let mut g = netlist();
    run(
        &mut g,
        &Edit::InsertBuffer {
            net: "out0".into(),
            cell: "buf1".into(),
            instance: "b1".into(),
            net_out: "nb1".into(),
            sinks: vec![pin_sink("u3", "A")],
        },
    )
    .unwrap();
    let mut want = netlist();
    let buf = want.find_cell("buf1").unwrap();
    let n = want.find_net("out0", None).unwrap();
    let u3 = want.find_instance("u3").unwrap();
    let u3a = want.find_pin(u3, "A").unwrap();
    want.insert_buffer(n, &[u3a], buf, "A", "Y", "b1", Some("nb1"))
        .unwrap();
    assert_eq!(g.freeze(), want.freeze());

    let mut g = netlist();
    run(
        &mut g,
        &Edit::InsertBuffer {
            net: "out0".into(),
            cell: "buf1".into(),
            instance: "b2".into(),
            net_out: "nb2".into(),
            sinks: vec![
                Sink::Port {
                    port: "out0".into(),
                },
                pin_sink("u2", "A"),
            ],
        },
    )
    .unwrap();
    let mut want = netlist();
    let n = want.find_net("out0", None).unwrap();
    let m = want.add_net(Some("nb2"), None).unwrap();
    let b2 = want.add_instance("b2", buf).unwrap();
    let u2a = {
        let u2 = want.find_instance("u2").unwrap();
        want.find_pin(u2, "A").unwrap()
    };
    let stay: Vec<_> = want
        .net(n)
        .unwrap()
        .pins
        .iter()
        .copied()
        .filter(|&p| p != u2a)
        .collect();
    for p in stay {
        want.move_pin(p, Conn::Net(m)).unwrap();
    }
    want.connect(b2, "A", Conn::Net(m)).unwrap();
    want.connect(b2, "Y", Conn::Net(n)).unwrap();
    assert_eq!(g.freeze(), want.freeze());

    // A sink off the net, an input port, an inverter-shaped three-pin cell and a taken name are
    // all rejected without a change.
    let before = g.freeze();
    let bad = [
        (
            Edit::InsertBuffer {
                net: "in0".into(),
                cell: "buf1".into(),
                instance: "b9".into(),
                net_out: "nb9".into(),
                sinks: vec![pin_sink("u2", "A")],
            },
            "W0310",
        ),
        (
            Edit::InsertBuffer {
                net: "in0".into(),
                cell: "buf1".into(),
                instance: "b9".into(),
                net_out: "nb9".into(),
                sinks: vec![Sink::Port { port: "in0".into() }],
            },
            "W0310",
        ),
        (
            Edit::InsertBuffer {
                net: "nb2".into(),
                cell: "nand2".into(),
                instance: "b9".into(),
                net_out: "nb9".into(),
                sinks: vec![pin_sink("b2", "A")],
            },
            "W0311",
        ),
        (
            Edit::InsertBuffer {
                net: "nb2".into(),
                cell: "buf1".into(),
                instance: "b9".into(),
                net_out: "n3".into(),
                sinks: vec![pin_sink("b2", "A")],
            },
            "W0309",
        ),
        (
            Edit::InsertBuffer {
                net: "nb2".into(),
                cell: "buf1".into(),
                instance: "u1".into(),
                net_out: "nb9".into(),
                sinks: vec![pin_sink("b2", "A")],
            },
            "W0309",
        ),
        (
            Edit::InsertBuffer {
                net: "nope".into(),
                cell: "buf1".into(),
                instance: "b9".into(),
                net_out: "nb9".into(),
                sinks: vec![pin_sink("b2", "A")],
            },
            "W0306",
        ),
    ];
    for (e, code) in &bad {
        assert_eq!(run(&mut g, e).unwrap_err().code.as_str(), *code, "{e:?}");
        assert_eq!(g.freeze(), before);
    }
}

// WS-REQ-0148: `remove_buffer` merges the output net into the input net, or, when the output net
// is a port bit, the input net into the output net.
#[test]
fn remove_buffer_in_both_directions() {
    let mut g = netlist();
    run(
        &mut g,
        &Edit::RemoveBuffer {
            instance: "u3".into(),
        },
    )
    .unwrap();
    let mut want = netlist();
    let u3 = want.find_instance("u3").unwrap();
    want.remove_buffer(u3).unwrap();
    assert_eq!(g.freeze(), want.freeze());

    // `u2` drives the port bit `out1`: the pins of `out0` move to `out1`, which `u1` then drives.
    let mut g = netlist();
    let e = run(
        &mut g,
        &Edit::RemoveBuffer {
            instance: "u2".into(),
        },
    )
    .unwrap_err();
    // `out0` is a port bit too: neither net can go.
    assert_eq!(e.code.as_str(), "W0311");
    let mut g = netlist();
    run(
        &mut g,
        &Edit::InsertBuffer {
            net: "out0".into(),
            cell: "buf1".into(),
            instance: "b2".into(),
            net_out: "nb2".into(),
            sinks: vec![Sink::Port {
                port: "out0".into(),
            }],
        },
    )
    .unwrap();
    run(
        &mut g,
        &Edit::RemoveBuffer {
            instance: "b2".into(),
        },
    )
    .unwrap();
    let mut want = netlist();
    let n = want.find_net("out0", None).unwrap();
    let a = want.add_net(Some("nb2"), None).unwrap();
    let buf = want.find_cell("buf1").unwrap();
    let b2 = want.add_instance("b2", buf).unwrap();
    let stay: Vec<_> = want.net(n).unwrap().pins.clone();
    for &p in &stay {
        want.move_pin(p, Conn::Net(a)).unwrap();
    }
    want.connect(b2, "A", Conn::Net(a)).unwrap();
    want.connect(b2, "Y", Conn::Net(n)).unwrap();
    let b2a = want.find_pin(b2, "A").unwrap();
    let moved: Vec<_> = want
        .net(a)
        .unwrap()
        .pins
        .iter()
        .copied()
        .filter(|&p| p != b2a)
        .collect();
    for &p in &moved {
        want.move_pin(p, Conn::Net(n)).unwrap();
    }
    want.remove_instance(b2).unwrap();
    want.remove_net(a).unwrap();
    assert_eq!(g.freeze(), want.freeze());
    let e = run(
        &mut g,
        &Edit::RemoveBuffer {
            instance: "u1".into(),
        },
    )
    .unwrap_err();
    assert_eq!(e.code.as_str(), "W0311");
}

// WS-REQ-0149: `swap_pins` moves each input pin to the other's connection, `pin_a` first; the
// pins keep their places in the instance's connection order.
#[test]
fn swap_pins_exchanges_connections() {
    let mut g = netlist();
    run(
        &mut g,
        &Edit::SwapPins {
            instance: "u1".into(),
            pin_a: "A".into(),
            pin_b: "B".into(),
        },
    )
    .unwrap();
    let mut want = netlist();
    let u1 = want.find_instance("u1").unwrap();
    let (a, b) = (
        want.find_pin(u1, "A").unwrap(),
        want.find_pin(u1, "B").unwrap(),
    );
    let (in0, in1) = (
        want.find_net("in0", None).unwrap(),
        want.find_net("in1", None).unwrap(),
    );
    want.move_pin(a, Conn::Net(in1)).unwrap();
    want.move_pin(b, Conn::Net(in0)).unwrap();
    assert_eq!(g.freeze(), want.freeze());
    for (pa, pb, code) in [
        ("A", "A", "W0312"),
        ("A", "Y", "W0312"),
        ("A", "Q", "W0307"),
    ] {
        let e = run(
            &mut g,
            &Edit::SwapPins {
                instance: "u1".into(),
                pin_a: pa.into(),
                pin_b: pb.into(),
            },
        )
        .unwrap_err();
        assert_eq!(e.code.as_str(), code);
    }
}

// WS-REQ-0150: `clone` copies the instance with the connections of its pins in their order, its
// output on the new net, and moves the listed sinks there; with a port sink the copy drives the
// port's net and the original with the other pins moves to the new net.
#[test]
fn clone_with_pin_and_port_sinks() {
    let mut g = netlist();
    let applied = run(
        &mut g,
        &Edit::Clone {
            instance: "u1".into(),
            new_instance: "u1c".into(),
            new_net: "n1c".into(),
            sinks: vec![pin_sink("u3", "A")],
        },
    )
    .unwrap();
    let mut want = netlist();
    let nand = want.find_cell("nand2").unwrap();
    let n = want.find_net("out0", None).unwrap();
    let m = want.add_net(Some("n1c"), None).unwrap();
    let k = want.add_instance("u1c", nand).unwrap();
    for (p, net) in [("A", "in0"), ("B", "in1")] {
        let nid = want.find_net(net, None).unwrap();
        want.connect(k, p, Conn::Net(nid)).unwrap();
    }
    want.connect(k, "Y", Conn::Net(m)).unwrap();
    let u3a = {
        let u3 = want.find_instance("u3").unwrap();
        want.find_pin(u3, "A").unwrap()
    };
    want.move_pin(u3a, Conn::Net(m)).unwrap();
    assert_eq!(g.freeze(), want.freeze());
    assert!(applied.instances.contains(&g.find_instance("u1c").unwrap()));

    let mut g = netlist();
    run(
        &mut g,
        &Edit::Clone {
            instance: "u1".into(),
            new_instance: "u1c".into(),
            new_net: "n1c".into(),
            sinks: vec![Sink::Port {
                port: "out0".into(),
            }],
        },
    )
    .unwrap();
    let mut want = netlist();
    let m = want.add_net(Some("n1c"), None).unwrap();
    let k = want.add_instance("u1c", nand).unwrap();
    let stay: Vec<_> = want.net(n).unwrap().pins.clone();
    for (p, net) in [("A", "in0"), ("B", "in1")] {
        let nid = want.find_net(net, None).unwrap();
        want.connect(k, p, Conn::Net(nid)).unwrap();
    }
    want.connect(k, "Y", Conn::Net(n)).unwrap();
    for p in stay {
        want.move_pin(p, Conn::Net(m)).unwrap();
    }
    assert_eq!(g.freeze(), want.freeze());

    let e = run(
        &mut g,
        &Edit::Clone {
            instance: "u1".into(),
            new_instance: "u1d".into(),
            new_net: "n1d".into(),
            sinks: vec![pin_sink("u1", "A")],
        },
    )
    .unwrap_err();
    assert_eq!(e.code.as_str(), "W0310");
    assert!(find_net(&g, "n1d").is_err());
    assert!(port_bit_net(&g, "out1").is_some() && port_bit_net(&g, "n3").is_none());
}
