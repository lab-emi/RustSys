// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `rewrite`: local resynthesis over cuts of at most 4 leaves (SYNTH-REQ-0411). A node is
//! replaced by the structure of its cut function (`lib4`) when building that structure adds fewer
//! nodes, counting nodes that already exist outside the freed cone as free, than the node's
//! fanout-free cone over the cut frees [@mishchenko2006dag].

use std::sync::Mutex;

use rayon::prelude::*;
use rustc_hash::FxHashMap;

use super::lib4::{Lib4, dry_cost};
use super::parallel::{Levels, Mffc, WorkLimits, WorkStats};
use super::{design_roots, set_roots, sweep};
use crate::aig::{Aig, Lit};
use crate::elab::Design;
use crate::map::cuts::Cut;

const K: usize = 4;
const CUTS_PER_NODE: usize = 8;

pub fn rewrite(d: &mut Design) {
    let _ = rewrite_with_limits(d, false, WorkLimits::default());
}

/// `rewrite` that also takes replacements that free exactly as many nodes as they add, which
/// changes the structure without growing it and exposes gains to later passes.
pub fn rewrite_zero(d: &mut Design) {
    let _ = rewrite_with_limits(d, true, WorkLimits::default());
}

struct Scratch {
    lib: Lib4,
    mffc: Mffc,
}

struct Candidate {
    cuts: Box<[Cut]>,
    replacement: Option<([u32; 4], u16)>,
    bounded: bool,
}

fn candidate(
    aig: &Aig,
    id: u32,
    cuts: &[Box<[Cut]>],
    refs: &[u32],
    zero: bool,
    limits: WorkLimits,
    scratch: &mut Scratch,
) -> Candidate {
    let (f0, f1) = aig.fanins(id);
    let trivial0 = [Cut::trivial(f0.node())];
    let trivial1 = [Cut::trivial(f1.node())];
    let first = if aig.is_and(f0.node()) || f0.node() == 0 {
        &cuts[f0.node() as usize][..]
    } else {
        &trivial0
    };
    let second = if aig.is_and(f1.node()) || f1.node() == 0 {
        &cuts[f1.node() as usize][..]
    } else {
        &trivial1
    };
    // At most 9*9 cut pairs, then exactly the old stable ranking/tie order.
    let mut cand: Vec<Cut> = Vec::new();
    for a in first {
        for b in second {
            if let Some(c) = Cut::merge(a, f0, b, f1, K) {
                if c.n >= 2 && !cand.iter().any(|x| x.leaves() == c.leaves()) {
                    cand.push(c);
                }
            }
        }
    }
    cand.sort_by_key(|c| c.n);
    cand.truncate(CUTS_PER_NODE);
    let mut best: Option<(usize, [u32; 4], u16)> = None;
    let mut bounded = false;
    if refs[id as usize] > 0 {
        for c in &cand {
            let leaves = c.leaves();
            if !scratch
                .mffc
                .compute(aig, id, leaves, refs, limits.max_window_nodes.min(16_384))
            {
                bounded = true;
                continue;
            }
            let mut ls = [0u32; 4];
            ls[..leaves.len()].copy_from_slice(leaves);
            let lits = ls.map(|node| Lit::new(node, false));
            let f = (c.tt & 0xFFFF) as u16;
            let cost = dry_cost(&mut scratch.lib, aig, f, &lits, &scratch.mffc.freed);
            let saved = scratch.mffc.freed.len();
            let ok = cost < saved || (zero && cost == saved && saved > 1);
            if ok && best.as_ref().is_none_or(|b| saved - cost > b.0) {
                best = Some((saved - cost, ls, f));
            }
        }
    }
    cand.push(Cut::trivial(id));
    Candidate {
        // Do not retain the capacity of the larger temporary candidate list
        // at every graph node: stored cuts have at most nine entries.
        cuts: cand.into_boxed_slice(),
        replacement: best.map(|(_, leaves, f)| (leaves, f)),
        bounded,
    }
}

/// Plan independently within topological levels, then commit in stable order.
/// On a storage/node budget the optional pass leaves the whole design unchanged.
pub fn rewrite_with_limits(d: &mut Design, zero: bool, limits: WorkLimits) -> WorkStats {
    let mut stats = WorkStats::default();
    let n = d.aig.num_nodes();
    if n > limits.max_nodes.min(32_000_000) {
        stats.node_budget_reached = true;
        return stats;
    }
    let mut stored_bytes = n.saturating_mul(size_of::<Box<[Cut]>>());
    if stored_bytes > limits.max_cut_bytes {
        stats.storage_budget_reached = true;
        return stats;
    }
    let roots = design_roots(d);
    let aig = &d.aig;
    let mut refs = vec![0u32; n];
    for id in 1..n as u32 {
        if aig.is_and(id) {
            let (a, b) = aig.fanins(id);
            refs[a.node() as usize] += 1;
            refs[b.node() as usize] += 1;
        }
    }
    for r in &roots {
        refs[r.node() as usize] += 1;
    }
    let mut cuts: Vec<Box<[Cut]>> = vec![Box::default(); n];
    let mut plan: FxHashMap<u32, ([u32; 4], u16)> = FxHashMap::default();
    let levels = Levels::new(aig);
    let mut workers: Vec<_> = (0..limits.workers())
        .map(|_| {
            Mutex::new(Scratch {
                lib: Lib4::bounded(4096),
                mffc: Mffc::default(),
            })
        })
        .collect();
    for range in levels.offsets.windows(2) {
        for batch in levels.nodes[range[0]..range[1]].chunks(limits.batch()) {
            let out: Vec<_> = if workers.len() == 1 || batch.len() < 64 {
                let scratch = workers[0].get_mut().expect("worker cache poisoned");
                batch
                    .iter()
                    .map(|&id| candidate(aig, id, &cuts, &refs, zero, limits, scratch))
                    .collect()
            } else {
                batch
                    .par_iter()
                    .with_min_len(32)
                    .map(|&id| {
                        let index = rayon::current_thread_index().unwrap_or(0) % workers.len();
                        let mut scratch = workers[index].lock().expect("worker cache poisoned");
                        candidate(aig, id, &cuts, &refs, zero, limits, &mut scratch)
                    })
                    .collect()
            };
            for (&id, result) in batch.iter().zip(out) {
                stats.windows += 1;
                stats.bounded_windows += u64::from(result.bounded);
                stored_bytes = stored_bytes.saturating_add(result.cuts.len() * size_of::<Cut>());
                if stored_bytes > limits.max_cut_bytes {
                    stats.storage_budget_reached = true;
                    return stats;
                }
                cuts[id as usize] = result.cuts;
                if let Some(replacement) = result.replacement {
                    plan.insert(id, replacement);
                }
            }
        }
    }
    drop(cuts);
    drop(levels);
    drop(workers);
    drop(refs);
    if plan.is_empty() {
        return stats;
    }
    let mut lib = Lib4::new();
    let backup = d.clone();
    let aig = &backup.aig;
    // Rebuild with the planned replacements.
    let mut ng = Aig::new();
    let mut img = vec![Lit::FALSE; n];
    for &i in aig.inputs() {
        img[i as usize] = ng.input();
    }
    for id in aig.cone(&roots) {
        if !aig.is_and(id) {
            continue;
        }
        img[id as usize] = match plan.get(&id) {
            Some((leaves, f)) => {
                // Unused leaf slots hold node 0 (the constant); the function ignores them.
                let ls: [Lit; 4] = std::array::from_fn(|i| img[leaves[i] as usize]);
                lib.build(&mut ng, *f, &ls)
            }
            None => {
                let (a, b) = aig.fanins(id);
                let x = img[a.node() as usize].xor(a.is_compl());
                let y = img[b.node() as usize].xor(b.is_compl());
                ng.and(x, y)
            }
        };
    }
    let new_roots: Vec<Lit> = roots
        .iter()
        .map(|r| img[r.node() as usize].xor(r.is_compl()))
        .collect();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in p.bits.iter_mut() {
            *b = img[b.node() as usize];
        }
    }
    for s in d.states.iter_mut() {
        s.q = img[s.q.node() as usize];
    }
    d.aig = ng;
    set_roots(d, &new_roots);
    sweep(d);
    // Keep the result only if it is not larger (the gain estimate ignores sharing).
    if d.aig.num_ands() > backup.aig.num_ands() {
        *d = backup;
    }
    stats
}
