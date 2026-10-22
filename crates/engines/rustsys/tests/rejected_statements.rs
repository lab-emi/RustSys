// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Procedural statements that the reader keeps without their operands and that change when or
//! whether statements run are rejected with `S0235` and their location, never dropped
//! (SYNTH-REQ-0133, SYNTH-REQ-0141, Rule 7.1); assertions remain ignored with a note
//! (SYNTH-REQ-0121).

mod common;

use common::elab;

/// Asserts that `src` fails with one `S0235` error at `line:col` of `f0.sv` naming `what`.
fn assert_rejected(src: &str, line: u32, col: u32, what: &str) {
    let err = elab(&[src], None).map(|_| ()).unwrap_err();
    let errors: Vec<&str> = err.lines().filter(|l| l.starts_with("error[")).collect();
    assert_eq!(errors.len(), 1, "{err}");
    let at = format!("error[S0235]: f0.sv:{line}:{col}: ");
    assert!(errors[0].starts_with(&at), "{at:?} in {err}");
    assert!(errors[0].contains(what), "{what:?} in {err}");
}

// SYNTH-REQ-0133: `wait (c) stmt`; its statement would be lost with it.
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

// SYNTH-REQ-0133: `wait_order` is read as `wait` and rejected the same way.
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

// SYNTH-REQ-0133: `disable` of a named block.
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

// SYNTH-REQ-0133: event triggers `->` and `->>`.
#[test]
fn event_triggers_are_rejected() {
    for op in ["->", "->>"] {
        assert_rejected(
            &format!(
                "module m(input logic a, output logic y);
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

// SYNTH-REQ-0121, SYNTH-REQ-0133: an immediate assertion is still ignored.
#[test]
fn assertions_remain_ignored() {
    let d = elab(
        &["module m(input logic a, output logic y);
  always_comb begin
    assert (a || !a);
    y = a;
  end
endmodule"],
        None,
    );
    assert!(d.is_ok(), "{:?}", d.err());
}
