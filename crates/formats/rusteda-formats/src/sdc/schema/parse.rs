// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Option argument parser and unit scaling for SDC commands (`SDC-REQ-0002`, `SDC-REQ-0016`).

use rusteda_core::diag::{Diag, Severity};

use super::super::Units;
use super::super::cmd::{
    CmdKind, EarlyLateScope, EdgeScope, ExceptionEnd, MinMaxScope, SetupHoldScope, StartEndScope,
};
use super::super::eval::TclVal;
use super::super::filter::Filter;
use super::super::obj::{Obj, ObjQuery};
use super::build::build_cmd_kind;
use super::{CmdSchema, CmdStatus, OptValueKind, find_query_schema, find_schema};

/// Active unit scaling factor applied at import time so that `ps -> ns` division (`v / 1000.0`)
/// avoids IEEE 754 `f64` rounding drift (`300.0 / 1000.0 == 0.3`; SDC-REQ-0016).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum UnitScale {
    #[default]
    Identity,
    Mul(f64),
    Div(f64),
}

impl UnitScale {
    /// Applies this unit scale to `v`, normalizing `-0.0` to `+0.0`.
    pub fn apply(self, v: f64) -> f64 {
        let r = match self {
            UnitScale::Identity => v,
            UnitScale::Mul(m) => v * m,
            UnitScale::Div(d) => v / d,
        };
        if r == 0.0 { 0.0 } else { r }
    }
}

/// Active unit scales maintained during evaluation of an SDC command stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct ActiveUnits {
    pub time: UnitScale,
    pub cap: UnitScale,
    pub res: UnitScale,
    pub voltage: UnitScale,
    pub current: UnitScale,
    pub power: UnitScale,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) enum ParsedOptVal {
    Flag,
    Str(String),
    Float(f64),
    Int(u32),
    FloatPair([f64; 2]),
    FloatTriple([f64; 3]),
    IntTriple([u32; 3]),
    Obj(Obj),
    Filter(Filter),
}

#[derive(Debug, Default)]
pub(super) struct ParsedArgs {
    pub(super) opts: Vec<(&'static str, ParsedOptVal)>,
    pub(super) pos: Vec<TclVal>,
}

impl ParsedArgs {
    pub(super) fn has_flag(&self, name: &str) -> bool {
        self.opts
            .iter()
            .any(|(k, v)| *k == name && matches!(v, ParsedOptVal::Flag))
    }

    pub(super) fn get_str(&self, name: &str) -> Option<String> {
        self.opts.iter().find_map(|(k, v)| match v {
            ParsedOptVal::Str(s) if *k == name => Some(s.clone()),
            _ => None,
        })
    }

    pub(super) fn get_float(&self, name: &str) -> Option<f64> {
        self.opts.iter().find_map(|(k, v)| match v {
            ParsedOptVal::Float(x) if *k == name => Some(*x),
            _ => None,
        })
    }

    pub(super) fn get_float_pair(&self, name: &str) -> Option<[f64; 2]> {
        self.opts.iter().find_map(|(k, v)| match v {
            ParsedOptVal::FloatPair(x) if *k == name => Some(*x),
            _ => None,
        })
    }

    pub(super) fn get_obj(&self, name: &str) -> Option<Obj> {
        self.opts.iter().find_map(|(k, v)| match v {
            ParsedOptVal::Obj(o) if *k == name => Some(o.clone()),
            _ => None,
        })
    }

    pub(super) fn get_objs(&self, name: &str) -> Vec<Obj> {
        self.opts
            .iter()
            .filter_map(|(k, v)| match v {
                ParsedOptVal::Obj(o) if *k == name => Some(o.clone()),
                _ => None,
            })
            .collect()
    }

    pub(super) fn get_filter(&self, name: &str) -> Option<Filter> {
        self.opts.iter().find_map(|(k, v)| match v {
            ParsedOptVal::Filter(f) if *k == name => Some(f.clone()),
            _ => None,
        })
    }

    pub(super) fn edge_scope(&self) -> EdgeScope {
        let rise = self.has_flag("-rise");
        let fall = self.has_flag("-fall");
        match (rise, fall) {
            (true, false) => EdgeScope::Rise,
            (false, true) => EdgeScope::Fall,
            _ => EdgeScope::Both,
        }
    }

    pub(super) fn min_max_scope(&self) -> MinMaxScope {
        let min = self.has_flag("-min");
        let max = self.has_flag("-max");
        match (min, max) {
            (true, false) => MinMaxScope::Min,
            (false, true) => MinMaxScope::Max,
            _ => MinMaxScope::Both,
        }
    }

    pub(super) fn setup_hold_scope(&self) -> SetupHoldScope {
        let setup = self.has_flag("-setup");
        let hold = self.has_flag("-hold");
        match (setup, hold) {
            (true, false) => SetupHoldScope::Setup,
            (false, true) => SetupHoldScope::Hold,
            _ => SetupHoldScope::Both,
        }
    }

    pub(super) fn early_late_scope(&self) -> EarlyLateScope {
        let early = self.has_flag("-early");
        let late = self.has_flag("-late");
        match (early, late) {
            (true, false) => EarlyLateScope::Early,
            (false, true) => EarlyLateScope::Late,
            _ => EarlyLateScope::Both,
        }
    }

    pub(super) fn start_end_scope(&self) -> StartEndScope {
        if self.has_flag("-start") {
            StartEndScope::Start
        } else if self.has_flag("-end") {
            StartEndScope::End
        } else {
            StartEndScope::Default
        }
    }

    pub(super) fn end_from(&self) -> Option<ExceptionEnd> {
        if let Some(o) = self.get_obj("-from") {
            Some(ExceptionEnd::both(o))
        } else if let Some(o) = self.get_obj("-rise_from") {
            Some(ExceptionEnd::rise(o))
        } else {
            self.get_obj("-fall_from").map(ExceptionEnd::fall)
        }
    }

    pub(super) fn to_end(&self) -> Option<ExceptionEnd> {
        if let Some(o) = self.get_obj("-to") {
            Some(ExceptionEnd::both(o))
        } else if let Some(o) = self.get_obj("-rise_to") {
            Some(ExceptionEnd::rise(o))
        } else {
            self.get_obj("-fall_to").map(ExceptionEnd::fall)
        }
    }
}

fn is_signed_number_token(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'-' {
        return false;
    }
    let next = bytes[1];
    if next.is_ascii_digit() || (next == b'.' && bytes.len() >= 3 && bytes[2].is_ascii_digit()) {
        s.parse::<f64>().is_ok()
    } else {
        false
    }
}

pub(super) fn parse_f64_val(
    v: &TclVal,
    cmd: &str,
    what: &str,
) -> Result<f64, (&'static str, String)> {
    let s = v.as_scalar_str().ok_or_else(|| {
        (
            "D0005",
            format!("'{cmd}' expects a number for {what}, found object collection"),
        )
    })?;
    let n = s.trim().parse::<f64>().map_err(|_| {
        (
            "D0005",
            format!("'{cmd}' expects a number for {what}, found '{s}'"),
        )
    })?;
    if !n.is_finite() {
        return Err((
            "D0005",
            format!("'{cmd}' expects a finite number for {what}, found '{s}'"),
        ));
    }
    Ok(if n == 0.0 { 0.0 } else { n })
}

pub(super) fn parse_u32_val(
    v: &TclVal,
    cmd: &str,
    what: &str,
) -> Result<u32, (&'static str, String)> {
    let s = v.as_scalar_str().ok_or_else(|| {
        (
            "D0005",
            format!("'{cmd}' expects an integer for {what}, found object collection"),
        )
    })?;
    s.trim().parse::<u32>().map_err(|_| {
        (
            "D0005",
            format!("'{cmd}' expects a non-negative integer for {what}, found '{s}'"),
        )
    })
}

pub(super) fn parse_obj_val(
    v: &TclVal,
    cmd: &str,
    what: &str,
) -> Result<Obj, (&'static str, String)> {
    v.to_obj().ok_or_else(|| {
        (
            "D0005",
            format!("'{cmd}' expects a design object or name list for {what}"),
        )
    })
}

fn parse_schema_args(
    schema: &CmdSchema,
    args: &[TclVal],
) -> Result<ParsedArgs, (&'static str, String)> {
    let mut out = ParsedArgs::default();
    let mut i = 0;
    while i < args.len() {
        if let Some(tok) = args[i].as_option_token()
            && (!is_signed_number_token(tok) || schema.options.iter().any(|o| o.name == tok))
        {
            let opt = schema
                .options
                .iter()
                .find(|o| o.name == tok)
                .ok_or_else(|| {
                    (
                        "D0003",
                        format!("unknown option '{tok}' for '{}'", schema.name),
                    )
                })?;
            if opt.phase2b {
                return Err((
                    "D0011",
                    format!(
                        "option '{tok}' of '{}' is not supported yet (Phase 2b)",
                        schema.name
                    ),
                ));
            }
            let canon_name = if opt.name == "-hier" {
                "-hierarchical"
            } else {
                opt.name
            };
            if !opt.repeatable && out.opts.iter().any(|(k, _)| *k == canon_name) {
                return Err((
                    "D0004",
                    format!("duplicate option '{tok}' in '{}'", schema.name),
                ));
            }
            if let Some(grp) = opt.exclusive_group
                && let Some((prev, _)) = out.opts.iter().find(|(k, _)| {
                    schema.options.iter().any(|o| {
                        o.name == *k && o.exclusive_group == Some(grp) && o.name != canon_name
                    })
                })
            {
                return Err((
                    "D0004",
                    format!(
                        "options '{prev}' and '{tok}' are mutually exclusive in '{}'",
                        schema.name
                    ),
                ));
            }
            if opt.kind == OptValueKind::Flag {
                out.opts.push((canon_name, ParsedOptVal::Flag));
                i += 1;
                continue;
            }
            if i + 1 >= args.len() {
                return Err((
                    "D0004",
                    format!("option '{tok}' in '{}' requires a value", schema.name),
                ));
            }
            let val_arg = &args[i + 1];
            let parsed_val = match opt.kind {
                OptValueKind::Flag => unreachable!(),
                OptValueKind::Str => {
                    let s = val_arg.as_scalar_str().ok_or_else(|| {
                        (
                            "D0005",
                            format!(
                                "'{}' expects a string for '{tok}', found object collection",
                                schema.name
                            ),
                        )
                    })?;
                    ParsedOptVal::Str(s)
                }
                OptValueKind::Float => {
                    ParsedOptVal::Float(parse_f64_val(val_arg, schema.name, tok)?)
                }
                OptValueKind::Int => ParsedOptVal::Int(parse_u32_val(val_arg, schema.name, tok)?),
                OptValueKind::FloatPair => {
                    let items = val_arg.to_string_list().ok_or_else(|| {
                        (
                            "D0005",
                            format!("'{}' expects a two-number list for '{tok}'", schema.name),
                        )
                    })?;
                    if items.len() != 2 {
                        return Err((
                            "D0005",
                            format!(
                                "'{}' option '{tok}' expects 2 numbers, got {}",
                                schema.name,
                                items.len()
                            ),
                        ));
                    }
                    let a = items[0].parse::<f64>().map_err(|_| {
                        (
                            "D0005",
                            format!("'{}' option '{tok}' expects numbers", schema.name),
                        )
                    })?;
                    let b = items[1].parse::<f64>().map_err(|_| {
                        (
                            "D0005",
                            format!("'{}' option '{tok}' expects numbers", schema.name),
                        )
                    })?;
                    if !a.is_finite() || !b.is_finite() {
                        return Err((
                            "D0005",
                            format!("'{}' option '{tok}' expects finite numbers", schema.name),
                        ));
                    }
                    ParsedOptVal::FloatPair([
                        if a == 0.0 { 0.0 } else { a },
                        if b == 0.0 { 0.0 } else { b },
                    ])
                }
                OptValueKind::FloatTriple => {
                    let items = val_arg.to_string_list().ok_or_else(|| {
                        (
                            "D0005",
                            format!("'{}' expects a three-number list for '{tok}'", schema.name),
                        )
                    })?;
                    if items.len() != 3 {
                        return Err((
                            "D0005",
                            format!(
                                "'{}' option '{tok}' expects 3 numbers, got {}",
                                schema.name,
                                items.len()
                            ),
                        ));
                    }
                    let mut arr = [0.0; 3];
                    for (idx, it) in items.iter().enumerate() {
                        let v = it.parse::<f64>().map_err(|_| {
                            (
                                "D0005",
                                format!("'{}' option '{tok}' expects numbers", schema.name),
                            )
                        })?;
                        if !v.is_finite() {
                            return Err((
                                "D0005",
                                format!("'{}' option '{tok}' expects finite numbers", schema.name),
                            ));
                        }
                        arr[idx] = if v == 0.0 { 0.0 } else { v };
                    }
                    ParsedOptVal::FloatTriple(arr)
                }
                OptValueKind::IntTriple => {
                    let items = val_arg.to_string_list().ok_or_else(|| {
                        (
                            "D0005",
                            format!("'{}' expects a three-integer list for '{tok}'", schema.name),
                        )
                    })?;
                    if items.len() != 3 {
                        return Err((
                            "D0005",
                            format!(
                                "'{}' option '{tok}' expects 3 integers, got {}",
                                schema.name,
                                items.len()
                            ),
                        ));
                    }
                    let mut arr = [0u32; 3];
                    for (idx, it) in items.iter().enumerate() {
                        arr[idx] = it.parse::<u32>().map_err(|_| {
                            (
                                "D0005",
                                format!("'{}' option '{tok}' expects integers", schema.name),
                            )
                        })?;
                    }
                    ParsedOptVal::IntTriple(arr)
                }
                OptValueKind::Obj => ParsedOptVal::Obj(parse_obj_val(val_arg, schema.name, tok)?),
                OptValueKind::Filter => {
                    let s = val_arg.as_scalar_str().ok_or_else(|| {
                        (
                            "D0005",
                            format!("'{}' expects a filter expression string", schema.name),
                        )
                    })?;
                    let f = Filter::parse(&s).map_err(|e| ("D0002", e))?;
                    ParsedOptVal::Filter(f)
                }
            };
            out.opts.push((canon_name, parsed_val));
            i += 2;
        } else {
            out.pos.push(args[i].clone());
            i += 1;
        }
    }
    if out.pos.len() < schema.min_pos || out.pos.len() > schema.max_pos {
        return Err((
            "D0004",
            format!(
                "'{}' expects {}..={} positional arguments, got {}",
                schema.name,
                schema.min_pos,
                schema.max_pos,
                out.pos.len()
            ),
        ));
    }
    Ok(out)
}

/// Parses an object-query command (`get_ports`, `all_inputs`, etc.; SDC-REQ-0005).
pub fn parse_query_cmd(name: &str, args: &[TclVal]) -> Result<Obj, (&'static str, String)> {
    let schema = find_query_schema(name).ok_or_else(|| {
        (
            "D0010",
            format!("command '{name}' is not a supported object query"),
        )
    })?;
    let parsed = parse_schema_args(schema, args)?;
    match name {
        "all_inputs" => Ok(Obj::AllInputs {
            no_clocks: parsed.has_flag("-no_clocks"),
        }),
        "all_outputs" => Ok(Obj::AllOutputs),
        "all_clocks" => Ok(Obj::AllClocks),
        "all_registers" => Ok(Obj::AllRegisters),
        "current_design" => {
            if !parsed.pos.is_empty() {
                return Err((
                    "D0004",
                    "object query '[current_design]' takes no arguments".to_string(),
                ));
            }
            Ok(Obj::CurrentDesign)
        }
        "get_ports" | "get_port" | "get_pins" | "get_cells" | "get_nets" | "get_clocks"
        | "get_lib_cells" | "get_lib_pins" => {
            let of_objects = parsed.get_obj("-of_objects").map(Box::new);
            let filter = parsed.get_filter("-filter");
            let mut patterns = Vec::new();
            for p in &parsed.pos {
                let items = p.to_string_list().ok_or_else(|| {
                    (
                        "D0005",
                        format!("'{name}' expects pattern strings, found object collection"),
                    )
                })?;
                patterns.extend(items);
            }
            if of_objects.is_some() && !patterns.is_empty() {
                return Err((
                    "D0004",
                    format!("'{name}' cannot combine '-of_objects' with positional patterns"),
                ));
            }
            if of_objects.is_none() && filter.is_none() && patterns.is_empty() {
                return Err((
                    "D0004",
                    format!("'{name}' requires at least one pattern, '-of_objects', or '-filter'"),
                ));
            }
            let q = ObjQuery {
                patterns,
                hierarchical: parsed.has_flag("-hierarchical"),
                regexp: parsed.has_flag("-regexp"),
                nocase: parsed.has_flag("-nocase"),
                of_objects,
                filter,
                quiet: parsed.has_flag("-quiet"),
            };
            Ok(match name {
                "get_ports" | "get_port" => Obj::Ports(q),
                "get_pins" => Obj::Pins(q),
                "get_cells" => Obj::Cells(q),
                "get_nets" => Obj::Nets(q),
                "get_clocks" => Obj::Clocks(q),
                "get_lib_cells" => Obj::LibCells(q),
                "get_lib_pins" => Obj::LibPins(q),
                _ => unreachable!(),
            })
        }
        _ => unreachable!(),
    }
}

pub(super) fn parse_unit_spec(raw: &str, suffixes: &[(&str, UnitScale)]) -> Option<UnitScale> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let mut num_end = 0;
    let bytes = s.as_bytes();
    while num_end < bytes.len()
        && (bytes[num_end].is_ascii_digit()
            || matches!(bytes[num_end], b'.' | b'+' | b'-' | b'e' | b'E'))
    {
        num_end += 1;
    }
    let (mult, unit_part) = if num_end == 0 {
        (1.0, s)
    } else {
        let m = s[..num_end].parse::<f64>().ok()?;
        if !m.is_finite() || m <= 0.0 {
            return None;
        }
        (m, s[num_end..].trim())
    };
    let (_, base_scale) = suffixes
        .iter()
        .find(|(suf, _)| unit_part.eq_ignore_ascii_case(suf))?;
    if (mult - 1.0).abs() < 1e-15 {
        return Some(*base_scale);
    }
    match *base_scale {
        UnitScale::Identity => Some(UnitScale::Mul(mult)),
        UnitScale::Mul(f) => Some(UnitScale::Mul(mult * f)),
        UnitScale::Div(d) => {
            if d >= mult && (d / mult).fract() == 0.0 {
                let ratio = d / mult;
                if (ratio - 1.0).abs() < 1e-15 {
                    Some(UnitScale::Identity)
                } else {
                    Some(UnitScale::Div(ratio))
                }
            } else {
                Some(UnitScale::Mul(mult / d))
            }
        }
    }
}

pub(super) const TIME_UNITS: &[(&str, UnitScale)] = &[
    ("s", UnitScale::Mul(1_000_000_000.0)),
    ("ms", UnitScale::Mul(1_000_000.0)),
    ("us", UnitScale::Mul(1_000.0)),
    ("ns", UnitScale::Identity),
    ("ps", UnitScale::Div(1_000.0)),
    ("fs", UnitScale::Div(1_000_000.0)),
];

pub(super) const CAP_UNITS: &[(&str, UnitScale)] = &[
    ("f", UnitScale::Mul(1_000_000_000_000.0)),
    ("mf", UnitScale::Mul(1_000_000_000.0)),
    ("uf", UnitScale::Mul(1_000_000.0)),
    ("nf", UnitScale::Mul(1_000.0)),
    ("pf", UnitScale::Identity),
    ("ff", UnitScale::Div(1_000.0)),
    ("af", UnitScale::Div(1_000_000.0)),
];

pub(super) const RES_UNITS: &[(&str, UnitScale)] = &[
    ("mohm", UnitScale::Div(1_000_000.0)),
    ("ohm", UnitScale::Div(1_000.0)),
    ("kohm", UnitScale::Identity),
    ("megohm", UnitScale::Mul(1_000.0)),
];

pub(super) const VOLT_UNITS: &[(&str, UnitScale)] = &[
    ("kv", UnitScale::Mul(1_000.0)),
    ("v", UnitScale::Identity),
    ("mv", UnitScale::Div(1_000.0)),
    ("uv", UnitScale::Div(1_000_000.0)),
];

pub(super) const CURR_UNITS: &[(&str, UnitScale)] = &[
    ("a", UnitScale::Mul(1_000.0)),
    ("ma", UnitScale::Identity),
    ("ua", UnitScale::Div(1_000.0)),
    ("na", UnitScale::Div(1_000_000.0)),
    ("pa", UnitScale::Div(1_000_000_000.0)),
];

pub(super) const POWER_UNITS: &[(&str, UnitScale)] = &[
    ("w", UnitScale::Mul(1_000_000_000.0)),
    ("mw", UnitScale::Mul(1_000_000.0)),
    ("uw", UnitScale::Mul(1_000.0)),
    ("nw", UnitScale::Identity),
    ("pw", UnitScale::Div(1_000.0)),
    ("fw", UnitScale::Div(1_000_000.0)),
];

/// Parses and validates a top-level SDC command (SDC-REQ-0002, SDC-REQ-0016 to SDC-REQ-0039).
pub fn parse_top_cmd(
    name: &str,
    args: &[TclVal],
    active: &mut ActiveUnits,
    sdc_units: &mut Units,
    current_design: &mut Option<String>,
    loc: (&str, u32, u32),
    diags: &mut Vec<Diag>,
) -> Option<CmdKind> {
    let Some(schema) = find_schema(name) else {
        diags.push(
            Diag::new(
                Severity::Error,
                "D0010",
                format!("command '{name}' is not supported"),
            )
            .at(loc.0, loc.1, loc.2),
        );
        return None;
    };

    match schema.status {
        CmdStatus::Phase2b => {
            diags.push(
                Diag::new(
                    Severity::Error,
                    "D0010",
                    format!("command '{name}' is not supported yet (Phase 2b)"),
                )
                .at(loc.0, loc.1, loc.2),
            );
            return None;
        }
        CmdStatus::Unsupported => {
            diags.push(
                Diag::new(
                    Severity::Error,
                    "D0010",
                    format!("command '{name}' is not supported"),
                )
                .at(loc.0, loc.1, loc.2),
            );
            return None;
        }
        CmdStatus::Interpreted | CmdStatus::RecordedDrc | CmdStatus::RecordedNoTimingEffect => {}
    }

    let parsed = match parse_schema_args(schema, args) {
        Ok(p) => p,
        Err((code, msg)) => {
            diags.push(Diag::new(Severity::Error, code, msg).at(loc.0, loc.1, loc.2));
            return None;
        }
    };

    let build_res = build_cmd_kind(schema.name, &parsed, active, sdc_units, current_design);
    match build_res {
        Ok(kind) => {
            match schema.status {
                CmdStatus::RecordedDrc => {
                    diags.push(
                        Diag::new(
                            Severity::Warning,
                            "D0021",
                            format!(
                                "design-rule command '{}' is recorded in CONSTR v1 but not analyzed by the timing engine",
                                schema.name
                            ),
                        )
                        .at(loc.0, loc.1, loc.2),
                    );
                }
                CmdStatus::RecordedNoTimingEffect => {
                    diags.push(
                        Diag::new(
                            Severity::Note,
                            "D0020",
                            format!(
                                "command '{}' is recorded in CONSTR v1 without timing effect",
                                schema.name
                            ),
                        )
                        .at(loc.0, loc.1, loc.2),
                    );
                }
                _ => {}
            }
            Some(kind)
        }
        Err((code, msg)) => {
            diags.push(Diag::new(Severity::Error, code, msg).at(loc.0, loc.1, loc.2));
            None
        }
    }
}
