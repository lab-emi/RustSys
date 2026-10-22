// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Library model built from the syntax tree (LIBERTY-REQ-0010 to LIBERTY-REQ-0019,
//! LIBERTY-REQ-0021, LIBERTY-REQ-0023 to LIBERTY-REQ-0026, LIBERTY-REQ-0040 to LIBERTY-REQ-0048),
//! with the messages of the reader (LIBERTY-REQ-0050, LIBERTY-REQ-0057 to LIBERTY-REQ-0059).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use rusteda_core::diag::{Diag, Severity};
use rusteda_core::liberty::{
    AntennaDiodeType, Cell, Direction, Func, InternalPower, LevelShifterType, Library, PgPin,
    PgType, Pin, SeqKind, Sequential, SwitchCellType, Table, Template, TimingArc, TimingSense,
    TimingType, WireLoadMode,
};

use super::Error;
use super::bus::{self, BusType};
use super::classify::{Classifier, Ctx};
use super::diags::{Diags, code};
use super::func::parse_function;
use super::syntax::{AttrValue, Group};
use super::{tables, units, wire_load};

fn parse_f64(s: &str) -> Option<f64> {
    s.trim().trim_matches('"').parse::<f64>().ok()
}

/// The first simple attribute `name` of `g` with its location.
fn simple_at<'g>(g: &'g Group, name: &str) -> Option<(&'g str, (u32, u32))> {
    g.attrs.iter().find_map(|a| match &a.value {
        AttrValue::Simple(v) if a.name == name => Some((v.as_str(), (a.line, a.col))),
        _ => None,
    })
}

/// The location of the first attribute `name` of `g`, else of `g` itself.
fn at(g: &Group, name: &str) -> (u32, u32) {
    g.attr(name).map_or((g.line, g.col), |a| (a.line, a.col))
}

/// Reads a Liberty file.
pub fn read_file(path: &Path) -> Result<Library, Error> {
    read_file_diags(path).map(|(lib, _)| lib)
}

/// Reads Liberty text; `name` names it in messages. The first `library` group is used.
pub fn read_text(text: &str, name: &str) -> Result<Library, Error> {
    read_text_diags(text, name).map(|(lib, _)| lib)
}

/// Reads a Liberty file and returns the library with the reader's messages (LIBERTY-REQ-0050).
pub fn read_file_diags(path: &Path) -> Result<(Library, Vec<Diag>), Error> {
    let text = std::fs::read(path).map_err(|e| Error::Io {
        path: path.display().to_string(),
        msg: e.to_string(),
    })?;
    let text = String::from_utf8_lossy(&text);
    read_text_diags(&text, &path.display().to_string())
}

/// Reads Liberty text and returns the library with the reader's messages (LIBERTY-REQ-0050);
/// `name` names the text in messages. The first `library` group is used.
pub fn read_text_diags(text: &str, name: &str) -> Result<(Library, Vec<Diag>), Error> {
    let root = super::syntax::parse(text, name)?;
    let lib = root
        .children("library")
        .next()
        .ok_or_else(|| Error::Model {
            file: name.to_string(),
            msg: "no library group".into(),
        })?;
    for attr in [
        "default_max_capacitance",
        "default_max_transition",
        "default_max_fanout",
        "default_fanout_load",
    ] {
        if let Some(s) = lib.simple(attr) {
            if !parse_f64(s).is_some_and(|v| v.is_finite() && v >= 0.0) {
                return Err(Error::Model {
                    file: name.into(),
                    msg: format!("{attr} must be finite and nonnegative"),
                });
            }
        }
    }
    let mut diags = Diags::new(name);
    let mut classes = Classifier::new(lib);
    classes.outside(&mut diags, &root, lib);
    let library = Builder {
        diags: &mut diags,
        classes,
    }
    .library(lib);
    Ok((library, diags.finish()))
}

/// Builds the model and reports what it cannot represent: each group it reads is classified
/// when it is read (LIBERTY-REQ-0051).
struct Builder<'d> {
    diags: &'d mut Diags,
    classes: Classifier,
}

/// Construct paths of the groups the builder reads.
const LIB: &str = "library";
const CELL: &str = "library/cell";
const PIN: &str = "library/cell/pin";
const TIMING: &str = "library/cell/pin/timing";
const POWER: &str = "library/cell/pin/internal_power";

/// Locations of a pin's `function`, `three_state`, and `state_function` attributes.
type PinFuncLocs = ((u32, u32), (u32, u32), (u32, u32));

/// The table groups of a timing group, with their construct paths.
const ARC_TABLES: [(&str, &str); 6] = [
    ("cell_rise", "library/cell/pin/timing/cell_rise"),
    ("cell_fall", "library/cell/pin/timing/cell_fall"),
    ("rise_transition", "library/cell/pin/timing/rise_transition"),
    ("fall_transition", "library/cell/pin/timing/fall_transition"),
    ("rise_constraint", "library/cell/pin/timing/rise_constraint"),
    ("fall_constraint", "library/cell/pin/timing/fall_constraint"),
];

/// The table groups of an internal_power group, with their construct paths.
const POWER_TABLES: [(&str, &str); 3] = [
    ("power", "library/cell/pin/internal_power/power"),
    ("rise_power", "library/cell/pin/internal_power/rise_power"),
    ("fall_power", "library/cell/pin/internal_power/fall_power"),
];

impl Builder<'_> {
    /// The number of simple attribute `name` of `g` at the construct path `path`, with a warning
    /// when it is not a finite number (LIBERTY-REQ-0058); the parsed text is returned even then.
    fn number(&mut self, g: &Group, path: &str, name: &str) -> Option<f64> {
        let (v, at) = simple_at(g, name)?;
        let x = parse_f64(v);
        if !x.is_some_and(f64::is_finite) {
            self.diags
                .warn(code::NOT_A_NUMBER, &format!("{path}/{name}"), at, || {
                    format!("'{v}' is not a finite number; the default applies")
                });
        }
        x
    }

    /// Percentage threshold in `(0, 100)` (LIBERTY-REQ-0045): reports `L0002` error and returns
    /// `None` when not a finite number strictly between 0 and 100.
    fn threshold_pct(&mut self, g: &Group, name: &str) -> Option<f64> {
        let (v, at) = simple_at(g, name)?;
        match parse_f64(v).filter(|x| x.is_finite() && *x > 0.0 && *x < 100.0) {
            Some(x) => Some(x),
            None => {
                self.diags.report(
                    Severity::Error,
                    code::BAD_THRESHOLD,
                    &format!("{LIB}/{name}"),
                    at,
                    || format!("'{v}' is not in (0, 100); the default applies"),
                );
                None
            }
        }
    }

    /// `slew_derate_from_library` in `(0, 1]` (LIBERTY-REQ-0045): reports `L0002` error and
    /// returns `None` when out of range.
    fn slew_derate(&mut self, g: &Group) -> Option<f64> {
        const NAME: &str = "slew_derate_from_library";
        let (v, at) = simple_at(g, NAME)?;
        match parse_f64(v).filter(|x| x.is_finite() && *x > 0.0 && *x <= 1.0) {
            Some(x) => Some(x),
            None => {
                self.diags.report(
                    Severity::Error,
                    code::BAD_THRESHOLD,
                    &format!("{LIB}/{NAME}"),
                    at,
                    || format!("'{v}' is not in (0, 1]; the default applies"),
                );
                None
            }
        }
    }

    /// Boolean attribute `name` of `g`: `true` or `1` (any letter case) is true; `false` or `0`
    /// is false, as is anything else, with a warning (LIBERTY-REQ-0058).
    fn flag(&mut self, g: &Group, path: &str, name: &str) -> bool {
        let Some((v, at)) = simple_at(g, name) else {
            return false;
        };
        match v.trim().trim_matches('"').to_ascii_lowercase().as_str() {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => {
                self.diags
                    .warn(code::UNKNOWN_VALUE, &format!("{path}/{name}"), at, || {
                        format!("'{v}' is not a Boolean value; false applies")
                    });
                false
            }
        }
    }

    /// Enumerated attribute `name` of `g`, kept as written with a warning when its value is not
    /// a known name (LIBERTY-REQ-0058).
    fn named<T: std::fmt::Display>(
        &mut self,
        g: &Group,
        path: &str,
        name: &str,
        parse: impl Fn(&str) -> T,
        known: impl Fn(&T) -> bool,
    ) -> Option<T> {
        let (v, at) = simple_at(g, name)?;
        let value = parse(v.trim());
        if !known(&value) {
            self.diags
                .warn(code::UNKNOWN_VALUE, &format!("{path}/{name}"), at, || {
                    format!("unknown {name} '{value}'")
                });
        }
        Some(value)
    }

    /// Records a problem of cell `cell` (LIBERTY-REQ-0057): its text in the cell's problems and
    /// one aggregated warning.
    fn problem(
        &mut self,
        problems: &mut Vec<String>,
        cell: &str,
        code: &'static str,
        path: &str,
        at: (u32, u32),
        text: String,
    ) {
        self.diags.warn(code, path, at, || {
            format!("cell {cell}: {text}; the cell is unusable")
        });
        problems.push(text);
    }

    fn library(&mut self, g: &Group) -> Library {
        self.classes.group(self.diags, Ctx::Library, g, LIB);
        for kind in tables::TEMPLATE_KINDS {
            let path = format!("{LIB}/{kind}");
            for t in g.children(kind) {
                self.classes.group(self.diags, Ctx::Template, t, &path);
            }
        }
        let lib_types = bus::bus_types(g, LIB, &mut self.classes, self.diags);
        let wire_loads = wire_load::wire_loads(g, &mut self.classes, self.diags);
        let wire_load_tables = wire_load::wire_load_tables(g, &mut self.classes, self.diags);
        let wire_load_selections =
            wire_load::wire_load_selections(g, &mut self.classes, self.diags);
        let operating_conditions =
            wire_load::operating_conditions(g, &mut self.classes, self.diags);
        let bus_naming_style = g.simple("bus_naming_style").unwrap_or("%s[%d]");
        let units = units::units(g, self.diags);
        let templates = tables::templates(g, self.diags);
        let default = |name: &str| g.simple(name).and_then(parse_f64);
        let mut lib = Library {
            name: g.args.first().cloned().unwrap_or_default(),
            time_unit_ns: units.time.scale,
            cap_unit_pf: units.capacitance.scale,
            default_max_capacitance: default("default_max_capacitance"),
            default_max_transition: default("default_max_transition"),
            default_max_fanout: default("default_max_fanout"),
            default_fanout_load: default("default_fanout_load").unwrap_or(1.0),
            templates,
            cells: Vec::new(),
            units,
            default_input_pin_cap: None,
            default_output_pin_cap: None,
            default_inout_pin_cap: None,
            input_threshold_pct_rise: self.threshold_pct(g, "input_threshold_pct_rise"),
            input_threshold_pct_fall: self.threshold_pct(g, "input_threshold_pct_fall"),
            output_threshold_pct_rise: self.threshold_pct(g, "output_threshold_pct_rise"),
            output_threshold_pct_fall: self.threshold_pct(g, "output_threshold_pct_fall"),
            slew_lower_threshold_pct_rise: self.threshold_pct(g, "slew_lower_threshold_pct_rise"),
            slew_upper_threshold_pct_rise: self.threshold_pct(g, "slew_upper_threshold_pct_rise"),
            slew_lower_threshold_pct_fall: self.threshold_pct(g, "slew_lower_threshold_pct_fall"),
            slew_upper_threshold_pct_fall: self.threshold_pct(g, "slew_upper_threshold_pct_fall"),
            slew_derate_from_library: self.slew_derate(g),
            wire_loads,
            wire_load_tables,
            wire_load_selections,
            default_wire_load: g.simple("default_wire_load").map(str::to_string),
            default_wire_load_mode: self.named(
                g,
                LIB,
                "default_wire_load_mode",
                WireLoadMode::from_name,
                WireLoadMode::is_known,
            ),
            default_wire_load_selection: g
                .simple("default_wire_load_selection")
                .map(str::to_string),
            default_wire_load_area: None,
            default_wire_load_capacitance: None,
            default_wire_load_resistance: None,
            operating_conditions,
            default_operating_conditions: g
                .simple("default_operating_conditions")
                .map(str::to_string),
            nom_process: None,
            nom_temperature: None,
            nom_voltage: None,
        };
        if (lib.slew_lower_threshold_pct_rise.is_some()
            || lib.slew_upper_threshold_pct_rise.is_some())
            && lib.slew_lower_threshold_pct_rise.unwrap_or(20.0)
                >= lib.slew_upper_threshold_pct_rise.unwrap_or(80.0)
        {
            let attr = if g.attr("slew_lower_threshold_pct_rise").is_some() {
                "slew_lower_threshold_pct_rise"
            } else {
                "slew_upper_threshold_pct_rise"
            };
            self.diags.report(
                Severity::Error,
                code::BAD_THRESHOLD,
                &format!("{LIB}/{attr}"),
                at(g, attr),
                || "slew lower threshold must be less than slew upper threshold (rise)".into(),
            );
            lib.slew_lower_threshold_pct_rise = None;
            lib.slew_upper_threshold_pct_rise = None;
        }
        if (lib.slew_lower_threshold_pct_fall.is_some()
            || lib.slew_upper_threshold_pct_fall.is_some())
            && lib.slew_lower_threshold_pct_fall.unwrap_or(20.0)
                >= lib.slew_upper_threshold_pct_fall.unwrap_or(80.0)
        {
            let attr = if g.attr("slew_lower_threshold_pct_fall").is_some() {
                "slew_lower_threshold_pct_fall"
            } else {
                "slew_upper_threshold_pct_fall"
            };
            self.diags.report(
                Severity::Error,
                code::BAD_THRESHOLD,
                &format!("{LIB}/{attr}"),
                at(g, attr),
                || "slew lower threshold must be less than slew upper threshold (fall)".into(),
            );
            lib.slew_lower_threshold_pct_fall = None;
            lib.slew_upper_threshold_pct_fall = None;
        }
        let finite = |x: Option<f64>| x.filter(|v| v.is_finite());
        lib.default_input_pin_cap = finite(self.number(g, LIB, "default_input_pin_cap"));
        lib.default_output_pin_cap = finite(self.number(g, LIB, "default_output_pin_cap"));
        lib.default_inout_pin_cap = finite(self.number(g, LIB, "default_inout_pin_cap"));
        lib.default_wire_load_area = finite(self.number(g, LIB, "default_wire_load_area"));
        lib.default_wire_load_capacitance =
            finite(self.number(g, LIB, "default_wire_load_capacitance"));
        lib.default_wire_load_resistance =
            finite(self.number(g, LIB, "default_wire_load_resistance"));
        lib.nom_process = finite(self.number(g, LIB, "nom_process"));
        lib.nom_temperature = finite(self.number(g, LIB, "nom_temperature"));
        lib.nom_voltage = finite(self.number(g, LIB, "nom_voltage"));
        let mut names = HashSet::new();
        for c in g.children("cell") {
            let cell = self.cell(&lib.templates, &lib_types, bus_naming_style, c);
            if !names.insert(cell.name.clone()) {
                self.diags.warn(code::DUPLICATE, CELL, (c.line, c.col), || {
                    format!(
                        "cell '{}' is defined more than once; both are kept and a lookup by \
                         name finds the first",
                        cell.name
                    )
                });
            }
            lib.cells.push(cell);
        }
        lib
    }

    fn pg_pin(&mut self, p: &Group) -> PgPin {
        const PG: &str = "library/cell/pg_pin";
        self.classes.group(self.diags, Ctx::PgPin, p, PG);
        let mut pg = PgPin::new(p.args.first().cloned().unwrap_or_default());
        pg.pg_type = self.named(p, PG, "pg_type", PgType::from_name, PgType::is_known);
        pg.voltage_name = p.simple("voltage_name").map(str::to_string);
        pg
    }

    fn cell(
        &mut self,
        templates: &BTreeMap<String, Template>,
        lib_types: &BTreeMap<String, Result<BusType, String>>,
        bus_naming_style: &str,
        c: &Group,
    ) -> Cell {
        self.classes.group(self.diags, Ctx::Cell, c, CELL);
        let cell_types = bus::bus_types(c, CELL, &mut self.classes, self.diags);
        let clock_gating_integrated_cell =
            c.simple("clock_gating_integrated_cell").map(str::to_string);
        let mut cell = Cell {
            name: c.args.first().cloned().unwrap_or_default(),
            area: self.number(c, CELL, "area").unwrap_or(0.0),
            dont_use: self.flag(c, CELL, "dont_use"),
            pins: Vec::new(),
            pg_pins: c
                .children("pg_pin")
                .filter_map(|p| p.args.first().cloned())
                .collect(),
            sequentials: Vec::new(),
            has_statetable: c.children("statetable").next().is_some(),
            has_test_cell: c.children("test_cell").next().is_some(),
            is_clock_gating: clock_gating_integrated_cell.is_some(),
            has_bus: c.children("bus").next().is_some() || c.children("bundle").next().is_some(),
            leakage_power: self.number(c, CELL, "cell_leakage_power"),
            problems: Vec::new(),
            pg_pin_groups: Vec::new(),
            is_isolation_cell: self.flag(c, CELL, "is_isolation_cell"),
            is_level_shifter: self.flag(c, CELL, "is_level_shifter"),
            level_shifter_type: self.named(
                c,
                CELL,
                "level_shifter_type",
                LevelShifterType::from_name,
                LevelShifterType::is_known,
            ),
            always_on: self.flag(c, CELL, "always_on"),
            cell_footprint: c.simple("cell_footprint").map(str::to_string),
            dont_touch: self.flag(c, CELL, "dont_touch"),
            is_macro_cell: self.flag(c, CELL, "is_macro_cell"),
            pad_cell: self.flag(c, CELL, "pad_cell"),
            switch_cell_type: self.named(
                c,
                CELL,
                "switch_cell_type",
                SwitchCellType::from_name,
                SwitchCellType::is_known,
            ),
            retention_cell: c.simple("retention_cell").map(str::to_string),
            antenna_diode_type: self.named(
                c,
                CELL,
                "antenna_diode_type",
                AntennaDiodeType::from_name,
                AntennaDiodeType::is_known,
            ),
            clock_gating_integrated_cell,
            statetable: None,
            line: c.line,
        };
        for p in c.children("pg_pin") {
            let pg = self.pg_pin(p);
            if cell.pg_pin_groups.iter().any(|q| q.name == pg.name) {
                self.diags.warn(
                    code::DUPLICATE,
                    "library/cell/pg_pin",
                    (p.line, p.col),
                    || format!("pg_pin '{}' is declared more than once", pg.name),
                );
            }
            cell.pg_pin_groups.push(pg);
        }
        // Locations of each pin's `function`, `three_state`, and `state_function`, for
        // LIBERTY-REQ-0021.
        let mut func_at: Vec<PinFuncLocs> = Vec::new();
        for g in &c.groups {
            match g.kind.as_str() {
                "pin" => {
                    if g.args.is_empty() {
                        self.diags.warn(code::UNKNOWN, PIN, (g.line, g.col), || {
                            "pin group without a name is ignored".to_string()
                        });
                    }
                    for (k, name) in g.args.iter().enumerate() {
                        if cell.pins.iter().any(|q| &q.name == name) {
                            self.diags.warn(code::DUPLICATE, PIN, (g.line, g.col), || {
                                format!(
                                    "cell {}: pin '{name}' is declared more than once; a lookup \
                                     by name finds the first",
                                    cell.name
                                )
                            });
                        }
                        // A group with several names is read once per name; it is reported once.
                        self.diags.muted = k > 0;
                        let pin = self.pin_group(
                            templates,
                            &cell.name,
                            g,
                            name,
                            &mut cell.problems,
                            true,
                        );
                        self.diags.muted = false;
                        cell.pins.push(pin);
                        func_at.push((
                            at(g, "function"),
                            at(g, "three_state"),
                            at(g, "state_function"),
                        ));
                    }
                }
                "bus" | "bundle" => {
                    let is_bus = g.kind == "bus";
                    let (ctx, path) = if is_bus {
                        (Ctx::Bus, "library/cell/bus")
                    } else {
                        (Ctx::Bundle, "library/cell/bundle")
                    };
                    self.classes.group(self.diags, ctx, g, path);
                    let child_pin_path = format!("{path}/pin");
                    for cp in g.children("pin") {
                        self.classes
                            .group(self.diags, Ctx::Pin, cp, &child_pin_path);
                    }
                    let expanded = if is_bus {
                        bus::expand_bus(g, lib_types, &cell_types, bus_naming_style)
                    } else {
                        bus::expand_bundle(g)
                    };
                    match expanded {
                        Ok(members) => {
                            for (k, mp) in members.iter().enumerate() {
                                let name = &mp.args[0];
                                if cell.pins.iter().any(|q| &q.name == name) {
                                    self.diags.warn(
                                        code::DUPLICATE,
                                        path,
                                        (mp.line, mp.col),
                                        || {
                                            format!(
                                                "cell {}: pin '{name}' is declared more than \
                                                 once; a lookup by name finds the first",
                                                cell.name
                                            )
                                        },
                                    );
                                }
                                self.diags.muted = k > 0;
                                let pin = self.pin_group(
                                    templates,
                                    &cell.name,
                                    mp,
                                    name,
                                    &mut cell.problems,
                                    false,
                                );
                                self.diags.muted = false;
                                cell.pins.push(pin);
                                func_at.push((
                                    at(mp, "function"),
                                    at(mp, "three_state"),
                                    at(mp, "state_function"),
                                ));
                            }
                        }
                        Err(e) => {
                            let name = cell.name.clone();
                            self.problem(
                                &mut cell.problems,
                                &name,
                                code::BAD_BUS,
                                path,
                                (g.line, g.col),
                                e,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        for g in &c.groups {
            match g.kind.as_str() {
                "ff" | "latch" => {
                    let (kind, ctx, path) = if g.kind == "ff" {
                        (SeqKind::FlipFlop, Ctx::Ff, "library/cell/ff")
                    } else {
                        (SeqKind::Latch, Ctx::Latch, "library/cell/latch")
                    };
                    self.classes.group(self.diags, ctx, g, path);
                    match seq_from_group(kind, g) {
                        Ok(s) => cell.sequentials.push(s),
                        Err(e) => {
                            let name = cell.name.clone();
                            self.problem(
                                &mut cell.problems,
                                &name,
                                code::BAD_SEQUENTIAL,
                                path,
                                (g.line, g.col),
                                e,
                            );
                        }
                    }
                }
                "statetable" => {
                    const ST_PATH: &str = "library/cell/statetable";
                    self.classes.group(self.diags, Ctx::Statetable, g, ST_PATH);
                    if cell.statetable.is_none() {
                        match bus::statetable(g) {
                            Ok(st) => cell.statetable = Some(st),
                            Err(e) => {
                                let name = cell.name.clone();
                                self.problem(
                                    &mut cell.problems,
                                    &name,
                                    code::BAD_SEQUENTIAL,
                                    ST_PATH,
                                    (g.line, g.col),
                                    e,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.check_names(&mut cell, c, &func_at);
        cell
    }

    /// LIBERTY-REQ-0021, LIBERTY-REQ-0026, LIBERTY-REQ-0044: functions, `state_function`, `when`,
    /// `ff`/`latch`, and `statetable` may only refer to pins, internal nodes, and state variables.
    fn check_names(&mut self, cell: &mut Cell, c: &Group, func_at: &[PinFuncLocs]) {
        let mut known: Vec<String> = cell.pins.iter().map(|p| p.name.clone()).collect();
        for p in &cell.pins {
            if let Some(n) = &p.internal_node {
                if !known.contains(n) {
                    known.push(n.clone());
                }
            }
        }
        for s in &cell.sequentials {
            known.push(s.state.clone());
            known.push(s.state_n.clone());
        }
        if let Some(st) = &cell.statetable {
            for n in &st.internal_nodes {
                if !known.contains(n) {
                    known.push(n.clone());
                }
            }
        }
        let mut found = Vec::new();
        for (p, &(f_at, t_at, s_at)) in cell.pins.iter().zip(func_at) {
            let funcs = [
                (&p.function, "function", f_at),
                (&p.three_state_function, "three_state", t_at),
                (&p.state_function, "state_function", s_at),
            ];
            for (f, attr, at) in funcs {
                for v in f.iter().flat_map(Func::vars) {
                    if !known.contains(&v) {
                        let text = format!("pin {} {attr} refers to unknown name '{v}'", p.name);
                        found.push((format!("{PIN}/{attr}"), at, text));
                    }
                }
            }
            let mut seen_when_lines = HashSet::new();
            for arc in &p.arcs {
                if !seen_when_lines.insert(arc.line) {
                    continue;
                }
                if let Some(w) = &arc.when {
                    for v in w.vars() {
                        if !known.contains(&v) {
                            let text =
                                format!("pin {} timing when refers to unknown name '{v}'", p.name);
                            found.push((format!("{TIMING}/when"), (arc.line, 1), text));
                        }
                    }
                }
            }
        }
        if let Some(st) = &cell.statetable {
            let st_at = c
                .children("statetable")
                .next()
                .map_or((c.line, c.col), |g| (g.line, g.col));
            for v in &st.input_nodes {
                if !known.contains(v) {
                    found.push((
                        format!("{CELL}/statetable"),
                        st_at,
                        format!("statetable refers to unknown input node '{v}'"),
                    ));
                }
            }
        }
        let seq_groups = c
            .groups
            .iter()
            .filter(|g| g.kind == "ff" || g.kind == "latch");
        // Sequential groups that failed to read are not in `sequentials`; match by state name.
        for s in &cell.sequentials {
            let g = seq_groups
                .clone()
                .find(|g| g.args.first() == Some(&s.state));
            let (path, at) = g.map_or((format!("{CELL}/ff"), (c.line, c.col)), |g| {
                (format!("{CELL}/{}", g.kind), (g.line, g.col))
            });
            let exprs = [
                Some(&s.data),
                Some(&s.clock),
                s.clear.as_ref(),
                s.preset.as_ref(),
            ];
            for f in exprs.into_iter().flatten() {
                for v in f.vars() {
                    if !known.contains(&v) {
                        found.push((
                            path.clone(),
                            at,
                            format!("sequential group refers to unknown name '{v}'"),
                        ));
                    }
                }
            }
        }
        let name = cell.name.clone();
        for (path, at, text) in found {
            self.problem(
                &mut cell.problems,
                &name,
                code::UNKNOWN_NAME,
                &path,
                at,
                text,
            );
        }
    }

    /// A function attribute: the text, and the parsed function when it parses; a text that does
    /// not parse is a cell problem.
    fn function(
        &mut self,
        p: &Group,
        attr: &str,
        cell: &str,
        name: &str,
        problems: &mut Vec<String>,
    ) -> (Option<String>, Option<Func>) {
        let Some((t, at)) = simple_at(p, attr) else {
            return (None, None);
        };
        match parse_function(t) {
            Ok(f) => (Some(t.to_string()), Some(f)),
            Err(e) => {
                let path = format!("{PIN}/{attr}");
                let text = format!("pin {name} {attr} '{t}': {e}");
                self.problem(problems, cell, code::BAD_FUNCTION, &path, at, text);
                (Some(t.to_string()), None)
            }
        }
    }

    /// The tables of `kinds` (kind and construct path) among the groups of `g`, a timing or
    /// internal_power group; a malformed table is a cell problem, described with `context`.
    fn tables<const N: usize>(
        &mut self,
        templates: &BTreeMap<String, Template>,
        g: &Group,
        kinds: [(&str, &str); N],
        context: impl Fn() -> String,
        cell: &str,
        problems: &mut Vec<String>,
    ) -> [Option<Table>; N] {
        kinds.map(|(kind, path)| {
            let t = g.children(kind).next()?;
            self.classes.group(self.diags, Ctx::Table, t, path);
            let (table, found) = tables::table(templates, t, path, self.diags);
            for (code, text) in found {
                let text = format!("{}: {text}", context());
                self.problem(problems, cell, code, path, (t.line, t.col), text);
            }
            Some(table)
        })
    }

    fn pin_group(
        &mut self,
        templates: &BTreeMap<String, Template>,
        cell: &str,
        p: &Group,
        name: &str,
        problems: &mut Vec<String>,
        classify_pin: bool,
    ) -> Pin {
        if classify_pin {
            self.classes.group(self.diags, Ctx::Pin, p, PIN);
        }
        let direction = match p.simple("direction").map(|d| d.trim().to_ascii_lowercase()) {
            Some(d) if d == "input" => Direction::Input,
            Some(d) if d == "output" => Direction::Output,
            Some(d) if d == "inout" => Direction::Inout,
            Some(d) if d == "internal" => Direction::Internal,
            other => {
                let text = format!("pin {name} has invalid direction {other:?}");
                let path = format!("{PIN}/direction");
                self.problem(
                    problems,
                    cell,
                    code::BAD_DIRECTION,
                    &path,
                    at(p, "direction"),
                    text,
                );
                Direction::Internal
            }
        };
        let (function_text, function) = self.function(p, "function", cell, name, problems);
        let (state_function_text, state_function) =
            self.function(p, "state_function", cell, name, problems);
        let mut arcs = Vec::new();
        for t in p.children("timing") {
            let related = t.simple("related_pin").unwrap_or("").to_string();
            if related.split_whitespace().next().is_none() {
                self.diags
                    .warn(code::NO_RELATED_PIN, TIMING, (t.line, t.col), || {
                        format!("cell {cell}: pin {name}: timing group without related_pin")
                    });
                continue;
            }
            self.classes.group(self.diags, Ctx::Timing, t, TIMING);
            let timing_type = t
                .simple("timing_type")
                .unwrap_or("combinational")
                .to_string();
            if t.simple("timing_type").is_some() {
                self.named(
                    t,
                    TIMING,
                    "timing_type",
                    TimingType::from_name,
                    TimingType::is_known,
                );
            }
            self.named(
                t,
                TIMING,
                "timing_sense",
                TimingSense::from_name,
                TimingSense::is_known,
            );
            let (when_text, when) = match simple_at(t, "when") {
                None => (None, None),
                Some((w, w_at)) => match parse_function(w) {
                    Ok(f) => (Some(w.to_string()), Some(f)),
                    Err(e) => {
                        let path = format!("{TIMING}/when");
                        let text = format!("pin {name} timing when '{w}': {e}");
                        self.problem(problems, cell, code::BAD_FUNCTION, &path, w_at, text);
                        (Some(w.to_string()), None)
                    }
                },
            };
            let sdf_cond = t.simple("sdf_cond").map(str::to_string);
            let context = || format!("pin {name} arc from {related}");
            let [
                cell_rise,
                cell_fall,
                rise_transition,
                fall_transition,
                rise_constraint,
                fall_constraint,
            ] = self.tables(templates, t, ARC_TABLES, context, cell, problems);
            for rp in related.split_whitespace() {
                arcs.push(TimingArc {
                    related_pin: rp.to_string(),
                    timing_sense: t.simple("timing_sense").map(str::to_string),
                    timing_type: timing_type.clone(),
                    cell_rise: cell_rise.clone(),
                    cell_fall: cell_fall.clone(),
                    rise_transition: rise_transition.clone(),
                    fall_transition: fall_transition.clone(),
                    rise_constraint: rise_constraint.clone(),
                    fall_constraint: fall_constraint.clone(),
                    when_text: when_text.clone(),
                    when: when.clone(),
                    sdf_cond: sdf_cond.clone(),
                    line: t.line,
                });
            }
        }
        let mut internal_power = Vec::new();
        for ip in p.children("internal_power") {
            self.classes
                .group(self.diags, Ctx::InternalPower, ip, POWER);
            let context = || format!("pin {name} internal_power");
            let [both, rise_power, fall_power] =
                self.tables(templates, ip, POWER_TABLES, context, cell, problems);
            let rise_power = rise_power.or_else(|| both.clone());
            let fall_power = fall_power.or(both);
            let related: Vec<Option<String>> = match ip.simple("related_pin") {
                Some(r) if !r.trim().is_empty() => {
                    r.split_whitespace().map(|x| Some(x.to_string())).collect()
                }
                _ => vec![None],
            };
            for related_pin in related {
                internal_power.push(InternalPower {
                    related_pin,
                    rise_power: rise_power.clone(),
                    fall_power: fall_power.clone(),
                });
            }
        }
        let mut limit = |b: &mut Self, attr: &str| {
            let (s, at) = simple_at(p, attr)?;
            let v = parse_f64(s);
            if v.is_some_and(|v| v.is_finite() && v >= 0.0) {
                v
            } else {
                let text = format!("pin {name} {attr} must be finite and nonnegative");
                let path = format!("{PIN}/{attr}");
                b.problem(problems, cell, code::BAD_LIMIT, &path, at, text);
                None
            }
        };
        let max_capacitance = limit(self, "max_capacitance");
        let max_transition = limit(self, "max_transition");
        let max_fanout = limit(self, "max_fanout");
        let fanout_load = limit(self, "fanout_load");
        let (three_state, three_state_function) =
            self.function(p, "three_state", cell, name, problems);
        let capacitance = self.number(p, PIN, "capacitance");
        Pin {
            name: name.to_string(),
            direction,
            function_text,
            function,
            three_state,
            three_state_function,
            capacitance: capacitance.unwrap_or(0.0),
            capacitance_given: capacitance.is_some_and(f64::is_finite),
            rise_capacitance: self.number(p, PIN, "rise_capacitance"),
            fall_capacitance: self.number(p, PIN, "fall_capacitance"),
            rise_capacitance_range: self.range(p, "rise_capacitance_range"),
            fall_capacitance_range: self.range(p, "fall_capacitance_range"),
            min_capacitance: self
                .number(p, PIN, "min_capacitance")
                .filter(|v| v.is_finite()),
            max_capacitance,
            max_transition,
            max_fanout,
            fanout_load,
            is_clock: self.flag(p, PIN, "clock"),
            arcs,
            internal_power,
            isolation_cell_enable_pin: self.flag(p, PIN, "isolation_cell_enable_pin"),
            isolation_cell_data_pin: self.flag(p, PIN, "isolation_cell_data_pin"),
            level_shifter_enable_pin: self.flag(p, PIN, "level_shifter_enable_pin"),
            level_shifter_data_pin: self.flag(p, PIN, "level_shifter_data_pin"),
            always_on: self.flag(p, PIN, "always_on"),
            related_power_pin: p.simple("related_power_pin").map(str::to_string),
            related_ground_pin: p.simple("related_ground_pin").map(str::to_string),
            power_down_function: p.simple("power_down_function").map(str::to_string),
            state_function_text,
            state_function,
            internal_node: p.simple("internal_node").map(str::to_string),
            clock_gate_clock_pin: self.flag(p, PIN, "clock_gate_clock_pin"),
            clock_gate_enable_pin: self.flag(p, PIN, "clock_gate_enable_pin"),
            clock_gate_out_pin: self.flag(p, PIN, "clock_gate_out_pin"),
            clock_gate_test_pin: self.flag(p, PIN, "clock_gate_test_pin"),
            clock_gate_obs_pin: self.flag(p, PIN, "clock_gate_obs_pin"),
            line: p.line,
        }
    }

    /// A complex attribute with two numbers, such as `rise_capacitance_range (0.1, 0.2)`; any
    /// other content is ignored with a warning.
    fn range(&mut self, g: &Group, name: &str) -> Option<(f64, f64)> {
        let v = tables::numbers(g, name, PIN, self.diags)?;
        match v.as_slice() {
            [lo, hi] if lo <= hi => Some((*lo, *hi)),
            _ => {
                self.diags.warn(
                    code::NOT_A_NUMBER,
                    &format!("{PIN}/{name}"),
                    at(g, name),
                    || format!("{name} needs two numbers, the minimum first; it is ignored"),
                );
                None
            }
        }
    }
}

fn seq_from_group(kind: SeqKind, g: &Group) -> Result<Sequential, String> {
    let state = g
        .args
        .first()
        .cloned()
        .ok_or("sequential group without state variable")?;
    let state_n = g.args.get(1).cloned().unwrap_or_default();
    let (data_name, clock_name) = match kind {
        SeqKind::FlipFlop => ("next_state", "clocked_on"),
        SeqKind::Latch => ("data_in", "enable"),
    };
    let parse = |attr: &str| -> Result<Option<(String, Func)>, String> {
        match g.simple(attr) {
            None => Ok(None),
            Some(t) => parse_function(t)
                .map(|f| Some((t.to_string(), f)))
                .map_err(|e| format!("{attr} '{t}': {e}")),
        }
    };
    let (data_text, data) = parse(data_name)?.ok_or_else(|| format!("missing {data_name}"))?;
    let (clock_text, clock) = parse(clock_name)?.ok_or_else(|| format!("missing {clock_name}"))?;
    let clear = parse("clear")?;
    let preset = parse("preset")?;
    Ok(Sequential {
        kind,
        state,
        state_n,
        data_text,
        data,
        clock_text,
        clock,
        clocked_on_also: g.simple("clocked_on_also").map(str::to_string),
        clear_text: clear.as_ref().map(|c| c.0.clone()),
        clear: clear.map(|c| c.1),
        preset_text: preset.as_ref().map(|c| c.0.clone()),
        preset: preset.map(|c| c.1),
        clear_preset_var1: g.simple("clear_preset_var1").map(str::to_string),
        clear_preset_var2: g.simple("clear_preset_var2").map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = r#"
library (t) {
  time_unit : "1ps";
  capacitive_load_unit (1, ff);
  lu_table_template (d2) { variable_1 : input_net_transition; variable_2 : total_output_net_capacitance;
    index_1 ("1, 2"); index_2 ("10, 20"); }
  cell (nd2) { area : 2;
    pin (A, B) { direction : input; capacitance : 1.5; }
    pin (Y) { direction : output; function : "(A B)'";
      timing () { related_pin : "A B"; cell_rise (d2) { values ("1, 2", "3, 4"); } } }
  }
  cell (dff) { area : 8; dont_use : true;
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; clear : "!RN"; }
    pin (D) { direction : input; }
    pin (CK) { direction : input; clock : true; }
    pin (RN) { direction : input; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (bad) { pin (Y) { direction : output; function : "Z"; } }
}
"#;

    // LIBERTY-REQ-0010 to LIBERTY-REQ-0014, LIBERTY-REQ-0021
    #[test]
    fn builds_model() {
        let lib = read_text(LIB, "t.lib").unwrap();
        assert!((lib.time_unit_ns - 1e-3).abs() < 1e-15);
        assert!((lib.cap_unit_pf - 1e-3).abs() < 1e-15);
        let nd2 = &lib.cells[0];
        assert_eq!(nd2.pins.len(), 3);
        assert_eq!(nd2.inputs().count(), 2);
        let y = nd2.pin("Y").unwrap();
        assert_eq!(
            y.function.as_ref().unwrap().truth_table(&["A", "B"]),
            Some(0b0111)
        );
        assert_eq!(y.arcs.len(), 2);
        let t = y.arcs[0].cell_rise.as_ref().unwrap();
        assert_eq!(t.index_2, vec![10.0, 20.0]);
        assert!((t.lookup_slew_load(1.5, 15.0) - 2.5).abs() < 1e-12);
        let dff = &lib.cells[1];
        assert!(dff.dont_use);
        assert!(dff.problems.is_empty(), "{:?}", dff.problems);
        let s = &dff.sequentials[0];
        assert_eq!(s.kind, SeqKind::FlipFlop);
        assert_eq!(s.clear_text.as_deref(), Some("!RN"));
        assert!(dff.pin("CK").unwrap().is_clock);
        assert!(!lib.cells[2].problems.is_empty());
    }
}
