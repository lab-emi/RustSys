// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Wire-load groups (`wire_load`, `wire_load_table`, `wire_load_selection`) and
//! `operating_conditions` groups (LIBERTY-REQ-0047, LIBERTY-REQ-0048).

use std::collections::BTreeMap;

use rusteda_core::liberty::{
    OperatingConditions, TreeType, WireLoad, WireLoadAreaRule, WireLoadSelection, WireLoadTable,
};

use super::classify::{Classifier, Ctx};
use super::diags::{Diags, code};
use super::syntax::{AttrValue, Group};

fn parse_f64(s: &str) -> Option<f64> {
    s.trim().trim_matches('"').parse::<f64>().ok()
}

fn parse_u32(s: &str) -> Option<u32> {
    let t = s.trim().trim_matches('"');
    if let Ok(u) = t.parse::<u32>() {
        return Some(u);
    }
    let f = t.parse::<f64>().ok()?;
    if f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= f64::from(u32::MAX) {
        Some(f as u32)
    } else {
        None
    }
}

fn number(g: &Group, path: &str, name: &str, diags: &mut Diags) -> Option<f64> {
    let (v, at) = g.attrs.iter().find_map(|a| match &a.value {
        AttrValue::Simple(v) if a.name == name => Some((v.as_str(), (a.line, a.col))),
        _ => None,
    })?;
    let x = parse_f64(v).filter(|x| x.is_finite());
    if x.is_none() {
        diags.warn(code::NOT_A_NUMBER, &format!("{path}/{name}"), at, || {
            format!("'{v}' is not a finite number; the default applies")
        });
    }
    x
}

/// Parses all complex `(fanout, value)` list attributes `name` of `g` at construct path `path`,
/// sorted by ascending fanout.
fn fanout_pairs(g: &Group, path: &str, name: &str, diags: &mut Diags) -> Vec<(u32, f64)> {
    let attr_path = format!("{path}/{name}");
    let mut pairs = Vec::new();
    for a in &g.attrs {
        let AttrValue::Complex(args) = &a.value else {
            continue;
        };
        if a.name != name {
            continue;
        }
        let at = (a.line, a.col);
        let tokens: Vec<&str> = args
            .iter()
            .flat_map(|s| s.split(','))
            .map(|t| t.trim().trim_matches('"'))
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.len() < 2 || !tokens.len().is_multiple_of(2) {
            diags.warn(code::NOT_A_NUMBER, &attr_path, at, || {
                format!("'{name}' requires (fanout, value) pairs; statement is skipped")
            });
            continue;
        }
        for pair in tokens.chunks_exact(2) {
            let (Some(fanout), Some(val)) = (
                parse_u32(pair[0]),
                parse_f64(pair[1]).filter(|v| v.is_finite()),
            ) else {
                diags.warn(code::NOT_A_NUMBER, &attr_path, at, || {
                    format!(
                        "'({}, {})' is not a valid (fanout, finite number) pair; it is skipped",
                        pair[0], pair[1]
                    )
                });
                continue;
            };
            pairs.push((fanout, val));
        }
    }
    pairs.sort_by_key(|&(f, _)| f);
    pairs
}

/// Reads all `wire_load` groups of the library group `lib` (LIBERTY-REQ-0047).
pub(super) fn wire_loads(
    lib: &Group,
    classes: &mut Classifier,
    diags: &mut Diags,
) -> BTreeMap<String, WireLoad> {
    const PATH: &str = "library/wire_load";
    let mut out = BTreeMap::new();
    for w in lib.children("wire_load") {
        classes.group(diags, Ctx::WireLoad, w, PATH);
        let Some(name) = w
            .args
            .first()
            .map(|s| s.trim().trim_matches('"').to_string())
            .filter(|s| !s.is_empty())
        else {
            diags.warn(code::UNKNOWN_VALUE, PATH, (w.line, w.col), || {
                "wire_load without a name is ignored".into()
            });
            continue;
        };
        let wl = WireLoad {
            name: name.clone(),
            resistance: number(w, PATH, "resistance", diags).unwrap_or(0.0),
            capacitance: number(w, PATH, "capacitance", diags).unwrap_or(0.0),
            area: number(w, PATH, "area", diags).unwrap_or(0.0),
            slope: number(w, PATH, "slope", diags).unwrap_or(0.0),
            fanout_length: fanout_pairs(w, PATH, "fanout_length", diags),
        };
        if out.contains_key(&name) {
            diags.warn(code::DUPLICATE, PATH, (w.line, w.col), || {
                format!("wire_load '{name}' is defined more than once; the last definition is used")
            });
        }
        out.insert(name, wl);
    }
    out
}

/// Reads all `wire_load_table` groups of the library group `lib` (LIBERTY-REQ-0047).
pub(super) fn wire_load_tables(
    lib: &Group,
    classes: &mut Classifier,
    diags: &mut Diags,
) -> BTreeMap<String, WireLoadTable> {
    const PATH: &str = "library/wire_load_table";
    let mut out = BTreeMap::new();
    for t in lib.children("wire_load_table") {
        classes.group(diags, Ctx::WireLoadTable, t, PATH);
        let Some(name) = t
            .args
            .first()
            .map(|s| s.trim().trim_matches('"').to_string())
            .filter(|s| !s.is_empty())
        else {
            diags.warn(code::UNKNOWN_VALUE, PATH, (t.line, t.col), || {
                "wire_load_table without a name is ignored".into()
            });
            continue;
        };
        let wlt = WireLoadTable {
            name: name.clone(),
            fanout_length: fanout_pairs(t, PATH, "fanout_length", diags),
            fanout_capacitance: fanout_pairs(t, PATH, "fanout_capacitance", diags),
            fanout_resistance: fanout_pairs(t, PATH, "fanout_resistance", diags),
            fanout_area: fanout_pairs(t, PATH, "fanout_area", diags),
        };
        if out.contains_key(&name) {
            diags.warn(code::DUPLICATE, PATH, (t.line, t.col), || {
                format!(
                    "wire_load_table '{name}' is defined more than once; the last definition is used"
                )
            });
        }
        out.insert(name, wlt);
    }
    out
}

/// Reads all `wire_load_selection` groups of the library group `lib` (LIBERTY-REQ-0047).
pub(super) fn wire_load_selections(
    lib: &Group,
    classes: &mut Classifier,
    diags: &mut Diags,
) -> BTreeMap<String, WireLoadSelection> {
    const PATH: &str = "library/wire_load_selection";
    const RULE_PATH: &str = "library/wire_load_selection/wire_load_from_area";
    let mut out = BTreeMap::new();
    for s in lib.children("wire_load_selection") {
        classes.group(diags, Ctx::WireLoadSelection, s, PATH);
        let Some(name) = s
            .args
            .first()
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
        else {
            diags.warn(code::UNKNOWN_VALUE, PATH, (s.line, s.col), || {
                "wire_load_selection without a name is ignored".into()
            });
            continue;
        };
        let mut rules = Vec::new();
        for a in &s.attrs {
            let AttrValue::Complex(args) = &a.value else {
                continue;
            };
            if a.name != "wire_load_from_area" {
                continue;
            }
            let at = (a.line, a.col);
            if args.len() != 3 {
                diags.warn(code::NOT_A_NUMBER, RULE_PATH, at, || {
                    "'wire_load_from_area' requires (min_area, max_area, wire_load); it is skipped"
                        .into()
                });
                continue;
            }
            let (Some(min_area), Some(max_area)) = (
                parse_f64(&args[0]).filter(|v| v.is_finite()),
                parse_f64(&args[1]).filter(|v| v.is_finite()),
            ) else {
                diags.warn(code::NOT_A_NUMBER, RULE_PATH, at, || {
                    format!(
                        "'({}, {})' are not finite area bounds; rule is skipped",
                        args[0], args[1]
                    )
                });
                continue;
            };
            let wire_load = args[2].trim().trim_matches('"').to_string();
            rules.push(WireLoadAreaRule {
                min_area,
                max_area,
                wire_load,
            });
        }
        let sel = WireLoadSelection {
            name: name.clone(),
            rules,
        };
        if out.contains_key(&name) {
            diags.warn(code::DUPLICATE, PATH, (s.line, s.col), || {
                format!(
                    "wire_load_selection '{name}' is defined more than once; the last definition is used"
                )
            });
        }
        out.insert(name, sel);
    }
    out
}

/// Reads all `operating_conditions` groups of the library group `lib` (LIBERTY-REQ-0048).
pub(super) fn operating_conditions(
    lib: &Group,
    classes: &mut Classifier,
    diags: &mut Diags,
) -> BTreeMap<String, OperatingConditions> {
    const PATH: &str = "library/operating_conditions";
    let mut out = BTreeMap::new();
    for oc in lib.children("operating_conditions") {
        classes.group(diags, Ctx::OperatingConditions, oc, PATH);
        let Some(name) = oc
            .args
            .first()
            .map(|s| s.trim().trim_matches('"').to_string())
            .filter(|s| !s.is_empty())
        else {
            diags.warn(code::UNKNOWN_VALUE, PATH, (oc.line, oc.col), || {
                "operating_conditions without a name is ignored".into()
            });
            continue;
        };
        let tree_type = oc.attrs.iter().find_map(|a| match &a.value {
            AttrValue::Simple(v) if a.name == "tree_type" => {
                let tt = TreeType::from_name(v.trim().trim_matches('"'));
                if !tt.is_known() {
                    diags.warn(
                        code::UNKNOWN_VALUE,
                        "library/operating_conditions/tree_type",
                        (a.line, a.col),
                        || format!("unknown tree_type '{tt}'"),
                    );
                }
                Some(tt)
            }
            _ => None,
        });
        let cond = OperatingConditions {
            name: name.clone(),
            process: number(oc, PATH, "process", diags),
            temperature: number(oc, PATH, "temperature", diags),
            voltage: number(oc, PATH, "voltage", diags),
            tree_type,
        };
        if out.contains_key(&name) {
            diags.warn(code::DUPLICATE, PATH, (oc.line, oc.col), || {
                format!(
                    "operating_conditions '{name}' is defined more than once; the last definition is used"
                )
            });
        }
        out.insert(name, cond);
    }
    out
}
