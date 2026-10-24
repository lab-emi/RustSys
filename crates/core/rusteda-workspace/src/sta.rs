// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! A validated, read-only, zero-copy view of the timing table `sta.rbin` (`STA` v2, v1 still read) that the
//! static timing analysis tool of the suite commits, and a small builder of such tables
//! (WS-REQ-0126 to WS-REQ-0136, `docs/specs/workspace/sta.md`).
//!
//! The reader serves the other tools of the suite: the synthesis tool resolves `path<N>`
//! handles to the instances on a ranked path, and the workbench lists endpoints and paths,
//! without either depending on the timing engine. [`StaTable::open_rev`] maps the committed
//! table through [`Revision::open_rbin`](crate::Revision::open_rbin), which validates the rbin
//! header and the upstream map of `meta.json` (WS-REQ-0038); [`StaTable::from_bytes`] opens
//! bytes in memory. Opening validates the 24-section directory, the element sizes and the
//! invariants of `META` (WS-REQ-0128); every index read from the table is checked before it is
//! used (WS-REQ-0129), so a malformed table is `CORRUPT_TABLE` and never a panic or an
//! allocation beyond the sizes of its sections.
//!
//! [`StaTableBuilder`] writes a minimal valid table into a workspace transaction, for tools and
//! tests that need a timing table without a timing engine (WS-REQ-0135).

mod build;
mod layout;
mod read;
#[cfg(test)]
mod tests;

use std::ops::RangeInclusive;

pub use build::{NodeRef, PathSpec, StaTableBuilder};
pub use read::StaTable;

/// The table name of the timing table in `meta.json` (WS-REQ-0126).
pub const TABLE_NAME: &str = "sta";
/// The file of the timing table in a revision directory (WS-REQ-0126).
pub const TABLE_PATH: &str = "sta.rbin";
/// The rbin kind of the timing table (WS-REQ-0126).
pub const KIND: &str = "STA";
/// The format version this build writes (WS-REQ-0126).
pub const FORMAT_VERSION: u32 = 2;
/// The format versions this build reads (WS-REQ-0126).
pub const VERSIONS: RangeInclusive<u32> = 1..=FORMAT_VERSION;
/// An absent index in a record (WS-REQ-0127).
pub const NO_INDEX: u32 = u32::MAX;
/// The name of the path group that aggregates every endpoint of a corner (WS-REQ-0131).
pub const OVERALL_GROUP: &str = "**";

/// What the table stores per node (WS-REQ-0128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreMode {
    /// `NODE_REC`, `TAG_EXTRA_PTR` and `TAG_EXTRA` hold the values of every (corner, node).
    Full,
    /// Those three sections are empty; endpoints, ranked paths and the summary remain.
    Summary,
}

impl StoreMode {
    /// `full` or `summary`.
    pub fn as_str(self) -> &'static str {
        match self {
            StoreMode::Full => "full",
            StoreMode::Summary => "summary",
        }
    }
}

/// The check of a ranked path (WS-REQ-0133).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// A setup check.
    Setup,
    /// A hold check.
    Hold,
    /// A recovery check of an asynchronous control.
    Recovery,
    /// A removal check of an asynchronous control.
    Removal,
}

impl CheckKind {
    /// `setup`, `hold`, `recovery` or `removal`.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckKind::Setup => "setup",
            CheckKind::Hold => "hold",
            CheckKind::Recovery => "recovery",
            CheckKind::Removal => "removal",
        }
    }

    /// Whether the check compares early arrivals: hold and removal.
    pub fn is_hold_family(self) -> bool {
        matches!(self, CheckKind::Hold | CheckKind::Removal)
    }
}

/// A signal transition (WS-REQ-0127).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Rising.
    Rise,
    /// Falling.
    Fall,
}

/// What reaches a point of a path (WS-REQ-0134).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointKind {
    /// A port.
    Port,
    /// A net arc into a pin.
    Net,
    /// A cell arc to an output pin.
    Cell,
    /// The launching clock edge.
    ClockLaunch,
}

/// The kind of a checked endpoint (WS-REQ-0132).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    /// The data pin of a flip-flop.
    FlipFlop,
    /// The data pin of a latch.
    Latch,
    /// An output port.
    OutputPort,
    /// An asynchronous control pin with recovery and removal checks.
    RecoveryRemoval,
    /// The enable of a clock gate.
    ClockGate,
}

impl EndpointKind {
    /// `flip_flop`, `latch`, `output_port`, `recovery_removal` or `clock_gate`.
    pub fn as_str(self) -> &'static str {
        match self {
            EndpointKind::FlipFlop => "flip_flop",
            EndpointKind::Latch => "latch",
            EndpointKind::OutputPort => "output_port",
            EndpointKind::RecoveryRemoval => "recovery_removal",
            EndpointKind::ClockGate => "clock_gate",
        }
    }
}

/// The counts of `META` (WS-REQ-0128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Timing nodes: port bits first, then instance pins.
    pub nodes: u32,
    /// Port bits, the nodes `0..port_bits`.
    pub port_bits: u32,
    /// Cell instances.
    pub cells: u32,
    /// Analysis corners.
    pub corners: u32,
    /// Clocks.
    pub clocks: u32,
    /// Checked endpoints.
    pub endpoints: u32,
    /// Ranked paths, the rows of `path<N>` handles.
    pub ranked_paths: u32,
    /// Ranked paths that are materialized with arrivals, the first ones in rank order.
    pub top_k_paths: u32,
    /// Path groups per corner in `SUMMARY`.
    pub path_groups: u32,
}

/// A clock of the constraints (WS-REQ-0131).
#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    /// Its name.
    pub name: String,
    /// Its period in ns.
    pub period_ns: f64,
    /// Its setup uncertainty in ns.
    pub setup_uncertainty_ns: f64,
}

/// One row of `SUMMARY`: the endpoints of one path group in one corner (WS-REQ-0131).
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRow {
    /// The corner index.
    pub corner: usize,
    /// The corner name.
    pub corner_name: String,
    /// The path group; [`OVERALL_GROUP`] for all endpoints of the corner.
    pub group: String,
    /// Worst setup slack in ns; `None` without setup endpoints.
    pub setup_wns_ns: Option<f64>,
    /// Total negative setup slack in ns.
    pub setup_tns_ns: f64,
    /// Endpoints with negative setup slack.
    pub setup_violating: u32,
    /// Endpoints with a setup slack.
    pub setup_endpoints: u32,
    /// Worst hold slack in ns; `None` without hold endpoints.
    pub hold_wns_ns: Option<f64>,
    /// Total negative hold slack in ns.
    pub hold_tns_ns: f64,
    /// Endpoints with negative hold slack.
    pub hold_violating: u32,
    /// Endpoints with a hold slack.
    pub hold_endpoints: u32,
    /// Nets over their transition limit.
    pub max_transition_violating: u32,
    /// Nets over their capacitance limit.
    pub max_capacitance_violating: u32,
    /// Checked endpoints.
    pub total_endpoints: u32,
    /// Endpoints without a constraint.
    pub unconstrained_endpoints: u32,
}

/// A checked endpoint with its slacks in one corner (WS-REQ-0132).
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// The row in `ENDPOINTS`.
    pub index: usize,
    /// Its data node.
    pub node: u32,
    /// The name of the data node: `inst/pin` or a port name.
    pub name: String,
    /// The instance of the data pin; `None` for a port.
    pub instance: Option<String>,
    /// What it is.
    pub kind: EndpointKind,
    /// Its setup slack in ns, `None` without a setup check.
    pub setup_slack_ns: Option<f64>,
    /// Its hold slack in ns, `None` without a hold check.
    pub hold_slack_ns: Option<f64>,
    /// The row of its worst ranked setup-family path, over all corners.
    pub worst_setup_path: Option<usize>,
    /// The row of its worst ranked hold-family path, over all corners.
    pub worst_hold_path: Option<usize>,
}

/// The rank record of a path without its points (WS-REQ-0133).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathRank {
    /// The 0-based row; the handle is `path<row + 1>`.
    pub row: usize,
    /// The check.
    pub kind: CheckKind,
    /// The corner index.
    pub corner: usize,
    /// The endpoint row.
    pub endpoint: usize,
    /// The transition at the endpoint.
    pub end_transition: Transition,
    /// The slack in ns, stored as `f32`.
    pub slack_ns: f64,
}

/// A ranked path with its points (WS-REQ-0133, WS-REQ-0134).
#[derive(Debug, Clone, PartialEq)]
pub struct RankedPath {
    /// The 0-based row; the handle is `path<row + 1>`.
    pub row: usize,
    /// The check.
    pub kind: CheckKind,
    /// The corner index.
    pub corner: usize,
    /// The corner name.
    pub corner_name: String,
    /// The endpoint row.
    pub endpoint: usize,
    /// The endpoint name.
    pub endpoint_name: String,
    /// The slack in ns.
    pub slack_ns: f64,
    /// Whether `slack_ns` is the `f64` of a materialized path (`true`) or the `f32` of the
    /// rank record (`false`).
    pub slack_exact: bool,
    /// Whether the path is materialized (`PATHS`), with arrival times at its points.
    pub materialized: bool,
    /// The points from the startpoint to the endpoint.
    pub points: Vec<PathPoint>,
}

impl RankedPath {
    /// The handle `path<row + 1>`.
    pub fn handle(&self) -> String {
        format!("path{}", self.row + 1)
    }

    /// The instances of the pins on the path, each once, in path order.
    pub fn instances(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for p in &self.points {
            if let Some(inst) = p.instance.as_deref() {
                if !out.contains(&inst) {
                    out.push(inst);
                }
            }
        }
        out
    }
}

/// A point of a path (WS-REQ-0134).
#[derive(Debug, Clone, PartialEq)]
pub struct PathPoint {
    /// Its timing node.
    pub node: u32,
    /// Its name: `inst/pin`, or a port name.
    pub name: String,
    /// The instance of a pin, the prefix of its name in `NAME_HASH`; `None` for a port.
    pub instance: Option<String>,
    /// The pin name, or the port name.
    pub pin: String,
    /// What reaches it.
    pub kind: PointKind,
    /// Its transition.
    pub transition: Transition,
    /// Its arrival time in ns, for a materialized path only.
    pub arrival_ns: Option<f64>,
}
