// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded read-only planning storage for deterministic parallel optimization.
//!
//! At 13M nodes: refs + levels + order are about 156 MiB, boxed-cut headers
//! about 208 MiB, and retained cuts have an explicit 8 GiB ceiling. Worker
//! scratch is local to at most 16 shards, never a full per-worker graph copy.
//! A 16K-node MFFC visits at most 32K fanin edges and uses under 4 MiB/shard.
//! A 65536-node rewrite batch keeps temporary cut results below 32 MiB.
//! Refactor plans charge every retained template against a separate 2 GiB cap.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::aig::Aig;

#[derive(Debug, Clone, Copy)]
pub struct WorkLimits {
    pub max_nodes: usize,
    pub max_window_nodes: usize,
    pub max_cut_bytes: usize,
    pub max_plan_bytes: usize,
    pub max_workers: usize,
    pub batch_nodes: usize,
}

impl Default for WorkLimits {
    fn default() -> Self {
        Self {
            max_nodes: 32_000_000,
            max_window_nodes: 16_384,
            max_cut_bytes: 8 * 1024 * 1024 * 1024,
            max_plan_bytes: 2 * 1024 * 1024 * 1024,
            max_workers: 16,
            batch_nodes: 65_536,
        }
    }
}

impl WorkLimits {
    pub(super) fn workers(self) -> usize {
        rayon::current_num_threads().min(self.max_workers.clamp(1, 16))
    }

    pub(super) fn batch(self) -> usize {
        self.batch_nodes.clamp(1, 65_536)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WorkStats {
    pub windows: u64,
    pub bounded_windows: u64,
    pub node_budget_reached: bool,
    pub storage_budget_reached: bool,
}

/// CSR ordering of AND nodes by topological level, stable within each level.
pub(super) struct Levels {
    pub nodes: Vec<u32>,
    pub offsets: Vec<usize>,
}

impl Levels {
    pub fn new(aig: &Aig) -> Self {
        let levels = aig.levels();
        let height = levels.iter().copied().max().unwrap_or(0) as usize;
        assert!(height < aig.num_nodes(), "AIG must be topological");
        let mut offsets = vec![0usize; height + 2];
        for id in 1..aig.num_nodes() as u32 {
            if aig.is_and(id) {
                offsets[levels[id as usize] as usize + 1] += 1;
            }
        }
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }
        let mut cursor = offsets[..offsets.len() - 1].to_vec();
        let mut nodes = vec![0; *offsets.last().unwrap()];
        for id in 1..aig.num_nodes() as u32 {
            if aig.is_and(id) {
                let at = &mut cursor[levels[id as usize] as usize];
                nodes[*at] = id;
                *at += 1;
            }
        }
        Self { nodes, offsets }
    }
}

/// Sparse decrement overlay: a planning worker never mutates global fanout counts.
#[derive(Debug, Default)]
pub(super) struct Mffc {
    remaining: FxHashMap<u32, u32>,
    stack: Vec<u32>,
    pub freed: FxHashSet<u32>,
}

impl Mffc {
    pub fn compute(
        &mut self,
        aig: &Aig,
        root: u32,
        leaves: &[u32],
        refs: &[u32],
        limit: usize,
    ) -> bool {
        self.remaining.clear();
        self.stack.clear();
        self.freed.clear();
        if limit == 0 {
            return false;
        }
        self.freed.insert(root);
        self.stack.push(root);
        while let Some(node) = self.stack.pop() {
            let (a, b) = aig.fanins(node);
            for f in [a.node(), b.node()] {
                if !aig.is_and(f) || leaves.contains(&f) {
                    continue;
                }
                // At most two edges per freed node; duplicate visits only
                // decrement the same overlay entry, never expand all paths.
                if self.remaining.len() >= limit.saturating_mul(2) {
                    return false;
                }
                let count = self.remaining.entry(f).or_insert(refs[f as usize]);
                let Some(next) = count.checked_sub(1) else {
                    return false;
                };
                *count = next;
                if next == 0 {
                    if self.freed.len() >= limit {
                        return false;
                    }
                    self.freed.insert(f);
                    self.stack.push(f);
                }
            }
        }
        true
    }
}
