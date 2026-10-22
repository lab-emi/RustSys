// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Sources other than RTL (`docs/specs/synth/inputs.md`): the lowering of BLIF, AIGER and
//! gate-level netlists into the design, checked by simulation of the design against known
//! answers, and the errors of each rule.

use rusteda_formats::liberty::Library;
use rustsys::aig::{Aig, Lit};
use rustsys::elab::{Design, StateKind};
use rustsys::ingest::{SourceFormat, format_of, lower_sources};
use rustsys::{Effort, Goal, InitialValues, Sources, SynthOptions, synthesize};
use rustsys_fixtures::liberty::{Variant, library};

fn texts(v: &[(&str, &str)]) -> Sources {
    Sources::Texts(
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    )
}

fn rich() -> Library {
    rusteda_formats::liberty::read_text(&library(Variant::Rich), "wt_rich.lib").unwrap()
}

fn lower(v: &[(&str, &str)], opts: &SynthOptions) -> Result<(Design, Vec<String>), Vec<String>> {
    lower_sources(&texts(v), opts)
}

fn ok(v: &[(&str, &str)], opts: &SynthOptions) -> (Design, Vec<String>) {
    lower(v, opts).unwrap_or_else(|e| panic!("{e:?}"))
}

/// The code of the first message of a failure, and the message.
fn fails(v: &[(&str, &str)], opts: &SynthOptions) -> (String, String) {
    let e = lower(v, opts).expect_err("an error");
    let m = e[0].clone();
    let code = m
        .find('[')
        .and_then(|a| {
            m[a + 1..]
                .find(']')
                .map(|b| m[a + 1..a + 1 + b].to_string())
        })
        .unwrap_or_default();
    (code, m)
}

fn code(v: &[(&str, &str)], opts: &SynthOptions) -> String {
    fails(v, opts).0
}

/// The values of all graph nodes for one assignment of the graph inputs.
fn sim(d: &Design, inputs: &[bool]) -> Vec<u64> {
    assert_eq!(inputs.len(), d.aig.num_inputs());
    let words: Vec<u64> = inputs.iter().map(|&b| if b { 1 } else { 0 }).collect();
    d.simulate(&words)
}

fn val(s: &[u64], l: Lit) -> bool {
    Aig::lit_value(s, l) & 1 == 1
}

fn bits(m: u32, n: usize) -> Vec<bool> {
    (0..n).map(|k| m >> k & 1 == 1).collect()
}

/// The literal of input port `name`.
fn input(d: &Design, name: &str) -> Lit {
    let p = d.port(name).unwrap_or_else(|| panic!("no port {name}"));
    assert!(p.input);
    p.bits[0]
}

fn port_names(d: &Design) -> Vec<(String, bool)> {
    d.ports.iter().map(|p| (p.name.clone(), p.input)).collect()
}

// SYNTH-REQ-0900: formats by extension, ignoring letter case; one format per run; one AIGER
// file; one gate-level netlist.
#[test]
fn formats() {
    assert_eq!(SourceFormat::of_name("dir/A.BLIF"), SourceFormat::Blif);
    assert_eq!(SourceFormat::of_name("x.aag"), SourceFormat::Aiger);
    assert_eq!(SourceFormat::of_name("x.AIG"), SourceFormat::Aiger);
    assert_eq!(SourceFormat::of_name("x.sv"), SourceFormat::SystemVerilog);
    assert_eq!(SourceFormat::of_name("blif"), SourceFormat::SystemVerilog);
    let f = |v: &[(&str, &str)], gate: bool| format_of(&texts(v), gate);
    assert_eq!(
        f(&[("a.blif", ""), ("b.Blif", "")], false),
        Ok(SourceFormat::Blif)
    );
    assert!(
        f(&[("a.blif", ""), ("b.sv", "")], false)
            .unwrap_err()
            .starts_with("error[S0910]")
    );
    assert!(
        f(&[("a.aag", ""), ("b.aag", "")], false)
            .unwrap_err()
            .starts_with("error[S0910]")
    );
    assert!(
        f(&[("a.v", ""), ("b.v", "")], true)
            .unwrap_err()
            .starts_with("error[S0910]")
    );
    assert_eq!(f(&[("n.v", "")], true), Ok(SourceFormat::GateNetlist));
    assert_eq!(f(&[], false), Ok(SourceFormat::SystemVerilog));
}

// SYNTH-REQ-0901: the options of SystemVerilog sources are rejected for other formats.
#[test]
fn systemverilog_options() {
    let src = [(
        "m.blif",
        ".model m\n.inputs a\n.outputs y\n.names a y\n1 1\n.end\n",
    )];
    for o in [
        SynthOptions {
            defines: vec![("X".into(), String::new())],
            ..SynthOptions::default()
        },
        SynthOptions {
            params: vec![("W".into(), "2".into())],
            ..SynthOptions::default()
        },
        SynthOptions {
            assume_full_case: true,
            ..SynthOptions::default()
        },
        SynthOptions {
            assume_parallel_case: true,
            ..SynthOptions::default()
        },
    ] {
        assert_eq!(code(&src, &o), "S0911");
    }
    // Include directories have no meaning and are not read.
    let o = SynthOptions {
        include_dirs: vec!["inc".into()],
        no_predefined_macros: true,
        ..SynthOptions::default()
    };
    ok(&src, &o);
}

const LOGIC: &str = ".model logic\n.inputs a b c\n.outputs y z k\n\
     .names a b c y\n11- 1\n--1 1\n.names a b z\n11 0\n.names k\n1\n.end\n";

// SYNTH-REQ-0902, SYNTH-REQ-0903, SYNTH-REQ-0908: module name, scalar ports in order, and the
// functions of covers.
#[test]
fn blif_logic() {
    let (d, w) = ok(&[("l.blif", LOGIC)], &SynthOptions::default());
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(d.top, "logic");
    assert_eq!(
        port_names(&d),
        [
            ("a".to_string(), true),
            ("b".into(), true),
            ("c".into(), true),
            ("y".into(), false),
            ("z".into(), false),
            ("k".into(), false)
        ]
    );
    assert!(d.ports.iter().all(|p| p.scalar && p.width == 1));
    for m in 0..8 {
        let x = bits(m, 3);
        let s = sim(&d, &x);
        let out = |n: &str| val(&s, d.port(n).unwrap().bits[0]);
        assert_eq!(out("y"), x[0] && x[1] || x[2], "{m}");
        assert_eq!(out("z"), !(x[0] && x[1]), "{m}");
        assert!(out("k"));
    }
}

const SEQ: &str = ".model seq\n.inputs d c clk\n.outputs q1 q2 q3 q4 q5 q6\n\
     .latch d q1 0\n.latch d q2 re c 1\n.latch d q3 fe c 2\n.latch d q4 ah c\n\
     .latch d q5 al c 3\n.latch d q6 re NIL 1\n.end\n";

// SYNTH-REQ-0904, SYNTH-REQ-0905, SYNTH-REQ-0906, SYNTH-REQ-0907: latch types, the global clock
// (named clk_1 as clk is taken), the initial-value port and the asynchronous initial values.
#[test]
fn blif_latches() {
    let (d, _) = ok(&[("s.blif", SEQ)], &SynthOptions::default());
    let names: Vec<String> = port_names(&d).into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        [
            "d", "c", "clk", "clk_1", "init", "q1", "q2", "q3", "q4", "q5", "q6"
        ]
    );
    let (c, g, i) = (input(&d, "c"), input(&d, "clk_1"), input(&d, "init"));
    let want = [
        ("q1", StateKind::Flop, g, Some((i, false))),
        ("q2", StateKind::Flop, c, Some((i, true))),
        ("q3", StateKind::Flop, !c, None),
        ("q4", StateKind::Latch, c, None),
        ("q5", StateKind::Latch, !c, None),
        ("q6", StateKind::Flop, g, Some((i, true))),
    ];
    assert_eq!(d.states.len(), 6);
    for (s, (n, k, clk, arst)) in d.states.iter().zip(want) {
        assert_eq!((s.name.as_str(), s.kind, s.clk, s.arst), (n, k, clk, arst));
        assert_eq!(s.d, input(&d, "d"));
        assert_eq!(d.port(n).unwrap().bits, [s.q]);
    }
}

// SYNTH-REQ-0906: under `arbitrary`, declared initial values are not implemented and a warning
// says how many.
#[test]
fn initial_values_arbitrary() {
    let o = SynthOptions {
        initial_values: InitialValues::Arbitrary,
        ..SynthOptions::default()
    };
    let (d, w) = ok(&[("s.blif", SEQ)], &o);
    assert!(d.port("init").is_none());
    assert!(d.states.iter().all(|s| s.arst.is_none()));
    assert_eq!(w.len(), 1);
    assert!(
        w[0].starts_with("warning[S0913]") && w[0].contains(" 3 "),
        "{w:?}"
    );
    // The initial-value port avoids the names of ports, states and the clock.
    let t = ".model m\n.inputs init d\n.outputs clk\n.latch d clk 1\n.end\n";
    let (d, _) = ok(&[("m.blif", t)], &SynthOptions::default());
    let names: Vec<String> = port_names(&d).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["init", "d", "clk_1", "init_1", "clk"]);
}

// SYNTH-REQ-0903, SYNTH-REQ-0907: port names are distinct and printable; a state may share its
// name only with the output that is the state itself.
#[test]
fn names() {
    let o = SynthOptions::default();
    assert_eq!(
        code(&[("m.blif", ".model m\n.inputs a\n.outputs a\n.end\n")], &o),
        "S0912"
    );
    assert_eq!(
        code(&[("m.aag", "aag 1 1 0 2 0\n2\n2\n3\no0 x\no1 x\n")], &o),
        "S0912"
    );
    assert_eq!(
        code(&[("m.aag", "aag 1 1 0 1 0\n2\n2\ni0 my sig\n")], &o),
        "S0912"
    );
    assert_eq!(
        code(&[("m.aag", "aag 2 1 1 1 0\n2\n4 2\n4\ni0 s\nl0 s\n")], &o),
        "S0914"
    );
    assert_eq!(
        code(&[("m.aag", "aag 2 1 1 1 0\n2\n4 2\n5\nl0 q\no0 q\n")], &o),
        "S0914"
    );
    let (d, _) = ok(&[("m.aag", "aag 2 1 1 1 0\n2\n4 2\n4\nl0 q\no0 q\n")], &o);
    assert_eq!(d.states[0].name, "q");
}

// SYNTH-REQ-0908, SYNTH-REQ-0909: signals without a driver read as 0, with the reader's warning
// or, for cell inputs, a warning `S0921`.
#[test]
fn undriven_signals_read_zero() {
    let t = ".model m\n.inputs a\n.outputs y z\n.names a b y\n1- 1\n-1 1\n.end\n";
    let (d, w) = ok(&[("m.blif", t)], &SynthOptions::default());
    assert!(w.iter().any(|m| m.starts_with("warning[B0007]")), "{w:?}");
    for x in [false, true] {
        let v = sim(&d, &[x]);
        assert_eq!(val(&v, d.port("y").unwrap().bits[0]), x);
        assert!(!val(&v, d.port("z").unwrap().bits[0]));
    }
    let o = SynthOptions {
        gate_libs: vec![rich()],
        ..SynthOptions::default()
    };
    let t = ".model m\n.inputs a\n.outputs y\n.gate wt_nand2_x1 A=a B=u Y=y\n.end\n";
    let (d, w) = ok(&[("m.blif", t)], &o);
    assert!(w.iter().any(|m| m.starts_with("warning[S0921]")), "{w:?}");
    let v = sim(&d, &[true]);
    assert!(val(&v, d.port("y").unwrap().bits[0]));
}

// SYNTH-REQ-0908: a combinational cycle names its signals.
#[test]
fn combinational_cycle() {
    let t = ".model m\n.inputs a\n.outputs y\n.names a w y\n11 1\n.names y w\n1 1\n.end\n";
    let (c, m) = fails(&[("m.blif", t)], &SynthOptions::default());
    assert_eq!(c, "S0915");
    assert!(m.contains('y') && m.contains('w'), "{m}");
}

// SYNTH-REQ-0902, SYNTH-REQ-0903, SYNTH-REQ-0904, SYNTH-REQ-0906, SYNTH-REQ-0907,
// SYNTH-REQ-0914: an AIGER graph with default names, an uninitialized latch, its module name from
// the file name or `--top`; the messages of the AIGER reader pass through.
#[test]
fn aiger_lowering() {
    let t = "aag 4 1 1 1 2\n2\n4 8 4\n6\n6 2 4\n8 3 4\n";
    let (d, _) = ok(&[("dir/counter.aag", t)], &SynthOptions::default());
    assert_eq!(d.top, "counter");
    let names: Vec<String> = port_names(&d).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["i0", "clk", "o0"]);
    let s = &d.states[0];
    assert_eq!(
        (s.name.as_str(), s.kind, s.clk, s.arst),
        ("l0", StateKind::Flop, input(&d, "clk"), None)
    );
    for m in 0..4 {
        let x = bits(m, 2);
        let v = sim(&d, &[x[0], false, x[1]]);
        assert_eq!(val(&v, d.port("o0").unwrap().bits[0]), x[0] && x[1], "{m}");
        assert_eq!(val(&v, s.d), !x[0] && x[1], "{m}");
    }
    let o = SynthOptions {
        top: Some("named".into()),
        ..SynthOptions::default()
    };
    let (d, _) = ok(
        &[("counter.aag", "aag 4 1 1 1 2\n2\n4 8 1\n6\n6 2 4\n8 3 4\n")],
        &o,
    );
    assert_eq!(d.top, "named");
    assert_eq!(d.states[0].arst, Some((input(&d, "init"), true)));
    // Binary AIGER as a text source and an AIGER file with properties.
    let (d, _) = ok(
        &[("b.aig", "aig 3 2 0 1 1\n6\n\u{2}\u{2}")],
        &SynthOptions::default(),
    );
    let v = sim(&d, &[true, true]);
    assert!(val(&v, d.port("o0").unwrap().bits[0]));
    assert_eq!(code(&[("p.aag", "aag 1 1 0 0 0 1\n2\n2\n")], &o), "G0010");
}

const CELLS: &str = ".model cells\n.inputs a b ci ck\n.outputs y s co q\n\
     .gate wt_nand2_x1 A=a B=b Y=n1\n.gate wt_inv_x1 A=n1 Y=y\n\
     .gate wt_fa_x1 A=a B=b CI=ci S=s CO=co\n.mlatch wt_dff_x1 D=y Q=q ck 1\n.end\n";

// SYNTH-REQ-0909, SYNTH-REQ-0911: `.gate` and `.mlatch` resolved against `--gate-lib` cells:
// multi-output cells, the clock pin of an `.mlatch`, its initial value.
#[test]
fn blif_cells() {
    let o = SynthOptions {
        gate_libs: vec![rich()],
        ..SynthOptions::default()
    };
    let (d, _) = ok(&[("c.blif", CELLS)], &o);
    let s = &d.states[0];
    assert_eq!(s.name, "q");
    assert_eq!(s.clk, input(&d, "ck"));
    assert_eq!(s.arst, Some((input(&d, "init"), true)));
    for m in 0..8 {
        let x = bits(m, 3);
        let v = sim(&d, &[x[0], x[1], x[2], false, false, false]);
        let out = |n: &str| val(&v, d.port(n).unwrap().bits[0]);
        assert_eq!(out("y"), x[0] && x[1]);
        assert_eq!(out("s"), x[0] ^ x[1] ^ x[2]);
        assert_eq!(out("co"), (x[0] && x[1]) || (x[2] && (x[0] ^ x[1])));
        assert_eq!(val(&v, s.d), x[0] && x[1]);
    }
}

// SYNTH-REQ-0909: cells without libraries, unknown cells and pins, unconnected inputs, bad
// `.mlatch` instances and signal drivers.
#[test]
fn blif_cell_errors() {
    let o = SynthOptions {
        gate_libs: vec![rich()],
        ..SynthOptions::default()
    };
    let head = ".model m\n.inputs a b ck\n.outputs y\n";
    assert_eq!(
        fails(&[("m.blif", CELLS)], &SynthOptions::default()),
        (
            "S0917".to_string(),
            "error[S0917]: m.blif:4:1: instance of library cell wt_nand2_x1: .gate and .mlatch \
             need the libraries of their cells (--gate-lib)"
                .to_string()
        )
    );
    for (body, want) in [
        (".gate wt_nope_x1 A=a Y=y\n", "S0917"),
        (".gate wt_inv_x1 X=a Y=y\n", "S0917"),
        (".gate wt_nand2_x1 A=a Y=y\n", "S0917"),
        (".mlatch wt_inv_x1 A=a Y=y ck\n", "S0917"),
        (".mlatch wt_dff_x1 D=a Q=y CK=ck ck\n", "S0917"),
        (".gate wt_dff_x1 D=a CK=ck QN=y\n", "S0917"),
        (".gate wt_inv_x1 A=a Y=y\n.names a y\n1 1\n", "S0918"),
    ] {
        let t = format!("{head}{body}.end\n");
        assert_eq!(code(&[("m.blif", &t)], &o), want, "{body}");
    }
}

const NETLIST: &str = "module top(a, b, ck, rn, sn, g, y, z, w);\n\
     input [1:0] a; input b, ck, rn, sn, g; output y; output [1:0] z; output w;\n\
     wire n1, q1, q2;\n\
     wt_nand2_x1 g1 (.A(a[0]), .B(a[1]), .Y(n1));\n\
     wt_dffr_x1 \\r1[0] (.D(n1), .CK(ck), .RN(rn), .Q(q1), .QN());\n\
     wt_dffs_x1 r2 (.D(b), .CK(ck), .SN(sn), .Q(q2));\n\
     wt_lat_x1 l3 (.D(a[1]), .G(g), .Q(w));\n\
     assign y = q1;\n\
     assign z[0] = q2;\n\
     assign z[1] = 1'b1;\n\
     endmodule\n";

fn gate_opts() -> SynthOptions {
    SynthOptions {
        gate_libs: vec![rich()],
        gate_netlist: true,
        ..SynthOptions::default()
    }
}

// SYNTH-REQ-0903, SYNTH-REQ-0910, SYNTH-REQ-0911, SYNTH-REQ-0912: a gate-level netlist with
// vector ports, assignments, constants, a clear, a preset and a latch.
#[test]
fn gate_netlist() {
    let (d, _) = ok(&[("n.v", NETLIST)], &gate_opts());
    assert_eq!(d.top, "top");
    let a = d.port("a").unwrap();
    assert!(!a.scalar && a.width == 2);
    assert!(d.port("z").is_some_and(|p| !p.input && p.width == 2));
    let names: Vec<&str> = d.states.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["r1[0]", "r2", "l3"]);
    let (r1, r2, l3) = (&d.states[0], &d.states[1], &d.states[2]);
    assert_eq!(r1.arst, Some((!input(&d, "rn"), false)));
    assert_eq!(r2.arst, Some((!input(&d, "sn"), true)));
    assert_eq!((r1.kind, l3.kind), (StateKind::Flop, StateKind::Latch));
    assert_eq!(l3.clk, input(&d, "g"));
    for m in 0..64 {
        // a[0], a[1], b, ck, rn, sn, g, then the states r1, r2, l3.
        let mut x = bits(m, 6);
        x.extend([false, m & 1 == 0, m & 2 == 0, m & 4 == 0]);
        let v = sim(&d, &x);
        assert_eq!(val(&v, r1.d), !(x[0] && x[1]), "{m}");
        assert_eq!(val(&v, r2.d), x[2]);
        assert_eq!(val(&v, l3.d), x[1]);
        let z = &d.port("z").unwrap().bits;
        assert_eq!(val(&v, d.port("y").unwrap().bits[0]), x[7]);
        assert_eq!((val(&v, z[0]), val(&v, z[1])), (x[8], true));
        assert_eq!(val(&v, d.port("w").unwrap().bits[0]), x[9]);
    }
}

const TINY: &str = "library (tiny) {\n\
     cell (TBUF) { area : 1; pin (A) { direction : input; } pin (E) { direction : input; }\n\
       pin (Y) { direction : output; function : \"A\"; three_state : \"!E\"; } }\n\
     cell (NOFN) { area : 1; pin (A) { direction : input; } pin (Y) { direction : output; } }\n\
     cell (BUF) { area : 1; pin (A) { direction : input; } pin (Y) { direction : output; function : \"A\"; } }\n\
     }\n";

// SYNTH-REQ-0902, SYNTH-REQ-0903, SYNTH-REQ-0910, SYNTH-REQ-0911, SYNTH-REQ-0914: the errors
// of gate-level netlists (S0915 to S0920), the messages of the structural reader passed through,
// and state outputs that feed their own cells.
#[test]
fn gate_netlist_errors() {
    let o = gate_opts();
    let m = |body: &str| format!("module top(a, b, y);\ninput a, b; output y;\n{body}endmodule\n");
    for (body, want) in [
        ("wt_inv_x1 i (.A(u), .Y(y));\n", "S0918"),
        (
            "wt_inv_x1 i (.A(a), .Y(y));\nwt_inv_x1 j (.A(b), .Y(y));\n",
            "S0918",
        ),
        ("wt_inv_x1 i (.A(a), .Y(y));\nassign y = b;\n", "S0918"),
        ("wt_inv_x1 i (.A(a), .Y(a));\nassign y = b;\n", "S0918"),
        ("wt_nand2_x1 i (.A(a), .Y(y));\n", "S0917"),
        (
            "wt_dffrs_x1 r (.D(a), .CK(b), .RN(a), .SN(b), .Q(y));\n",
            "S0919",
        ),
    ] {
        assert_eq!(code(&[("n.v", &m(body))], &o), want, "{body}");
    }
    let inout = "module top(a, y);\ninout a; output y;\nassign y = 1'b0;\nendmodule\n";
    assert_eq!(code(&[("n.v", inout)], &o), "S0916");
    let named = SynthOptions {
        top: Some("other".into()),
        ..gate_opts()
    };
    assert_eq!(
        code(&[("n.v", &m("wt_inv_x1 i (.A(a), .Y(y));\n"))], &named),
        "S0920"
    );
    // A state output that feeds the cell's own data input is no cycle: it reads the state, not
    // the data pin.
    let toggle = m("wt_dff_x1 r (.D(n), .CK(a), .Q(y), .QN());\nwt_inv_x1 i (.A(y), .Y(n));\n");
    let (d, _) = ok(&[("n.v", &toggle)], &o);
    assert_eq!(d.states[0].d, !d.states[0].q);
    let blif = ".model t\n.inputs ck\n.outputs q\n.gate wt_dff_x1 D=n CK=ck Q=q\n\
                .gate wt_inv_x1 A=q Y=n\n.end\n";
    let o2 = SynthOptions {
        gate_libs: vec![rich()],
        ..SynthOptions::default()
    };
    let (d, _) = ok(&[("t.blif", blif)], &o2);
    assert_eq!(d.states[0].d, !d.states[0].q);
    let tiny = rusteda_formats::liberty::read_text(TINY, "tiny.lib").unwrap();
    let t = SynthOptions {
        gate_libs: vec![tiny],
        gate_netlist: true,
        ..SynthOptions::default()
    };
    assert_eq!(
        code(&[("n.v", &m("TBUF i (.A(a), .E(b), .Y(y));\n"))], &t),
        "S0917"
    );
    assert_eq!(
        code(&[("n.v", &m("NOFN i (.A(a), .Y(y));\n"))], &t),
        "S0917"
    );
    // Unknown cells are reported by the structural reader.
    assert_eq!(
        code(&[("n.v", &m("NOPE i (.A(a), .Y(y));\n"))], &t),
        "V0302"
    );
    // A cycle through cells.
    let cyc = m("BUF i (.A(n), .Y(y));\nBUF j (.A(y), .Y(n));\n");
    assert_eq!(code(&[("n.v", &cyc)], &t), "S0915");
}

// SYNTH-REQ-0913: synthesis of a BLIF source records the source in its report.
#[test]
fn report_fields() {
    let lib = rich();
    let opts = SynthOptions {
        effort: Effort::Low,
        goal: Goal::Area,
        ..SynthOptions::default()
    };
    let out = synthesize(
        &texts(&[("s.blif", SEQ)]),
        std::slice::from_ref(&lib),
        &opts,
    )
    .unwrap_or_else(|e| panic!("{:?}", e.messages));
    let r = &out.report;
    assert_eq!(r.top, "seq");
    assert_eq!(r.source_format.as_deref(), Some("blif"));
    assert_eq!(r.initial_values_mode.as_deref(), Some("reset"));
    assert_eq!(r.clock_port.as_deref(), Some("clk_1"));
    assert_eq!(r.init_port.as_deref(), Some("init"));
    assert_eq!(
        r.initial_values,
        [
            ("q1".to_string(), false),
            ("q2".to_string(), true),
            ("q6".to_string(), true)
        ]
    );
    assert_eq!(r.state_bits_rtl, 6);
    assert_eq!(r.sequential_cells, 6);
    let phases: Vec<&str> = r.phase_s.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(phases, ["read", "elaborate", "optimize", "map", "emit"]);
    assert!(out.text.contains("input wire init,"), "{}", out.text);
    let json = serde_json::to_value(r).unwrap();
    assert_eq!(json["source_format"], "blif");
    // A SystemVerilog report has none of the fields.
    let sv = synthesize(
        &texts(&[(
            "t.sv",
            "module t(input a, output y); assign y = !a; endmodule\n",
        )]),
        std::slice::from_ref(&lib),
        &opts,
    )
    .unwrap();
    let json = serde_json::to_value(&sv.report).unwrap();
    assert!(json.get("source_format").is_none() && json.get("initial_values").is_none());
}
