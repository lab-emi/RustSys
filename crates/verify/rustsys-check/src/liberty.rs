// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Liberty constructs that change the behavior of a cell but that the library model of
//! `rusteda_formats::liberty` does not keep, found in the library text (CHECK-REQ-0023). An instance
//! of such a cell is rejected, never evaluated without them.

use rusteda_formats::liberty::syntax::{self, AttrValue, Group};

use crate::netlist::{DUAL_EDGE, THREE_STATE, UNMODELED_CELL};

/// A construct of a library cell that the checker does not model and that the library model does
/// not keep (CHECK-REQ-0023).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    pub cell: String,
    /// The code of the rejection: `C0101`, `C0102` or `C0104`.
    pub code: &'static str,
    /// The construct, where in the cell it is, and why the checker does not model it.
    pub what: String,
}

/// Attributes of an `ff` group that the library model keeps, or that do not change the logic of a
/// powered cell (`power_down_function`, CHECK-REQ-0022).
const FF: &[&str] = &[
    "next_state",
    "clocked_on",
    "clocked_on_also",
    "clear",
    "preset",
    "clear_preset_var1",
    "clear_preset_var2",
    "power_down_function",
];

/// The same for a `latch` group.
const LATCH: &[&str] = &[
    "data_in",
    "enable",
    "clocked_on_also",
    "clear",
    "preset",
    "clear_preset_var1",
    "clear_preset_var2",
    "power_down_function",
];

/// The constructs of every cell of the first `library` group of `text`, the group that the
/// library model reads: pin `driver_type` (`C0101`); latch `enable_also` (`C0102`); pin
/// `x_function`, `state_function`, `internal_node` and `retention_pin`, cell `retention_cell`,
/// `ff_bank` and `latch_bank` groups, and attributes or groups of an `ff` or `latch` group that the
/// model does not keep (`C0104`). The constructs that the model keeps, such as `three_state`,
/// `clocked_on_also` and `statetable`, are judged from the model. A `test_cell` group describes
/// the cell again for test and is not read.
pub fn unread(text: &str, file: &str) -> Result<Vec<Unread>, String> {
    let root = syntax::parse(text, file).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let Some(lib) = root.children("library").next() else {
        return Ok(out);
    };
    for cell in lib.children("cell") {
        let name = cell.args.first().cloned().unwrap_or_default();
        let mut add = |code: &'static str, what: String| {
            out.push(Unread {
                cell: name.clone(),
                code,
                what,
            });
        };
        if let Some(v) = cell.simple("retention_cell") {
            add(
                UNMODELED_CELL,
                format!(
                    "retention_cell \"{v}\" makes it a retention cell; the checker does not \
                     model saving and restoring state"
                ),
            );
        }
        for g in &cell.groups {
            match g.kind.as_str() {
                "pin" => pin(g, &mut add),
                // A bus or bundle gives its pins its own attributes.
                "bus" | "bundle" => {
                    pin(g, &mut add);
                    for p in g.children("pin") {
                        pin(p, &mut add);
                    }
                }
                "ff" => sequential(g, FF, &mut add),
                "latch" => sequential(g, LATCH, &mut add),
                "ff_bank" | "latch_bank" => add(
                    UNMODELED_CELL,
                    format!(
                        "an {} group ({}); the checker models one ff or latch group per cell",
                        g.kind,
                        g.args.join(", ")
                    ),
                ),
                _ => {}
            }
        }
    }
    Ok(out)
}

/// The text of an attribute value: a simple value in quotes, a complex one in parentheses.
fn value(v: &AttrValue) -> String {
    match v {
        AttrValue::Simple(s) => format!("\"{s}\""),
        AttrValue::Complex(a) => format!("({})", a.join(", ")),
    }
}

/// The constructs of a pin, bus or bundle group.
fn pin(g: &Group, add: &mut dyn FnMut(&'static str, String)) {
    let at = format!("{} {}", g.kind, g.args.join(", "));
    for a in &g.attrs {
        let v = value(&a.value);
        match a.name.as_str() {
            "driver_type" => add(
                THREE_STATE,
                format!(
                    "{at} has driver_type {v}; the checker does not model high impedance or \
                     drive strengths (open-drain, open-source, pull, bus-hold or resistive \
                     drivers)"
                ),
            ),
            "x_function" => add(
                UNMODELED_CELL,
                format!(
                    "{at} has x_function {v}, the condition under which it is unknown; the \
                     checker does not model unknown values"
                ),
            ),
            "state_function" | "internal_node" => add(
                UNMODELED_CELL,
                format!(
                    "{at} has {} {v}; the checker does not model the internal nodes of a \
                     cell",
                    a.name
                ),
            ),
            "retention_pin" => add(
                UNMODELED_CELL,
                format!(
                    "{at} has retention_pin {v}; the checker does not model saving and restoring \
                     state"
                ),
            ),
            _ => {}
        }
    }
}

/// The attributes and groups of an `ff` or `latch` group that the model does not keep.
fn sequential(g: &Group, kept: &[&str], add: &mut dyn FnMut(&'static str, String)) {
    let at = format!("its {} group ({})", g.kind, g.args.join(", "));
    for a in &g.attrs {
        let v = value(&a.value);
        if a.name == "enable_also" {
            add(
                DUAL_EDGE,
                format!(
                    "enable_also {v} of {at} makes it a master-slave latch pair; the checker \
                     models a latch with one enable"
                ),
            );
        } else if !kept.contains(&a.name.as_str()) {
            add(
                UNMODELED_CELL,
                format!(
                    "{at} has the attribute {} {v}, which the checker does not read",
                    a.name
                ),
            );
        }
    }
    for inner in &g.groups {
        add(
            UNMODELED_CELL,
            format!(
                "{at} holds a {} group, which the checker does not read",
                inner.kind
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::unread;

    // CHECK-REQ-0023: the constructs that the library model does not keep are found per cell,
    // in a bus too; power_down_function, test cells and the attributes the model keeps are not.
    #[test]
    fn finds_the_constructs_of_each_cell() {
        let text = r#"library (l) {
  cell (od) { pin (Y) { direction : output; function : "A"; driver_type : "open_drain"; } }
  cell (lat2) {
    latch (IQ, IQN) { enable : "G"; data_in : "D"; enable_also : "G2"; power_down_function : "!VDD"; }
  }
  cell (ff_x) { ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; master : "M"; } }
  cell (bank) { ff_bank (IQ, IQN, 4) { next_state : "D"; clocked_on : "CK"; } }
  cell (b) { bus (Q) { direction : output; pin (Q[0]) { x_function : "E"; } } }
  cell (ok) {
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; clear : "!R"; clear_preset_var1 : L; }
    pin (Q) { direction : output; function : "IQ"; power_down_function : "!VDD"; }
    test_cell () { pin (D) { driver_type : pull_up; } }
  }
}
library (second) { cell (od2) { pin (Y) { driver_type : pull_up; } } }"#;
        let found = unread(text, "l.lib").expect("scan");
        let got: Vec<(&str, &str)> = found.iter().map(|u| (u.cell.as_str(), u.code)).collect();
        assert_eq!(
            got,
            [
                ("od", "C0101"),
                ("lat2", "C0102"),
                ("ff_x", "C0104"),
                ("bank", "C0104"),
                ("b", "C0104"),
            ]
        );
        assert!(
            found[0]
                .what
                .starts_with("pin Y has driver_type \"open_drain\"")
        );
        assert!(
            found[1]
                .what
                .starts_with("enable_also \"G2\" of its latch group (IQ, IQN)")
        );
        assert!(found[2].what.contains("the attribute master \"M\""));
        assert!(found[3].what.starts_with("an ff_bank group (IQ, IQN, 4)"));
        assert!(found[4].what.starts_with("pin Q[0] has x_function \"E\""));
        assert!(unread("library (l) { cell (", "l.lib").is_err());
    }
}
