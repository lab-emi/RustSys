// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Procedural statements that change when or whether statements run, and that the reader keeps
//! without their operands, are outside the checker's RTL model: `wait`, `disable` and event
//! triggers are rejected with `C0106` and their RTL location instead of being skipped, so a
//! netlist is never proved against a procedure that is not the RTL (CHECK-REQ-0014, Rule 7.1).

use rusteda_core::job::Diag;
use rusteda_formats::liberty::Library;
use rustsys_check::{CheckOptions, CheckReport, Input, Verdict, check};
use rustsys_fixtures::liberty::{Variant, library};

/// A netlist with the ports of every RTL below.
const NETLIST: &str = "module m(clk, a, y);
  input clk, a; output y;
  wtf_buf_1 u1 (.A(a), .X(y));
endmodule";

fn run(rtl: &str) -> CheckReport {
    let lib: Library =
        rusteda_formats::liberty::read_text(&library(Variant::Full), "full.lib").expect("library");
    check(
        &CheckOptions {
            rtl: Input::Texts(vec![("m.sv".into(), rtl.into())]),
            netlist: Input::Texts(vec![("m.v".into(), NETLIST.into())]),
            ..Default::default()
        },
        std::slice::from_ref(&lib),
    )
}

/// Asserts the verdict `error` with one `C0106` message at `line` and `col` of the RTL that
/// names `what`.
fn assert_rejected(rtl: &str, line: u32, col: u32, what: &str) {
    let r = run(rtl);
    assert_eq!(r.verdict, Verdict::Error, "{r:?}");
    assert!(!r.internal_error, "{r:?}");
    assert_eq!(r.errors.len(), 1, "{r:?}");
    let d = Diag::parse(&r.errors[0]).unwrap_or_else(|| panic!("not coded: {}", r.errors[0]));
    assert_eq!(d.code, "C0106", "{d:?}");
    let at = d.location.as_ref().expect("location");
    assert_eq!(
        (at.file.as_str(), at.line, at.column),
        ("m.sv", line, col),
        "{d:?}"
    );
    assert!(
        d.message.contains(what),
        "{what:?} missing from {:?}",
        d.message
    );
}

// CHECK-REQ-0014: `wait (c) stmt` would be read as nothing, its statement included.
#[test]
fn wait_is_rejected() {
    assert_rejected(
        "module m(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) begin
    y <= a;
    wait (a) y <= 1'b0;
  end
endmodule",
        4,
        5,
        "`wait`",
    );
}

// CHECK-REQ-0014: `disable` leaves a named block early.
#[test]
fn disable_is_rejected() {
    assert_rejected(
        "module m(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) begin : blk
    if (a) disable blk;
    y <= a;
  end
endmodule",
        3,
        12,
        "`disable`",
    );
}

// CHECK-REQ-0014: event triggers (`->`, `->>`) wake processes that wait on a named event.
#[test]
fn event_triggers_are_rejected() {
    for op in ["->", "->>"] {
        assert_rejected(
            &format!(
                "module m(input logic clk, input logic a, output logic y);
  always_comb begin
    y = a;
    {op} done_ev;
  end
endmodule"
            ),
            4,
            5,
            "event trigger",
        );
    }
}

// CHECK-REQ-0014: `wait_order` is read as `wait` and rejected the same way.
#[test]
fn wait_order_is_rejected() {
    assert_rejected(
        "module m(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) begin
    wait_order (a) y <= 1'b0;
    y <= a;
  end
endmodule",
        3,
        5,
        "`wait`",
    );
}

// CHECK-REQ-0014: an immediate assertion stays outside the logic and is not rejected.
#[test]
fn assertions_are_not_rejected() {
    let r = run("module m(input logic clk, input logic a, output logic y);
  always_comb begin
    assert (a || !a);
    y = a;
  end
endmodule");
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
}
