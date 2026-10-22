// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Typed canonical SDC command AST (`Cmd`, `CmdKind`, and supporting option enums; SDC-REQ-0002,
//! SDC-REQ-0020 to SDC-REQ-0039).

use super::obj::Obj;

/// Edge transition qualifier (`-rise`, `-fall`, or both; SDC-REQ-0021).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EdgeScope {
    #[default]
    Both,
    Rise,
    Fall,
}

/// Min/max analysis qualifier (`-min`, `-max`, or both; SDC-REQ-0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MinMaxScope {
    #[default]
    Both,
    Min,
    Max,
}

/// Setup/hold check qualifier (`-setup`, `-hold`, or both; SDC-REQ-0021).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SetupHoldScope {
    #[default]
    Both,
    Setup,
    Hold,
}

/// Early/late path qualifier (`-early`, `-late`, or both; SDC-REQ-0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EarlyLateScope {
    #[default]
    Both,
    Early,
    Late,
}

/// Multicycle reference clock qualifier (`-start`, `-end`, or default; SDC-REQ-0029).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StartEndScope {
    #[default]
    Default,
    Start,
    End,
}

/// Clock-group relationship kind (SDC-REQ-0031).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClockGroupsKind {
    Asynchronous,
    LogicallyExclusive,
    PhysicallyExclusive,
}

/// Case-analysis constant or transition value (SDC-REQ-0032).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaseValue {
    Zero,
    One,
    Rise,
    Fall,
}

impl CaseValue {
    /// Canonical token in SDC syntax.
    pub fn as_str(self) -> &'static str {
        match self {
            CaseValue::Zero => "0",
            CaseValue::One => "1",
            CaseValue::Rise => "rise",
            CaseValue::Fall => "fall",
        }
    }

    /// Parses a `set_case_analysis` value argument.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "0" | "zero" => Some(CaseValue::Zero),
            "1" | "one" => Some(CaseValue::One),
            "rise" | "rising" => Some(CaseValue::Rise),
            "fall" | "falling" => Some(CaseValue::Fall),
            _ => None,
        }
    }
}

/// Wire-load mode (`set_wire_load_mode`; SDC-REQ-0036).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WireLoadMode {
    Top,
    Enclosed,
    Segmented,
}

impl WireLoadMode {
    /// Canonical token in SDC syntax.
    pub fn as_str(self) -> &'static str {
        match self {
            WireLoadMode::Top => "top",
            WireLoadMode::Enclosed => "enclosed",
            WireLoadMode::Segmented => "segmented",
        }
    }

    /// Parses a `set_wire_load_mode` argument.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "top" => Some(WireLoadMode::Top),
            "enclosed" => Some(WireLoadMode::Enclosed),
            "segmented" => Some(WireLoadMode::Segmented),
            _ => None,
        }
    }
}

/// Operating-condition analysis type (`set_operating_conditions -analysis_type`; SDC-REQ-0035).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnalysisType {
    Single,
    BcWc,
    OnChipVariation,
}

impl AnalysisType {
    /// Canonical token in SDC syntax.
    pub fn as_str(self) -> &'static str {
        match self {
            AnalysisType::Single => "single",
            AnalysisType::BcWc => "bc_wc",
            AnalysisType::OnChipVariation => "on_chip_variation",
        }
    }

    /// Parses an `-analysis_type` value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "single" => Some(AnalysisType::Single),
            "bc_wc" => Some(AnalysisType::BcWc),
            "on_chip_variation" => Some(AnalysisType::OnChipVariation),
            _ => None,
        }
    }
}

/// Endpoint or through-point specifier with optional edge transition (`-from`, `-rise_from`,
/// `-fall_from`, `-to`, `-rise_to`, `-fall_to`, `-through`, `-rise_through`, `-fall_through`;
/// SDC-REQ-0028).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExceptionEnd {
    pub obj: Obj,
    pub edge: EdgeScope,
}

impl ExceptionEnd {
    /// Creates an unqualified endpoint (`EdgeScope::Both`).
    pub fn both(obj: Obj) -> Self {
        Self {
            obj,
            edge: EdgeScope::Both,
        }
    }

    /// Creates a rising-edge endpoint (`EdgeScope::Rise`).
    pub fn rise(obj: Obj) -> Self {
        Self {
            obj,
            edge: EdgeScope::Rise,
        }
    }

    /// Creates a falling-edge endpoint (`EdgeScope::Fall`).
    pub fn fall(obj: Obj) -> Self {
        Self {
            obj,
            edge: EdgeScope::Fall,
        }
    }
}

/// One canonical SDC command in evaluation order, with its origin in the source table
/// (SDC-REQ-0002).
///
/// Semantic equality (`PartialEq`) compares [`Cmd::kind`], so canonical round-trip
/// (`read(write(s)) == s`) and metamorphic equivalence checks compare command semantics independent
/// of source line and column numbers. Use [`Cmd::exact_eq`] to compare source provenance as well.
#[derive(Debug, Clone)]
pub struct Cmd {
    pub kind: CmdKind,
    /// Index into [`super::Sdc::sources`].
    pub source: u16,
    /// 1-based line number in `sources[source]`.
    pub line: u32,
    /// 1-based column number in `sources[source]`.
    pub col: u32,
}

impl PartialEq for Cmd {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Cmd {
    /// Constructs a `Cmd` at the given source index, 1-based line, and 1-based column.
    pub fn new(kind: CmdKind, source: u16, line: u32, col: u32) -> Self {
        Self {
            kind,
            source,
            line,
            col,
        }
    }

    /// Compares both command semantics and source coordinates `(source, line, col)`.
    pub fn exact_eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.source == other.source
            && self.line == other.line
            && self.col == other.col
    }

    /// Canonical SDC command name.
    pub fn name(&self) -> &'static str {
        self.kind.name()
    }
}

/// Typed payload of a canonical SDC command (SDC-REQ-0020 to SDC-REQ-0039).
#[derive(Debug, Clone, PartialEq)]
pub enum CmdKind {
    /// `set_units` (canonicalized to `ns`, `pF`, `kOhm` on import; SDC-REQ-0016, SDC-REQ-0034).
    SetUnits {
        time: Option<String>,
        capacitance: Option<String>,
        resistance: Option<String>,
        voltage: Option<String>,
        current: Option<String>,
        power: Option<String>,
    },
    /// `current_design <name>` (SDC-REQ-0035).
    CurrentDesign { name: String },
    /// `set_operating_conditions` (SDC-REQ-0035).
    SetOperatingConditions {
        condition: Option<String>,
        min: Option<String>,
        max: Option<String>,
        library: Option<String>,
        min_library: Option<String>,
        max_library: Option<String>,
        analysis_type: Option<AnalysisType>,
        objects: Option<Obj>,
    },
    /// `set_wire_load_model` (SDC-REQ-0036).
    SetWireLoadModel {
        name: String,
        library: Option<String>,
        min: bool,
        max: bool,
        objects: Option<Obj>,
    },
    /// `set_wire_load_mode` (SDC-REQ-0036).
    SetWireLoadMode { mode: WireLoadMode },
    /// `set_wire_load_selection_group` (SDC-REQ-0036).
    SetWireLoadSelectionGroup {
        group_name: String,
        library: Option<String>,
        min: bool,
        max: bool,
        objects: Option<Obj>,
    },
    /// `create_clock` (SDC-REQ-0020).
    CreateClock {
        name: Option<String>,
        period: f64,
        waveform: Option<[f64; 2]>,
        add: bool,
        comment: Option<String>,
        sources: Option<Obj>,
    },
    /// `create_generated_clock` (Phase 2b; `D0010` in Phase 2a).
    CreateGeneratedClock {
        name: Option<String>,
        source: Obj,
        master_clock: Option<Obj>,
        divide_by: Option<u32>,
        multiply_by: Option<u32>,
        duty_cycle: Option<f64>,
        edges: Option<[u32; 3]>,
        edge_shift: Option<[f64; 3]>,
        invert: bool,
        combinational: bool,
        add: bool,
        comment: Option<String>,
        objects: Obj,
    },
    /// `set_propagated_clock` (Phase 2b; `D0010` in Phase 2a).
    SetPropagatedClock { objects: Obj },
    /// `unset_propagated_clock` (Phase 2b; `D0010` in Phase 2a).
    UnsetPropagatedClock { objects: Obj },
    /// `set_clock_groups` (SDC-REQ-0031).
    SetClockGroups {
        kind: ClockGroupsKind,
        name: Option<String>,
        allow_paths: bool,
        comment: Option<String>,
        groups: Vec<Obj>,
    },
    /// `set_clock_uncertainty` (SDC-REQ-0021).
    SetClockUncertainty {
        uncertainty: f64,
        setup_hold: SetupHoldScope,
        from: Option<ExceptionEnd>,
        to: Option<ExceptionEnd>,
        objects: Option<Obj>,
    },
    /// `set_clock_latency` (SDC-REQ-0022).
    SetClockLatency {
        delay: f64,
        source: bool,
        edge: EdgeScope,
        min_max: MinMaxScope,
        early_late: EarlyLateScope,
        clock: Option<Obj>,
        objects: Obj,
    },
    /// `set_clock_transition` (SDC-REQ-0023).
    SetClockTransition {
        transition: f64,
        edge: EdgeScope,
        min_max: MinMaxScope,
        clocks: Obj,
    },
    /// `set_input_delay` (SDC-REQ-0024).
    SetInputDelay {
        delay: f64,
        clock: Option<Obj>,
        clock_fall: bool,
        level_sensitive: bool,
        edge: EdgeScope,
        min_max: MinMaxScope,
        add_delay: bool,
        network_latency_included: bool,
        source_latency_included: bool,
        reference_pin: Option<Obj>,
        ports_or_pins: Obj,
    },
    /// `set_output_delay` (SDC-REQ-0024).
    SetOutputDelay {
        delay: f64,
        clock: Option<Obj>,
        clock_fall: bool,
        level_sensitive: bool,
        edge: EdgeScope,
        min_max: MinMaxScope,
        add_delay: bool,
        network_latency_included: bool,
        source_latency_included: bool,
        reference_pin: Option<Obj>,
        ports_or_pins: Obj,
    },
    /// `set_input_transition` (SDC-REQ-0025).
    SetInputTransition {
        transition: f64,
        edge: EdgeScope,
        min_max: MinMaxScope,
        clock: Option<Obj>,
        clock_fall: bool,
        ports: Obj,
    },
    /// `set_driving_cell` (SDC-REQ-0026).
    SetDrivingCell {
        lib_cell: String,
        library: Option<String>,
        pin: Option<String>,
        from_pin: Option<String>,
        edge: EdgeScope,
        min_max: MinMaxScope,
        input_transition_rise: Option<f64>,
        input_transition_fall: Option<f64>,
        multiply_by: Option<f64>,
        dont_scale: bool,
        no_design_rule: bool,
        clock: Option<Obj>,
        clock_fall: bool,
        ports: Obj,
    },
    /// `set_load` (SDC-REQ-0027).
    SetLoad {
        value: f64,
        pin_load: bool,
        wire_load: bool,
        subtract_pin_load: bool,
        min_max: MinMaxScope,
        objects: Obj,
    },
    /// `set_false_path` (SDC-REQ-0028).
    SetFalsePath {
        setup_hold: SetupHoldScope,
        edge: EdgeScope,
        from: Option<ExceptionEnd>,
        through: Vec<ExceptionEnd>,
        to: Option<ExceptionEnd>,
        comment: Option<String>,
    },
    /// `set_multicycle_path` (SDC-REQ-0029).
    SetMulticyclePath {
        multiplier: u32,
        setup_hold: SetupHoldScope,
        start_end: StartEndScope,
        edge: EdgeScope,
        from: Option<ExceptionEnd>,
        through: Vec<ExceptionEnd>,
        to: Option<ExceptionEnd>,
        comment: Option<String>,
    },
    /// `set_max_delay` (SDC-REQ-0030).
    SetMaxDelay {
        delay: f64,
        edge: EdgeScope,
        from: Option<ExceptionEnd>,
        through: Vec<ExceptionEnd>,
        to: Option<ExceptionEnd>,
        ignore_clock_latency: bool,
        comment: Option<String>,
    },
    /// `set_min_delay` (SDC-REQ-0030).
    SetMinDelay {
        delay: f64,
        edge: EdgeScope,
        from: Option<ExceptionEnd>,
        through: Vec<ExceptionEnd>,
        to: Option<ExceptionEnd>,
        ignore_clock_latency: bool,
        comment: Option<String>,
    },
    /// `set_case_analysis` (SDC-REQ-0032).
    SetCaseAnalysis {
        value: CaseValue,
        ports_or_pins: Obj,
    },
    /// `set_logic_zero` (SDC-REQ-0032).
    SetLogicZero { ports: Obj },
    /// `set_logic_one` (SDC-REQ-0032).
    SetLogicOne { ports: Obj },
    /// `set_disable_timing` (SDC-REQ-0033).
    SetDisableTiming {
        from: Option<String>,
        to: Option<String>,
        objects: Obj,
    },
    /// `set_max_time_borrow` (SDC-REQ-0033).
    SetMaxTimeBorrow { value: f64, objects: Obj },
    /// `set_timing_derate` (Phase 2b; `D0010` in Phase 2a).
    SetTimingDerate {
        factor: f64,
        early_late: EarlyLateScope,
        edge: EdgeScope,
        clock: bool,
        data: bool,
        net_delay: bool,
        cell_delay: bool,
        cell_check: bool,
        static_derate: bool,
        dynamic: bool,
        increment: bool,
        objects: Option<Obj>,
    },
    /// `set_min_pulse_width` (Phase 2b; `D0010` in Phase 2a).
    SetMinPulseWidth {
        value: f64,
        high: bool,
        low: bool,
        objects: Obj,
    },
    /// `set_ideal_network` (Phase 2b; `D0010` in Phase 2a).
    SetIdealNetwork { no_propagate: bool, objects: Obj },
    /// `group_path` (SDC-REQ-0037).
    GroupPath {
        name: Option<String>,
        default: bool,
        weight: Option<f64>,
        critical_range: Option<f64>,
        from: Option<ExceptionEnd>,
        through: Vec<ExceptionEnd>,
        to: Option<ExceptionEnd>,
        comment: Option<String>,
    },
    /// `set_max_transition` (recorded DRC command, emits `D0021`; SDC-REQ-0038).
    SetMaxTransition {
        value: f64,
        clock_path: bool,
        data_path: bool,
        edge: EdgeScope,
        objects: Obj,
    },
    /// `set_max_capacitance` (recorded DRC command, emits `D0021`; SDC-REQ-0038).
    SetMaxCapacitance {
        value: f64,
        clock_path: bool,
        data_path: bool,
        edge: EdgeScope,
        objects: Obj,
    },
    /// `set_min_capacitance` (recorded DRC command, emits `D0021`; SDC-REQ-0038).
    SetMinCapacitance {
        value: f64,
        clock_path: bool,
        data_path: bool,
        edge: EdgeScope,
        objects: Obj,
    },
    /// `set_max_fanout` (recorded DRC command, emits `D0021`; SDC-REQ-0038).
    SetMaxFanout { value: f64, objects: Obj },
    /// `set_fanout_load` (recorded DRC command, emits `D0021`; SDC-REQ-0038).
    SetFanoutLoad { value: f64, ports: Obj },
    /// `set_port_fanout_number` (recorded DRC command, emits `D0021`; SDC-REQ-0038).
    SetPortFanoutNumber { value: u32, ports: Obj },
    /// `set_dont_use` (recorded synthesis command, emits `D0020`; SDC-REQ-0039).
    SetDontUse { lib_cells: Obj },
    /// `set_dont_touch` (recorded synthesis command, emits `D0020`; SDC-REQ-0039).
    SetDontTouch { objects: Obj },
    /// `set_dont_touch_network` (recorded synthesis command, emits `D0020`; SDC-REQ-0039).
    SetDontTouchNetwork { no_propagate: bool, objects: Obj },
    /// `set_size_only` (recorded synthesis command, emits `D0020`; SDC-REQ-0039).
    SetSizeOnly { instances: Obj, no_instances: bool },
    /// `set_max_area` (recorded synthesis command, emits `D0020`; SDC-REQ-0039).
    SetMaxArea { value: f64, ignore_tns: bool },
    /// `set_max_dynamic_power` (recorded synthesis command, emits `D0020`; SDC-REQ-0039).
    SetMaxDynamicPower { value: f64, unit: Option<String> },
    /// `set_max_leakage_power` (recorded synthesis command, emits `D0020`; SDC-REQ-0039).
    SetMaxLeakagePower { value: f64, unit: Option<String> },
}

impl CmdKind {
    /// Returns the canonical SDC command name.
    pub fn name(&self) -> &'static str {
        match self {
            CmdKind::SetUnits { .. } => "set_units",
            CmdKind::CurrentDesign { .. } => "current_design",
            CmdKind::SetOperatingConditions { .. } => "set_operating_conditions",
            CmdKind::SetWireLoadModel { .. } => "set_wire_load_model",
            CmdKind::SetWireLoadMode { .. } => "set_wire_load_mode",
            CmdKind::SetWireLoadSelectionGroup { .. } => "set_wire_load_selection_group",
            CmdKind::CreateClock { .. } => "create_clock",
            CmdKind::CreateGeneratedClock { .. } => "create_generated_clock",
            CmdKind::SetPropagatedClock { .. } => "set_propagated_clock",
            CmdKind::UnsetPropagatedClock { .. } => "unset_propagated_clock",
            CmdKind::SetClockGroups { .. } => "set_clock_groups",
            CmdKind::SetClockUncertainty { .. } => "set_clock_uncertainty",
            CmdKind::SetClockLatency { .. } => "set_clock_latency",
            CmdKind::SetClockTransition { .. } => "set_clock_transition",
            CmdKind::SetInputDelay { .. } => "set_input_delay",
            CmdKind::SetOutputDelay { .. } => "set_output_delay",
            CmdKind::SetInputTransition { .. } => "set_input_transition",
            CmdKind::SetDrivingCell { .. } => "set_driving_cell",
            CmdKind::SetLoad { .. } => "set_load",
            CmdKind::SetFalsePath { .. } => "set_false_path",
            CmdKind::SetMulticyclePath { .. } => "set_multicycle_path",
            CmdKind::SetMaxDelay { .. } => "set_max_delay",
            CmdKind::SetMinDelay { .. } => "set_min_delay",
            CmdKind::SetCaseAnalysis { .. } => "set_case_analysis",
            CmdKind::SetLogicZero { .. } => "set_logic_zero",
            CmdKind::SetLogicOne { .. } => "set_logic_one",
            CmdKind::SetDisableTiming { .. } => "set_disable_timing",
            CmdKind::SetMaxTimeBorrow { .. } => "set_max_time_borrow",
            CmdKind::SetTimingDerate { .. } => "set_timing_derate",
            CmdKind::SetMinPulseWidth { .. } => "set_min_pulse_width",
            CmdKind::SetIdealNetwork { .. } => "set_ideal_network",
            CmdKind::GroupPath { .. } => "group_path",
            CmdKind::SetMaxTransition { .. } => "set_max_transition",
            CmdKind::SetMaxCapacitance { .. } => "set_max_capacitance",
            CmdKind::SetMinCapacitance { .. } => "set_min_capacitance",
            CmdKind::SetMaxFanout { .. } => "set_max_fanout",
            CmdKind::SetFanoutLoad { .. } => "set_fanout_load",
            CmdKind::SetPortFanoutNumber { .. } => "set_port_fanout_number",
            CmdKind::SetDontUse { .. } => "set_dont_use",
            CmdKind::SetDontTouch { .. } => "set_dont_touch",
            CmdKind::SetDontTouchNetwork { .. } => "set_dont_touch_network",
            CmdKind::SetSizeOnly { .. } => "set_size_only",
            CmdKind::SetMaxArea { .. } => "set_max_area",
            CmdKind::SetMaxDynamicPower { .. } => "set_max_dynamic_power",
            CmdKind::SetMaxLeakagePower { .. } => "set_max_leakage_power",
        }
    }
}
