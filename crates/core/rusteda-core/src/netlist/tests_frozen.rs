// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Frozen view: table layout, round trips, section bytes, and rejection of corrupted tables
//! without panics.

use std::borrow::Cow;

use super::tests::{assert_consistent, library};
use super::tests_random::Rng;
use super::*;

/// Every kind of entity: scalar and bus ports (descending and ascending), named, anonymous and
/// indexed nets, constants, an unconnected pin, assignments, an instance named like a net, and
/// the slot of a removed instance reused by a later one.
fn sample() -> GateNetlist {
    let mut g = GateNetlist::new("top").unwrap();
    let [inv, _, nand2, _, dff, tie] = library(&mut g);
    g.add_port("clk", PortDir::Input, None).unwrap();
    g.add_port("d", PortDir::Input, Some((1, 0))).unwrap();
    g.add_port("q", PortDir::Output, Some((0, 1))).unwrap();
    let net = |g: &GateNetlist, n: &str, i: Option<i32>| g.find_net(n, i).unwrap();
    let (clk, d1, d0) = (
        net(&g, "clk", None),
        net(&g, "d", Some(1)),
        net(&g, "d", Some(0)),
    );
    let (q0, q1) = (net(&g, "q", Some(0)), net(&g, "q", Some(1)));
    let n = g.add_net(Some("n"), None).unwrap();
    let w = g.add_net(None, None).unwrap();
    g.add_net(Some("v"), Some(1)).unwrap();
    g.add_net(Some("v"), Some(0)).unwrap();
    let gone = g.add_instance("gone", inv).unwrap();
    g.connect(gone, "A", Conn::Net(n)).unwrap();
    let u1 = g.add_instance("u1", nand2).unwrap();
    g.connect(u1, "Y", Conn::Net(n)).unwrap();
    g.connect(u1, "B", Conn::Const(true)).unwrap();
    g.connect(u1, "A", Conn::Net(d1)).unwrap();
    let r = g.add_instance("r", dff).unwrap();
    g.connect(r, "D", Conn::Net(n)).unwrap();
    g.connect(r, "CK", Conn::Net(clk)).unwrap();
    g.connect(r, "Q", Conn::Net(q0)).unwrap();
    let t = g.add_instance("v", tie).unwrap();
    g.connect(t, "H", Conn::Net(w)).unwrap();
    g.remove_instance(gone).unwrap();
    let u2 = g.add_instance("u2", inv).unwrap();
    g.connect(u2, "A", Conn::Net(d0)).unwrap();
    g.connect(u2, "Y", Conn::Net(w)).unwrap();
    g.add_assign(q1, Conn::Net(w)).unwrap();
    g.add_assign(q1, Conn::Const(false)).unwrap();
    g
}

fn strings<'f>(f: &'f FrozenNetlist<'_>) -> Vec<&'f str> {
    f.str_offs
        .windows(2)
        .map(|w| std::str::from_utf8(&f.str_bytes[w[0] as usize..w[1] as usize]).unwrap())
        .collect()
}

/// Overwrites string `k` of `f` with a text of the same length.
fn overwrite_string(f: &mut FrozenNetlist<'_>, k: usize, text: &[u8]) {
    let at = f.str_offs[k] as usize;
    assert_eq!(f.str_offs[k + 1] as usize - at, text.len());
    f.str_bytes.to_mut()[at..at + text.len()].copy_from_slice(text);
}

// CORE-REQ-0130 to 0134: the sections, records, special values, numbering and string order of
// a netlist with every kind of entity.
#[test]
fn table_layout() {
    let f = sample().freeze();
    f.validate().unwrap();
    assert_eq!(
        strings(&f),
        [
            "top", "inv", "A", "Y", "buf", "nand2", "B", "nand2r", "dff", "D", "CK", "Q", "QN",
            "tie", "H", "clk", "d", "q", "n", "v", "u2", "u1", "r"
        ]
    );
    // Module name, ports, nets, instances, connected pins, assignments.
    assert_eq!(f.module[..], [0, 3, 9, 4, 9, 2]);
    assert_eq!(f.module[module_entry::N_PINS], 9);
    assert_eq!(f.cells[..], [1, 4, 5, 7, 8, 13]);
    assert_eq!(f.cell_pin_start[..], [0, 2, 4, 7, 10, 14, 15]);
    assert_eq!(
        f.cell_pin_name[..],
        [2, 3, 2, 3, 2, 6, 3, 3, 6, 2, 9, 10, 11, 12, 14]
    );
    assert_eq!(
        f.cell_pin_dir[..],
        [0, 1, 0, 1, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 1]
    );
    let port = |name, dir, is_bus, msb, lsb| PortRec {
        name,
        dir,
        is_bus,
        _pad: 0,
        msb,
        lsb,
    };
    assert_eq!(
        f.ports[..],
        [
            port(15, 0, 0, 0, 0),
            port(16, 0, 1, 1, 0),
            port(17, 1, 1, 0, 1)
        ]
    );
    assert_eq!(f.port_bit_start[..], [0, 1, 3, 5]);
    assert_eq!(f.port_bit_net[..], [0, 1, 2, 3, 4]);
    let net = |name, index| NetRec { name, index };
    assert_eq!(
        f.nets[..],
        [
            net(15, SCALAR),
            net(16, 1),
            net(16, 0),
            net(17, 0),
            net(17, 1),
            net(18, SCALAR),
            net(ANONYMOUS, SCALAR),
            net(19, 1),
            net(19, 0)
        ]
    );
    // The reused slot puts u2 first; only connected pins are stored, in connection order (u1
    // connected Y, B, A).
    let inst = |name, cell| InstRec { name, cell };
    assert_eq!(
        f.instances[..],
        [inst(20, 0), inst(21, 2), inst(22, 4), inst(19, 5)]
    );
    assert_eq!(f.inst_pin_start[..], [0, 2, 5, 8, 9]);
    assert_eq!(f.pin_cellpin[..], [0, 1, 2, 1, 0, 0, 1, 2, 0]);
    assert_eq!(f.pin_net[..], [2, 6, 5, CONST1, 1, 5, 0, 3, 6]);
    assert_eq!(
        f.assigns[..],
        [
            AssignRec { lhs: 4, rhs: 6 },
            AssignRec {
                lhs: 4,
                rhs: CONST0
            }
        ]
    );
    assert_eq!(f.net_pin_start[..], [0, 1, 2, 3, 4, 4, 6, 8, 8, 8]);
    assert_eq!(f.net_pins[..], [6, 4, 0, 7, 2, 5, 1, 8]);
    assert_eq!(size_of::<PortRec>(), 16);
    assert_eq!(module_entry::COUNT, 6);
    let sizes: Vec<Option<u32>> = (0..=19).map(section::elem_size).collect();
    assert_eq!(sizes[0], None);
    assert_eq!(sizes[19], None);
    let declared: Vec<u32> = f.sections().iter().map(|s| s.1).collect();
    assert_eq!(
        sizes[1..=18].iter().map(|s| s.unwrap()).collect::<Vec<_>>(),
        declared
    );
    assert_eq!(
        (section::STR_BYTES, section::NET_PINS, FORMAT_VERSION),
        (1, 18, 1)
    );
    assert_eq!(section::name(section::PIN_NET), "PIN_NET");
    assert_eq!(section::name(0), "unknown");
}

// CORE-REQ-0136: thawing and freezing give back the tables, borrowed or owned.
#[test]
fn round_trips() {
    let g = sample();
    let f = g.freeze();
    let t = f.thaw().unwrap();
    assert_consistent(&t);
    assert_eq!(t.freeze(), f, "freeze(thaw(f)) == f");
    assert_eq!(t.freeze().thaw().unwrap().freeze(), f, "idempotent");
    assert_eq!(t.name(), "top");
    let u2 = t.find_instance("u2").unwrap();
    assert_eq!(t.instance(u2).unwrap().cell, t.find_cell("inv").unwrap());
    let r = t.find_instance("r").unwrap();
    let qn = t.find_pin(r, "QN").unwrap();
    assert_eq!(t.pin(qn).unwrap().conn, None);
    // The connection order survives: u1 connected Y, B, A.
    let u1 = t.find_instance("u1").unwrap();
    let order: Vec<String> = t
        .instance(u1)
        .unwrap()
        .connected
        .iter()
        .map(|&p| t.pin_label(p))
        .collect();
    assert_eq!(order, ["u1.Y", "u1.B", "u1.A"]);
    // A borrowed view is equal to its owned copy and round-trips alike.
    let borrowed = FrozenNetlist {
        str_bytes: Cow::Borrowed(&f.str_bytes[..]),
        str_offs: Cow::Borrowed(&f.str_offs[..]),
        ..f.clone()
    };
    assert_eq!(borrowed, f);
    assert_eq!(borrowed.clone().into_owned(), f);
    assert_eq!(borrowed.thaw().unwrap().freeze(), f);
    // The smallest netlist: a module without anything.
    let empty = GateNetlist::new("m").unwrap().freeze();
    empty.validate().unwrap();
    assert_eq!(strings(&empty), ["m"]);
    assert_eq!(empty.net_pin_start[..], [0]);
    assert_eq!(empty.thaw().unwrap().freeze(), empty);
}

// CORE-REQ-0130, 0138: the section bytes of a netlist file and the view over them.
#[test]
fn section_bytes() {
    let f = sample().freeze();
    let secs = f.sections();
    let ids: Vec<u32> = secs.iter().map(|s| s.0).collect();
    assert_eq!(ids, (1..=18).collect::<Vec<u32>>());
    assert_eq!((secs[2].1, secs[2].2.len()), (4, 24));
    assert_eq!(secs[2].2, bytemuck::cast_slice::<u32, u8>(&f.module));
    // Aligned copies are borrowed, misaligned ones copied; both give the same tables.
    let aligned: Vec<Vec<u64>> = secs
        .iter()
        .map(|s| {
            let mut v = vec![0u64; s.2.len().div_ceil(8)];
            bytemuck::cast_slice_mut::<u64, u8>(&mut v)[..s.2.len()].copy_from_slice(s.2);
            v
        })
        .collect();
    let get = |id: u32| {
        let k = id.checked_sub(1)? as usize;
        let v = aligned.get(k)?;
        Some((
            secs[k].1,
            &bytemuck::cast_slice::<u64, u8>(v)[..secs[k].2.len()],
        ))
    };
    let view = FrozenNetlist::from_sections(get).unwrap();
    assert_eq!(view, f);
    assert!(matches!(view.pin_net, Cow::Borrowed(_)));
    let shifted: Vec<Vec<u64>> = secs
        .iter()
        .map(|s| {
            let mut v = vec![0u64; (s.2.len() + 1).div_ceil(8)];
            bytemuck::cast_slice_mut::<u64, u8>(&mut v)[1..=s.2.len()].copy_from_slice(s.2);
            v
        })
        .collect();
    let get = |id: u32| {
        let k = id.checked_sub(1)? as usize;
        let v = shifted.get(k)?;
        Some((
            secs[k].1,
            &bytemuck::cast_slice::<u64, u8>(v)[1..=secs[k].2.len()],
        ))
    };
    let copied = FrozenNetlist::from_sections(get).unwrap();
    assert_eq!(copied, f);
    assert!(matches!(copied.pin_net, Cow::Owned(_)));
    let empty = |id: u32| section::elem_size(id).map(|e| (e, &[][..]));
    let missing = FrozenNetlist::from_sections(|id| empty(id).filter(|_| id != 7));
    assert_eq!(
        missing.unwrap_err().to_string(),
        "netlist section 7 (CELL_PIN_DIR): section missing"
    );
    let odd = [0u8; 3];
    let e =
        FrozenNetlist::from_sections(|id| Some((section::elem_size(id)?, &odd[..]))).unwrap_err();
    assert_eq!(e.section, section::STR_OFFS);
    assert!(e.message.contains("3 bytes"), "{e}");
    // A declared element size must be the section's, even when the length would divide by it.
    let wrong = |id: u32| empty(id).map(|(e, b)| (if id == 11 { 4 } else { e }, b));
    let e = FrozenNetlist::from_sections(wrong).unwrap_err();
    assert_eq!(
        e.to_string(),
        "netlist section 11 (NETS): element size 4, expected 8"
    );
    // The module section is six 4-byte entries, not one 24-byte record.
    let module = [0u8; 24];
    let record = |id: u32| match id {
        3 => Some((24, &module[..])),
        _ => empty(id),
    };
    let e = FrozenNetlist::from_sections(record).unwrap_err();
    assert_eq!(
        e.to_string(),
        "netlist section 3 (MODULE): element size 24, expected 4"
    );
}

// CORE-REQ-0138: queries on the tables without thawing: strings, rows and relations.
#[test]
fn accessors() {
    let f = sample().freeze();
    f.validate().unwrap();
    assert_eq!(f.string(0), Ok("top"));
    assert_eq!(f.string(22), Ok("r"));
    assert_eq!(f.string(23).unwrap_err().section, section::STR_OFFS);
    let names = |rows: std::ops::Range<usize>| -> Vec<&str> {
        rows.map(|k| f.string(f.cell_pin_name[k]).unwrap())
            .collect()
    };
    assert_eq!(names(f.cell_pins(3)), ["Y", "B", "A"]);
    assert_eq!(names(f.cell_pins(5)), ["H"]);
    // u1 (instance 1) connected Y, B, A; its pins are rows 2 to 4.
    assert_eq!(f.instance_pins(1), 2..5);
    assert_eq!(f.instance_pins(3), 8..9);
    assert_eq!(f.net_pins_of(5), [2, 5]);
    assert_eq!(f.net_pins_of(4), [] as [u32; 0]);
    assert_eq!(f.port_bits(1), [1, 2]);
    assert_eq!(f.port_bits(2), [3, 4]);
    let insts: Vec<u32> = (0..9).map(|p| f.pin_instance(p)).collect();
    assert_eq!(insts, [0, 0, 1, 1, 1, 2, 2, 2, 3]);
    let dirs: Vec<PinDir> = (0..9).map(|p| f.pin_dir(p)).collect();
    use PinDir::{Input as I, Output as O};
    assert_eq!(dirs, [I, O, O, I, I, I, I, O, O]);
    // An instance without connected pins has an empty row, which pin_instance skips.
    let mut g = sample();
    let idle = g.add_instance("idle", g.find_cell("inv").unwrap()).unwrap();
    let a = g.find_net("d", Some(1)).unwrap();
    let late = g.add_instance("late", g.find_cell("inv").unwrap()).unwrap();
    g.connect(late, "A", Conn::Net(a)).unwrap();
    let f = g.freeze();
    assert_eq!(f.instance_pins(4), 9..9);
    assert_eq!(f.pin_instance(9), 5);
    assert!(g.instance(idle).unwrap().connected.is_empty());
}

#[test]
#[should_panic(expected = "pin 9 out of range")]
fn pin_instance_out_of_range() {
    sample().freeze().pin_instance(9);
}

// CORE-REQ-0133: any connection order of distinct cell pins is valid; a cell pin connected twice
// is not.
#[test]
fn connection_orders() {
    let mut f = sample().freeze();
    let rows = f.instance_pins(2);
    f.pin_cellpin.to_mut()[rows.clone()].reverse();
    f.validate().unwrap();
    let t = f.thaw().unwrap();
    assert_consistent(&t);
    let r = t.find_instance("r").unwrap();
    let order: Vec<String> = t
        .instance(r)
        .unwrap()
        .connected
        .iter()
        .map(|&p| t.pin_label(p))
        .collect();
    assert_eq!(order, ["r.Q", "r.CK", "r.D"]);
    assert_eq!(t.freeze(), f);
    f.pin_cellpin.to_mut()[rows.start] = f.pin_cellpin[rows.start + 1];
    let e = f.validate().unwrap_err();
    assert_eq!(e.section, section::PIN_CELLPIN);
    assert!(e.message.contains("connected twice"), "{e}");
}

type Corruption = fn(&mut FrozenNetlist<'static>);

// CORE-REQ-0135: each corruption breaks one rule; validation reports it in the expected section
// and thawing refuses the tables.
#[test]
fn corrupted_tables_are_rejected() {
    let f = sample().freeze();
    let cases: &[(&str, u32, Corruption)] = &[
        ("no string offsets", section::STR_OFFS, |f| {
            f.str_offs.to_mut().clear()
        }),
        ("string end missing", section::STR_OFFS, |f| {
            f.str_offs.to_mut().pop();
        }),
        ("strings not from 0", section::STR_OFFS, |f| {
            f.str_offs.to_mut()[0] = 1
        }),
        ("string ends before it starts", section::STR_OFFS, |f| {
            f.str_offs.to_mut()[2] = 0
        }),
        ("empty string", section::STR_BYTES, |f| {
            f.str_offs.to_mut()[2] = f.str_offs[1]
        }),
        ("invalid UTF-8", section::STR_BYTES, |f| {
            f.str_bytes.to_mut()[0] = 0xFF
        }),
        (
            "module name with a control character",
            section::STR_BYTES,
            |f| overwrite_string(f, 0, b"t\x01p"),
        ),
        ("cell name with a blank", section::STR_BYTES, |f| {
            overwrite_string(f, 1, b"i v")
        }),
        ("pin name DEL", section::STR_BYTES, |f| {
            overwrite_string(f, 2, b"\x7f")
        }),
        ("port name with an empty segment", section::PORTS, |f| {
            overwrite_string(f, 15, b"/lk")
        }),
        ("net name with a stray backslash", section::NETS, |f| {
            overwrite_string(f, 18, b"\\")
        }),
        ("instance name ending in a slash", section::INSTANCES, |f| {
            overwrite_string(f, 20, b"u/")
        }),
        ("name of nets and an instance", section::NETS, |f| {
            overwrite_string(f, 19, b"/")
        }),
        ("repeated string", section::STR_BYTES, |f| {
            let s = f.str_offs[4] as usize;
            f.str_bytes.to_mut()[s..s + 3].copy_from_slice(b"inv")
        }),
        ("unused string", section::STR_OFFS, |f| {
            f.str_bytes.to_mut().extend_from_slice(b"zz");
            let end = f.str_bytes.len() as u32;
            f.str_offs.to_mut().push(end)
        }),
        ("strings not in first-use order", section::CELLS, |f| {
            f.cells.to_mut().swap(0, 1)
        }),
        ("module name not the first string", section::MODULE, |f| {
            f.module.to_mut()[module_entry::NAME] = 1
        }),
        ("no module entries", section::MODULE, |f| {
            f.module.to_mut().clear()
        }),
        ("module entry missing", section::MODULE, |f| {
            f.module.to_mut().pop();
        }),
        ("extra module entry", section::MODULE, |f| {
            f.module.to_mut().push(0)
        }),
        ("net count", section::MODULE, |f| {
            f.module.to_mut()[module_entry::N_NETS] += 1
        }),
        ("pin count", section::MODULE, |f| {
            f.pin_net.to_mut().pop();
        }),
        ("cell name out of range", section::CELLS, |f| {
            f.cells.to_mut()[5] = 999
        }),
        ("cell pin row decreases", section::CELL_PIN_START, |f| {
            f.cell_pin_start.to_mut()[1] = 5
        }),
        ("cell pin rows end early", section::CELL_PIN_START, |f| {
            *f.cell_pin_start.to_mut().last_mut().unwrap() -= 1
        }),
        ("cell pin direction code", section::CELL_PIN_DIR, |f| {
            f.cell_pin_dir.to_mut()[0] = 4
        }),
        ("cell pin directions short", section::CELL_PIN_DIR, |f| {
            f.cell_pin_dir.to_mut().pop();
        }),
        ("pin name repeated in a cell", section::CELL_PIN_NAME, |f| {
            f.cell_pin_name.to_mut()[9] = 6
        }),
        ("port direction code", section::PORTS, |f| {
            f.ports.to_mut()[0].dir = 3
        }),
        ("port bus flag", section::PORTS, |f| {
            f.ports.to_mut()[0].is_bus = 2
        }),
        ("port padding", section::PORTS, |f| {
            f.ports.to_mut()[1]._pad = 1
        }),
        ("scalar port with a range", section::PORTS, |f| {
            f.ports.to_mut()[0].msb = 1
        }),
        ("reserved port bound", section::PORTS, |f| {
            f.ports.to_mut()[1].lsb = SCALAR
        }),
        ("bus wider than the widest range", section::PORTS, |f| {
            f.ports.to_mut()[1].msb = MAX_RANGE_WIDTH as i32
        }),
        ("bus wider than its bits", section::PORT_BIT_NET, |f| {
            f.ports.to_mut()[1].msb = 2
        }),
        ("port bit out of range", section::PORT_BIT_NET, |f| {
            f.port_bit_net.to_mut()[0] = 99
        }),
        ("port bits out of order", section::PORT_BIT_NET, |f| {
            f.port_bit_net.to_mut().swap(1, 2)
        }),
        ("port bit rows end late", section::PORT_BIT_START, |f| {
            f.port_bit_start.to_mut()[3] = 6
        }),
        ("anonymous net with an index", section::NETS, |f| {
            f.nets.to_mut()[6].index = 0
        }),
        ("repeated net", section::NETS, |f| {
            f.nets.to_mut()[7].index = 0
        }),
        ("scalar and indexed net of one name", section::NETS, |f| {
            f.nets.to_mut()[8].index = SCALAR
        }),
        ("extra net of a port's name", section::NETS, |f| {
            f.nets.to_mut()[6] = NetRec { name: 16, index: 5 }
        }),
        ("net named like a scalar port", section::NETS, |f| {
            f.nets.to_mut()[6] = NetRec {
                name: 15,
                index: SCALAR,
            }
        }),
        ("instance cell out of range", section::INSTANCES, |f| {
            f.instances.to_mut()[0].cell = 6
        }),
        ("repeated instance name", section::INSTANCES, |f| {
            f.instances.to_mut()[3].name = 20
        }),
        ("instance pin row decreases", section::INST_PIN_START, |f| {
            f.inst_pin_start.to_mut()[1] = 6
        }),
        ("cell pin out of range", section::PIN_CELLPIN, |f| {
            f.pin_cellpin.to_mut()[1] = 2
        }),
        ("cell pin connected twice", section::PIN_CELLPIN, |f| {
            f.pin_cellpin.to_mut()[3] = 0
        }),
        ("pin net out of range", section::PIN_NET, |f| {
            f.pin_net.to_mut()[0] = 99
        }),
        ("constant on the left", section::ASSIGNS, |f| {
            f.assigns.to_mut()[0].lhs = CONST0
        }),
        ("assignment net out of range", section::ASSIGNS, |f| {
            f.assigns.to_mut()[1].rhs = 1000
        }),
        ("net pin rows short", section::NET_PIN_START, |f| {
            f.net_pin_start.to_mut().pop();
        }),
        ("net pin missing", section::NET_PINS, |f| {
            f.net_pins.to_mut().pop();
            *f.net_pin_start.to_mut().last_mut().unwrap() -= 1;
            f.net_pin_start.to_mut()[7] -= 1;
            f.net_pin_start.to_mut()[8] -= 1;
        }),
        ("net pins out of order", section::NET_PINS, |f| {
            f.net_pins.to_mut().swap(4, 5)
        }),
        ("pin listed under another net", section::NET_PINS, |f| {
            f.net_pins.to_mut()[0] = 2
        }),
    ];
    for (what, id, corrupt) in cases {
        let mut c = f.clone();
        corrupt(&mut c);
        let e = c.validate().expect_err(what);
        assert_eq!(e.section, *id, "{what}: {e}");
        assert_eq!(c.thaw().unwrap_err(), e, "{what}");
    }
}

/// Appends instances without connections of cell 0 to `f` until it has `n`; their names are
/// new strings, which come last in the string table.
fn add_idle_instances(f: &mut FrozenNetlist<'static>, n: usize) {
    for k in f.instances.len()..n {
        let name = f.str_offs.len() as u32 - 1;
        f.str_bytes
            .to_mut()
            .extend_from_slice(format!("u{k}").as_bytes());
        let end = f.str_bytes.len() as u32;
        f.str_offs.to_mut().push(end);
        f.instances.to_mut().push(InstRec { name, cell: 0 });
        let pins = *f.inst_pin_start.last().unwrap();
        f.inst_pin_start.to_mut().push(pins);
    }
    f.module.to_mut()[module_entry::N_INSTS] = n as u32;
}

// CORE-REQ-0137: thawing creates every pin of every instance, connected or not, so their number
// is bounded by the size of the tables (one pin per byte, or 2^24 for smaller tables): a wide
// cell with many instances and few connections cannot make a short file exhaust memory.
#[test]
fn pins_of_all_instances_are_bounded() {
    let mut g = GateNetlist::new("m").unwrap();
    let pins: Vec<(String, PinDir)> = (0..4096)
        .map(|k| (format!("P{k}"), PinDir::Input))
        .collect();
    let wide = g.add_cell_type("wide", &pins).unwrap();
    g.add_instance("u0", wide).unwrap();
    let mut f = g.freeze();
    // 4096 instances of 4096 pins: 2^24 pins, the bound of tables of less than 2^24 bytes.
    add_idle_instances(&mut f, 4096);
    f.validate().unwrap();
    add_idle_instances(&mut f, 4097);
    let bytes: usize = f.sections().iter().map(|s| s.2.len()).sum();
    let e = f.validate().unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "netlist section 12 (INSTANCES): instances 0 to 4096 have 16781312 pins, connected \
             or not, more than the 16777216 that tables of {bytes} bytes may have (one per \
             byte, at least 16777216, at most 4294967295)"
        )
    );
    assert_eq!(f.thaw().unwrap_err(), e);
}

// CORE-REQ-0136: thawing takes time in proportion to the tables, also for a cell with very many
// pins.
#[test]
fn wide_cells_thaw() {
    let mut g = GateNetlist::new("m").unwrap();
    let pins: Vec<(String, PinDir)> = (0..100_000)
        .map(|k| (format!("P{k}"), PinDir::Input))
        .collect();
    let wide = g.add_cell_type("wide", &pins).unwrap();
    let u = g.add_instance("u", wide).unwrap();
    g.connect(u, 99_999, Conn::Const(true)).unwrap();
    let f = g.freeze();
    assert_eq!(f.thaw().unwrap().freeze(), f);
}

/// Changes one section of `f` at random: a byte of an element, the length, or the order.
fn corrupt<T: bytemuck::Pod>(v: &mut Cow<'_, [T]>, rng: &mut Rng) {
    let v = v.to_mut();
    match rng.below(6) {
        0 | 1 if !v.is_empty() => {
            let bytes = bytemuck::cast_slice_mut::<T, u8>(v);
            let k = rng.below(bytes.len());
            bytes[k] = match rng.below(4) {
                0 => bytes[k].wrapping_add(1),
                1 => bytes[k].wrapping_sub(1),
                2 => rng.below(4) as u8,
                _ => rng.next_u64() as u8,
            };
        }
        2 => {
            v.pop();
        }
        3 if !v.is_empty() => {
            let e = v[rng.below(v.len())];
            v.push(e);
        }
        4 if v.len() > 1 => {
            let (a, b) = (rng.below(v.len()), rng.below(v.len()));
            v.swap(a, b);
        }
        _ => {
            let e = T::zeroed();
            v.insert(rng.below(v.len() + 1), e);
        }
    }
}

// CORE-REQ-0135, 0136: random damage to random sections never panics; whatever still validates
// is canonical, so it thaws and freezes back unchanged.
#[test]
fn random_corruptions() {
    let f = sample().freeze();
    let mut rng = Rng::new(11);
    let mut accepted = 0;
    for _ in 0..20_000 {
        let mut c = f.clone();
        for _ in 0..1 + rng.below(2) {
            match rng.below(18) {
                0 => corrupt(&mut c.str_bytes, &mut rng),
                1 => corrupt(&mut c.str_offs, &mut rng),
                2 => corrupt(&mut c.module, &mut rng),
                3 => corrupt(&mut c.cells, &mut rng),
                4 => corrupt(&mut c.cell_pin_start, &mut rng),
                5 => corrupt(&mut c.cell_pin_name, &mut rng),
                6 => corrupt(&mut c.cell_pin_dir, &mut rng),
                7 => corrupt(&mut c.ports, &mut rng),
                8 => corrupt(&mut c.port_bit_start, &mut rng),
                9 => corrupt(&mut c.port_bit_net, &mut rng),
                10 => corrupt(&mut c.nets, &mut rng),
                11 => corrupt(&mut c.instances, &mut rng),
                12 => corrupt(&mut c.inst_pin_start, &mut rng),
                13 => corrupt(&mut c.pin_cellpin, &mut rng),
                14 => corrupt(&mut c.pin_net, &mut rng),
                15 => corrupt(&mut c.assigns, &mut rng),
                16 => corrupt(&mut c.net_pin_start, &mut rng),
                _ => corrupt(&mut c.net_pins, &mut rng),
            }
        }
        match c.validate() {
            Ok(()) => {
                accepted += 1;
                let g = c.thaw().unwrap();
                assert_consistent(&g);
                assert_eq!(g.freeze(), c);
            }
            Err(e) => assert!(!e.to_string().is_empty()),
        }
    }
    // Some damage keeps the tables valid (a renamed string, a changed bus index).
    assert!(accepted > 0 && accepted < 5_000, "{accepted} accepted");
}

// CORE-REQ-0135, 0110, 0111: a string is checked against the rules of every name it is: as a
// cell or pin name any printable character will do, as a net or instance name the segments and
// escapes must be valid too.
#[test]
fn strings_follow_the_rules_of_every_use() {
    use PinDir::{Input as I, Output as O};
    let mut g = GateNetlist::new("m").unwrap();
    let c = g.add_cell_type("w", &[("z", I), ("y", O)]).unwrap();
    let only_cell = g.freeze();
    // The net shares the cell's name, the instance the pin's.
    g.add_net(Some("w"), None).unwrap();
    g.add_instance("z", c).unwrap();
    let shared = g.freeze();
    assert_eq!(strings(&shared), ["m", "w", "z", "y"]);
    let mut f = only_cell.clone();
    overwrite_string(&mut f, 1, b"/");
    overwrite_string(&mut f, 2, b"\\");
    f.validate().unwrap();
    assert_eq!(f.thaw().unwrap().freeze(), f);
    let mut f = shared.clone();
    overwrite_string(&mut f, 1, b"/");
    let e = f.validate().unwrap_err();
    assert_eq!(
        e.to_string(),
        "netlist section 11 (NETS): net 0: name '/' (string 1): the '/' at byte 0 leaves an \
         empty segment"
    );
    assert_eq!(f.thaw().unwrap_err(), e);
    let mut f = shared;
    overwrite_string(&mut f, 2, b"\\");
    let e = f.validate().unwrap_err();
    assert_eq!(
        e.to_string(),
        "netlist section 12 (INSTANCES): instance 0: name '\\' (string 2): the backslash at \
         byte 0 is not followed by '/' or '\\'"
    );
    // Names with valid segments and escapes are accepted.
    let mut g = GateNetlist::new("m").unwrap();
    g.add_net(Some(r"u1/a\/b\\c"), Some(2)).unwrap();
    g.add_port(r"p\/q", PortDir::Output, None).unwrap();
    let f = g.freeze();
    f.validate().unwrap();
    assert_eq!(strings(&f), ["m", r"p\/q", r"u1/a\/b\\c"]);
    assert_eq!(f.thaw().unwrap().freeze(), f);
}

// CORE-REQ-0135, 0138: values read from the tables are bounded before any arithmetic, so no sum
// overflows on a 32-bit target, where the index u32::MAX is usize::MAX; the helpers take such
// values on any target. `string` never panics.
#[test]
fn table_values_never_overflow() {
    let f = sample().freeze();
    for k in [u32::MAX, ANONYMOUS, CONST0, 23] {
        assert_eq!(f.string(k).unwrap_err().section, section::STR_OFFS, "{k}");
    }
    assert_eq!(frozen::string_bytes(&f.str_offs, usize::MAX), None);
    assert_eq!(frozen::string_bytes(&[0, 3], 0), Some(0..3));
    assert_eq!(frozen::string_bytes(&[0, 3], 1), None);
    let top = usize::MAX - 2..usize::MAX;
    assert_eq!(validate::cell_pin_slot(&top, u32::MAX), None);
    assert_eq!(validate::cell_pin_slot(&top, 2), None);
    assert_eq!(validate::cell_pin_slot(&top, 1), Some(usize::MAX - 1));
    assert_eq!(validate::cell_pin_slot(&(4..4), 0), None);
    let mut c = f.clone();
    c.pin_cellpin.to_mut()[0] = u32::MAX;
    assert_eq!(c.validate().unwrap_err().section, section::PIN_CELLPIN);
    for name in [ANONYMOUS, u32::MAX - 1] {
        let mut c = f.clone();
        c.ports.to_mut()[0].name = name;
        assert_eq!(c.validate().unwrap_err().section, section::PORTS);
        let mut c = f.clone();
        c.instances.to_mut()[0].name = name;
        assert_eq!(c.validate().unwrap_err().section, section::INSTANCES);
    }
}

// CORE-REQ-0138: the accessors assume validated tables and panic on others rather than wrap an
// index around or read another cell's pin.
#[test]
#[should_panic(expected = "the instance pin rows do not start at 0")]
fn pin_instance_of_unvalidated_rows() {
    let mut f = sample().freeze();
    f.inst_pin_start.to_mut()[0] = 1;
    f.pin_instance(0);
}

#[test]
#[should_panic(expected = "index out of bounds")]
fn pin_dir_outside_its_cell() {
    let mut f = sample().freeze();
    // Instance 0 is an inverter: cell pin 2 is the first pin of the next cell.
    f.pin_cellpin.to_mut()[0] = 2;
    f.pin_dir(0);
}
