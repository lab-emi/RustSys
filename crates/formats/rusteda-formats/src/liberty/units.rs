// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Unit attributes of a library (LIBERTY-REQ-0005, LIBERTY-REQ-0006): their text forms and their
//! size in the fixed unit of each quantity (LIBERTY-REQ-0017).

use rusteda_core::diag::Severity;
use rusteda_core::liberty::{Unit, Units};

use super::diags::{Diags, code};
use super::syntax::{AttrValue, Group};

/// Unit names (lower case) and their size in the fixed unit, longest names first so that a
/// suffix match finds the longest name.
const TIME: [(&str, f64); 6] = [
    ("ms", 1e6),
    ("us", 1e3),
    ("ns", 1.0),
    ("ps", 1e-3),
    ("fs", 1e-6),
    ("s", 1e9),
];
const CAPACITANCE: [(&str, f64); 6] = [
    ("mf", 1e9),
    ("uf", 1e6),
    ("nf", 1e3),
    ("pf", 1.0),
    ("ff", 1e-3),
    ("f", 1e12),
];
const RESISTANCE: [(&str, f64); 2] = [("kohm", 1.0), ("ohm", 1e-3)];
const VOLTAGE: [(&str, f64); 3] = [("mv", 1e-3), ("uv", 1e-6), ("v", 1.0)];
const CURRENT: [(&str, f64); 5] = [
    ("ma", 1.0),
    ("ua", 1e-3),
    ("na", 1e-6),
    ("pa", 1e-9),
    ("a", 1e3),
];
const POWER: [(&str, f64); 6] = [
    ("mw", 1e6),
    ("uw", 1e3),
    ("nw", 1.0),
    ("pw", 1e-3),
    ("fw", 1e-6),
    ("w", 1e9),
];

/// The size of `text`, a number followed by a unit name of `names` (letter case ignored, blanks
/// allowed between them; a missing number counts as 1), when it is positive and finite.
fn parse_simple(text: &str, names: &[(&str, f64)]) -> Option<f64> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    let (name, size) = names.iter().find(|(n, _)| lower.ends_with(n))?;
    let num = t[..t.len() - name.len()].trim();
    let n: f64 = if num.is_empty() {
        1.0
    } else {
        num.parse().ok()?
    };
    (n.is_finite() && n > 0.0).then_some(n * size)
}

/// The size of `capacitive_load_unit (<number>, <unit name>)`, when it is positive and finite.
fn parse_capacitive(args: &[String]) -> Option<f64> {
    let [num, name] = args else {
        return None;
    };
    let n: f64 = num.trim().parse().ok()?;
    let lower = name.trim().to_ascii_lowercase();
    let (_, size) = CAPACITANCE.iter().find(|(u, _)| *u == lower)?;
    (n.is_finite() && n > 0.0).then_some(n * size)
}

/// Reads one simple unit attribute: given when it parses, else defaulted with an error.
fn simple_unit(g: &Group, name: &str, names: &[(&str, f64)], diags: &mut Diags) -> Unit {
    let found = g.attrs.iter().find_map(|a| match &a.value {
        AttrValue::Simple(t) if a.name == name => Some((t, (a.line, a.col))),
        _ => None,
    });
    match found {
        Some((t, at)) => checked(parse_simple(t, names), t.clone(), name, at, diags),
        None => Unit::default(),
    }
}

/// A given unit, or the default unit with the text kept and an error reported.
fn checked(
    parsed: Option<f64>,
    text: String,
    name: &str,
    at: (u32, u32),
    diags: &mut Diags,
) -> Unit {
    match parsed {
        Some(scale) => Unit::given(scale, text),
        None => {
            diags.report(
                Severity::Error,
                code::BAD_UNIT,
                &format!("library/{name}"),
                at,
                || format!("'{text}' is not a valid unit"),
            );
            let mut u = Unit::default();
            u.text = Some(text);
            u
        }
    }
}

/// The units of the library group `g` (LIBERTY-REQ-0005, LIBERTY-REQ-0017).
pub(super) fn units(g: &Group, diags: &mut Diags) -> Units {
    let mut u = Units::default();
    u.time = simple_unit(g, "time_unit", &TIME, diags);
    u.resistance = simple_unit(g, "pulling_resistance_unit", &RESISTANCE, diags);
    u.voltage = simple_unit(g, "voltage_unit", &VOLTAGE, diags);
    u.current = simple_unit(g, "current_unit", &CURRENT, diags);
    u.leakage_power = simple_unit(g, "leakage_power_unit", &POWER, diags);
    let cap = g.attrs.iter().find_map(|a| match &a.value {
        AttrValue::Complex(args) if a.name == "capacitive_load_unit" => {
            Some((args, (a.line, a.col)))
        }
        _ => None,
    });
    if let Some((args, at)) = cap {
        let parsed = parse_capacitive(args);
        u.capacitance = checked(parsed, args.join(","), "capacitive_load_unit", at, diags);
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    // LIBERTY-REQ-0005: number and unit name, letter case ignored, blanks allowed, longest name
    // first.
    #[test]
    fn unit_texts() {
        let parse_time = |t: &str| parse_simple(t, &TIME);
        assert_eq!(parse_time("1ns"), Some(1.0));
        assert_eq!(parse_time("10ps"), Some(10.0 * 1e-3));
        assert_eq!(parse_time("1 NS"), Some(1.0));
        assert_eq!(parse_time("ns"), Some(1.0));
        assert_eq!(parse_time("1us"), Some(1e3));
        assert_eq!(parse_time("2s"), Some(2e9));
        assert_eq!(parse_time("1e-3ns"), Some(1e-3));
        assert_eq!(parse_time("1"), None);
        assert_eq!(parse_time("0ns"), None);
        assert_eq!(parse_time("-1ns"), None);
        assert_eq!(parse_time("1 nanosecond"), None);
        assert_eq!(parse_simple("1kohm", &RESISTANCE), Some(1.0));
        assert_eq!(parse_simple("100ohm", &RESISTANCE), Some(0.1));
        assert_eq!(parse_simple("10mV", &VOLTAGE), Some(0.01));
        assert_eq!(parse_simple("1V", &VOLTAGE), Some(1.0));
        assert_eq!(parse_simple("100uA", &CURRENT), Some(0.1));
        assert_eq!(parse_simple("1A", &CURRENT), Some(1e3));
        assert_eq!(parse_simple("1uW", &POWER), Some(1e3));
        assert_eq!(parse_simple("1pW", &POWER), Some(1e-3));
        let cap = |a: &str, b: &str| parse_capacitive(&[a.to_string(), b.to_string()]);
        assert_eq!(cap("1", "ff"), Some(1e-3));
        assert_eq!(cap("1.0000000000", "pf"), Some(1.0));
        assert_eq!(cap("10", "FF"), Some(10.0 * 1e-3));
        assert_eq!(cap("1", "farad"), None);
        assert_eq!(cap("x", "pf"), None);
        assert_eq!(parse_capacitive(&["1".to_string()]), None);
    }
}
