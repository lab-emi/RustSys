// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Options of the obligation export (CHECK-REQ-0021) and its untrusted state hints.

use std::path::PathBuf;

/// Bounded, combinational AIGER proof inputs for an external CEC executable.
/// Export is not a proof and always leaves the native check undecided.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateProposal {
    pub rtl_state: String,
    pub gate_instance: String,
    pub inverted: bool,
}

/// Export options, including optional untrusted one-to-one matching hints.
#[derive(Debug, Clone)]
pub struct ObligationExport {
    pub directory: PathBuf,
    pub max_and_nodes: usize,
    pub max_outputs: usize,
    /// Budgets for untrusted one-to-one state proposals when names were removed.
    pub max_discovery_definitions: usize,
    pub max_discovery_candidates: usize,
    pub max_discovery_support_entries: usize,
    pub max_discovery_sketch_bytes: usize,
    pub max_discovery_rounds: usize,
    pub max_discovery_work: u64,
    /// At most 8192 pairs / 8 MiB of names. These are never assumptions or proofs;
    /// all original output, event and next-state obligations still require CEC.
    pub state_proposals: Vec<StateProposal>,
    /// Explicit permission for an implementation state map: RTL state bits that the
    /// implementation merged into another element or replaced by a constant are defined by
    /// that map instead of staying free, and the manifest lists every chosen RTL bit. No
    /// reset is assumed. Without re-encoded registers every implementation initial state is
    /// still covered (`arbitrary_initial`) and the claim is refinement. With re-encoded
    /// registers or valid-code invariants the claim is restricted to runs from reset
    /// (`reset_reachable`): each invariant gets base and step obligations, so it is proved in
    /// the same miter, never assumed; power-up without reset is not covered.
    pub initial_state_map: bool,
    /// Relations an external counterexample refuted in an earlier round, by RTL state and
    /// implementation instance names. Discovery never proposes them again.
    pub excluded_relations: Vec<ExcludedRelation>,
    /// Re-encoded registers to use as given instead of exploring them (state map only). A
    /// hint is a proposal like any other: the export proves its reset base, the step of its
    /// valid-code invariant and every obligation under it, so a wrong hint cannot pass.
    pub encoding_hints: Vec<EncodingHint>,
}

/// A re-encoded register given by name (see `ObligationExport::encoding_hints`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncodingHint {
    /// RTL variable; its bits are the states `variable[0]`, `variable[1]`, ...
    pub variable: String,
    /// Implementation instances, in code bit order.
    pub elements: Vec<String>,
    /// Valid `(RTL code, element code)` pairs.
    pub codes: Vec<(u64, u64)>,
}

/// A relation refuted by a replayed external counterexample (see `ObligationExport`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExcludedRelation {
    pub rtl_state: String,
    /// `constant`, `shares` or `cell`.
    pub kind: String,
    /// Constant value or parity.
    pub value: bool,
    /// Representative RTL state of `shares`, implementation instance of `cell`.
    #[serde(default)]
    pub other: String,
}

impl ObligationExport {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            max_and_nodes: 64_000_000,
            max_outputs: 8_000_000,
            max_discovery_definitions: 64_000_000,
            max_discovery_candidates: 4_194_304,
            max_discovery_support_entries: 64_000_000,
            max_discovery_sketch_bytes: 1024 * 1024 * 1024,
            max_discovery_rounds: 32,
            max_discovery_work: 16_000_000_000,
            state_proposals: Vec::new(),
            initial_state_map: false,
            excluded_relations: Vec::new(),
            encoding_hints: Vec::new(),
        }
    }
}
