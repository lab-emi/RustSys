// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Libraries, cells, pins, timing arcs, sequential groups, wire loads and operating conditions
//! (LIBERTY-REQ-0010 to LIBERTY-REQ-0019, LIBERTY-REQ-0021, LIBERTY-REQ-0023 to LIBERTY-REQ-0026,
//! LIBERTY-REQ-0031 and LIBERTY-REQ-0040 to LIBERTY-REQ-0048).

use std::collections::BTreeMap;

use super::func::Func;
use super::kinds::{
    AntennaDiodeType, LevelShifterType, PgType, SwitchCellType, TableVariable, TimingSense,
    TimingType, TreeType, WireLoadMode,
};
use super::table::Table;
use super::units::Units;

/// Pin direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Input,
    Output,
    Inout,
    Internal,
}

/// One timing arc from `related_pin` to the owning pin.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimingArc {
    pub related_pin: String,
    pub timing_sense: Option<String>,
    pub timing_type: String,
    pub cell_rise: Option<Table>,
    pub cell_fall: Option<Table>,
    pub rise_transition: Option<Table>,
    pub fall_transition: Option<Table>,
    /// Constraint tables of timing checks (setup, hold, recovery, removal).
    pub rise_constraint: Option<Table>,
    pub fall_constraint: Option<Table>,
    /// Raw `when` condition text, when the timing group has one (LIBERTY-REQ-0026).
    pub when_text: Option<String>,
    /// Parsed `when` condition, when `when_text` parses (LIBERTY-REQ-0026).
    pub when: Option<Func>,
    /// Raw `sdf_cond` text, when the timing group has one (LIBERTY-REQ-0026).
    pub sdf_cond: Option<String>,
    /// 1-based line number of the `timing` group in the `.lib` file, or 0 when synthetic
    /// (LIBERTY-REQ-0046).
    pub line: u32,
}

impl TimingArc {
    /// The timing type of `timing_type` (LIBERTY-REQ-0023).
    pub fn kind(&self) -> TimingType {
        TimingType::from_name(self.timing_type.trim())
    }

    /// The timing sense of `timing_sense`, when the arc has one (LIBERTY-REQ-0024).
    pub fn sense(&self) -> Option<TimingSense> {
        self.timing_sense
            .as_deref()
            .map(|s| TimingSense::from_name(s.trim()))
    }
}

/// One `internal_power` entry of a pin (LIBERTY-REQ-0016): energy per transition.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InternalPower {
    pub related_pin: Option<String>,
    pub rise_power: Option<Table>,
    pub fall_power: Option<Table>,
}

/// A cell pin.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pin {
    pub name: String,
    pub direction: Direction,
    /// Raw `function` text, unparsed.
    pub function_text: Option<String>,
    /// Parsed function, when the text parses.
    pub function: Option<Func>,
    /// Raw `three_state` text: the condition under which the pin is in high impedance.
    pub three_state: Option<String>,
    /// Parsed `three_state` function, when the text parses (LIBERTY-REQ-0025).
    pub three_state_function: Option<Func>,
    /// `capacitance`, in library units; 0 when the pin states none (see `capacitance_given`).
    pub capacitance: f64,
    /// True when the pin states a valid `capacitance` (LIBERTY-REQ-0018).
    pub capacitance_given: bool,
    pub rise_capacitance: Option<f64>,
    pub fall_capacitance: Option<f64>,
    /// `rise_capacitance_range` and `fall_capacitance_range`: (minimum, maximum).
    pub rise_capacitance_range: Option<(f64, f64)>,
    pub fall_capacitance_range: Option<(f64, f64)>,
    /// `min_capacitance`: the smallest load the pin may drive, in library units.
    pub min_capacitance: Option<f64>,
    /// Pin design-rule limits, in library units. Missing limits inherit library defaults.
    pub max_capacitance: Option<f64>,
    pub max_transition: Option<f64>,
    pub max_fanout: Option<f64>,
    /// Dimensionless input load for the fanout design rule.
    pub fanout_load: Option<f64>,
    /// `min_pulse_width_high` and `min_pulse_width_low`: the unconditional minimum widths of the
    /// high and the low pulse at the pin, in library units (LIBERTY-REQ-0049).
    pub min_pulse_width_high: Option<f64>,
    pub min_pulse_width_low: Option<f64>,
    /// `min_period`: the unconditional minimum period of the signal at the pin, in library units
    /// (LIBERTY-REQ-0049).
    pub min_period: Option<f64>,
    pub is_clock: bool,
    pub arcs: Vec<TimingArc>,
    /// `internal_power` entries (LIBERTY-REQ-0016).
    pub internal_power: Vec<InternalPower>,
    /// `isolation_cell_enable_pin` and `isolation_cell_data_pin` (LIBERTY-REQ-0041).
    pub isolation_cell_enable_pin: bool,
    pub isolation_cell_data_pin: bool,
    /// `level_shifter_enable_pin` and `level_shifter_data_pin` (LIBERTY-REQ-0041).
    pub level_shifter_enable_pin: bool,
    pub level_shifter_data_pin: bool,
    /// `always_on`: the pin stays powered when its cell's domain is off (LIBERTY-REQ-0041).
    pub always_on: bool,
    /// Names of the power and ground pins of the cell that the pin's signal refers to.
    pub related_power_pin: Option<String>,
    pub related_ground_pin: Option<String>,
    /// Raw `power_down_function` text: the condition, over power and ground pins, under which
    /// the pin is unpowered.
    pub power_down_function: Option<String>,
    /// Raw `state_function` text of a clock-gating or statetable output pin (LIBERTY-REQ-0044).
    pub state_function_text: Option<String>,
    /// Parsed `state_function`, when `state_function_text` parses (LIBERTY-REQ-0044).
    pub state_function: Option<Func>,
    /// `internal_node` name of an internal state pin (LIBERTY-REQ-0044).
    pub internal_node: Option<String>,
    /// Clock-gating pin roles (LIBERTY-REQ-0043).
    pub clock_gate_clock_pin: bool,
    pub clock_gate_enable_pin: bool,
    pub clock_gate_out_pin: bool,
    pub clock_gate_test_pin: bool,
    pub clock_gate_obs_pin: bool,
    /// 1-based line number of the `pin` group in the `.lib` file, or 0 when synthetic
    /// (LIBERTY-REQ-0046).
    pub line: u32,
}

/// Kind of a sequential group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqKind {
    FlipFlop,
    Latch,
}

/// A parsed `ff` or `latch` group. Expressions are kept both raw and parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Sequential {
    pub kind: SeqKind,
    pub state: String,
    pub state_n: String,
    /// `next_state` for flip-flops, `data_in` for latches.
    pub data_text: String,
    pub data: Func,
    /// `clocked_on` for flip-flops, `enable` for latches.
    pub clock_text: String,
    pub clock: Func,
    pub clocked_on_also: Option<String>,
    pub clear_text: Option<String>,
    pub clear: Option<Func>,
    pub preset_text: Option<String>,
    pub preset: Option<Func>,
    pub clear_preset_var1: Option<String>,
    pub clear_preset_var2: Option<String>,
}

/// A cell's `statetable` group (LIBERTY-REQ-0044): input node names, internal node names,
/// and the `table` text and rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Statetable {
    /// Input node names from the first group argument.
    pub input_nodes: Vec<String>,
    /// Internal node names from the second group argument.
    pub internal_nodes: Vec<String>,
    /// Raw `table` attribute text as written.
    pub table: String,
    /// Comma-separated rows of `table`, trimmed.
    pub rows: Vec<String>,
}

/// A power or ground pin of a cell: one `pg_pin` group (LIBERTY-REQ-0042).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct PgPin {
    pub name: String,
    /// `pg_type`, when stated.
    pub pg_type: Option<PgType>,
    /// `voltage_name`: the name of the supply voltage the pin connects to, when stated.
    pub voltage_name: Option<String>,
}

impl PgPin {
    /// A pin with only its name.
    pub fn new(name: impl Into<String>) -> PgPin {
        PgPin {
            name: name.into(),
            ..PgPin::default()
        }
    }
}

/// A library cell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cell {
    pub name: String,
    pub area: f64,
    pub dont_use: bool,
    pub pins: Vec<Pin>,
    /// Names of the `pg_pin` groups, in source order; `pg_pin_groups` has their attributes.
    pub pg_pins: Vec<String>,
    /// All `ff`/`latch` groups of the cell.
    pub sequentials: Vec<Sequential>,
    pub has_statetable: bool,
    pub has_test_cell: bool,
    pub is_clock_gating: bool,
    pub has_bus: bool,
    /// `cell_leakage_power` in library units (LIBERTY-REQ-0016).
    pub leakage_power: Option<f64>,
    /// Problems found while reading the cell (LIBERTY-REQ-0021); a non-empty list marks the cell
    /// unusable.
    pub problems: Vec<String>,
    /// The `pg_pin` groups with their types, in source order (LIBERTY-REQ-0042).
    pub pg_pin_groups: Vec<PgPin>,
    /// Classes of special cells (LIBERTY-REQ-0040).
    pub is_isolation_cell: bool,
    pub is_level_shifter: bool,
    pub level_shifter_type: Option<LevelShifterType>,
    /// `always_on`: the cell stays powered when its domain is off.
    pub always_on: bool,
    /// `cell_footprint`: the name shared by cells that may replace each other in place.
    pub cell_footprint: Option<String>,
    pub dont_touch: bool,
    pub is_macro_cell: bool,
    pub pad_cell: bool,
    pub switch_cell_type: Option<SwitchCellType>,
    /// `retention_cell`: the retention type of a state-retention cell.
    pub retention_cell: Option<String>,
    pub antenna_diode_type: Option<AntennaDiodeType>,
    /// `clock_gating_integrated_cell` attribute text, when present (LIBERTY-REQ-0043).
    pub clock_gating_integrated_cell: Option<String>,
    /// Parsed `statetable` group, when present (LIBERTY-REQ-0044).
    pub statetable: Option<Statetable>,
    /// 1-based line number of the `cell` group in the `.lib` file, or 0 when synthetic
    /// (LIBERTY-REQ-0046).
    pub line: u32,
}

impl Cell {
    pub fn pin(&self, name: &str) -> Option<&Pin> {
        self.pins.iter().find(|p| p.name == name)
    }

    pub fn inputs(&self) -> impl Iterator<Item = &Pin> {
        self.pins.iter().filter(|p| p.direction == Direction::Input)
    }

    pub fn outputs(&self) -> impl Iterator<Item = &Pin> {
        self.pins
            .iter()
            .filter(|p| p.direction == Direction::Output)
    }
}

/// Lookup-table template (LIBERTY-REQ-0031): the variables and default indices of up to three
/// axes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Template {
    pub variable_1: Option<String>,
    pub variable_2: Option<String>,
    pub index_1: Vec<f64>,
    pub index_2: Vec<f64>,
    pub variable_3: Option<String>,
    pub index_3: Vec<f64>,
}

impl Template {
    /// The variable of axis `axis` (1 to 3), when the template declares it.
    pub fn variable(&self, axis: usize) -> Option<TableVariable> {
        let v = match axis {
            1 => &self.variable_1,
            2 => &self.variable_2,
            3 => &self.variable_3,
            _ => return None,
        };
        v.as_deref().map(|s| TableVariable::from_name(s.trim()))
    }
}

/// One `wire_load` group of a library (LIBERTY-REQ-0047).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireLoad {
    pub name: String,
    /// Resistance per unit wire length, in library units (0 when omitted).
    pub resistance: f64,
    /// Capacitance per unit wire length, in library units (0 when omitted).
    pub capacitance: f64,
    /// Area per unit wire length, in library units (0 when omitted).
    pub area: f64,
    /// Extrapolation slope (wire length per fanout step outside `fanout_length`, 0 when omitted).
    pub slope: f64,
    /// `(fanout, length)` pairs from `fanout_length`, sorted by ascending fanout.
    pub fanout_length: Vec<(u32, f64)>,
}

impl WireLoad {
    /// Piecewise-linear wire length for `fanout` with linear `slope` extrapolation outside the
    /// table and clamping at `0.0` below the minimum entry (LIBERTY-REQ-0047). Returns `0.0` when
    /// `fanout_length` is empty.
    pub fn length_for_fanout(&self, fanout: u32) -> f64 {
        let Some(&(f_min, l_min)) = self.fanout_length.first() else {
            return 0.0;
        };
        if fanout <= f_min {
            if fanout == f_min {
                return l_min;
            }
            return (l_min - self.slope * f64::from(f_min - fanout)).max(0.0);
        }
        let &(f_max, l_max) = self.fanout_length.last().unwrap_or(&(f_min, l_min));
        if fanout >= f_max {
            if fanout == f_max {
                return l_max;
            }
            return l_max + self.slope * f64::from(fanout - f_max);
        }
        for pair in self.fanout_length.windows(2) {
            let (f0, l0) = pair[0];
            let (f1, l1) = pair[1];
            if fanout >= f0 && fanout <= f1 {
                if fanout == f0 || f0 == f1 {
                    return l0;
                }
                if fanout == f1 {
                    return l1;
                }
                return l0 + (l1 - l0) * f64::from(fanout - f0) / f64::from(f1 - f0);
            }
        }
        l_max
    }

    /// Alias for [`Self::length_for_fanout`] (LIBERTY-REQ-0047).
    pub fn length(&self, fanout: u32) -> f64 {
        self.length_for_fanout(fanout)
    }

    /// Lumped wire capacitance `capacitance * length_for_fanout(fanout)` (LIBERTY-REQ-0047).
    pub fn capacitance_for_fanout(&self, fanout: u32) -> f64 {
        self.capacitance * self.length_for_fanout(fanout)
    }

    /// Total wire resistance `resistance * length_for_fanout(fanout)` (LIBERTY-REQ-0047).
    pub fn resistance_for_fanout(&self, fanout: u32) -> f64 {
        self.resistance * self.length_for_fanout(fanout)
    }
}

/// One `wire_load_table` group of a library (LIBERTY-REQ-0047).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireLoadTable {
    pub name: String,
    pub fanout_length: Vec<(u32, f64)>,
    pub fanout_capacitance: Vec<(u32, f64)>,
    pub fanout_resistance: Vec<(u32, f64)>,
    pub fanout_area: Vec<(u32, f64)>,
}

/// One `wire_load_from_area(min_area, max_area, wire_load)` entry of a [`WireLoadSelection`]
/// group (LIBERTY-REQ-0047).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireLoadAreaRule {
    pub min_area: f64,
    pub max_area: f64,
    pub wire_load: String,
}

/// One `wire_load_selection` group of a library (LIBERTY-REQ-0047).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireLoadSelection {
    pub name: String,
    pub rules: Vec<WireLoadAreaRule>,
}

impl WireLoadSelection {
    /// Returns the `wire_load` name of the first `wire_load_from_area` rule whose
    /// `[min_area, max_area]` interval contains `area` (LIBERTY-REQ-0047).
    pub fn select(&self, area: f64) -> Option<&str> {
        self.rules
            .iter()
            .find(|r| area >= r.min_area && area <= r.max_area)
            .map(|r| r.wire_load.as_str())
    }
}

/// One `operating_conditions` group of a library (LIBERTY-REQ-0048).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OperatingConditions {
    pub name: String,
    pub process: Option<f64>,
    pub temperature: Option<f64>,
    pub voltage: Option<f64>,
    pub tree_type: Option<TreeType>,
}

/// A Liberty library.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Library {
    pub name: String,
    /// Time unit expressed in nanoseconds; equal to `units.time.scale`.
    pub time_unit_ns: f64,
    /// Capacitive load unit expressed in picofarads; equal to `units.capacitance.scale`.
    pub cap_unit_pf: f64,
    pub default_max_capacitance: Option<f64>,
    pub default_max_transition: Option<f64>,
    pub default_max_fanout: Option<f64>,
    pub default_fanout_load: f64,
    /// Lookup-table templates by name.
    pub templates: BTreeMap<String, Template>,
    /// Cells in source order (LIBERTY-REQ-0019).
    pub cells: Vec<Cell>,
    /// All units, each marked given or defaulted (LIBERTY-REQ-0017).
    pub units: Units,
    /// Capacitances of pins that state none, by direction (LIBERTY-REQ-0018).
    pub default_input_pin_cap: Option<f64>,
    pub default_output_pin_cap: Option<f64>,
    pub default_inout_pin_cap: Option<f64>,
    /// Delay and slew threshold percentages and slew derate; `None` when defaulted
    /// (LIBERTY-REQ-0045).
    pub input_threshold_pct_rise: Option<f64>,
    pub input_threshold_pct_fall: Option<f64>,
    pub output_threshold_pct_rise: Option<f64>,
    pub output_threshold_pct_fall: Option<f64>,
    pub slew_lower_threshold_pct_rise: Option<f64>,
    pub slew_upper_threshold_pct_rise: Option<f64>,
    pub slew_lower_threshold_pct_fall: Option<f64>,
    pub slew_upper_threshold_pct_fall: Option<f64>,
    pub slew_derate_from_library: Option<f64>,
    /// `wire_load` groups by name (LIBERTY-REQ-0047).
    pub wire_loads: BTreeMap<String, WireLoad>,
    /// `wire_load_table` groups by name (LIBERTY-REQ-0047).
    pub wire_load_tables: BTreeMap<String, WireLoadTable>,
    /// `wire_load_selection` groups by name (LIBERTY-REQ-0047).
    pub wire_load_selections: BTreeMap<String, WireLoadSelection>,
    /// Library-level wire-load defaults (LIBERTY-REQ-0047).
    pub default_wire_load: Option<String>,
    pub default_wire_load_mode: Option<WireLoadMode>,
    pub default_wire_load_selection: Option<String>,
    pub default_wire_load_area: Option<f64>,
    pub default_wire_load_capacitance: Option<f64>,
    pub default_wire_load_resistance: Option<f64>,
    /// `operating_conditions` groups by name and `default_operating_conditions`
    /// (LIBERTY-REQ-0048).
    pub operating_conditions: BTreeMap<String, OperatingConditions>,
    pub default_operating_conditions: Option<String>,
    /// Nominal process, temperature and voltage (LIBERTY-REQ-0048).
    pub nom_process: Option<f64>,
    pub nom_temperature: Option<f64>,
    pub nom_voltage: Option<f64>,
}

impl Library {
    /// The first cell with the given name (a linear search).
    pub fn cell(&self, name: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.name == name)
    }

    /// The `wire_load` group with the given name (LIBERTY-REQ-0047).
    pub fn wire_load(&self, name: &str) -> Option<&WireLoad> {
        self.wire_loads.get(name)
    }

    /// The `wire_load_selection` group with the given name (LIBERTY-REQ-0047).
    pub fn wire_load_selection(&self, name: &str) -> Option<&WireLoadSelection> {
        self.wire_load_selections.get(name)
    }

    /// The `operating_conditions` group with the given name (LIBERTY-REQ-0048).
    pub fn operating_condition(&self, name: &str) -> Option<&OperatingConditions> {
        self.operating_conditions.get(name)
    }

    /// The capacitance of `pin` with the library default applied (LIBERTY-REQ-0018): its own
    /// `capacitance` when it states one, else the default of its direction, else 0.
    pub fn pin_capacitance(&self, pin: &Pin) -> f64 {
        if pin.capacitance_given {
            return pin.capacitance;
        }
        let default = match pin.direction {
            Direction::Input => self.default_input_pin_cap,
            Direction::Output => self.default_output_pin_cap,
            Direction::Inout => self.default_inout_pin_cap,
            Direction::Internal => None,
        };
        default.unwrap_or(0.0)
    }

    /// `max_transition` of `pin`, else the library default for every pin but internal ones
    /// (LIBERTY-REQ-0018).
    pub fn pin_max_transition(&self, pin: &Pin) -> Option<f64> {
        pin.max_transition.or(match pin.direction {
            Direction::Internal => None,
            _ => self.default_max_transition,
        })
    }

    /// `max_capacitance` of `pin`, else the library default for output and inout pins
    /// (LIBERTY-REQ-0018).
    pub fn pin_max_capacitance(&self, pin: &Pin) -> Option<f64> {
        pin.max_capacitance.or(if drives(pin) {
            self.default_max_capacitance
        } else {
            None
        })
    }

    /// `max_fanout` of `pin`, else the library default for output and inout pins
    /// (LIBERTY-REQ-0018).
    pub fn pin_max_fanout(&self, pin: &Pin) -> Option<f64> {
        pin.max_fanout.or(if drives(pin) {
            self.default_max_fanout
        } else {
            None
        })
    }

    /// `fanout_load` of `pin`, else the library default for input and inout pins, else 0 for
    /// pins that load no net (LIBERTY-REQ-0018).
    pub fn pin_fanout_load(&self, pin: &Pin) -> f64 {
        pin.fanout_load.unwrap_or(match pin.direction {
            Direction::Input | Direction::Inout => self.default_fanout_load,
            Direction::Output | Direction::Internal => 0.0,
        })
    }
}

/// True for the directions that drive a net.
fn drives(pin: &Pin) -> bool {
    matches!(pin.direction, Direction::Output | Direction::Inout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(direction: Direction) -> Pin {
        Pin {
            name: "P".into(),
            direction,
            ..Pin::default()
        }
    }

    fn library() -> Library {
        Library {
            name: "l".into(),
            time_unit_ns: 1.0,
            cap_unit_pf: 1.0,
            default_max_capacitance: Some(0.5),
            default_max_transition: Some(1.5),
            default_max_fanout: Some(8.0),
            default_fanout_load: 2.0,
            default_input_pin_cap: Some(0.01),
            default_output_pin_cap: Some(0.02),
            ..Library::default()
        }
    }

    // LIBERTY-REQ-0018: pin values win; library defaults apply by direction.
    #[test]
    fn library_defaults_by_direction() {
        let lib = library();
        let input = pin(Direction::Input);
        let output = pin(Direction::Output);
        let inout = pin(Direction::Inout);
        let internal = pin(Direction::Internal);
        assert_eq!(lib.pin_capacitance(&input), 0.01);
        assert_eq!(lib.pin_capacitance(&output), 0.02);
        assert_eq!(lib.pin_capacitance(&inout), 0.0);
        assert_eq!(lib.pin_capacitance(&internal), 0.0);
        let stated = Pin {
            capacitance: 0.003,
            capacitance_given: true,
            ..pin(Direction::Output)
        };
        assert_eq!(lib.pin_capacitance(&stated), 0.003);
        assert_eq!(lib.pin_max_transition(&input), Some(1.5));
        assert_eq!(lib.pin_max_transition(&output), Some(1.5));
        assert_eq!(lib.pin_max_transition(&internal), None);
        assert_eq!(lib.pin_max_capacitance(&input), None);
        assert_eq!(lib.pin_max_capacitance(&output), Some(0.5));
        assert_eq!(lib.pin_max_capacitance(&inout), Some(0.5));
        assert_eq!(lib.pin_max_fanout(&input), None);
        assert_eq!(lib.pin_max_fanout(&output), Some(8.0));
        assert_eq!(lib.pin_fanout_load(&input), 2.0);
        assert_eq!(lib.pin_fanout_load(&inout), 2.0);
        assert_eq!(lib.pin_fanout_load(&output), 0.0);
        let own = Pin {
            max_transition: Some(0.7),
            max_capacitance: Some(0.1),
            max_fanout: Some(3.0),
            fanout_load: Some(4.0),
            ..pin(Direction::Inout)
        };
        assert_eq!(lib.pin_max_transition(&own), Some(0.7));
        assert_eq!(lib.pin_max_capacitance(&own), Some(0.1));
        assert_eq!(lib.pin_max_fanout(&own), Some(3.0));
        assert_eq!(lib.pin_fanout_load(&own), 4.0);
    }

    // LIBERTY-REQ-0023, LIBERTY-REQ-0024, LIBERTY-REQ-0026: typed views of the kept texts and
    // conditional arc defaults.
    #[test]
    fn arc_kind_and_sense() {
        let arc = TimingArc {
            timing_type: "setup_rising".into(),
            timing_sense: Some("non_unate".into()),
            when_text: Some("EN".into()),
            when: Some(Func::Var("EN".into())),
            sdf_cond: Some("EN === 1'b1".into()),
            line: 42,
            ..TimingArc::default()
        };
        assert_eq!(arc.kind(), TimingType::SetupRising);
        assert_eq!(arc.sense(), Some(TimingSense::NonUnate));
        assert_eq!(arc.when_text.as_deref(), Some("EN"));
        assert_eq!(arc.sdf_cond.as_deref(), Some("EN === 1'b1"));
        assert_eq!(arc.line, 42);
        let plain = TimingArc {
            timing_type: "combinational".into(),
            ..TimingArc::default()
        };
        assert_eq!(plain.kind(), TimingType::Combinational);
        assert_eq!(plain.sense(), None);
        assert_eq!(plain.when, None);
        assert_eq!(plain.line, 0);
    }

    // LIBERTY-REQ-0031: typed template variables per axis.
    #[test]
    fn template_variables() {
        let t = Template {
            variable_1: Some("related_pin_transition".into()),
            variable_2: Some("constrained_pin_transition".into()),
            variable_3: Some("related_out_total_output_net_capacitance".into()),
            ..Template::default()
        };
        assert_eq!(t.variable(1), Some(TableVariable::RelatedPinTransition));
        assert_eq!(t.variable(2), Some(TableVariable::ConstrainedPinTransition));
        assert_eq!(
            t.variable(3),
            Some(TableVariable::RelatedOutTotalOutputNetCapacitance)
        );
        assert_eq!(t.variable(0), None);
        assert_eq!(t.variable(4), None);
        assert_eq!(Template::default().variable(1), None);
    }

    // LIBERTY-REQ-0042, LIBERTY-REQ-0043, LIBERTY-REQ-0044, LIBERTY-REQ-0045, LIBERTY-REQ-0046,
    // LIBERTY-REQ-0047, LIBERTY-REQ-0048
    #[test]
    fn pg_pin_and_extension_b_c_defaults() {
        let p = PgPin::new("VPWR");
        assert_eq!(p.name, "VPWR");
        assert_eq!(p.pg_type, None);
        assert_eq!(p.voltage_name, None);
        let st = Statetable::default();
        assert!(st.input_nodes.is_empty() && st.internal_nodes.is_empty() && st.rows.is_empty());
        let cell = Cell::default();
        assert_eq!(cell.clock_gating_integrated_cell, None);
        assert_eq!(cell.statetable, None);
        assert_eq!(cell.line, 0);
        let lib = Library::default();
        assert_eq!(lib.slew_derate_from_library, None);
        assert_eq!(lib.slew_lower_threshold_pct_rise, None);
        assert_eq!(lib.slew_upper_threshold_pct_fall, None);
        assert!(lib.wire_loads.is_empty());
        assert!(lib.operating_conditions.is_empty());

        let wl = WireLoad {
            name: "wl_lin".into(),
            resistance: 0.005,
            capacitance: 0.001,
            area: 0.1,
            slope: 2.0,
            fanout_length: vec![(1, 2.0), (3, 5.0)],
        };
        assert_eq!(wl.length_for_fanout(0), 0.0);
        assert_eq!(wl.length_for_fanout(1), 2.0);
        assert_eq!(wl.length_for_fanout(2), 3.5);
        assert_eq!(wl.length_for_fanout(3), 5.0);
        assert_eq!(wl.length_for_fanout(5), 9.0);
        assert_eq!(wl.length(5), 9.0);
        assert!((wl.capacitance_for_fanout(2) - 0.0035).abs() < 1e-12);
        assert!((wl.resistance_for_fanout(2) - 0.0175).abs() < 1e-12);
        assert_eq!(WireLoad::default().length_for_fanout(3), 0.0);

        let sel = WireLoadSelection {
            name: "sel".into(),
            rules: vec![
                WireLoadAreaRule {
                    min_area: 0.0,
                    max_area: 500.0,
                    wire_load: "wl_small".into(),
                },
                WireLoadAreaRule {
                    min_area: 500.0,
                    max_area: 2000.0,
                    wire_load: "wl_medium".into(),
                },
            ],
        };
        assert_eq!(sel.select(100.0), Some("wl_small"));
        assert_eq!(sel.select(1200.0), Some("wl_medium"));
        assert_eq!(sel.select(5000.0), None);
    }
}
