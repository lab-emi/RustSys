// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Reader acceptance and rejection, and conversion round trips (VERILOG-REQ-0300,
//! VERILOG-REQ-0301).

use rusteda_core::netlist::{Conn, GateNetlist, MAX_RANGE_WIDTH, PinDir, PortDir};

use super::super::write::{NlDir, NlInstance, NlModule, NlPort, NlRef, write_netlist};
use super::{from_gate_netlist, read_structural, to_gate_netlist};

/// Cells of the tests, pins in library order.
pub(super) fn cells(name: &str) -> Option<Vec<(String, PinDir)>> {
    use PinDir::{Inout, Input as I, Output as O};
    let pins: &[(&str, PinDir)] = match name {
        "inv" => &[("A", I), ("Y", O)],
        "nand2" => &[("A", I), ("B", I), ("Y", O)],
        "dff" => &[("D", I), ("CK", I), ("Q", O), ("QN", O)],
        "pad" => &[("PAD", Inout), ("EN", I)],
        "tie" => &[("H", O)],
        // Output first, as some libraries list it.
        "ao" => &[("Y", O), ("A", I), ("B", I)],
        _ => return None,
    };
    Some(pins.iter().map(|(n, d)| (n.to_string(), *d)).collect())
}

pub(super) fn read(text: &str) -> GateNetlist {
    read_structural(text, "t.v", &cells).unwrap_or_else(|e| panic!("{e}"))
}

/// Rendered diagnostics of a rejected text.
pub(super) fn errors(text: &str) -> Vec<String> {
    match read_structural(text, "t.v", &cells) {
        Ok(_) => panic!("accepted:\n{text}"),
        Err(e) => e.to_string().lines().map(str::to_string).collect(),
    }
}

/// `instance.pin=target` per connected pin, in instance and cell pin order.
pub(super) fn connections(g: &GateNetlist) -> Vec<String> {
    let mut out = Vec::new();
    for (_, i) in g.instances() {
        let cell = g.cell(i.cell).unwrap();
        for (k, &p) in i.pins.iter().enumerate() {
            let target = match g.pin(p).unwrap().conn {
                None => continue,
                Some(c) => conn(g, c),
            };
            let pin = g.resolve(cell.pins[k].name);
            out.push(format!("{}.{pin}={target}", g.resolve(i.name)));
        }
    }
    out
}

fn conn(g: &GateNetlist, c: Conn) -> String {
    match c {
        Conn::Const(v) => u8::from(v).to_string(),
        Conn::Net(n) => g.net_label(n),
    }
}

fn nets(g: &GateNetlist) -> Vec<String> {
    g.nets().map(|(id, _)| g.net_label(id)).collect()
}

fn assigns(g: &GateNetlist) -> Vec<String> {
    let a = g.assigns().iter();
    a.map(|a| format!("{}={}", g.net_label(a.lhs), conn(g, a.rhs)))
        .collect()
}

// The structural subset, with directives, comments, a source attribute, escaped and implicit
// names, constants and bit-selects of descending, ascending and negative ranges.
#[test]
fn reads_the_structural_subset() {
    let g = read(
        "`timescale 1ns/1ps
`define CELL inv
// A comment with (* keep *) and `include inside.
`celldefine
(* src = \"top.sv:1\" *)
module top(input wire clk, input [1:0] d, input [0:1] e, inout io, output y,
           output wire logic [2:0] q);
  wire n0, \\u.q[3] ;
  wire vectored [-1:-2] w;
  `CELL g0 (.A(d[1]), .Y(n0));
  nand2 g1 (.A(e[0]), .B(1'b1), .Y(\\u.q[3] ));
  dff r0 (.D(n0), .CK(clk), .Q(q[0]), .QN());
  inv g2 (.A(w[-2]), .Y(imp));
  pad p0 (.PAD(io), .EN(1'h0));
`ifdef NEVER
  inv dropped (.A(n0));
`endif
  assign q[2] = imp, q[1] = 1'b0;
  assign w = d;
  assign y = \\u.q[3] ;
  assign late = n0;
endmodule
`endcelldefine
",
    );
    assert_eq!(g.name(), "top");
    let ports: Vec<(&str, PortDir, Option<(i32, i32)>)> = g
        .ports()
        .iter()
        .map(|p| (g.resolve(p.name), p.dir, p.range))
        .collect();
    assert_eq!(
        ports,
        [
            ("clk", PortDir::Input, None),
            ("d", PortDir::Input, Some((1, 0))),
            ("e", PortDir::Input, Some((0, 1))),
            ("io", PortDir::Inout, None),
            ("y", PortDir::Output, None),
            ("q", PortDir::Output, Some((2, 0))),
        ]
    );
    assert_eq!(
        nets(&g),
        [
            "clk", "d[1]", "d[0]", "e[0]", "e[1]", "io", "y", "q[2]", "q[1]", "q[0]", "n0",
            "u.q[3]", "w[-1]", "w[-2]", "imp", "late"
        ]
    );
    assert_eq!(
        connections(&g),
        [
            "g0.A=d[1]",
            "g0.Y=n0",
            "g1.A=e[0]",
            "g1.B=1",
            "g1.Y=u.q[3]",
            "r0.D=n0",
            "r0.CK=clk",
            "r0.Q=q[0]",
            "g2.A=w[-2]",
            "g2.Y=imp",
            "p0.PAD=io",
            "p0.EN=0"
        ]
    );
    assert_eq!(
        assigns(&g),
        [
            "q[2]=imp",
            "q[1]=0",
            "w[-1]=d[1]",
            "w[-2]=d[0]",
            "y=u.q[3]",
            "late=n0"
        ]
    );
    let cells: Vec<&str> = g.cells().map(|(_, c)| g.resolve(c.name)).collect();
    assert_eq!(cells, ["inv", "nand2", "dff", "pad"]);
}

// Non-ANSI ports take the header order; a wire may repeat a port with its range.
#[test]
fn reads_non_ansi_ports() {
    let g = read(
        "module m(a, b, y);
  input [1:0] a;
  output y;
  wire [1:0] a;
  input b;
  wire y;
  nand2 u (.A(a[0]), .B(b), .Y(y));
endmodule
",
    );
    let ports: Vec<(&str, PortDir)> = g
        .ports()
        .iter()
        .map(|p| (g.resolve(p.name), p.dir))
        .collect();
    assert_eq!(
        ports,
        [
            ("a", PortDir::Input),
            ("b", PortDir::Input),
            ("y", PortDir::Output)
        ]
    );
    assert_eq!(nets(&g), ["a[1]", "a[0]", "b", "y"]);
    assert_eq!(connections(&g), ["u.A=a[0]", "u.B=b", "u.Y=y"]);
    let cases = [
        (
            "module m(a); input a; input [1:0] a; endmodule",
            "error[V0301]: t.v:1:35: port 'a' is declared twice",
        ),
        (
            "module m(a); input a; output c; endmodule",
            "error[V0301]: t.v:1:30: 'c' is not in the port list",
        ),
        (
            "module m(a, b); input a; endmodule",
            "error[V0301]: t.v:1:13: port 'b' has no direction declaration",
        ),
        (
            "module m(a); input a; wire [1:0] a; endmodule",
            "error[V0301]: t.v:1:34: wire 'a' does not repeat the range of port 'a'",
        ),
        // A net type completes a port's declaration (IEEE 1800-2017 clause 23.2.2.1).
        (
            "module m(a); input wire a; wire a; endmodule",
            "error[V0301]: t.v:1:33: port 'a' already has a net or variable type, which completes \
             its declaration, so it cannot be declared again as a wire (IEEE 1800-2017 clause \
             23.2.2.1)",
        ),
        (
            "module m(a); input a; wire a; wire a; endmodule",
            "error[V0301]: t.v:1:36: port 'a' already has a net or variable type",
        ),
        (
            "module m(a); wire a; input wire a; endmodule",
            "error[V0301]: t.v:1:19: port 'a' already has a net or variable type",
        ),
        (
            "module m(a[1:0]); endmodule",
            "error[V0300]: t.v:1:10: the port expression 'a[1:0]' is not supported",
        ),
        (
            "module m(a); output reg a; endmodule",
            "error[V0300]: t.v:1:14: a variable port is not supported",
        ),
    ];
    for (text, want) in cases {
        let e = errors(text);
        assert!(e[0].starts_with(want), "{text}\n{e:?}");
    }
}

// Every construct outside the subset is reported with its location, never skipped.
#[test]
fn rejects_what_it_cannot_represent() {
    let header = "module top(input a, input [1:0] b, output y);\n  wire n;\n";
    let cases: &[(&str, &str)] = &[
        (
            "assign y = 1'bx;",
            "V0300]: t.v:3:14: the x/z literal '1'bx'",
        ),
        (
            "assign y = 1'bz;",
            "V0300]: t.v:3:14: the x/z literal '1'bz'",
        ),
        (
            "assign y = a & n;",
            "V0300]: t.v:3:14: the expression 'a & n'",
        ),
        (
            "assign y = {a};",
            "V0300]: t.v:3:14: the concatenation '{a}'",
        ),
        (
            "assign y = b[1:1];",
            "V0300]: t.v:3:14: the part-select 'b[1:1]'",
        ),
        (
            "assign y = 2'b01;",
            "V0300]: t.v:3:14: the constant (only 1'b0 and 1'b1",
        ),
        ("assign y = 'b1;", "V0300]: t.v:3:14: the constant"),
        (
            "assign y = '1;",
            "V0300]: t.v:3:14: the unbased unsized literal",
        ),
        ("assign y = $random;", "V0300]: t.v:3:14: the function call"),
        (
            "assign y = p::c;",
            "V0300]: t.v:3:14: the package-scoped name",
        ),
        ("assign y = b[a];", "V0300]: t.v:3:16: the bit index 'a'"),
        (
            "inv u (.A(a), .Y(!n));",
            "V0300]: t.v:3:20: the expression '!n'",
        ),
        (
            "and g (y, a, n);",
            "V0300]: t.v:3:7: a gate primitive 'and'",
        ),
        (
            "inv u [1:0] (.A(a), .Y(n));",
            "V0300]: t.v:3:7: an instance array",
        ),
        ("inv u (a, y);", "V0300]: t.v:3:10: a positional connection"),
        (
            "inv u (.A, .Y(y));",
            "V0300]: t.v:3:11: the implicit connection '.A'",
        ),
        ("inv u (.*);", "V0300]: t.v:3:10: a '.*' connection"),
        (
            "inv #(1) u (.A(a), .Y(y));",
            "V0300]: t.v:3:7: a parameter value assignment",
        ),
        (
            "assign #1 y = a;",
            "V0300]: t.v:3:10: a delay on a continuous assignment",
        ),
        (
            "wire #2 m;",
            "V0300]: t.v:3:8: a delay on a net declaration",
        ),
        (
            "assign (strong0, strong1) y = a;",
            "V0300]: t.v:3:10: a drive strength on a continuous assignment",
        ),
        (
            "(* keep *) inv u (.A(a), .Y(y));",
            "V0300]: t.v:3:3: attribute 'keep'",
        ),
        (
            "(* src=\"x\", dont_touch *) inv u ();",
            "V0300]: t.v:3:3: attribute 'dont_touch'",
        ),
        (
            "`include \"cells.v\"",
            "V0300]: t.v:3:3: `include is not supported",
        ),
        (
            "`unconnected_drive pull1",
            "V0300]: t.v:3:3: `unconnected_drive is not supported",
        ),
        (
            "`pragma protect",
            "V0300]: t.v:3:3: `pragma is not supported",
        ),
        ("always @(a) n = a;", "V0300]: t.v:3:3: an always procedure"),
        (
            "initial begin end",
            "V0300]: t.v:3:11: an initial procedure",
        ),
        ("reg r;", "V0300]: t.v:3:3: a variable declaration"),
        ("tri t;", "V0300]: t.v:3:3: a 'tri' net declaration"),
        (
            "supply0 gnd;",
            "V0300]: t.v:3:3: a 'supply0' net declaration",
        ),
        (
            "wire m = a;",
            "V0300]: t.v:3:12: a net declaration assignment",
        ),
        ("wire signed s;", "V0300]: t.v:3:8: a signing keyword"),
        ("wire m [0:1];", "V0300]: t.v:3:11: an array of nets"),
        ("wire [W:0] m;", "V0300]: t.v:3:9: the range bound 'W'"),
        ("localparam W = 1;", "V0300]: t.v:3:14: a parameter"),
        ("foo u (.A(a));", "V0302]: t.v:3:3: unknown cell 'foo'"),
        (
            "inv u (.A(a), .Z(y));",
            "V0302]: t.v:3:18: cell 'inv' has no pin 'Z'",
        ),
        (
            "inv u (.A(a), .A(n));",
            "V0302]: t.v:3:18: pin 'A' of 'u' is connected twice",
        ),
        (
            "assign y = b[2];",
            "V0303]: t.v:3:14: bit 2 is outside the range [1:0] of 'b'",
        ),
        (
            "assign y = a[0];",
            "V0303]: t.v:3:14: 'a' is a scalar and has no bit 0",
        ),
        (
            "inv u (.A(b), .Y(y));",
            "V0303]: t.v:3:13: pin 'A' takes 1 bit, 'b' has 2 bits",
        ),
        (
            "assign y = b;",
            "V0303]: t.v:3:10: the right side has 2 bits, the left side 1 bit",
        ),
        ("assign y = m;", "V0301]: t.v:3:14: 'm' is not declared"),
        (
            "wire n;",
            "V0301]: t.v:3:8: 'n' is already declared as a wire at t.v:2:8",
        ),
        (
            "inv u (.A(a), .Y(m)); wire m;",
            "V0301]: t.v:3:30: 'm' is declared after its implicit declaration at t.v:3:20",
        ),
        (
            "inv n (.A(a), .Y(y));",
            "V0301]: t.v:3:7: 'n' is already declared as a wire",
        ),
        (
            "inv u (.A(u), .Y(y));",
            "V0301]: t.v:3:13: 'u' is an instance, not a net",
        ),
        (
            "wire [2147483646:0] w2;",
            "V0303]: t.v:3:9: range [2147483646:0] is wider than 16777216 bits",
        ),
        (
            "wire [0:-16777216] w2;",
            "V0303]: t.v:3:9: range [0:-16777216] is wider than 16777216 bits",
        ),
        // A based literal is unsigned: its negation wraps at its width (-2'd1 is 3).
        (
            "assign y = b[-2'd1];",
            "V0300]: t.v:3:16: the bit index '-2'd1' (only integer literals are accepted, with \
             a minus sign only before a decimal number)",
        ),
        (
            "wire [-'d1:0] w2;",
            "V0300]: t.v:3:9: the range bound '-'d1'",
        ),
        (
            "top u (.A(a));",
            "V0300]: t.v:3:3: an instance of the module 'top' itself (hierarchy)",
        ),
        (
            "`default_nettype none",
            "V0300]: t.v:3:3: `default_nettype inside a module is not supported",
        ),
    ];
    for (body, want) in cases {
        let text = format!("{header}  {body}\nendmodule\n");
        let e = errors(&text);
        assert!(
            e[0].starts_with(&format!("error[{want}")),
            "{body}\n  want error[{want}\n  got  {e:?}"
        );
        assert!(e[0].contains("structural netlist") || !want.starts_with("V0300"));
    }
}

#[test]
fn rejects_hierarchy_and_other_units() {
    let cases = [
        (
            "module a(); endmodule\nmodule b(); endmodule\n",
            "error[V0300]: t.v:2:8: a second module 'b' (hierarchy",
        ),
        (
            "package p; endpackage\nmodule m(); endmodule\n",
            "error[V0300]: t.v:1:1: a package",
        ),
        (
            "interface i; endinterface\n",
            "error[V0300]: t.v:1:1: an interface",
        ),
        (
            "module m #(parameter W = 1) (input a); endmodule\n",
            "error[V0300]: t.v:1:10: a parameter port list",
        ),
        (
            "module m(ref logic a); endmodule\n",
            "error[V0300]: t.v:1:10: a ref port",
        ),
        (
            "module m(output logic q); endmodule\n",
            "error[V0300]: t.v:1:10: the variable port 'q'",
        ),
        (
            "`default_nettype none\nmodule m(input wire a); inv u (.A(a), .Y(x)); endmodule\n",
            "error[V0301]: t.v:2:42: 'x' is not declared, and `default_nettype none forbids",
        ),
        (
            "`default_nettype tri\nmodule m(input wire a); inv u (.A(a), .Y(x)); endmodule\n",
            "error[V0301]: t.v:2:42: implicit net 'x' would have net type 'tri'",
        ),
        ("// nothing\n", "error[V0301]: t.v:1:1: no module found"),
        (
            "module m(input a);\n`ifdef NEVER\n(* keep *)\n`endif\nendmodule\n",
            "error[V0300]: t.v:3:1: attribute 'keep'",
        ),
        (
            "module m(input a); inv u (.A(a), .Y(;\nendmodule\n",
            "error[V0100]: t.v:1:37:",
        ),
    ];
    for (text, want) in cases {
        let e = errors(text);
        assert!(
            e[0].starts_with(want),
            "{text}\n  want {want}\n  got  {e:?}"
        );
    }
    // The debug form shows the rendered diagnostics, not the text.
    let e = read_structural("module m(); foo u (); endmodule // tail", "t.v", &cells).unwrap_err();
    let debug = format!("{e:?}");
    assert!(
        debug.contains("unknown cell 'foo'") && !debug.contains("tail"),
        "{debug}"
    );
    // A rejected declaration still declares its name: its uses add no errors.
    for text in [
        "module m(a); output reg a; inv u (.A(a)); endmodule\n",
        "module m(output logic q); assign q = 1'b0; endmodule\n",
        "module m(); reg [1:0] r; inv u (.A(r[1])); endmodule\n",
    ] {
        assert_eq!(errors(text).len(), 1, "{text}");
    }
    // All problems of a module are reported, in source order.
    let e = errors(
        "module m(input a, output y);\n  assign y = 1'bx;\n  foo u ();\n  reg r;\nendmodule\n",
    );
    assert_eq!(e.len(), 3, "{e:?}");
    assert!(e[0].contains("t.v:2:14") && e[1].contains("t.v:3:3") && e[2].contains("t.v:4:3"));
}

/// A module of the writer subset with connections in cell pin order.
fn sample_module() -> NlModule {
    let port = |name: &str, dir, width| NlPort {
        name: name.into(),
        dir,
        width,
    };
    let net = |n: &str| NlRef::Net(n.into());
    let bit = |n: &str, k| NlRef::Bit(n.into(), k);
    let inst = |cell: &str, name: &str, conns: &[(&str, NlRef)]| NlInstance {
        cell: cell.into(),
        name: name.into(),
        conns: conns
            .iter()
            .map(|(p, r)| (p.to_string(), r.clone()))
            .collect(),
    };
    NlModule {
        name: "top".into(),
        ports: vec![
            port("clk", NlDir::Input, None),
            port("a", NlDir::Input, Some(3)),
            port("y", NlDir::Output, Some(1)),
            port("z", NlDir::Output, None),
        ],
        wires: vec![
            ("n0".into(), None),
            ("u.q[3]".into(), None),
            ("w".into(), Some(2)),
            ("module".into(), None),
        ],
        instances: vec![
            inst(
                "nand2",
                "g0",
                &[
                    ("A", bit("a", 2)),
                    ("B", NlRef::Const(true)),
                    ("Y", net("n0")),
                ],
            ),
            inst(
                "dff",
                "r0",
                &[("D", net("n0")), ("CK", net("clk")), ("Q", net("u.q[3]"))],
            ),
            inst("inv", "g1", &[("A", bit("w", 1)), ("Y", net("module"))]),
            inst("tie", "t0", &[("H", bit("w", 0))]),
        ],
        assigns: vec![
            (bit("y", 0), net("u.q[3]")),
            (net("z"), NlRef::Const(false)),
            (bit("w", 1), bit("a", 0)),
        ],
    }
}

// VERILOG-REQ-0300, VERILOG-REQ-0301: the model, its core netlist, its frozen tables and its
// written text agree.
#[test]
fn conversions_round_trip() {
    let m = sample_module();
    let g = to_gate_netlist(&m, &cells).unwrap();
    assert_eq!(from_gate_netlist(&g).unwrap(), m);
    let frozen = g.freeze();
    assert_eq!(from_gate_netlist(&frozen.thaw().unwrap()).unwrap(), m);
    let text = write_netlist(&m);
    assert!(text.contains("\\module "), "{text}");
    assert_eq!(read(&text).freeze(), frozen);
    // A whole vector stands for its bits, and implicit nets become wires.
    let mut m2 = m.clone();
    m2.assigns
        .push((NlRef::Net("w".into()), NlRef::Bit("a".into(), 1)));
    let e = to_gate_netlist(&m2, &cells).unwrap_err();
    assert_eq!(
        e.to_string(),
        "assignment to w: the right side has 1 bit, the left side 2 bits"
    );
    m2.assigns.pop();
    m2.assigns
        .push((NlRef::Net("w".into()), NlRef::Net("w".into())));
    m2.instances[2].conns[1].1 = NlRef::Net("fresh".into());
    let g2 = to_gate_netlist(&m2, &cells).unwrap();
    assert_eq!(assigns(&g2)[3..], ["w[1]=w[1]", "w[0]=w[0]"]);
    assert_eq!(nets(&g2).last().map(String::as_str), Some("fresh"));
    let back = from_gate_netlist(&g2).unwrap();
    assert_eq!(back.wires.last(), Some(&("fresh".to_string(), None)));
    assert_eq!(read(&write_netlist(&m2)).freeze(), g2.freeze());
}

#[test]
fn conversion_errors() {
    let m = sample_module();
    let edits: &[(&str, fn(&mut NlModule))] = &[
        ("instance 'g1': unknown cell 'buf'", |m| {
            m.instances[2].cell = "buf".into()
        }),
        ("instance 'g1': cell 'inv' has no pin 'Z'", |m| {
            m.instances[2].conns[0].0 = "Z".into()
        }),
        ("instance 'g1': pin 'A' is connected twice", |m| {
            m.instances[2].conns[1].0 = "A".into()
        }),
        (
            "instance 'g1': bit 2 is outside the range [1:0] of 'w'",
            |m| m.instances[2].conns[0].1 = NlRef::Bit("w".into(), 2),
        ),
        ("instance 'g1': 'n0' is a scalar and has no bit 0", |m| {
            m.instances[2].conns[0].1 = NlRef::Bit("n0".into(), 0)
        }),
        ("instance 'g1': pin 'A' is connected to 3 bits", |m| {
            m.instances[2].conns[0].1 = NlRef::Net("a".into())
        }),
        ("assignment to z: 'm' is not declared", |m| {
            m.assigns[1].1 = NlRef::Net("m".into())
        }),
        ("'w' has width 0", |m| m.wires[2].1 = Some(0)),
        ("'n0' is already declared as a wire", |m| {
            m.wires.push(("n0".into(), None))
        }),
        ("instance 'n0': 'n0' is already declared as a wire", |m| {
            m.instances[0].name = "n0".into()
        }),
        (
            "instance 'g1': cell 'inv' has the name of the module, so the written instance \
             would instantiate the module itself",
            |m| m.name = "inv".into(),
        ),
        (
            "'w' has width 16777217, more than the 16777216 bits of a range",
            |m| m.wires[2].1 = Some(MAX_RANGE_WIDTH + 1),
        ),
        (
            "'a' has width 4294967295, more than the 16777216 bits",
            |m| m.ports[1].width = Some(u32::MAX),
        ),
    ];
    for (want, edit) in edits {
        let mut bad = m.clone();
        edit(&mut bad);
        let e = to_gate_netlist(&bad, &cells).unwrap_err().to_string();
        assert!(e.starts_with(want), "want {want}\n got {e}");
    }
    let g =
        read("module m(inout p, input [0:3] a, input [2:1] b); wire [2:1] w; wire n; endmodule");
    let e = from_gate_netlist(&g).unwrap_err().to_string();
    assert!(e.starts_with("inout port 'p' cannot be written"), "{e}");
    let g = read("module m(input [0:3] a); endmodule");
    let e = from_gate_netlist(&g).unwrap_err().to_string();
    assert!(e.contains("range [0:3]"), "{e}");
    let g = read("module m(input a); wire [2:1] w; endmodule");
    let e = from_gate_netlist(&g).unwrap_err().to_string();
    assert_eq!(e, "the 2 bits of net 'w' do not form the range [1:0]");
    let mut g = read("module m(input a); endmodule");
    g.add_net(None, None).unwrap();
    let e = from_gate_netlist(&g).unwrap_err().to_string();
    assert!(e.contains("has no name"), "{e}");
    let mut g = read("module m(input a); wire n; endmodule");
    let inv = g.add_cell_type("inv", &cells("inv").unwrap()).unwrap();
    g.add_instance("n", inv).unwrap();
    let e = from_gate_netlist(&g).unwrap_err().to_string();
    assert_eq!(
        e,
        "instance 'n' has the name of a net; Verilog needs distinct names"
    );
    let mut g = read("module m(input a); endmodule");
    g.add_net(Some("u1/n2"), None).unwrap();
    g.add_net(Some(r"u1\/n2"), None).unwrap();
    let e = from_gate_netlist(&g).unwrap_err().to_string();
    assert_eq!(
        e,
        r"net 'u1\/n2' and net 'u1/n2' are both written as 'u1/n2'; Verilog needs distinct names"
    );
    // Verilog reads an instance of a cell named like the module as one of the module itself.
    let mut g = read("module inv(input a); endmodule");
    let inv = g.add_cell_type("inv", &cells("inv").unwrap()).unwrap();
    g.add_instance("u", inv).unwrap();
    let e = from_gate_netlist(&g).unwrap_err().to_string();
    assert_eq!(
        e,
        "instance 'u' has the module's name 'inv' as its cell; Verilog would read it as an \
         instance of the module itself"
    );
    let e = errors("module inv(input A, output Y); inv u (.A(A), .Y(Y)); endmodule\n");
    assert!(
        e[0].starts_with("error[V0300]: t.v:1:32: an instance of the module 'inv' itself"),
        "{e:?}"
    );
}

// Connections keep their order through the core netlist, whatever the library's pin order:
// the conversions invert each other and the written text reads back to the same tables.
#[test]
fn keeps_connection_order() {
    let mut m = sample_module();
    m.instances[0].conns.reverse();
    m.instances.push(NlInstance {
        cell: "ao".into(),
        name: "g2".into(),
        conns: vec![
            ("A".into(), NlRef::Net("n0".into())),
            ("B".into(), NlRef::Bit("a".into(), 1)),
            ("Y".into(), NlRef::Net("z".into())),
        ],
    });
    let g = to_gate_netlist(&m, &cells).unwrap();
    let u = g.find_instance("g0").unwrap();
    let order: Vec<String> = g
        .instance(u)
        .unwrap()
        .connected
        .iter()
        .map(|&p| g.pin_label(p))
        .collect();
    assert_eq!(order, ["g0.Y", "g0.B", "g0.A"]);
    assert_eq!(from_gate_netlist(&g).unwrap(), m);
    let frozen = g.freeze();
    assert_eq!(from_gate_netlist(&frozen.thaw().unwrap()).unwrap(), m);
    let text = write_netlist(&m);
    assert!(
        text.contains("nand2 g0 (.Y(n0), .B(1'b1), .A(a[2]));"),
        "{text}"
    );
    assert!(text.contains("ao g2 (.A(n0), .B(a[1]), .Y(z));"), "{text}");
    assert_eq!(read(&text).freeze(), frozen);
    assert_eq!(
        write_netlist(&from_gate_netlist(&read(&text)).unwrap()),
        text
    );
}

/// xorshift64*.
fn next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn below(state: &mut u64, n: usize) -> usize {
    (next(state) % n as u64) as usize
}

/// A random module of the writer subset, connections in random order.
fn random_module(state: &mut u64) -> NlModule {
    let mut m = NlModule {
        name: "rnd".into(),
        ..NlModule::default()
    };
    let width = |s: &mut u64| [None, None, Some(1), Some(2), Some(4)][below(s, 5)];
    let mut refs = Vec::new();
    let add = |name: String, w: Option<u32>, refs: &mut Vec<NlRef>| match w {
        None => refs.push(NlRef::Net(name)),
        Some(w) => refs.extend((0..w).map(|k| NlRef::Bit(name.clone(), k))),
    };
    for k in 0..1 + below(state, 4) {
        let name = if below(state, 3) == 0 {
            format!("p.{k}[0]")
        } else {
            format!("p{k}")
        };
        let w = width(state);
        let dir = [NlDir::Input, NlDir::Output][below(state, 2)];
        add(name.clone(), w, &mut refs);
        m.ports.push(NlPort {
            name,
            dir,
            width: w,
        });
    }
    for k in 0..below(state, 6) {
        let name = ["w", "wire", "s/"][below(state, 3)].to_string() + &k.to_string();
        let w = width(state);
        add(name.clone(), w, &mut refs);
        m.wires.push((name, w));
    }
    let pick = |s: &mut u64, refs: &[NlRef]| -> NlRef {
        if below(s, 6) == 0 {
            NlRef::Const(below(s, 2) == 1)
        } else {
            refs[below(s, refs.len())].clone()
        }
    };
    for k in 0..below(state, 8) {
        let cell = ["inv", "nand2", "dff", "pad", "tie", "ao"][below(state, 6)];
        let mut conns = Vec::new();
        for (pin, _) in cells(cell).unwrap() {
            if below(state, 4) != 0 {
                conns.push((pin, pick(state, &refs)));
            }
        }
        for k in (1..conns.len()).rev() {
            conns.swap(k, below(state, k + 1));
        }
        m.instances.push(NlInstance {
            cell: cell.into(),
            name: format!("g{k}"),
            conns,
        });
    }
    for _ in 0..below(state, 5) {
        let lhs = refs[below(state, refs.len())].clone();
        m.assigns.push((lhs, pick(state, &refs)));
    }
    m
}

// Random modules of the writer subset: model -> netlist -> model is the identity, and reading
// the written text freezes to the netlist's tables.
#[test]
fn random_round_trips() {
    let mut state = 0x5EED_u64;
    for _ in 0..500 {
        let m = random_module(&mut state);
        let g = to_gate_netlist(&m, &cells).unwrap_or_else(|e| panic!("{e}\n{m:?}"));
        assert_eq!(from_gate_netlist(&g).unwrap(), m);
        let frozen = g.freeze();
        assert_eq!(frozen.thaw().unwrap().freeze(), frozen);
        let text = write_netlist(&m);
        assert_eq!(read(&text).freeze(), frozen, "{text}");
    }
}
