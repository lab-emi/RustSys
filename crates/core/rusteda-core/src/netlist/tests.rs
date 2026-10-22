// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Mutation API: every operation, its error cases, and the invariants after each.

use super::*;

/// Checks every invariant of the module documentation on the internal tables (CORE-REQ-0121),
/// and that the frozen view validates and thaws back to itself (CORE-REQ-0135, 0136).
pub(super) fn assert_consistent(g: &GateNetlist) {
    let hierarchical = |s: SymbolId| names::check_hierarchical(g.resolve(s)).is_ok();
    assert!(names::check_verbatim(g.name()).is_ok());
    assert_eq!(g.cell_by_name.len(), g.cells.len());
    for (id, c) in g.cells.iter() {
        assert_eq!(g.cell_by_name[&c.name], id);
        assert!(names::check_verbatim(g.resolve(c.name)).is_ok());
        for (k, p) in c.pins.iter().enumerate() {
            assert!(names::check_verbatim(g.resolve(p.name)).is_ok());
            assert!(c.pins[..k].iter().all(|q| q.name != p.name));
        }
    }
    assert_eq!(g.port_by_name.len(), g.ports.len());
    for (k, p) in g.ports.iter().enumerate() {
        assert_eq!(g.port_by_name[&p.name], k);
        assert!(hierarchical(p.name));
        let want: Vec<Option<i32>> = match p.range {
            None => vec![None],
            Some((m, l)) => range_indices(m, l).map(Some).collect(),
        };
        let got: Vec<Option<i32>> = p.bits.iter().map(|&b| g.nets[b].index).collect();
        assert_eq!(got, want);
        assert!(p.bits.iter().all(|&b| g.nets[b].name == Some(p.name)));
        let same_name = g
            .nets
            .iter()
            .filter(|(_, n)| n.name == Some(p.name))
            .count();
        assert_eq!(same_name, p.bits.len(), "nets named after port");
    }
    let mut named = 0;
    let mut per_name: FxHashMap<SymbolId, usize> = FxHashMap::default();
    for (id, n) in g.nets.iter() {
        match n.name {
            None => assert_eq!(n.index, None),
            Some(s) => {
                named += 1;
                assert!(hierarchical(s));
                *per_name.entry(s).or_default() += 1;
                assert_eq!(g.net_by_key[&(s, n.index)], id);
                let usage = if n.index.is_some() {
                    NameUse::Indexed
                } else {
                    NameUse::Scalar
                };
                assert_eq!(g.net_names[&s].0, usage);
            }
        }
        for (k, &p) in n.pins.iter().enumerate() {
            assert_eq!(g.pins[p].conn, Some(Conn::Net(id)));
            assert!(!n.pins[..k].contains(&p), "pin listed twice");
        }
    }
    assert_eq!(g.net_by_key.len(), named);
    assert_eq!(g.net_names.len(), per_name.len());
    for (s, count) in per_name {
        assert_eq!(g.net_names[&s].1, count, "nets of a name");
    }
    assert_eq!(g.inst_by_name.len(), g.insts.len());
    for (id, i) in g.insts.iter() {
        assert_eq!(g.inst_by_name[&i.name], id);
        assert!(hierarchical(i.name));
        assert_eq!(i.pins.len(), g.cells[i.cell].pins.len());
        for (k, &p) in i.pins.iter().enumerate() {
            assert_eq!((g.pins[p].inst, g.pins[p].cell_pin), (id, k as u32));
        }
        // The connection order lists exactly the connected pins, once each.
        let connected = i.pins.iter().filter(|&&p| g.pins[p].conn.is_some());
        assert_eq!(connected.count(), i.connected.len());
        for (k, &p) in i.connected.iter().enumerate() {
            assert!(i.pins.contains(&p) && g.pins[p].conn.is_some());
            assert!(!i.connected[..k].contains(&p), "pin listed twice");
        }
    }
    let listed: usize = g.insts.iter().map(|(_, i)| i.pins.len()).sum();
    assert_eq!(listed, g.pins.len(), "every pin belongs to an instance");
    for (id, p) in g.pins.iter() {
        if let Some(Conn::Net(n)) = p.conn {
            assert!(g.nets[n].pins.contains(&id));
        }
    }
    for a in &g.assigns {
        assert!(g.nets.contains(a.lhs));
        if let Conn::Net(n) = a.rhs {
            assert!(g.nets.contains(n));
        }
    }
    let f = g.freeze();
    f.validate().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(f.thaw().unwrap().freeze(), f);
}

/// A state to compare before and after a rejected operation: a refused call changes nothing
/// and interns nothing (CORE-REQ-0122).
pub(super) fn snapshot(g: &GateNetlist) -> (FrozenNetlist<'static>, usize, Vec<Vec<PinId>>) {
    let net_pins = g.nets.iter().map(|(_, n)| n.pins.clone()).collect();
    (g.freeze(), g.symbols.len(), net_pins)
}

/// Cells inv, buf, nand2 and its pin-permuted variant nand2r, dff and tie.
pub(super) fn library(g: &mut GateNetlist) -> [CellId; 6] {
    use PinDir::{Input as I, Output as O};
    [
        g.add_cell_type("inv", &[("A", I), ("Y", O)]).unwrap(),
        g.add_cell_type("buf", &[("A", I), ("Y", O)]).unwrap(),
        g.add_cell_type("nand2", &[("A", I), ("B", I), ("Y", O)])
            .unwrap(),
        g.add_cell_type("nand2r", &[("Y", O), ("B", I), ("A", I)])
            .unwrap(),
        g.add_cell_type("dff", &[("D", I), ("CK", I), ("Q", O), ("QN", O)])
            .unwrap(),
        g.add_cell_type("tie", &[("H", O)]).unwrap(),
    ]
}

// CORE-REQ-0120, 0122: cell types with their pins in library order.
#[test]
fn cell_types() {
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, ..] = library(&mut g);
    assert_eq!(g.name(), "top");
    // Adding the same cell again is idempotent; other pins are a conflict.
    let pins = vec![
        ("A".to_string(), PinDir::Input),
        ("Y".into(), PinDir::Output),
    ];
    assert_eq!(g.add_cell_type("inv", &pins), Ok(inv));
    let before = snapshot(&g);
    let errors = [
        g.add_cell_type("inv", &[("A", PinDir::Input)]).unwrap_err(),
        g.add_cell_type("inv", &[("A", PinDir::Input), ("Y", PinDir::Inout)])
            .unwrap_err(),
        g.add_cell_type("x", &[("A", PinDir::Input), ("A", PinDir::Output)])
            .unwrap_err(),
        g.add_cell_type("", &[("A", PinDir::Input)]).unwrap_err(),
        g.add_cell_type("x", &[("", PinDir::Input)]).unwrap_err(),
    ];
    assert_eq!(
        errors.map(|e| e.to_string()),
        [
            "cell 'inv' exists with different pins",
            "cell 'inv' exists with different pins",
            "cell 'x' lists pin 'A' twice",
            "empty cell name",
            "empty cell pin name",
        ]
    );
    assert_eq!(snapshot(&g), before);
    assert_eq!(g.cell_count(), 6);
    let c = g.cell(inv).unwrap();
    let names: Vec<&str> = c.pins.iter().map(|p| g.resolve(p.name)).collect();
    assert_eq!(names, ["A", "Y"]);
    assert_eq!(
        g.find_cell("nand2r").map(|c| g.cell(c).unwrap().pins.len()),
        Some(3)
    );
    assert_eq!(g.find_cell("nand3"), None);
    let order: Vec<&str> = g.cells().map(|(_, c)| g.resolve(c.name)).collect();
    assert_eq!(order, ["inv", "buf", "nand2", "nand2r", "dff", "tie"]);
    assert!(GateNetlist::new("").is_err());
    assert_consistent(&g);
}

// CORE-REQ-0120, 0122, 0128: ports with their bit nets, named, indexed and anonymous nets,
// and the reserved index and range width.
#[test]
fn ports_and_nets() {
    let mut g = GateNetlist::new("top").unwrap();
    assert_eq!(g.add_port("clk", PortDir::Input, None), Ok(0));
    assert_eq!(g.add_port("d", PortDir::Input, Some((3, 0))), Ok(1));
    assert_eq!(g.add_port("q", PortDir::Output, Some((0, 2))), Ok(2));
    assert_eq!(g.add_port("io", PortDir::Inout, Some((-1, -1))), Ok(3));
    let bits = |g: &GateNetlist, k: usize| -> Vec<String> {
        g.ports()[k].bits.iter().map(|&b| g.net_label(b)).collect()
    };
    assert_eq!(bits(&g, 0), ["clk"]);
    assert_eq!(bits(&g, 1), ["d[3]", "d[2]", "d[1]", "d[0]"]);
    assert_eq!(bits(&g, 2), ["q[0]", "q[1]", "q[2]"]);
    assert_eq!(bits(&g, 3), ["io[-1]"]);
    assert_eq!(g.find_port("q").map(|p| p.range), Some(Some((0, 2))));
    assert_eq!(g.find_port("x"), None);
    let n1 = g.add_net(Some("n1"), None).unwrap();
    let w5 = g.add_net(Some("w"), Some(5)).unwrap();
    let w6 = g.add_net(Some("w"), Some(-6)).unwrap();
    let anon = g.add_net(None, None).unwrap();
    let anon2 = g.add_net(None, None).unwrap();
    assert_ne!(anon, anon2);
    assert_eq!(g.find_net("n1", None), Some(n1));
    assert_eq!(g.find_net("w", Some(5)), Some(w5));
    assert_eq!(g.find_net("w", Some(-6)), Some(w6));
    assert_eq!(g.find_net("w", None), None);
    assert_eq!(g.find_net("d", Some(2)), Some(g.ports()[1].bits[1]));
    assert_eq!(g.net(anon).unwrap().name, None);
    assert_eq!(g.net_label(anon), format!("#{} (anonymous)", anon.index()));
    assert_eq!((g.port_count(), g.net_count()), (4, 9 + 5));
    let before = snapshot(&g);
    let errors = [
        g.add_port("clk", PortDir::Input, None),
        g.add_port("n1", PortDir::Input, None),
        g.add_port("w", PortDir::Input, Some((1, 0))),
        g.add_port("p", PortDir::Input, Some((i32::MIN, 0))),
        g.add_port("", PortDir::Input, None),
        g.add_port("p", PortDir::Input, Some((MAX_RANGE_WIDTH as i32, 0))),
        g.add_port("p", PortDir::Input, Some((-1, i32::MAX))),
    ]
    .map(|r| r.unwrap_err().to_string());
    assert_eq!(
        errors,
        [
            "port 'clk' exists already",
            "net 'n1': a port cannot take the name of existing nets",
            "net 'w': a port cannot take the name of existing nets",
            "port 'p': range bound i32::MIN is reserved",
            "empty port name",
            "port 'p': range [16777216:0] is wider than 16777216 bits",
            "port 'p': range [-1:2147483647] is wider than 16777216 bits",
        ]
    );
    let errors = [
        g.add_net(Some("n1"), None),
        g.add_net(Some("n1"), Some(0)),
        g.add_net(Some("w"), Some(5)),
        g.add_net(Some("w"), None),
        g.add_net(Some("d"), Some(7)),
        g.add_net(Some("clk"), None),
        g.add_net(None, Some(1)),
        g.add_net(Some("z"), Some(i32::MIN)),
        g.add_net(Some(""), None),
    ]
    .map(|r| r.unwrap_err().to_string());
    assert_eq!(
        errors,
        [
            "net 'n1': the net exists already",
            "net 'n1[0]': the name belongs to a scalar net",
            "net 'w[5]': the net exists already",
            "net 'w': the name belongs to indexed nets",
            "net 'd[7]': the name belongs to a port",
            "net 'clk': the name belongs to a port",
            "an anonymous net has no bit index",
            "net 'z': bit index i32::MIN is reserved",
            "empty net name",
        ]
    );
    assert_eq!(snapshot(&g), before, "failed operations change nothing");
    // Nets iterate in id order: port bits first here, then the added nets.
    let labels: Vec<String> = g.nets().map(|(id, _)| g.net_label(id)).collect();
    assert_eq!(labels[..2], ["clk", "d[3]"]);
    assert_eq!(labels[9..12], ["n1", "w[5]", "w[-6]"]);
    assert_consistent(&g);
}

// CORE-REQ-0124, 0122: instances, connections in connection order, and assignments.
#[test]
fn instances_and_connections() {
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, _, nand2, ..] = library(&mut g);
    g.add_port("a", PortDir::Input, None).unwrap();
    let a = g.find_net("a", None).unwrap();
    let n = g.add_net(Some("n"), None).unwrap();
    let u1 = g.add_instance("u1", nand2).unwrap();
    let u2 = g.add_instance("u2", inv).unwrap();
    assert_eq!(g.find_instance("u2"), Some(u2));
    assert_eq!((g.instance_count(), g.pin_count()), (2, 5));
    let i1 = g.instance(u1).unwrap();
    assert_eq!(i1.cell, nand2);
    assert!(i1.pins.iter().all(|&p| g.pin(p).unwrap().conn.is_none()));
    let pa = g.connect(u1, "A", Conn::Net(a)).unwrap();
    let pb = g.connect(u1, 1, Conn::Const(true)).unwrap();
    let py = g.connect(u1, "Y", Conn::Net(n)).unwrap();
    let p2 = g.connect(u2, "A", Conn::Net(n)).unwrap();
    assert_eq!(g.find_pin(u1, "B"), Ok(pb));
    assert_eq!(g.find_pin(u1, 0), Ok(pa));
    assert_eq!(g.net(n).unwrap().pins, [py, p2]);
    assert_eq!(g.pin(pb).unwrap().conn, Some(Conn::Const(true)));
    assert_eq!(g.pin_label(py), "u1.Y");
    let before = snapshot(&g);
    let stale_net = Id::new(n.index(), 7);
    let errors = [
        g.add_instance("u1", inv).map(|_| ()),
        g.add_instance("", inv).map(|_| ()),
        g.add_instance("u3", Id::new(9, 0)).map(|_| ()),
        g.connect(u1, "A", Conn::Net(n)).map(|_| ()),
        g.connect(u1, "Z", Conn::Net(n)).map(|_| ()),
        g.connect(u1, 3, Conn::Net(n)).map(|_| ()),
        g.connect(u2, "Y", Conn::Net(stale_net)).map(|_| ()),
        g.add_assign(stale_net, Conn::Const(false)),
        g.add_assign(n, Conn::Net(stale_net)),
    ]
    .map(|r| r.unwrap_err().to_string());
    assert_eq!(
        errors,
        [
            "instance 'u1' exists already",
            "empty instance name",
            "stale cell id (slot 9, generation 0): it addresses no live cell of this netlist",
            "pin 'u1.A' is connected already",
            "cell 'nand2' has no pin 'Z'",
            "cell 'nand2' has no pin #3",
            "stale net id (slot 1, generation 7): it addresses no live net of this netlist",
            "stale net id (slot 1, generation 7): it addresses no live net of this netlist",
            "stale net id (slot 1, generation 7): it addresses no live net of this netlist",
        ]
    );
    assert_eq!(snapshot(&g), before);
    assert_eq!(g.instance(u1).unwrap().connected, [pa, pb, py]);
    assert_eq!(g.disconnect(py), Ok(Some(Conn::Net(n))));
    assert_eq!(g.disconnect(py), Ok(None));
    assert_eq!(g.net(n).unwrap().pins, [p2]);
    assert_eq!(g.disconnect(pb), Ok(Some(Conn::Const(true))));
    assert_eq!(g.instance(u1).unwrap().connected, [pa]);
    // A reconnected pin comes last in the connection order.
    g.connect_pin(py, Conn::Net(a)).unwrap();
    g.connect_pin(pb, Conn::Const(false)).unwrap();
    assert_eq!(g.instance(u1).unwrap().connected, [pa, py, pb]);
    assert_eq!(g.net(a).unwrap().pins, [pa, py]);
    g.add_assign(n, Conn::Net(a)).unwrap();
    g.add_assign(n, Conn::Const(false)).unwrap();
    assert_eq!(
        g.assigns(),
        [
            Assign {
                lhs: n,
                rhs: Conn::Net(a)
            },
            Assign {
                lhs: n,
                rhs: Conn::Const(false)
            }
        ]
    );
    assert_eq!(g.assign_count(), 2);
    assert_consistent(&g);
}

// CORE-REQ-0125, 0101, 0102: removed instances and pins leave stale ids; their slots are
// reused.
#[test]
fn removing_instances() {
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, ..] = library(&mut g);
    let n = g.add_net(Some("n"), None).unwrap();
    let m = g.add_net(Some("m"), None).unwrap();
    let u1 = g.add_instance("u1", inv).unwrap();
    let u2 = g.add_instance("u2", inv).unwrap();
    let a1 = g.connect(u1, "A", Conn::Net(n)).unwrap();
    g.connect(u1, "Y", Conn::Net(m)).unwrap();
    let a2 = g.connect(u2, "A", Conn::Net(m)).unwrap();
    let y2 = g.connect(u2, "Y", Conn::Net(n)).unwrap();
    g.remove_instance(u1).unwrap();
    assert_eq!(g.net(n).unwrap().pins, [y2]);
    assert_eq!(g.net(m).unwrap().pins, [a2]);
    assert_eq!((g.instance_count(), g.pin_count()), (1, 2));
    assert_eq!(g.find_instance("u1"), None);
    // Ids of the removed instance and its pins are stale everywhere.
    let before = snapshot(&g);
    assert!(matches!(
        g.instance(u1),
        Err(NetlistError::StaleId {
            kind: "instance",
            ..
        })
    ));
    assert!(matches!(
        g.pin(a1),
        Err(NetlistError::StaleId { kind: "pin", .. })
    ));
    assert!(g.remove_instance(u1).is_err());
    assert!(g.connect(u1, "A", Conn::Const(false)).is_err());
    assert!(g.disconnect(a1).is_err());
    assert!(g.swap_cell(u1, inv).is_err());
    assert_eq!(snapshot(&g), before);
    // The name is free again; freed slots are reused with new generations.
    let u3 = g.add_instance("u1", inv).unwrap();
    assert_eq!((u3.index(), u3.generation()), (u1.index(), 1));
    assert!(g.pin(a1).is_err(), "old pin id stays stale");
    assert_consistent(&g);
    // Dense frozen numbering follows slot order: u1 (new) comes before u2.
    let f = g.freeze();
    let names: Vec<&str> = f
        .instances
        .iter()
        .map(|i| {
            let r = f.str_offs[i.name as usize] as usize..f.str_offs[i.name as usize + 1] as usize;
            std::str::from_utf8(&f.str_bytes[r]).unwrap()
        })
        .collect();
    assert_eq!(names, ["u1", "u2"]);
}

// CORE-REQ-0126: pin-compatible cell swaps.
#[test]
fn swapping_cells() {
    use PinDir::{Input as I, Output as O};
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, buf, nand2, nand2r, dff, _] = library(&mut g);
    let odd = g.add_cell_type("odd", &[("A", I), ("Z", O)]).unwrap();
    let rev = g.add_cell_type("rev", &[("A", O), ("Y", I)]).unwrap();
    let a = g.add_net(Some("a"), None).unwrap();
    let y = g.add_net(Some("y"), None).unwrap();
    let u = g.add_instance("u", nand2).unwrap();
    let pa = g.connect(u, "A", Conn::Net(a)).unwrap();
    let pb = g.connect(u, "B", Conn::Const(false)).unwrap();
    let py = g.connect(u, "Y", Conn::Net(y)).unwrap();
    let unswapped = g.freeze();
    // Same names and directions in another order: pins keep their ids, connections and
    // connection order.
    g.swap_cell(u, nand2r).unwrap();
    let i = g.instance(u).unwrap();
    assert_eq!(i.cell, nand2r);
    assert_eq!(i.pins, [py, pb, pa]);
    assert_eq!(i.connected, [pa, pb, py]);
    assert_eq!(g.find_pin(u, "A"), Ok(pa));
    assert_eq!(g.pin(pa).unwrap().cell_pin, 2);
    assert_eq!(g.pin(pa).unwrap().conn, Some(Conn::Net(a)));
    assert_consistent(&g);
    g.swap_cell(u, nand2).unwrap();
    assert_eq!(g.instance(u).unwrap().pins, [pa, pb, py]);
    assert_eq!(g.freeze(), unswapped, "swapping back restores the tables");
    g.swap_cell(u, nand2).unwrap();
    let v = g.add_instance("v", inv).unwrap();
    let before = snapshot(&g);
    let errors = [
        g.swap_cell(u, inv),
        g.swap_cell(u, dff),
        g.swap_cell(v, odd),
        g.swap_cell(v, rev),
        g.swap_cell(v, Id::new(40, 0)),
    ]
    .map(|r| r.unwrap_err().to_string());
    assert_eq!(
        errors,
        [
            "instance 'u' cannot take cell 'inv': it has 2 pins, 'nand2' has 3",
            "instance 'u' cannot take cell 'dff': it has 4 pins, 'nand2' has 3",
            "instance 'v' cannot take cell 'odd': pin 'Z' is not a pin of 'inv'",
            "instance 'v' cannot take cell 'rev': pin 'A' has direction output, not input as in \
             'inv'",
            "stale cell id (slot 40, generation 0): it addresses no live cell of this netlist",
        ]
    );
    assert_eq!(snapshot(&g), before);
    g.swap_cell(v, buf).unwrap();
    assert_eq!(g.instance(v).unwrap().cell, buf);
    assert_consistent(&g);
}

// CORE-REQ-0127: buffer insertion splits a net; every check comes first.
#[test]
fn inserting_buffers() {
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, buf, nand2, _, dff, _] = library(&mut g);
    let n = g.add_net(Some("n"), None).unwrap();
    let drv = g.add_instance("drv", inv).unwrap();
    let dy = g.connect(drv, "Y", Conn::Net(n)).unwrap();
    let mut sinks = Vec::new();
    for k in 0..3 {
        let s = g.add_instance(&format!("s{k}"), nand2).unwrap();
        sinks.push(g.connect(s, "B", Conn::Net(n)).unwrap());
    }
    let ff = g.add_instance("ff", dff).unwrap();
    let ck = g.connect(ff, "CK", Conn::Net(n)).unwrap();
    let q = g.connect(ff, "Q", Conn::Net(n)).unwrap();
    let other = g.add_net(Some("other"), None).unwrap();
    let s0 = g.find_instance("s0").unwrap();
    let off_net = g.connect(s0, "A", Conn::Net(other)).unwrap();
    let before = snapshot(&g);
    let errors = [
        g.insert_buffer(n, &[sinks[0], off_net], buf, "A", "Y", "b", None),
        g.insert_buffer(n, &[sinks[1], dy], buf, "A", "Y", "b", None),
        g.insert_buffer(n, &[q], buf, "A", "Y", "b", None),
        g.insert_buffer(n, &[sinks[1], sinks[1]], buf, "A", "Y", "b", None),
        g.insert_buffer(n, &[sinks[1]], buf, "Y", "A", "b", None),
        g.insert_buffer(n, &[sinks[1]], buf, "A", "Z", "b", None),
        g.insert_buffer(n, &[sinks[1]], buf, "A", "Y", "drv", None),
        g.insert_buffer(n, &[sinks[1]], buf, "A", "Y", "b", Some("n")),
        g.insert_buffer(n, &[sinks[1]], buf, "A", "Y", "", None),
    ]
    .map(|r| r.unwrap_err().to_string());
    assert_eq!(
        errors,
        [
            "pin 's0.A' is not connected to net 'n'",
            "pin 'drv.Y' drives its net and cannot move behind a buffer",
            "pin 'ff.Q' drives its net and cannot move behind a buffer",
            "pin 's1.B' is listed twice",
            "pin 'Y' of buffer cell 'buf' is not an input pin",
            "cell 'buf' has no pin 'Z'",
            "instance 'drv' exists already",
            "net 'n': the net exists already",
            "empty instance name",
        ]
    );
    assert_eq!(snapshot(&g), before);
    let (b, out) = g
        .insert_buffer(n, &[sinks[2], ck], buf, "A", 1, "b", Some("n_buf"))
        .unwrap();
    let bin = g.find_pin(b, "A").unwrap();
    let bout = g.find_pin(b, "Y").unwrap();
    assert_eq!(g.net(n).unwrap().pins, [dy, sinks[0], sinks[1], q, bin]);
    assert_eq!(g.net(out).unwrap().pins, [bout, sinks[2], ck]);
    // Moved sinks keep their place in their instance's connection order.
    assert_eq!(g.instance(b).unwrap().connected, [bin, bout]);
    assert_eq!(g.instance(ff).unwrap().connected, [ck, q]);
    assert_eq!(g.find_net("n_buf", None), Some(out));
    assert_eq!(g.pin(ck).unwrap().conn, Some(Conn::Net(out)));
    // An anonymous output net and no sinks are allowed.
    let (_, anon) = g.insert_buffer(out, &[], buf, 0, "Y", "b2", None).unwrap();
    assert_eq!(g.net(anon).unwrap().name, None);
    assert_eq!(g.net(anon).unwrap().pins.len(), 1);
    assert_consistent(&g);
}

// CORE-REQ-0124, 0125: moved pins keep their place; only unused nets can be removed.
#[test]
fn moving_pins_and_removing_nets() {
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, _, nand2, ..] = library(&mut g);
    g.add_port("a", PortDir::Input, None).unwrap();
    let a = g.find_net("a", None).unwrap();
    let n = g.add_net(Some("n"), None).unwrap();
    let m = g.add_net(Some("m"), Some(0)).unwrap();
    let u = g.add_instance("u", nand2).unwrap();
    let pa = g.connect(u, "A", Conn::Net(n)).unwrap();
    let pb = g.connect(u, "B", Conn::Net(a)).unwrap();
    let py = g.connect(u, "Y", Conn::Net(m)).unwrap();
    // A moved pin keeps its place in its instance's connection order.
    assert_eq!(g.move_pin(pa, Conn::Net(m)), Ok(Conn::Net(n)));
    assert_eq!(g.move_pin(pb, Conn::Const(true)), Ok(Conn::Net(a)));
    assert_eq!(g.instance(u).unwrap().connected, [pa, pb, py]);
    assert_eq!(g.net(m).unwrap().pins, [py, pa]);
    assert!(g.net(n).unwrap().pins.is_empty() && g.net(a).unwrap().pins.is_empty());
    assert_eq!(g.move_pin(pa, Conn::Net(m)), Ok(Conn::Net(m)));
    assert_eq!(
        g.net(m).unwrap().pins,
        [py, pa],
        "a move to the same net is no change"
    );
    assert_consistent(&g);
    g.add_assign(n, Conn::Const(false)).unwrap();
    let v = g.add_instance("v", inv).unwrap();
    let free = g.find_pin(v, "A").unwrap();
    let before = snapshot(&g);
    let errors = [
        g.move_pin(free, Conn::Net(n)).map(drop),
        g.move_pin(pa, Conn::Net(Id::new(n.index(), 9))).map(drop),
        g.remove_net(m),
        g.remove_net(a),
        g.remove_net(n),
        g.remove_assign(1).map(drop),
    ]
    .map(|r| r.unwrap_err().to_string());
    assert_eq!(
        errors,
        [
            "pin 'v.A' is not connected",
            "stale net id (slot 1, generation 9): it addresses no live net of this netlist",
            "net 'm[0]' cannot be removed: pins are connected to it",
            "net 'a' cannot be removed: it is a bit of a port",
            "net 'n' cannot be removed: an assignment refers to it",
            "no assignment at position 1: the netlist has 1",
        ]
    );
    assert_eq!(snapshot(&g), before, "failed operations change nothing");
    assert_eq!(
        g.remove_assign(0),
        Ok(Assign {
            lhs: n,
            rhs: Conn::Const(false)
        })
    );
    assert_eq!(g.assign_count(), 0);
    g.remove_net(n).unwrap();
    assert!(g.net(n).is_err() && g.remove_net(n).is_err());
    assert_eq!(g.find_net("n", None), None);
    // Once its last net is gone, a name is free for nets of the other kind.
    g.move_pin(pa, Conn::Net(a)).unwrap();
    g.move_pin(py, Conn::Const(false)).unwrap();
    g.remove_net(m).unwrap();
    let m2 = g.add_net(Some("m"), None).unwrap();
    let n2 = g.add_net(Some("n"), Some(4)).unwrap();
    assert_eq!(
        (m2.index(), m2.generation()),
        (m.index(), 1),
        "the freed slot is reused"
    );
    assert_eq!(g.find_net("n", Some(4)), Some(n2));
    assert_eq!(g.instance(u).unwrap().connected, [pa, pb, py]);
    assert_consistent(&g);
}

// CORE-REQ-0127: removing a buffer undoes its insertion: the sinks go back to the buffered net
// in their places, and the frozen tables are those before the insertion.
#[test]
fn removing_buffers() {
    use PinDir::{Inout, Input, Output};
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, buf, nand2, _, dff, _] = library(&mut g);
    let pad = g
        .add_cell_type("pad", &[("A", Input), ("Y", Output), ("IO", Inout)])
        .unwrap();
    g.add_port("y", PortDir::Output, None).unwrap();
    let y = g.find_net("y", None).unwrap();
    let n = g.add_net(Some("n"), None).unwrap();
    let drv = g.add_instance("drv", inv).unwrap();
    g.connect(drv, "Y", Conn::Net(n)).unwrap();
    let mut sinks = Vec::new();
    for k in 0..3 {
        let s = g.add_instance(&format!("s{k}"), nand2).unwrap();
        sinks.push(g.connect(s, "B", Conn::Net(n)).unwrap());
        g.connect(s, "A", Conn::Const(false)).unwrap();
    }
    let ff = g.add_instance("ff", dff).unwrap();
    let ck = g.connect(ff, "CK", Conn::Net(n)).unwrap();
    let d = g.connect(ff, "D", Conn::Net(n)).unwrap();
    let original = g.freeze();
    let (b, out) = g
        .insert_buffer(
            n,
            &[sinks[2], ck, sinks[0]],
            buf,
            "A",
            "Y",
            "b",
            Some("n_b"),
        )
        .unwrap();
    assert_ne!(g.freeze(), original);
    assert_eq!(g.remove_buffer(b), Ok(n));
    assert_eq!(g.freeze(), original);
    assert!(g.instance(b).is_err() && g.net(out).is_err());
    assert_eq!(g.instance(ff).unwrap().connected, [ck, d]);
    assert_eq!(g.pin(ck).unwrap().conn, Some(Conn::Net(n)));
    assert_consistent(&g);
    // The same by hand: move the sinks back, then remove the buffer and its net.
    let (b, out) = g
        .insert_buffer(n, &[sinks[1], d], buf, "A", "Y", "b", None)
        .unwrap();
    for p in [d, sinks[1]] {
        assert_eq!(g.move_pin(p, Conn::Net(n)), Ok(Conn::Net(out)));
    }
    g.remove_instance(b).unwrap();
    g.remove_net(out).unwrap();
    assert_eq!(g.freeze(), original);
    // Instances that are not buffers between two nets, and buffers whose output net is in use.
    let (b, out) = g
        .insert_buffer(n, &[sinks[1]], buf, "A", "Y", "b", Some("n_b"))
        .unwrap();
    let to_port = g.add_instance("to_port", buf).unwrap();
    g.connect(to_port, "A", Conn::Net(n)).unwrap();
    g.connect(to_port, "Y", Conn::Net(y)).unwrap();
    let looped = g.add_instance("looped", buf).unwrap();
    g.connect(looped, "Y", Conn::Net(n)).unwrap();
    g.connect(looped, "A", Conn::Net(n)).unwrap();
    let io = g.add_instance("io", pad).unwrap();
    g.connect(io, "A", Conn::Net(n)).unwrap();
    g.connect(io, "IO", Conn::Net(y)).unwrap();
    let before = snapshot(&g);
    let errors = [
        g.remove_buffer(drv),
        g.remove_buffer(g.find_instance("s0").unwrap()),
        g.remove_buffer(ff),
        g.remove_buffer(looped),
        g.remove_buffer(io),
        g.remove_buffer(to_port),
        g.remove_buffer(Id::new(77, 0)),
    ]
    .map(|r| r.unwrap_err().to_string());
    assert_eq!(
        errors,
        [
            "instance 'drv' is not a removable buffer: it does not connect both an input and an \
             output",
            "instance 's0' is not a removable buffer: pin 's0.A' is connected to a constant",
            "instance 'ff' is not a removable buffer: pin 'ff.D' is a second input",
            "instance 'looped' is not a removable buffer: its input and output are on the same \
             net 'n'",
            "instance 'io' is not a removable buffer: pin 'io.IO' is neither an input nor an \
             output",
            "net 'y' cannot be removed: it is a bit of a port",
            "stale instance id (slot 77, generation 0): it addresses no live instance of this \
             netlist",
        ]
    );
    assert_eq!(snapshot(&g), before);
    let other = g.add_net(Some("other"), None).unwrap();
    g.add_assign(other, Conn::Net(out)).unwrap();
    let e = g.remove_buffer(b).unwrap_err().to_string();
    assert_eq!(e, "net 'n_b' cannot be removed: an assignment refers to it");
    g.remove_assign(0).unwrap();
    let w = g.add_instance("w", inv).unwrap();
    g.connect(w, "Y", Conn::Net(out)).unwrap();
    let e = g.remove_buffer(b).unwrap_err().to_string();
    assert_eq!(
        e,
        "instance 'b' is not a removable buffer: pin 'w.Y' drives its output net 'n_b' too"
    );
    g.remove_instance(w).unwrap();
    g.remove_buffer(b).unwrap();
    assert_consistent(&g);
}

// CORE-REQ-0128: bit indices follow the range from msb to lsb for any bounds, without overflow.
#[test]
fn range_indices_of_any_bounds() {
    let down: Vec<i32> = range_indices(2, -1).collect();
    assert_eq!(down, [2, 1, 0, -1]);
    assert_eq!(range_indices(-3, -1).collect::<Vec<_>>(), [-3, -2, -1]);
    assert_eq!(range_indices(5, 5).collect::<Vec<_>>(), [5]);
    let mut wide = range_indices(i32::MAX, i32::MIN + 1);
    assert_eq!(
        (wide.next(), wide.next(), wide.next_back()),
        (Some(i32::MAX), Some(i32::MAX - 1), Some(i32::MIN + 1))
    );
    let mut up = range_indices(i32::MIN + 1, i32::MAX);
    assert_eq!(
        (up.next(), up.next_back()),
        (Some(i32::MIN + 1), Some(i32::MAX))
    );
    assert_eq!(range_width(i32::MAX, i32::MIN + 1), (1 << 32) - 1);
    assert_eq!(range_width(i32::MIN, i32::MAX), 1 << 32);
    assert_eq!(range_width(-1, 1), 3);
}

// CORE-REQ-0101: ids carry no mark of their netlist: one of another netlist is refused only
// when it addresses no live entry here, so ids must stay with their netlist.
#[test]
fn ids_of_another_netlist() {
    let mut g1 = GateNetlist::new("a").unwrap();
    let mut g2 = GateNetlist::new("b").unwrap();
    let n1 = g1.add_net(Some("x"), None).unwrap();
    g1.add_net(Some("y"), None).unwrap();
    let late = g1.add_net(Some("z"), None).unwrap();
    g2.add_net(Some("w"), None).unwrap();
    assert_eq!(
        g2.net(n1).map(|n| n.name.map(|s| g2.resolve(s))),
        Ok(Some("w"))
    );
    assert!(matches!(
        g2.net(late),
        Err(NetlistError::StaleId { kind: "net", .. })
    ));
}

// CORE-REQ-0123, 0122, 0110, 0111: every call that takes a name checks it against the rules of
// its kind and refuses a bad one with the kind, the name and the reason, changing nothing and
// interning nothing. Port, net and instance names may have segments and escapes; module, cell
// and pin names are verbatim, so slashes and backslashes are plain characters there.
#[test]
fn names_follow_the_rules_of_their_kind() {
    use PinDir::{Input as I, Output as O};
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, buf, ..] = library(&mut g);
    g.add_port(r"io\/a", PortDir::Input, Some((1, 0))).unwrap();
    let n = g.add_net(Some("u1/n2"), None).unwrap();
    g.add_net(Some(r"u1\\x/q"), Some(3)).unwrap();
    let u = g.add_instance(r"u1/u\/2", inv).unwrap();
    g.connect(u, "A", Conn::Net(n)).unwrap();
    let odd = g
        .add_cell_type("lib/c//x", &[(r"A\", I), ("/Y", O)])
        .unwrap();
    g.add_instance(&names::join(["u1", "a/b", r"c\d"]), odd)
        .unwrap();
    assert_eq!(g.find_net("u1/n2", None), Some(n));
    assert_eq!(
        g.find_instance(r"u1/a\/b/c\\d")
            .map(|i| g.instance(i).unwrap().cell),
        Some(odd)
    );
    assert_eq!(GateNetlist::new(r"m/x//y\").unwrap().name(), r"m/x//y\");
    let before = snapshot(&g);
    let interned = g.symbols().len();
    let errors = [
        GateNetlist::new("a b").map(drop),
        GateNetlist::new("t\u{e9}").map(drop),
        g.add_cell_type("in v", &[("A", I)]).map(drop),
        g.add_cell_type("x", &[("A\tB", I)]).map(drop),
        g.add_port("/p", PortDir::Input, None).map(drop),
        g.add_port("p\u{7f}", PortDir::Input, Some((1, 0)))
            .map(drop),
        g.add_net(Some("a//b"), None).map(drop),
        g.add_net(Some(r"a\b"), None).map(drop),
        g.add_net(Some("n/"), Some(0)).map(drop),
        g.add_instance("u 1", inv).map(drop),
        g.add_instance(r"u\", inv).map(drop),
        g.insert_buffer(n, &[], buf, "A", "Y", "b//1", None)
            .map(drop),
        g.insert_buffer(n, &[], buf, "A", "Y", "b1", Some("n\0"))
            .map(drop),
    ]
    .map(|r| r.unwrap_err().to_string());
    assert_eq!(
        errors,
        [
            "invalid module name 'a\\u{20}b': character U+0020 at byte 1 is not printable ASCII \
             (33 to 126)",
            "invalid module name 't\\u{e9}': character U+00E9 at byte 1 is not printable ASCII \
             (33 to 126)",
            "invalid cell name 'in\\u{20}v': character U+0020 at byte 2 is not printable ASCII \
             (33 to 126)",
            "invalid cell pin name 'A\\u{9}B': character U+0009 at byte 1 is not printable ASCII \
             (33 to 126)",
            "invalid port name '/p': the '/' at byte 0 leaves an empty segment",
            "invalid port name 'p\\u{7f}': character U+007F at byte 1 is not printable ASCII (33 \
             to 126)",
            "invalid net name 'a//b': the '/' at byte 2 leaves an empty segment",
            "invalid net name 'a\\b': the backslash at byte 1 is not followed by '/' or '\\'",
            "invalid net name 'n/': the '/' at byte 1 leaves an empty segment",
            "invalid instance name 'u\\u{20}1': character U+0020 at byte 1 is not printable ASCII \
             (33 to 126)",
            "invalid instance name 'u\\': the backslash at byte 1 is not followed by '/' or '\\'",
            "invalid instance name 'b//1': the '/' at byte 2 leaves an empty segment",
            "invalid net name 'n\\u{0}': character U+0000 at byte 1 is not printable ASCII (33 to \
             126)",
        ]
    );
    assert_eq!(snapshot(&g), before, "refused names change nothing");
    assert_eq!(
        g.symbols().len(),
        interned,
        "refused names are not interned"
    );
    assert_eq!(
        g.add_net(Some("a//b"), None),
        Err(NetlistError::InvalidName {
            kind: "net",
            name: "a//b".into(),
            problem: NameError::EmptySegment { at: 2 }
        })
    );
    // A name that is valid as a cell name can be invalid as a net name.
    assert!(g.add_cell_type("c//d", &[("A", I)]).is_ok());
    assert!(g.add_net(Some("c//d"), None).is_err());
    assert_consistent(&g);
}
