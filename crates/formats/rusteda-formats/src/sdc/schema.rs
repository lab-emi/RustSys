// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Per-command static option schemas, status table, unit conversion, and typed command builder
//! (SDC-REQ-0002, SDC-REQ-0016 to SDC-REQ-0039).

/// Classification of an SDC command under Rule 7 (SDC-REQ-0018, SDC-REQ-0019).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmdStatus {
    /// Interpreted by the Phase 2a timing engine.
    Interpreted,
    /// Recorded in `CONSTR v1` and emits a `D0021` warning (`drc_status: "unchecked"`).
    RecordedDrc,
    /// Recorded in `CONSTR v1` for synthesis/downstream tools and emits a `D0020` note.
    RecordedNoTimingEffect,
    /// Scheduled for Phase 2b (`S2`); rejected in Phase 2a with `D0010` error.
    Phase2b,
    /// Outside Phase 2 scope; rejected with `D0010` error.
    Unsupported,
}

/// Expected value type of a command option (SDC-REQ-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptValueKind {
    Flag,
    Str,
    Float,
    Int,
    FloatPair,
    FloatTriple,
    IntTriple,
    Obj,
    Filter,
}

/// Static specification of one command option (SDC-REQ-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptSpec {
    pub name: &'static str,
    pub kind: OptValueKind,
    pub repeatable: bool,
    pub exclusive_group: Option<u8>,
    pub phase2b: bool,
}

impl OptSpec {
    const fn flag(name: &'static str) -> Self {
        Self {
            name,
            kind: OptValueKind::Flag,
            repeatable: false,
            exclusive_group: None,
            phase2b: false,
        }
    }

    const fn flag_ex(name: &'static str, group: u8) -> Self {
        Self {
            name,
            kind: OptValueKind::Flag,
            repeatable: false,
            exclusive_group: Some(group),
            phase2b: false,
        }
    }

    const fn flag_2b(name: &'static str) -> Self {
        Self {
            name,
            kind: OptValueKind::Flag,
            repeatable: false,
            exclusive_group: None,
            phase2b: true,
        }
    }

    const fn val(name: &'static str, kind: OptValueKind) -> Self {
        Self {
            name,
            kind,
            repeatable: false,
            exclusive_group: None,
            phase2b: false,
        }
    }

    const fn val_ex(name: &'static str, kind: OptValueKind, group: u8) -> Self {
        Self {
            name,
            kind,
            repeatable: false,
            exclusive_group: Some(group),
            phase2b: false,
        }
    }

    const fn val_rep(name: &'static str, kind: OptValueKind) -> Self {
        Self {
            name,
            kind,
            repeatable: true,
            exclusive_group: None,
            phase2b: false,
        }
    }

    const fn val_2b(name: &'static str, kind: OptValueKind, repeatable: bool) -> Self {
        Self {
            name,
            kind,
            repeatable,
            exclusive_group: None,
            phase2b: true,
        }
    }
}

/// Static schema for one SDC command or object query (SDC-REQ-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdSchema {
    pub name: &'static str,
    pub status: CmdStatus,
    pub min_pos: usize,
    pub max_pos: usize,
    pub options: &'static [OptSpec],
}

const OPTS_NONE: &[OptSpec] = &[];

const OPTS_GET_OBJ: &[OptSpec] = &[
    OptSpec::flag("-hierarchical"),
    OptSpec::flag("-hier"),
    OptSpec::flag("-regexp"),
    OptSpec::flag("-nocase"),
    OptSpec::flag("-quiet"),
    OptSpec::flag("-exact"),
    OptSpec::val("-of_objects", OptValueKind::Obj),
    OptSpec::val("-filter", OptValueKind::Filter),
];

const OPTS_ALL_INPUTS: &[OptSpec] = &[
    OptSpec::flag("-no_clocks"),
    OptSpec::val("-clock", OptValueKind::Obj),
    OptSpec::flag_ex("-rise", 1),
    OptSpec::flag_ex("-fall", 1),
    OptSpec::flag_ex("-level_sensitive", 2),
    OptSpec::flag_ex("-edge_triggered", 2),
];

const OPTS_ALL_OUTPUTS: &[OptSpec] = &[
    OptSpec::val("-clock", OptValueKind::Obj),
    OptSpec::flag_ex("-rise", 1),
    OptSpec::flag_ex("-fall", 1),
    OptSpec::flag_ex("-level_sensitive", 2),
    OptSpec::flag_ex("-edge_triggered", 2),
];

const OPTS_ALL_REGISTERS: &[OptSpec] = &[
    OptSpec::val_ex("-clock", OptValueKind::Obj, 1),
    OptSpec::val_ex("-rise_clock", OptValueKind::Obj, 1),
    OptSpec::val_ex("-fall_clock", OptValueKind::Obj, 1),
    OptSpec::flag_ex("-cells", 2),
    OptSpec::flag_ex("-data_pins", 2),
    OptSpec::flag_ex("-clock_pins", 2),
    OptSpec::flag_ex("-async_pins", 2),
    OptSpec::flag_ex("-output_pins", 2),
    OptSpec::flag_ex("-level_sensitive", 3),
    OptSpec::flag_ex("-edge_triggered", 3),
    OptSpec::flag_ex("-master_slave", 3),
    OptSpec::flag("-no_hierarchy"),
];

const OPTS_SET_UNITS: &[OptSpec] = &[
    OptSpec::val("-time", OptValueKind::Str),
    OptSpec::val("-capacitance", OptValueKind::Str),
    OptSpec::val("-resistance", OptValueKind::Str),
    OptSpec::val("-voltage", OptValueKind::Str),
    OptSpec::val("-current", OptValueKind::Str),
    OptSpec::val("-power", OptValueKind::Str),
    OptSpec::val("-altitude", OptValueKind::Str),
];

const OPTS_SET_OP_COND: &[OptSpec] = &[
    OptSpec::val("-library", OptValueKind::Str),
    OptSpec::val("-analysis_type", OptValueKind::Str),
    OptSpec::val("-max", OptValueKind::Str),
    OptSpec::val("-min", OptValueKind::Str),
    OptSpec::val("-max_library", OptValueKind::Str),
    OptSpec::val("-min_library", OptValueKind::Str),
    OptSpec::val("-object_list", OptValueKind::Obj),
];

const OPTS_SET_WLM: &[OptSpec] = &[
    OptSpec::val("-name", OptValueKind::Str),
    OptSpec::val("-library", OptValueKind::Str),
    OptSpec::flag("-min"),
    OptSpec::flag("-max"),
];

const OPTS_SET_WLSG: &[OptSpec] = &[
    OptSpec::val("-library", OptValueKind::Str),
    OptSpec::flag("-min"),
    OptSpec::flag("-max"),
];

const OPTS_CREATE_CLOCK: &[OptSpec] = &[
    OptSpec::val("-name", OptValueKind::Str),
    OptSpec::val("-period", OptValueKind::Float),
    OptSpec::val("-waveform", OptValueKind::FloatPair),
    OptSpec::flag("-add"),
    OptSpec::val("-comment", OptValueKind::Str),
];

const OPTS_CREATE_GEN_CLOCK: &[OptSpec] = &[
    OptSpec::val("-name", OptValueKind::Str),
    OptSpec::val("-source", OptValueKind::Obj),
    OptSpec::val("-master_clock", OptValueKind::Obj),
    OptSpec::val_ex("-divide_by", OptValueKind::Int, 1),
    OptSpec::val_ex("-multiply_by", OptValueKind::Int, 1),
    OptSpec::val("-duty_cycle", OptValueKind::Float),
    OptSpec::val_ex("-edges", OptValueKind::IntTriple, 1),
    OptSpec::val("-edge_shift", OptValueKind::FloatTriple),
    OptSpec::flag("-invert"),
    OptSpec::flag("-combinational"),
    OptSpec::flag("-add"),
    OptSpec::val("-comment", OptValueKind::Str),
];

const OPTS_CLOCK_GROUPS: &[OptSpec] = &[
    OptSpec::flag_ex("-asynchronous", 1),
    OptSpec::flag_ex("-logically_exclusive", 1),
    OptSpec::flag_ex("-physically_exclusive", 1),
    OptSpec::val_rep("-group", OptValueKind::Obj),
    OptSpec::val("-name", OptValueKind::Str),
    OptSpec::flag("-allow_paths"),
    OptSpec::val("-comment", OptValueKind::Str),
];

const OPTS_CLOCK_UNCERTAINTY: &[OptSpec] = &[
    OptSpec::flag("-setup"),
    OptSpec::flag("-hold"),
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::val_ex("-from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-rise_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-fall_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-rise_to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-fall_to", OptValueKind::Obj, 2),
];

const OPTS_CLOCK_LATENCY: &[OptSpec] = &[
    OptSpec::flag("-source"),
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::flag("-min"),
    OptSpec::flag("-max"),
    OptSpec::flag("-early"),
    OptSpec::flag("-late"),
    OptSpec::val("-clock", OptValueKind::Obj),
    OptSpec::val_2b("-dynamic", OptValueKind::Float, false),
];

const OPTS_CLOCK_TRANSITION: &[OptSpec] = &[
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::flag("-min"),
    OptSpec::flag("-max"),
];

const OPTS_IO_DELAY: &[OptSpec] = &[
    OptSpec::val("-clock", OptValueKind::Obj),
    OptSpec::flag("-clock_fall"),
    OptSpec::flag("-level_sensitive"),
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::flag("-min"),
    OptSpec::flag("-max"),
    OptSpec::flag("-add_delay"),
    OptSpec::flag("-network_latency_included"),
    OptSpec::flag("-source_latency_included"),
    OptSpec::val_2b("-reference_pin", OptValueKind::Obj, false),
];

const OPTS_INPUT_TRANSITION: &[OptSpec] = &[
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::flag("-min"),
    OptSpec::flag("-max"),
    OptSpec::val("-clock", OptValueKind::Obj),
    OptSpec::flag("-clock_fall"),
];

const OPTS_DRIVING_CELL: &[OptSpec] = &[
    OptSpec::val("-lib_cell", OptValueKind::Str),
    OptSpec::val("-library", OptValueKind::Str),
    OptSpec::val("-pin", OptValueKind::Str),
    OptSpec::val("-from_pin", OptValueKind::Str),
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::flag("-min"),
    OptSpec::flag("-max"),
    OptSpec::val("-input_transition_rise", OptValueKind::Float),
    OptSpec::val("-input_transition_fall", OptValueKind::Float),
    OptSpec::val("-multiply_by", OptValueKind::Float),
    OptSpec::flag("-dont_scale"),
    OptSpec::flag("-no_design_rule"),
    OptSpec::val("-clock", OptValueKind::Obj),
    OptSpec::flag("-clock_fall"),
];

const OPTS_SET_LOAD: &[OptSpec] = &[
    OptSpec::flag("-pin_load"),
    OptSpec::flag("-wire_load"),
    OptSpec::flag("-subtract_pin_load"),
    OptSpec::flag("-min"),
    OptSpec::flag("-max"),
];

const OPTS_FALSE_PATH: &[OptSpec] = &[
    OptSpec::flag("-setup"),
    OptSpec::flag("-hold"),
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::val_ex("-from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-rise_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-fall_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-rise_to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-fall_to", OptValueKind::Obj, 2),
    OptSpec::val_2b("-through", OptValueKind::Obj, true),
    OptSpec::val_2b("-rise_through", OptValueKind::Obj, true),
    OptSpec::val_2b("-fall_through", OptValueKind::Obj, true),
    OptSpec::flag_2b("-reset_path"),
    OptSpec::val("-comment", OptValueKind::Str),
];

const OPTS_MULTICYCLE_PATH: &[OptSpec] = &[
    OptSpec::flag("-setup"),
    OptSpec::flag("-hold"),
    OptSpec::flag_ex("-start", 3),
    OptSpec::flag_ex("-end", 3),
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::val_ex("-from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-rise_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-fall_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-rise_to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-fall_to", OptValueKind::Obj, 2),
    OptSpec::val_2b("-through", OptValueKind::Obj, true),
    OptSpec::val_2b("-rise_through", OptValueKind::Obj, true),
    OptSpec::val_2b("-fall_through", OptValueKind::Obj, true),
    OptSpec::flag_2b("-reset_path"),
    OptSpec::val("-comment", OptValueKind::Str),
];

const OPTS_MAX_MIN_DELAY: &[OptSpec] = &[
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::val_ex("-from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-rise_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-fall_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-rise_to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-fall_to", OptValueKind::Obj, 2),
    OptSpec::val_2b("-through", OptValueKind::Obj, true),
    OptSpec::val_2b("-rise_through", OptValueKind::Obj, true),
    OptSpec::val_2b("-fall_through", OptValueKind::Obj, true),
    OptSpec::flag("-ignore_clock_latency"),
    OptSpec::flag_2b("-reset_path"),
    OptSpec::val("-comment", OptValueKind::Str),
];

const OPTS_DISABLE_TIMING: &[OptSpec] = &[
    OptSpec::val("-from", OptValueKind::Str),
    OptSpec::val("-to", OptValueKind::Str),
];

const OPTS_GROUP_PATH: &[OptSpec] = &[
    OptSpec::val_ex("-name", OptValueKind::Str, 3),
    OptSpec::flag_ex("-default", 3),
    OptSpec::val("-weight", OptValueKind::Float),
    OptSpec::val("-critical_range", OptValueKind::Float),
    OptSpec::val_ex("-from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-rise_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-fall_from", OptValueKind::Obj, 1),
    OptSpec::val_ex("-to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-rise_to", OptValueKind::Obj, 2),
    OptSpec::val_ex("-fall_to", OptValueKind::Obj, 2),
    OptSpec::val_2b("-through", OptValueKind::Obj, true),
    OptSpec::val_2b("-rise_through", OptValueKind::Obj, true),
    OptSpec::val_2b("-fall_through", OptValueKind::Obj, true),
    OptSpec::val("-comment", OptValueKind::Str),
];

const OPTS_DRC_PATH: &[OptSpec] = &[
    OptSpec::flag("-clock_path"),
    OptSpec::flag("-data_path"),
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
];

const OPTS_DONT_TOUCH_NET: &[OptSpec] = &[OptSpec::flag("-no_propagate")];
const OPTS_SIZE_ONLY: &[OptSpec] = &[OptSpec::flag("-no_instances")];
const OPTS_MAX_AREA: &[OptSpec] = &[OptSpec::flag("-ignore_tns")];
const OPTS_MAX_POWER: &[OptSpec] = &[OptSpec::val("-unit", OptValueKind::Str)];

const OPTS_TIMING_DERATE: &[OptSpec] = &[
    OptSpec::flag("-early"),
    OptSpec::flag("-late"),
    OptSpec::flag("-rise"),
    OptSpec::flag("-fall"),
    OptSpec::flag("-clock"),
    OptSpec::flag("-data"),
    OptSpec::flag("-net_delay"),
    OptSpec::flag("-cell_delay"),
    OptSpec::flag("-cell_check"),
    OptSpec::flag("-static"),
    OptSpec::flag("-dynamic"),
    OptSpec::flag("-increment"),
];

const OPTS_MIN_PULSE_WIDTH: &[OptSpec] = &[OptSpec::flag("-high"), OptSpec::flag("-low")];

/// Static table of top-level SDC command schemas (SDC-REQ-0002, SDC-REQ-0018).
pub static SCHEMAS: &[CmdSchema] = &[
    // Phase 2a interpreted commands
    CmdSchema {
        name: "set_units",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 0,
        options: OPTS_SET_UNITS,
    },
    CmdSchema {
        name: "current_design",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_operating_conditions",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 1,
        options: OPTS_SET_OP_COND,
    },
    CmdSchema {
        name: "set_wire_load_model",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 1,
        options: OPTS_SET_WLM,
    },
    CmdSchema {
        name: "set_wire_load_mode",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_wire_load_selection_group",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 2,
        options: OPTS_SET_WLSG,
    },
    CmdSchema {
        name: "create_clock",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 1,
        options: OPTS_CREATE_CLOCK,
    },
    CmdSchema {
        name: "set_clock_groups",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 0,
        options: OPTS_CLOCK_GROUPS,
    },
    CmdSchema {
        name: "set_clock_uncertainty",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 2,
        options: OPTS_CLOCK_UNCERTAINTY,
    },
    CmdSchema {
        name: "set_clock_latency",
        status: CmdStatus::Interpreted,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_CLOCK_LATENCY,
    },
    CmdSchema {
        name: "set_clock_transition",
        status: CmdStatus::Interpreted,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_CLOCK_TRANSITION,
    },
    CmdSchema {
        name: "set_input_delay",
        status: CmdStatus::Interpreted,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_IO_DELAY,
    },
    CmdSchema {
        name: "set_output_delay",
        status: CmdStatus::Interpreted,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_IO_DELAY,
    },
    CmdSchema {
        name: "set_input_transition",
        status: CmdStatus::Interpreted,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_INPUT_TRANSITION,
    },
    CmdSchema {
        name: "set_driving_cell",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_DRIVING_CELL,
    },
    CmdSchema {
        name: "set_load",
        status: CmdStatus::Interpreted,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_SET_LOAD,
    },
    CmdSchema {
        name: "set_false_path",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 0,
        options: OPTS_FALSE_PATH,
    },
    CmdSchema {
        name: "set_multicycle_path",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_MULTICYCLE_PATH,
    },
    CmdSchema {
        name: "set_max_delay",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_MAX_MIN_DELAY,
    },
    CmdSchema {
        name: "set_min_delay",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_MAX_MIN_DELAY,
    },
    CmdSchema {
        name: "set_case_analysis",
        status: CmdStatus::Interpreted,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_logic_zero",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_logic_one",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_disable_timing",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_DISABLE_TIMING,
    },
    CmdSchema {
        name: "set_max_time_borrow",
        status: CmdStatus::Interpreted,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "group_path",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 0,
        options: OPTS_GROUP_PATH,
    },
    // Recorded DRC commands (D0021 warning)
    CmdSchema {
        name: "set_max_transition",
        status: CmdStatus::RecordedDrc,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_DRC_PATH,
    },
    CmdSchema {
        name: "set_max_capacitance",
        status: CmdStatus::RecordedDrc,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_DRC_PATH,
    },
    CmdSchema {
        name: "set_min_capacitance",
        status: CmdStatus::RecordedDrc,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_DRC_PATH,
    },
    CmdSchema {
        name: "set_max_fanout",
        status: CmdStatus::RecordedDrc,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_fanout_load",
        status: CmdStatus::RecordedDrc,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_port_fanout_number",
        status: CmdStatus::RecordedDrc,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_NONE,
    },
    // Recorded synthesis/power commands (D0020 note)
    CmdSchema {
        name: "set_dont_use",
        status: CmdStatus::RecordedNoTimingEffect,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_dont_touch",
        status: CmdStatus::RecordedNoTimingEffect,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_dont_touch_network",
        status: CmdStatus::RecordedNoTimingEffect,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_DONT_TOUCH_NET,
    },
    CmdSchema {
        name: "set_size_only",
        status: CmdStatus::RecordedNoTimingEffect,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_SIZE_ONLY,
    },
    CmdSchema {
        name: "set_max_area",
        status: CmdStatus::RecordedNoTimingEffect,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_MAX_AREA,
    },
    CmdSchema {
        name: "set_max_dynamic_power",
        status: CmdStatus::RecordedNoTimingEffect,
        min_pos: 1,
        max_pos: 2,
        options: OPTS_MAX_POWER,
    },
    CmdSchema {
        name: "set_max_leakage_power",
        status: CmdStatus::RecordedNoTimingEffect,
        min_pos: 1,
        max_pos: 2,
        options: OPTS_MAX_POWER,
    },
    // Phase 2b commands (D0010 in Phase 2a)
    CmdSchema {
        name: "create_generated_clock",
        status: CmdStatus::Phase2b,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_CREATE_GEN_CLOCK,
    },
    CmdSchema {
        name: "set_propagated_clock",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "unset_propagated_clock",
        status: CmdStatus::Interpreted,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_timing_derate",
        status: CmdStatus::Phase2b,
        min_pos: 1,
        max_pos: 2,
        options: OPTS_TIMING_DERATE,
    },
    CmdSchema {
        name: "set_min_pulse_width",
        status: CmdStatus::Phase2b,
        min_pos: 2,
        max_pos: 2,
        options: OPTS_MIN_PULSE_WIDTH,
    },
    CmdSchema {
        name: "set_ideal_network",
        status: CmdStatus::Phase2b,
        min_pos: 1,
        max_pos: 1,
        options: OPTS_DONT_TOUCH_NET,
    },
    // Explicitly classified unsupported commands (D0010)
    CmdSchema {
        name: "set_sense",
        status: CmdStatus::Unsupported,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_clock_sense",
        status: CmdStatus::Unsupported,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_data_check",
        status: CmdStatus::Unsupported,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_logic_dc",
        status: CmdStatus::Unsupported,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_resistance",
        status: CmdStatus::Unsupported,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "set_clock_gating_check",
        status: CmdStatus::Unsupported,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_NONE,
    },
];

/// Static table of object-query command schemas (SDC-REQ-0005).
pub static QUERY_SCHEMAS: &[CmdSchema] = &[
    CmdSchema {
        name: "get_ports",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_GET_OBJ,
    },
    CmdSchema {
        name: "get_port",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_GET_OBJ,
    },
    CmdSchema {
        name: "get_pins",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_GET_OBJ,
    },
    CmdSchema {
        name: "get_cells",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_GET_OBJ,
    },
    CmdSchema {
        name: "get_nets",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_GET_OBJ,
    },
    CmdSchema {
        name: "get_clocks",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_GET_OBJ,
    },
    CmdSchema {
        name: "get_lib_cells",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_GET_OBJ,
    },
    CmdSchema {
        name: "get_lib_pins",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: usize::MAX,
        options: OPTS_GET_OBJ,
    },
    CmdSchema {
        name: "all_inputs",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 0,
        options: OPTS_ALL_INPUTS,
    },
    CmdSchema {
        name: "all_outputs",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 0,
        options: OPTS_ALL_OUTPUTS,
    },
    CmdSchema {
        name: "all_clocks",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 0,
        options: OPTS_NONE,
    },
    CmdSchema {
        name: "all_registers",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 0,
        options: OPTS_ALL_REGISTERS,
    },
    CmdSchema {
        name: "current_design",
        status: CmdStatus::Interpreted,
        min_pos: 0,
        max_pos: 1,
        options: OPTS_NONE,
    },
];

/// Looks up a top-level SDC command schema by name.
pub fn find_schema(name: &str) -> Option<&'static CmdSchema> {
    SCHEMAS.iter().find(|s| s.name == name)
}

/// Looks up an object-query command schema by name.
pub fn find_query_schema(name: &str) -> Option<&'static CmdSchema> {
    QUERY_SCHEMAS.iter().find(|s| s.name == name)
}

mod build;
mod parse;

pub use parse::{ActiveUnits, UnitScale, parse_query_cmd, parse_top_cmd};
