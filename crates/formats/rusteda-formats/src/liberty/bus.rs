// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Bus and bundle pin expansion and `statetable` parsing (LIBERTY-REQ-0044, LIBERTY-REQ-0059).

use std::collections::BTreeMap;

use rusteda_core::liberty::Statetable;

use super::classify::{Classifier, Ctx, is_supported_in_bus_or_bundle};
use super::diags::Diags;
use super::syntax::{Attr, AttrValue, Group};

/// A bit-vector `type(<name>)` definition (LIBERTY-REQ-0059).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BusType {
    pub bit_from: i32,
    pub bit_to: i32,
}

impl BusType {
    /// Bit indices in declaration order (`bit_from` to `bit_to`, inclusive).
    pub(super) fn indices(&self) -> Vec<i32> {
        if self.bit_from <= self.bit_to {
            (self.bit_from..=self.bit_to).collect()
        } else {
            (self.bit_to..=self.bit_from).rev().collect()
        }
    }

    /// Formats a member pin name using `style` (default `"%s[%d]"`).
    pub(super) fn format_member(bus_name: &str, index: i32, style: &str) -> String {
        if style.contains("%s") && style.contains("%d") {
            style
                .replacen("%s", bus_name, 1)
                .replacen("%d", &index.to_string(), 1)
        } else {
            format!("{bus_name}[{index}]")
        }
    }

    /// Member pin names for `bus_name` under `style` in declaration order.
    pub(super) fn member_names(&self, bus_name: &str, style: &str) -> Vec<String> {
        self.indices()
            .into_iter()
            .map(|i| Self::format_member(bus_name, i, style))
            .collect()
    }
}

/// Reads and classifies all `type(<name>)` groups directly inside `g` (`library` or `cell`).
pub(super) fn bus_types(
    g: &Group,
    parent_path: &str,
    classes: &mut Classifier,
    diags: &mut Diags,
) -> BTreeMap<String, Result<BusType, String>> {
    let path = format!("{parent_path}/type");
    let mut out = BTreeMap::new();
    for t in g.children("type") {
        classes.group(diags, Ctx::Type, t, &path);
        let Some(name) = t.args.first().cloned() else {
            continue;
        };
        out.insert(name, parse_bus_type(t));
    }
    out
}

fn parse_bus_type(t: &Group) -> Result<BusType, String> {
    if let Some(base) = t.simple("base_type") {
        if !base.trim().eq_ignore_ascii_case("array") {
            return Err(format!("unsupported base_type '{base}'"));
        }
    }
    if let Some(dt) = t.simple("data_type") {
        if !dt.trim().eq_ignore_ascii_case("bit") {
            return Err(format!("unsupported data_type '{dt}'"));
        }
    }
    let parse_i32 = |attr: &str| -> Result<Option<i32>, String> {
        match t.simple(attr) {
            None => Ok(None),
            Some(s) => s
                .trim()
                .parse::<i32>()
                .map(Some)
                .map_err(|_| format!("invalid {attr} '{s}'")),
        }
    };
    let bit_width = parse_i32("bit_width")?;
    let bit_from = parse_i32("bit_from")?;
    let bit_to = parse_i32("bit_to")?;
    let downto = match t
        .simple("downto")
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        None => None,
        Some("true" | "1") => Some(true),
        Some("false" | "0") => Some(false),
        Some(other) => return Err(format!("invalid downto '{other}'")),
    };
    let (from, to) = match (bit_from, bit_to, bit_width) {
        (Some(f), Some(to), w) => {
            if let Some(dt) = downto {
                if dt && f < to {
                    return Err(format!("downto is true but bit_from ({f}) < bit_to ({to})"));
                }
                if !dt && f > to {
                    return Err(format!(
                        "downto is false but bit_from ({f}) > bit_to ({to})"
                    ));
                }
            }
            let expected_w = (f - to).abs() + 1;
            if let Some(w) = w {
                if w != expected_w {
                    return Err(format!(
                        "bit_width ({w}) does not match bit_from ({f})..bit_to ({to})"
                    ));
                }
            }
            (f, to)
        }
        (None, None, Some(w)) if w > 0 => {
            if downto == Some(false) {
                (0, w - 1)
            } else {
                (w - 1, 0)
            }
        }
        _ => {
            return Err("type group must specify bit_from and bit_to (or bit_width > 0)".into());
        }
    };
    Ok(BusType {
        bit_from: from,
        bit_to: to,
    })
}

/// Expands a `bus` group into synthesized scalar `pin` groups, one per member in bus order.
pub(super) fn expand_bus(
    b: &Group,
    lib_types: &BTreeMap<String, Result<BusType, String>>,
    cell_types: &BTreeMap<String, Result<BusType, String>>,
    bus_naming_style: &str,
) -> Result<Vec<Group>, String> {
    check_supported_tree(Ctx::Bus, b)?;
    if b.args.len() != 1 || b.args[0].trim().is_empty() {
        return Err("bus group requires exactly one bus name".into());
    }
    let bus_name = b.args[0].trim();
    let Some(bt_name) = b.simple("bus_type").map(str::trim) else {
        return Err(format!("bus {bus_name} is missing bus_type"));
    };
    let bt = match cell_types.get(bt_name).or_else(|| lib_types.get(bt_name)) {
        Some(Ok(bt)) => bt,
        Some(Err(e)) => return Err(format!("bus {bus_name} bus_type '{bt_name}': {e}")),
        None => {
            return Err(format!(
                "bus {bus_name} refers to unknown bus_type '{bt_name}'"
            ));
        }
    };
    let members = bt.member_names(bus_name, bus_naming_style);
    synthesize_members(b, bus_name, &members, Some((bt, bus_naming_style)))
}

/// Expands a `bundle` group into synthesized scalar `pin` groups, one per member in bundle order.
pub(super) fn expand_bundle(b: &Group) -> Result<Vec<Group>, String> {
    check_supported_tree(Ctx::Bundle, b)?;
    if b.args.len() != 1 || b.args[0].trim().is_empty() {
        return Err("bundle group requires exactly one bundle name".into());
    }
    let bundle_name = b.args[0].trim();
    let Some(AttrValue::Complex(raw_members)) = b.attr("members").map(|a| &a.value) else {
        return Err(format!("bundle {bundle_name} is missing members(...)"));
    };
    let members: Vec<String> = raw_members
        .iter()
        .flat_map(|s| s.split(|c: char| c == ',' || c.is_whitespace()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if members.is_empty() {
        return Err(format!("bundle {bundle_name} has an empty members list"));
    }
    synthesize_members(b, bundle_name, &members, None)
}

fn check_supported_tree(ctx: Ctx, g: &Group) -> Result<(), String> {
    is_supported_in_bus_or_bundle(ctx, g)?;
    for c in &g.groups {
        let child_ctx = match c.kind.as_str() {
            "pin" => Ctx::Pin,
            "timing" => Ctx::Timing,
            "internal_power" => Ctx::InternalPower,
            "cell_rise" | "cell_fall" | "rise_transition" | "fall_transition"
            | "rise_constraint" | "fall_constraint" | "power" | "rise_power" | "fall_power" => {
                Ctx::Table
            }
            _ => continue,
        };
        check_supported_tree(child_ctx, c)?;
    }
    Ok(())
}

/// Builds one synthesized `pin` group per member in `members`, merging bus/bundle attributes and
/// child groups with any per-member `pin(...)` overrides.
fn synthesize_members(
    b: &Group,
    group_name: &str,
    members: &[String],
    bus_info: Option<(&BusType, &str)>,
) -> Result<Vec<Group>, String> {
    let mut member_overrides: BTreeMap<String, Vec<&Group>> = BTreeMap::new();
    for p in b.children("pin") {
        if p.args.is_empty() {
            return Err(format!(
                "{} {group_name} has a pin group without a name",
                b.kind
            ));
        }
        for arg in &p.args {
            let targets = resolve_pin_arg(arg.trim(), group_name, members, bus_info)?;
            for t in targets {
                member_overrides.entry(t).or_default().push(p);
            }
        }
    }

    let base_attrs: Vec<&Attr> = b
        .attrs
        .iter()
        .filter(|a| a.name != "bus_type" && a.name != "members")
        .collect();
    let base_groups: Vec<&Group> = b.groups.iter().filter(|g| g.kind != "pin").collect();

    let mut out = Vec::with_capacity(members.len());
    for m in members {
        let overrides = member_overrides.get(m).map(Vec::as_slice).unwrap_or(&[]);
        let (line, col) = overrides
            .first()
            .map_or((b.line, b.col), |p| (p.line, p.col));
        let mut attrs: Vec<Attr> = Vec::new();
        for &ba in &base_attrs {
            let overridden = overrides
                .iter()
                .any(|p| p.attrs.iter().any(|pa| pa.name == ba.name));
            if !overridden {
                attrs.push((*ba).clone());
            }
        }
        for &p in overrides {
            attrs.extend(p.attrs.iter().cloned());
        }
        let mut groups: Vec<Group> = base_groups.iter().map(|&g| (*g).clone()).collect();
        for &p in overrides {
            groups.extend(p.groups.iter().cloned());
        }
        out.push(Group {
            kind: "pin".to_string(),
            args: vec![m.clone()],
            line,
            col,
            attrs,
            groups,
        });
    }
    Ok(out)
}

fn resolve_pin_arg(
    arg: &str,
    group_name: &str,
    members: &[String],
    bus_info: Option<(&BusType, &str)>,
) -> Result<Vec<String>, String> {
    if members.iter().any(|m| m == arg) {
        return Ok(vec![arg.to_string()]);
    }
    // Support bus slice syntax such as `A[3:0]` inside `bus(A)`.
    if let Some((bt, style)) = bus_info {
        if let Some(rest) = arg.strip_prefix(group_name) {
            if let Some(inner) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                if let Some((hi_s, lo_s)) = inner.split_once(':') {
                    if let (Ok(from), Ok(to)) =
                        (hi_s.trim().parse::<i32>(), lo_s.trim().parse::<i32>())
                    {
                        let valid = bt.indices();
                        let slice: Vec<i32> = if from <= to {
                            (from..=to).collect()
                        } else {
                            (to..=from).rev().collect()
                        };
                        if !slice.is_empty() && slice.iter().all(|i| valid.contains(i)) {
                            return Ok(slice
                                .into_iter()
                                .map(|i| BusType::format_member(group_name, i, style))
                                .collect());
                        }
                    }
                }
            }
        }
    }
    Err(format!(
        "pin '{arg}' is not a member of {group_name} (members: {})",
        members.join(", ")
    ))
}

/// Parses a `statetable` group (LIBERTY-REQ-0044).
pub(super) fn statetable(g: &Group) -> Result<Statetable, String> {
    let input_nodes: Vec<String> = g
        .args
        .first()
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let internal_nodes: Vec<String> = g
        .args
        .get(1)
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    if input_nodes.is_empty() || internal_nodes.is_empty() {
        return Err("statetable requires non-empty input and internal node lists".into());
    }
    let Some(raw_table) = g.simple("table") else {
        return Err("statetable is missing 'table' attribute".into());
    };
    let cleaned = raw_table.replace('\\', " ");
    let mut rows = Vec::new();
    for raw_row in cleaned.split(',') {
        let trimmed = raw_row.trim();
        if trimmed.is_empty() {
            continue;
        }
        let cols: Vec<&str> = trimmed.split(':').collect();
        if cols.len() != 3 {
            return Err(format!(
                "statetable row '{trimmed}' must have 3 colon-separated fields"
            ));
        }
        let in_toks: Vec<&str> = cols[0].split_whitespace().collect();
        let cur_toks: Vec<&str> = cols[1].split_whitespace().collect();
        let next_toks: Vec<&str> = cols[2].split_whitespace().collect();
        if in_toks.len() != input_nodes.len()
            || cur_toks.len() != internal_nodes.len()
            || next_toks.len() != internal_nodes.len()
        {
            return Err(format!(
                "statetable row '{trimmed}' has ({}, {}, {}) entries; expected ({}, {}, {})",
                in_toks.len(),
                cur_toks.len(),
                next_toks.len(),
                input_nodes.len(),
                internal_nodes.len(),
                internal_nodes.len()
            ));
        }
        rows.push(format!(
            "{} : {} : {}",
            in_toks.join(" "),
            cur_toks.join(" "),
            next_toks.join(" ")
        ));
    }
    if rows.is_empty() {
        return Err("statetable 'table' has no rows".into());
    }
    Ok(Statetable {
        input_nodes,
        internal_nodes,
        table: raw_table.to_string(),
        rows,
    })
}
