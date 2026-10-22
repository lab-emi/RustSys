// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! What front ends use of RustSys (PROTO-REQ-0002): the run entry point, options, report and
//! netlist types, and the names of phases, steps and metrics (SYNTH-REQ-0710).

pub use crate::electrical::{
    Incremental, IncrementalReport, IterationRecord, Options as ElectricalOptions,
    Report as ElectricalReport, SearchEffort, StopReason, StopRule, refine_corners,
};
pub use crate::estimate::{NetlistEstimate, netlist_estimate};
pub use crate::map::MapBackend;
pub use crate::map::library::CellLib;
pub use crate::{
    Effort, Goal, InitialValues, REPORT_FORMAT, Report, SourceFormat, Sources, SynthError,
    SynthOptions, SynthOutput, optimization_steps, synthesize_with,
};
pub use rusteda_formats::verilog::write::{NlInstance, NlModule, NlRef};

/// Phases of a synthesis run, in order.
pub const PHASES: [&str; 5] = ["read", "elaborate", "optimize", "map", "emit"];

/// Phases when electrical constraints are supplied.
pub const ELECTRICAL_PHASES: [&str; 7] = [
    "read",
    "elaborate",
    "optimize",
    "map",
    "emit",
    "electrical",
    "write",
];

/// Steps of the `map` phase.
pub const MAP_STEPS: [&str; 5] = [
    "gpu_wait",
    "delay_pass",
    "area_pass",
    "technology_pass",
    "exact_pass",
];

/// Metrics: name, unit and meaning.
pub const METRICS: &[(&str, &str, &str)] = &[
    (
        "aig_ands",
        "",
        "AND nodes after elaboration or an optimization pass",
    ),
    (
        "map_arrival",
        "library time units",
        "worst output arrival after a mapping pass",
    ),
    ("map_gates", "", "gates of the final cover"),
    (
        "electrical_slack_ns",
        "ns",
        "worst setup slack of the ideal-clock electrical estimate",
    ),
];
