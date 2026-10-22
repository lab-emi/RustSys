// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Mapping options, results and allocation estimates.

use super::{Aig, Cut, ExactLimits, FxHashSet, Match};

/// How a node is implemented in one phase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Impl {
    /// Not computed.
    None,
    /// Combinational input, positive phase (the input net itself).
    Input,
    /// Free complemented output of the input's driver (for example a flip-flop's QN pin).
    FreeNeg,
    /// Inverter from the other phase.
    Inv,
    /// Cell over cut `cut` of the node with match `m`.
    Cell { cut: u16, m: Match },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MapBackend {
    /// The mapper's own passes on the CPU threads of the current rayon pool.
    #[default]
    Cpu,
    /// The cut-pass task on the first GPU backend of this build that passes its self-test.
    Gpu,
    /// The cut-pass task on its CUDA backend.
    Cuda,
    /// The cut-pass task on its CPU backend: the kernels' semantics executed on the host, for
    /// verification (same netlist as `Cpu`, slower).
    CpuTask,
}

/// Mapping options.
#[derive(Debug, Clone, Copy)]
pub struct MapOptions {
    /// Priority cuts kept per node.
    pub max_cuts: usize,
    /// Total retained priority-cut storage, including per-node Vec headers.
    /// Large graphs reduce the per-node limit deterministically to fit.
    pub cut_memory_budget: u64,
    /// Relative delay relaxation for area recovery; `None` = no delay bound.
    pub delay_relax: Option<f64>,
    pub area_passes: usize,
    pub exact_passes: usize,
    pub exact_limits: ExactLimits,
    pub backend: MapBackend,
    /// Optional amortized cost of repairing references beyond this fanout.
    pub fanout_recovery: Option<u32>,
    /// Primary inputs precede state inputs; external drivers are outside the cover cost.
    pub primary_inputs: usize,
}

impl Default for MapOptions {
    fn default() -> MapOptions {
        MapOptions {
            max_cuts: 12,
            cut_memory_budget: 4 * 1024 * 1024 * 1024,
            delay_relax: Some(0.1),
            area_passes: 2,
            exact_passes: 2,
            exact_limits: ExactLimits::default(),
            backend: MapBackend::Cpu,
            fanout_recovery: None,
            primary_inputs: 0,
        }
    }
}

/// Compute the retained-cut limit without allocating graph-sized buffers.
/// Candidate tables remain separately bounded to 512 entries per worker.
pub fn effective_cut_limit(aig: &Aig, opts: MapOptions) -> Result<usize, String> {
    cut_limit_for_counts(
        aig.num_nodes(),
        aig.num_inputs(),
        opts.max_cuts,
        opts.cut_memory_budget,
    )
}

fn cut_limit_for_counts(
    nodes: usize,
    inputs: usize,
    requested: usize,
    budget: u64,
) -> Result<usize, String> {
    let nodes = nodes as u128;
    let inputs = inputs as u128;
    if nodes <= inputs {
        return Err("invalid graph/input counts for cut budget".into());
    }
    let ands = nodes - inputs - 1;
    let entry = size_of::<Cut>() as u128;
    let base = nodes * size_of::<Vec<Cut>>() as u128 + (inputs + 1) * entry;
    let remaining = (budget as u128)
        .checked_sub(base)
        .ok_or_else(|| "priority-cut memory budget is below input/table storage".to_string())?;
    let requested = requested.clamp(1, 512);
    if ands == 0 {
        return Ok(requested);
    }
    let slots = remaining / (ands * entry);
    if slots < 2 {
        return Err("priority-cut memory budget cannot retain direct and trivial cuts".into());
    }
    Ok(requested.min((slots - 1).min(512) as usize))
}

/// Timing at the Boolean boundary, in the mapper's fixed-point library-time units.
#[derive(Debug, Clone)]
pub struct TimingBoundary {
    pub launches: Vec<(u32, i32)>,
    /// One requirement per mapping root; controls that need separate STA use ARR_INF.
    pub required: Vec<i32>,
}

/// A mapped cell instance: output (node, phase) and inputs in cell pin order.
#[derive(Debug, Clone)]
pub struct MappedGate {
    pub cell: usize,
    pub out: (u32, bool),
    pub ins: Vec<(u32, bool)>,
}

/// Result of mapping the combinational logic.
#[derive(Debug, Clone)]
pub struct Mapped {
    pub gates: Vec<MappedGate>,
    /// Nodes whose complemented phase is taken from a free complemented output.
    pub free_neg_used: FxHashSet<u32>,
    pub max_arrival: f64,
    pub depth: u32,
    /// Where the cut passes ran: `cpu` or the GPU runtime.
    pub backend: &'static str,
    /// Seconds per mapping step, in execution order (SYNTH-REQ-0703).
    pub phase_s: Vec<(&'static str, f64)>,
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    // Arithmetic-only estimates, no large graph/test fixture allocation (<1 KiB).
    #[test]
    fn large_cut_tables_fit_configured_payload_budget() {
        let budget = 4 * 1024 * 1024 * 1024u64;
        assert_eq!(
            cut_limit_for_counts(1_000_001, 100_000, 40, budget).unwrap(),
            40
        );
        let (nodes, inputs) = (11_000_001usize, 1_500_000usize);
        let limit = cut_limit_for_counts(nodes, inputs, 40, budget).unwrap();
        assert!(limit > 0 && limit < 40);
        let bytes = nodes as u128 * size_of::<Vec<Cut>>() as u128
            + (inputs + 1) as u128 * size_of::<Cut>() as u128
            + (nodes - inputs - 1) as u128 * (limit + 1) as u128 * size_of::<Cut>() as u128;
        assert!(bytes <= budget as u128);
    }

    #[test]
    fn invalid_or_insufficient_cut_budgets_fail_before_allocation() {
        assert!(cut_limit_for_counts(100, 10, 40, 0).is_err());
        assert!(cut_limit_for_counts(2, 3, 40, u64::MAX).is_err());
        assert_eq!(
            cut_limit_for_counts(4, 2, usize::MAX, u64::MAX).unwrap(),
            512
        );
    }
}
