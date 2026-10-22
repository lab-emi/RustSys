// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Netlist constructs outside the checker's model are rejected with a coded message that names
//! the instance, the cell and the construct, never evaluated partially (CHECK-REQ-0020,
//! CHECK-REQ-0022, CHECK-REQ-0023). The three-state, dual-edge, `x`/`z`, supply net, drive
//! strength, duplicate connection, index, library order, `driver_type`, `x_function`,
//! `enable_also` and clock-gating probes below were reported `equivalent` before they were
//! rejected.

use rusteda_core::job::Diag;
use rusteda_formats::liberty::Library;
use rustsys_check::liberty::{Unread, unread};
use rustsys_check::{CheckOptions, CheckReport, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

/// A library as `rustsys check synth` reads it: the model, and the constructs of its text that
/// the model does not keep.
#[derive(Clone)]
struct Lib {
    model: Library,
    unread: Vec<Unread>,
}

fn lib_text(text: &str, name: &str) -> Lib {
    Lib {
        model: rusteda_formats::liberty::read_text(text, name).expect("library"),
        unread: unread(text, name).expect("library text"),
    }
}

fn fixture(v: Variant) -> Lib {
    lib_text(&library(v), v.name())
}

fn run(rtl: &str, netlist: &str, lib: &Lib) -> CheckReport {
    run_libs(rtl, netlist, std::slice::from_ref(lib))
}

fn run_libs(rtl: &str, netlist: &str, libs: &[Lib]) -> CheckReport {
    let models: Vec<Library> = libs.iter().map(|l| l.model.clone()).collect();
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![("m.sv".into(), rtl.into())]),
            netlist: Input::Texts(vec![("m.v".into(), netlist.into())]),
            unread_liberty: libs.iter().flat_map(|l| l.unread.clone()).collect(),
            ..Default::default()
        },
        &models,
    )
}

/// The probe library.
fn probe() -> Lib {
    lib_text(LIB, "probe.lib")
}

/// The one message of a rejected netlist, read as a coded message.
fn rejected(r: &CheckReport) -> Diag {
    assert_eq!(r.verdict, Verdict::Error, "{r:?}");
    assert!(r.mismatches.is_empty() && r.undecided.is_empty(), "{r:?}");
    assert_eq!(r.errors.len(), 1, "{r:?}");
    Diag::parse(&r.errors[0]).unwrap_or_else(|| panic!("not a coded message: {}", r.errors[0]))
}

/// Asserts the code, the netlist line and every listed part of the message.
fn assert_rejected(r: &CheckReport, code: &str, line: u32, parts: &[&str]) {
    let d = rejected(r);
    assert_eq!(d.code, code, "{d:?}");
    let at = d.location.as_ref().expect("location");
    assert_eq!((at.file.as_str(), at.line), ("m.v", line), "{d:?}");
    for p in parts {
        assert!(d.message.contains(p), "{p:?} missing from {:?}", d.message);
    }
}

const BUF: &str = "module m(input a, output y);
  assign y = a;
endmodule";

// A disabled three-state driver floats its output; the checker read it as a buffer and found the
// netlist equivalent to `assign y = a`. Enabled or not, the cell is rejected.
#[test]
fn three_state_cells_are_rejected() {
    let lib = fixture(Variant::Full);
    for te_b in ["1'b1", "1'b0"] {
        let net = format!(
            "module m(a, y);
  input a; output y;
  wtf_ebufn_1 u1 (.A(a), .TE_B({te_b}), .Z(y));
endmodule"
        );
        assert_rejected(
            &run(BUF, &net, &lib),
            "C0101",
            3,
            &[
                "instance u1 of cell wtf_ebufn_1",
                "pin Z is three-state",
                "three_state \"TE_B\"",
            ],
        );
    }
}

/// A flip-flop that also samples on the falling clock edge, and cells that the reader keeps but
/// the checker does not model.
const LIB: &str = r#"library (probe) {
  cell (dff_both) {
    area : 6;
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; clocked_on_also : "!CK"; }
    pin (D) { direction : input; }
    pin (CK) { direction : input; clock : true; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (dff_two) {
    area : 9;
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    ff (IP, IPN) { next_state : "E"; clocked_on : "CK"; }
    pin (D) { direction : input; }
    pin (E) { direction : input; }
    pin (CK) { direction : input; clock : true; }
    pin (Q) { direction : output; function : "IQ"; }
    pin (P) { direction : output; function : "IP"; }
  }
  cell (pad) {
    area : 3;
    pin (A) { direction : input; }
    pin (IO) { direction : inout; function : "A"; }
  }
  cell (inv) {
    area : 1;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "!A"; }
  }
  cell (and2) {
    area : 1;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A & B"; }
  }
  cell (inv2) {
    area : 1;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "!A"; }
    pin (Z) { direction : output; function : "A"; }
  }
  cell (bank1) {
    area : 5;
    ff_bank (IQ, IQN, 1) { next_state : "D"; clocked_on : "CK"; }
    pin (D) { direction : input; }
    pin (CK) { direction : input; clock : true; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (nand_int) {
    area : 2;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (N) { direction : internal; function : "A & B"; }
    pin (Y) { direction : output; function : "!N"; }
  }
  cell (no_fn) {
    area : 1;
    pin (A) { direction : input; }
    pin (Y) { direction : output; }
  }
  cell (od_buf) {
    area : 1;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "A"; driver_type : open_drain; }
  }
  cell (pu_buf) {
    area : 1;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "A"; driver_type : pull_up; }
  }
  cell (os_buf) {
    area : 1;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "A"; driver_type : "open_source"; }
  }
  cell (xf_buf) {
    area : 1;
    pin (A) { direction : input; }
    pin (EN) { direction : input; }
    pin (Y) { direction : output; function : "A"; x_function : "!EN"; }
  }
  cell (lat_also) {
    area : 4;
    latch (IQ, IQN) { enable : "G"; data_in : "D"; enable_also : "G2"; }
    pin (D) { direction : input; }
    pin (G) { direction : input; }
    pin (G2) { direction : input; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (lat) {
    area : 4;
    latch (IQ, IQN) { enable : "G"; data_in : "D"; power_down_function : "!VDD"; }
    pin (D) { direction : input; }
    pin (G) { direction : input; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (dff_attr) {
    area : 5;
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; data_in : "E"; }
    pin (D) { direction : input; }
    pin (E) { direction : input; }
    pin (CK) { direction : input; clock : true; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (icg_comb) {
    area : 3;
    clock_gating_integrated_cell : "latch_posedge";
    pin (CK) { direction : input; clock : true; clock_gate_clock_pin : true; }
    pin (E) { direction : input; clock_gate_enable_pin : true; }
    pin (GCK) { direction : output; clock_gate_out_pin : true; function : "CK & E"; }
  }
  cell (dff) {
    area : 5;
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    pin (D) { direction : input; }
    pin (CK) { direction : input; clock : true; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (ff_noclk) {
    area : 5;
    ff (IQ, IQN) { next_state : "D"; }
    pin (D) { direction : input; }
    pin (CK) { direction : input; clock : true; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (ff_badclr) {
    area : 5;
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; clear : "RN &&"; }
    pin (D) { direction : input; }
    pin (CK) { direction : input; clock : true; }
    pin (RN) { direction : input; }
    pin (Q) { direction : output; function : "IQ"; }
  }
}"#;

// A dual-edge flip-flop changes on both clock edges; the checker ignored `clocked_on_also` and
// found it equivalent to a rising-edge register.
#[test]
fn dual_edge_cells_are_rejected() {
    let lib = probe();
    let rtl = "module m(input clk, d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule";
    let net = "module m(clk, d, q);
  input clk; input d; output q;
  dff_both u1 (.D(d), .CK(clk), .Q(q));
endmodule";
    assert_rejected(
        &run(rtl, net, &lib),
        "C0102",
        3,
        &[
            "instance u1 of cell dff_both",
            "clocked_on_also \"!CK\"",
            "dual-edge",
        ],
    );
}

// `x` and `z` bits were read as 0, so `assign y = 1'bx` matched `assign y = 1'b0`. Every literal
// with such a bit is rejected: in an assignment, a connection, a bit index or a declared range.
#[test]
fn unknown_literal_bits_are_rejected() {
    let lib = fixture(Variant::Rich);
    let zero = "module m(input a, output y);
  assign y = 1'b0;
endmodule";
    let cases: [(&str, &str, &[&str]); 6] = [
        (zero, "assign y = 1'bx;", &["assign to y", "literal 1'bx"]),
        (zero, "assign y = 1'bz;", &["literal 1'bz"]),
        (zero, "assign y = 'x;", &["literal 'x"]),
        (
            zero,
            "wt_and2_x1 u1 (.A(a), .B(1'b?), .Y(y));",
            &["instance u1 of cell wt_and2_x1 pin B", "literal 1'b?"],
        ),
        (
            BUF,
            "wire [1:0] w; assign w[0] = a; assign w[1] = a; assign y = w[1'bx];",
            &["assign to y", "literal 1'bx"],
        ),
        (
            BUF,
            "wire [1'bx:0] w; assign y = a;",
            &["net w: range bound 1'bx"],
        ),
    ];
    for (rtl, body, parts) in cases {
        let net = format!("module m(a, y);\n  input a; output y;\n  {body}\nendmodule");
        let r = run(rtl, &net, &lib);
        let d = rejected(&r);
        assert_eq!(d.code, "C0103", "{body}: {d:?}");
        for p in parts {
            assert!(d.message.contains(p), "{body}: {p:?} missing from {d:?}");
        }
        assert!(d.message.contains("x or z bit"), "{d:?}");
    }
}

// Cells whose behavior lies in a statetable, in bus pins, in a second sequential group or in a
// bidirectional pin are rejected with the construct named.
#[test]
fn cells_outside_the_model_are_rejected() {
    let full = fixture(Variant::Full);
    let gate = "module m(input clk, en, output g);
  assign g = clk & en;
endmodule";
    let icg = "module m(clk, en, g);
  input clk; input en; output g;
  wtf_icg_1 u1 (.CLK(clk), .GATE(en), .GCLK(g));
endmodule";
    assert_rejected(
        &run(gate, icg, &full),
        "C0104",
        3,
        &["instance u1 of cell wtf_icg_1", "statetable"],
    );
    let bus = "module m(clk, en, g);
  input clk; input en; output g;
  wtf_dff4_1 u1 (.CLK(clk));
  assign g = 1'b0;
endmodule";
    assert_rejected(
        &run(gate, bus, &full),
        "C0104",
        3,
        &["instance u1 of cell wtf_dff4_1", "bus or bundle"],
    );
    let lib = probe();
    let two = "module m(input clk, d, e, output logic q, p);
  always_ff @(posedge clk) begin q <= d; p <= e; end
endmodule";
    let net = "module m(clk, d, e, q, p);
  input clk; input d; input e; output q; output p;
  dff_two u1 (.D(d), .E(e), .CK(clk), .Q(q), .P(p));
endmodule";
    assert_rejected(
        &run(two, net, &lib),
        "C0104",
        3,
        &["instance u1 of cell dff_two", "2 ff and latch groups"],
    );
    // The pad drives y against the inverter; the inout connection was ignored.
    let inv = "module m(input a, b, output y);
  assign y = !a;
endmodule";
    let net = "module m(a, b, y);
  input a; input b; output y;
  inv u0 (.A(a), .Y(y));
  pad u1 (.A(b), .IO(y));
endmodule";
    assert_rejected(
        &run(inv, net, &lib),
        "C0104",
        4,
        &["instance u1 of cell pad", "pin IO is bidirectional (inout)"],
    );
}

// Net declaration assignments, unpacked net dimensions, arrays of instances and parameters of
// cell instances were skipped; they are rejected (CHECK-REQ-0020).
#[test]
fn netlist_constructs_outside_the_subset_are_rejected() {
    let lib = fixture(Variant::Rich);
    let inv = "module m(input a, b, output y);
  assign y = !b;
endmodule";
    let cases = [
        // The declaration drives w against the assignment.
        (
            "wire w = a; wt_inv_x1 u1 (.A(b), .Y(w)); assign y = w;",
            "declared with an assignment",
        ),
        (
            "wire w [1:0]; wt_inv_x1 u1 (.A(b), .Y(w)); assign y = w;",
            "unpacked dimensions",
        ),
        ("wt_inv_x1 u1 [1:0] (.A(b), .Y(y));", "array of instances"),
        (
            "wt_inv_x1 #(.W(2)) u1 (.A(b), .Y(y));",
            "instantiated with parameters",
        ),
    ];
    for (body, what) in cases {
        let net = format!("module m(a, b, y);\n  input a; input b; output y;\n  {body}\nendmodule");
        let r = run(inv, &net, &lib);
        assert_eq!(r.verdict, Verdict::Error, "{body}: {r:?}");
        assert!(
            r.errors.len() == 1 && r.errors[0].starts_with("m.v:3:") && r.errors[0].contains(what),
            "{body}: {:?}",
            r.errors
        );
    }
}

/// The netlist of `m(a, b, y)` with the given body.
fn netlist(body: &str) -> String {
    format!("module m(a, b, y);\n  input a; input b; output y;\n  {body}\nendmodule")
}

const AND: &str = "module m(input a, b, output y);
  assign y = a & b;
endmodule";

// A supply net keeps its value whatever drives it: a driven one was read as a plain wire and
// found equivalent to its driver. A supply net is a constant, any driver of it and a logic port
// declared as one are rejected, and so are net kinds that resolve several drivers, pull or store
// charge.
#[test]
fn net_kinds_outside_the_model_are_rejected() {
    let lib = probe();
    let a = "module m(input a, b, output y);
  assign y = a;
endmodule";
    // Tied through a supply net, the and gate is a buffer.
    let tied = netlist("supply1 one;\n  and2 u1 (.A(a), .B(one), .Y(y));");
    let r = run(a, &tied, &lib);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_rejected(
        &run(
            a,
            &netlist("supply1 s;\n  and2 u1 (.A(a), .B(a), .Y(s));\n  assign y = s;"),
            &lib,
        ),
        "C0105",
        4,
        &[
            "instance u1 of cell and2 pin Y drives supply1 net s",
            "drive strengths",
        ],
    );
    assert_rejected(
        &run(
            a,
            &netlist("supply0 s;\n  assign s = a;\n  assign y = s;"),
            &lib,
        ),
        "C0105",
        4,
        &["assign to s drives supply0 net s"],
    );
    let ports = [
        "module m(a, b, y);\n  input a; input b; output y;\n  supply0 y;\n  and2 u1 (.A(a), .B(a), .Y(y));\nendmodule",
        "module m(a, b, y);\n  input a; input b; output supply0 y;\n  and2 u1 (.A(a), .B(a), .Y(y));\nendmodule",
        "module m(input a, b, output supply0 y);\n  and2 u1 (.A(a), .B(a), .Y(y));\nendmodule",
    ];
    for (net, line) in ports.iter().zip([3, 2, 1]) {
        assert_rejected(
            &run(a, net, &lib),
            "C0105",
            line,
            &["port y is declared supply0", "logic port"],
        );
    }
    for kind in ["wor", "wand", "tri0", "tri1", "trireg", "triand", "trior"] {
        let net = netlist(&format!(
            "{kind} w;\n  and2 u1 (.A(a), .B(a), .Y(w));\n  assign y = w;"
        ));
        assert_rejected(
            &run(a, &net, &lib),
            "C0105",
            3,
            &[&format!("net w is declared {kind}"), "one driver"],
        );
    }
    let net = format!("`default_nettype tri1\n{}", netlist("assign y = a;"));
    assert_rejected(&run(a, &net, &lib), "C0105", 2, &["`default_nettype tri1"]);
}

// `assign (highz1, strong0) y = a` floats y when a is 1; the parser skips drive strengths and the
// checker found it equivalent to `assign y = a`. A strength without high impedance does not change
// the value of a net with one driver.
#[test]
fn floating_drive_strengths_are_rejected() {
    let lib = probe();
    let a = "module m(input a, b, output y);
  assign y = a;
endmodule";
    for (strength, h) in [
        ("(highz1, strong0)", "highz1"),
        ("(weak1, highz0)", "highz0"),
    ] {
        let net = netlist(&format!("assign {strength} #1 y = a;"));
        assert_rejected(
            &run(a, &net, &lib),
            "C0101",
            3,
            &[
                &format!("continuous assignment `assign {strength} #1 y`"),
                &format!("drive strength {h}"),
            ],
        );
    }
    // A strength from a macro is found after preprocessing.
    let net = format!(
        "`define FLOAT (highz1, strong0)\n{}",
        netlist("assign `FLOAT y = a;")
    );
    assert_rejected(
        &run(a, &net, &lib),
        "C0101",
        4,
        &["continuous assignment `assign ( highz1 , strong0 ) y`"],
    );
    let r = run(a, &netlist("assign (strong0, weak1) y = a;"), &lib);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
}

// A pin connected twice kept only its last connection, so `.A(y), .A(a)` was read as `.A(a)`
// and found equal to `a & b`; power and ground pins took x and z literals; signed and wide index
// literals were read without their sign and upper bits. Each is rejected. An empty connection
// leaves an output unconnected.
#[test]
fn connections_and_indices_are_read_exactly() {
    let lib = probe();
    for conns in [".A(y), .A(a), .B(b), .Y(y)", ".A(a), .A(y), .B(b), .Y(y)"] {
        let r = run(AND, &netlist(&format!("and2 u1 ({conns});")), &lib);
        assert_eq!(r.verdict, Verdict::Error, "{conns}: {r:?}");
        assert!(
            r.errors.len() == 1
                && r.errors[0].starts_with("m.v:3:")
                && r.errors[0].contains("instance u1 of cell and2: pin A is connected twice"),
            "{conns}: {:?}",
            r.errors
        );
    }
    let inv = "module m(input a, b, output y);
  assign y = !a;
endmodule";
    let r = run(inv, &netlist("inv2 u1 (.A(a), .Y(y), .Z());"), &lib);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    let r = run(inv, &netlist("inv2 u1 (.A(), .Y(y));"), &lib);
    assert!(
        r.verdict == Verdict::Error && r.errors[0].contains("input pin A is not connected"),
        "{r:?}"
    );
    let full = fixture(Variant::Full);
    for (pg, t) in [
        (".VDD(1'bx), .VSS(1'b0)", "VDD: literal 1'bx"),
        (".VDD(1'b1), .VSS('z)", "VSS: literal 'z"),
    ] {
        let net = format!(
            "module m(a, b, y);\n  input a; input b; output y;\n  wtf_inv_1 u1 (.A(a), .Y(y), {pg});\nendmodule"
        );
        assert_rejected(
            &run(inv, &net, &full),
            "C0103",
            3,
            &["instance u1 of cell wtf_inv_1 power or ground pin", t],
        );
    }
    // w[2'sb11] is w[-1], outside the range; the low bits of the wide index are those of w[1].
    let w = "wire [3:0] w;\n  assign w[3] = a; assign w[2] = b; assign w[1] = b; assign w[0] = b;";
    for index in ["2'sb11", "64'h4000000000000001", "32'h80000003"] {
        let r = run(
            "module m(input a, b, output y);\n  assign y = a;\nendmodule",
            &netlist(&format!("{w}\n  assign y = w[{index}];")),
            &lib,
        );
        assert!(
            r.verdict == Verdict::Error
                && r.errors[0].contains(&format!("index {index} is outside the range [3:0] of w")),
            "{index}: {r:?}"
        );
    }
    // A signed index within the range reads its bit.
    let r = run(
        "module m(input a, b, output y);\n  assign y = a;\nendmodule",
        &netlist(&format!("{w}\n  assign y = w[3'sb011];")),
        &lib,
    );
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
}

// Two libraries that define a cell differently made the verdict depend on their order; the
// instance is rejected for either order. Identical copies are one cell.
#[test]
fn conflicting_cell_definitions_are_rejected() {
    let one = |f: &str| {
        lib_text(
            &format!(
                "library (l{}) {{ cell (invx) {{ area : 1; pin (A) {{ direction : input; }} \
                 pin (Y) {{ direction : output; function : \"{f}\"; }} }} }}",
                f.len()
            ),
            "l.lib",
        )
    };
    let (buf, inv) = (one("A"), one("A'"));
    let rtl = "module m(input a, b, output y);\n  assign y = a;\nendmodule";
    let net = netlist("invx u1 (.A(a), .Y(y));");
    for libs in [[buf.clone(), inv.clone()], [inv.clone(), buf.clone()]] {
        assert_rejected(
            &run_libs(rtl, &net, &libs),
            "C0003",
            3,
            &[
                "cell invx: libraries",
                "define it differently (function of pin Y:",
            ],
        );
    }
    let r = run_libs(rtl, &net, &[buf.clone(), buf]);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
}

// Behavior that the cell's functions read from outside its pins and its one ff or latch group:
// the state of an ff_bank group, an internal pin, or an output without a function.
#[test]
fn functions_outside_the_model_are_rejected() {
    let lib = probe();
    let ff = "module m(input clk, d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule";
    let bank = "module m(clk, d, q);\n  input clk; input d; output q;\n  bank1 u1 (.D(d), .CK(clk), .Q(q));\nendmodule";
    assert_rejected(
        &run(ff, bank, &lib),
        "C0104",
        3,
        &[
            "instance u1 of cell bank1: an ff_bank group (IQ, IQN, 1)",
            "one ff or latch group per cell",
        ],
    );
    // Without the scan of the library text the group is still not evaluated: the library
    // model reports the state that the function reads as an unknown name.
    let r = check(
        &CheckOptions {
            rtl: Input::Texts(vec![("m.sv".into(), ff.into())]),
            netlist: Input::Texts(vec![("m.v".into(), bank.into())]),
            ..Default::default()
        },
        std::slice::from_ref(&lib.model),
    );
    let d = rejected(&r);
    assert!(
        d.code == "C0003" && d.message.contains("refers to unknown name 'IQ'"),
        "{d:?}"
    );
    let nand = "module m(input a, b, output y);\n  assign y = !(a & b);\nendmodule";
    assert_rejected(
        &run(nand, &netlist("nand_int u1 (.A(a), .B(b), .Y(y));"), &lib),
        "C0104",
        3,
        &["instance u1 of cell nand_int: the function of pin Y reads internal pin N"],
    );
    let a = "module m(input a, b, output y);\n  assign y = a;\nendmodule";
    assert_rejected(
        &run(a, &netlist("no_fn u1 (.A(a), .Y(y));"), &lib),
        "C0104",
        3,
        &["instance u1 of cell no_fn: output pin Y has no function"],
    );
}

// Constructs that change what a simulator instantiates or reads, outside the flat module of
// library cells: a module or primitive of the netlist named like a cell, a `bind`, a second
// definition of the module, a name in a package scope, a port declared twice and a data type
// that is not a bit type.
#[test]
fn constructs_outside_the_flat_module_are_rejected() {
    let lib = probe();
    let inv = "module m(input a, b, output y);\n  assign y = !a;\nendmodule";
    let cases = [
        (
            format!("{}\nmodule inv(input A, output Y);\n  assign Y = A;\nendmodule", netlist("inv u1 (.A(a), .Y(y));")),
            "inv is a module of the netlist",
        ),
        (
            format!("primitive inv(output Y, input A);\n  table 0 : 0; 1 : 1; endtable\nendprimitive\n{}", netlist("inv u1 (.A(a), .Y(y));")),
            "a primitive construct in the netlist",
        ),
        (
            format!("{}\nbind m inv u2 (.A(b), .Y(y));", netlist("inv u1 (.A(a), .Y(y));")),
            "a bind construct in the netlist",
        ),
        (
            format!("{}\n{}", netlist("inv u1 (.A(a), .Y(y));"), netlist("assign y = a;")),
            "module m is defined twice",
        ),
        (
            format!("package p;\n  localparam logic a = 1'b0;\nendpackage\n{}", netlist("inv u1 (.A(p::a), .Y(y));")),
            "unsupported netlist expression",
        ),
        (
            "module m(a, b, y);\n  input a; input a; input b; output y;\n  inv u1 (.A(a), .Y(y));\nendmodule".to_string(),
            "port a is declared twice",
        ),
        (
            netlist("integer k;\n  inv u1 (.A(a), .Y(k));\n  assign y = k;"),
            "net k has a data type other than logic, reg or bit",
        ),
        (
            "module m(input a [1:0], input b, output y);\n  inv u1 (.A(b), .Y(y));\nendmodule"
                .to_string(),
            "port a has unpacked dimensions",
        ),
        (
            "module m(input a, input b, output y = 1'b0);\n  inv u1 (.A(a), .Y(y));\nendmodule"
                .to_string(),
            "port y is declared with a value",
        ),
    ];
    for (net, what) in cases {
        let r = run(inv, &net, &lib);
        assert_eq!(r.verdict, Verdict::Error, "{what}: {r:?}");
        assert!(
            r.errors.len() == 1 && r.errors[0].contains(what),
            "{what}: {:?}",
            r.errors
        );
    }
    // The x or z range bound of a port names the port.
    let net = "module m(a, b, y);\n  input [1'bz:0] a; input b; output y;\n  inv u1 (.A(b), .Y(y));\nendmodule";
    assert_rejected(
        &run(inv, net, &lib),
        "C0103",
        2,
        &["port a: range bound 1'bz"],
    );
}

// An open-drain output floats when its function is 1, and pull-up and open-source drivers drive
// one value weakly or not at all; the library model does not keep `driver_type`, so these cells
// were evaluated from their functions and found equivalent to `assign y = a`. The scan of the
// library text rejects them as high impedance.
#[test]
fn driver_types_are_rejected() {
    let lib = probe();
    let a = "module m(input a, b, output y);\n  assign y = a;\nendmodule";
    for (cell, kind) in [
        ("od_buf", "open_drain"),
        ("pu_buf", "pull_up"),
        ("os_buf", "open_source"),
    ] {
        assert_rejected(
            &run(a, &netlist(&format!("{cell} u1 (.A(a), .Y(y));")), &lib),
            "C0101",
            3,
            &[
                &format!("instance u1 of cell {cell}: pin Y has driver_type \"{kind}\""),
                "drive strengths",
            ],
        );
    }
}

// An output that `x_function` makes unknown, a latch pair that `enable_also` describes, and an
// attribute of an ff group that the library model does not read were each ignored: the probes
// were equivalent to a buffer, a plain latch and a plain register. A plain latch, whose group
// has only `power_down_function` besides, is checked.
#[test]
fn unread_cell_attributes_are_rejected() {
    let lib = probe();
    let a = "module m(input a, b, output y);\n  assign y = a;\nendmodule";
    assert_rejected(
        &run(a, &netlist("xf_buf u1 (.A(a), .EN(b), .Y(y));"), &lib),
        "C0104",
        3,
        &[
            "instance u1 of cell xf_buf: pin Y has x_function \"!EN\"",
            "unknown values",
        ],
    );
    let latch = "module m(input d, g, g2, output logic q);
  always_latch if (g) q = d;
endmodule";
    let net = |cell: &str, g2: &str| {
        format!(
            "module m(d, g, g2, q);\n  input d; input g; input g2; output q;\n  {cell} q_reg (.D(d), .G(g){g2}, .Q(q));\nendmodule"
        )
    };
    assert_rejected(
        &run(latch, &net("lat_also", ", .G2(g2)"), &lib),
        "C0102",
        3,
        &[
            "instance q_reg of cell lat_also: enable_also \"G2\" of its latch group (IQ, IQN)",
            "master-slave",
        ],
    );
    let r = run(latch, &net("lat", ""), &lib);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    let ff = "module m(input clk, d, e, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule";
    let net = "module m(clk, d, e, q);\n  input clk; input d; input e; output q;\n  dff_attr u1 (.D(d), .E(e), .CK(clk), .Q(q));\nendmodule";
    assert_rejected(
        &run(ff, net, &lib),
        "C0104",
        3,
        &["instance u1 of cell dff_attr: its ff group (IQ, IQN) has the attribute data_in \"E\""],
    );
}

// A clock-gating cell described by the function `CK & E` alone, without the latch of its
// enable, was checked as an AND gate against an RTL that gates the clock with an AND.
#[test]
fn clock_gating_cells_without_their_latch_are_rejected() {
    let lib = probe();
    let rtl = "module m(input clk, en, d, output logic q);
  wire g = clk & en;
  always_ff @(posedge g) q <= d;
endmodule";
    let net = "module m(clk, en, d, q);
  input clk; input en; input d; output q;
  wire g;
  icg_comb u0 (.CK(clk), .E(en), .GCK(g));
  dff q_reg (.D(d), .CK(g), .Q(q));
endmodule";
    assert_rejected(
        &run(rtl, net, &lib),
        "C0104",
        4,
        &[
            "instance u0 of cell icg_comb: clock_gating_integrated_cell",
            "no ff or latch group",
        ],
    );
}

// An ff group that does not read, without clocked_on or with a clear that does not parse, is
// dropped from the library model; the instance is rejected with the library's own reason, not
// with the state that its output then reads.
#[test]
fn cells_described_with_errors_are_rejected() {
    let lib = probe();
    let ff = "module m(input clk, rn, d, output logic q);
  always_ff @(posedge clk or negedge rn) if (!rn) q <= 1'b0; else q <= d;
endmodule";
    let net = |inst: &str| {
        format!(
            "module m(clk, rn, d, q);\n  input clk; input rn; input d; output q;\n  {inst}\nendmodule"
        )
    };
    assert_rejected(
        &run(ff, &net("ff_noclk q_reg (.D(d), .CK(clk), .Q(q));"), &lib),
        "C0003",
        3,
        &[
            "instance q_reg of cell ff_noclk: the library describes it with errors: missing clocked_on",
        ],
    );
    assert_rejected(
        &run(
            ff,
            &net("ff_badclr q_reg (.D(d), .CK(clk), .RN(rn), .Q(q));"),
            &lib,
        ),
        "C0003",
        3,
        &["instance q_reg of cell ff_badclr: the library describes it with errors: clear 'RN &&'"],
    );
}

// Primitives and procedures in a netlist were reported with a dump of the syntax tree. A
// primitive that can float, switch or pull a net is high impedance; any other item is named with
// its location.
#[test]
fn items_outside_the_netlist_subset_are_named() {
    let lib = probe();
    let a = "module m(input a, b, output y);\n  assign y = a;\nendmodule";
    for (body, prim) in [
        ("bufif1 g1 (y, a, b);", "bufif1"),
        ("pullup (y);", "pullup"),
        ("tranif1 t1 (y, a, b);", "tranif1"),
    ] {
        assert_rejected(
            &run(a, &netlist(body), &lib),
            "C0101",
            3,
            &[
                &format!("a gate primitive '{prim}'"),
                "high impedance, switches or pulls",
            ],
        );
    }
    for (body, what) in [
        (
            "and g1 (y, a, b);",
            "a gate primitive 'and' g1 in the netlist",
        ),
        ("always @* y = a;", "an always procedure in the netlist"),
        ("alias y = a;", "an alias construct in the netlist"),
        (
            "and2 u1 (.A(a), .B(a), .Y(y));\n  defparam u1.W = 2;",
            "a defparam in the netlist",
        ),
        (
            "localparam P = 1;\n  assign y = a;",
            "a parameter in the netlist",
        ),
    ] {
        let r = run(a, &netlist(body), &lib);
        assert_eq!(r.verdict, Verdict::Error, "{body}: {r:?}");
        assert!(
            r.errors.len() == 1
                && r.errors[0].starts_with("m.v:")
                && r.errors[0].contains(what)
                && r.errors[0].contains("instances of library cells")
                && !r.errors[0].contains("span"),
            "{body}: {:?}",
            r.errors
        );
    }
}
