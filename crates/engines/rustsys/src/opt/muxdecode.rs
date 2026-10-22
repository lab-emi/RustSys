// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `mux_decode`: nested multiplexers whose selects repeat across bits become sums of products
//! over shared select products (SYNTH-REQ-0414).
//!
//! Memory plan: the input graph is bounded before cloning; auxiliary storage is
//! two byte-valued bool vectors, two u32 node tables, and a capped signature
//! table. At C910's ~15M total nodes this is <256 MiB plus traversal scratch,
//! the old/new AIGs and rollback design.
//! No per-root path vectors survive a scan. A tree has <=8 paths of <=3 selects;
//! configurable node/signature/work limits skip this optional pass atomically.

use rustc_hash::FxHashMap;

use super::{design_roots, set_roots, sweep};
use crate::aig::{Aig, Lit};
use crate::elab::Design;

/// Deepest nesting of a decoded tree: at most `2^DEPTH` paths.
const DEPTH: usize = 3;
const MAX_PATHS: usize = 1 << DEPTH;

/// Hard limits for one optional mux-decoding pass.
#[derive(Debug, Clone, Copy)]
pub struct DecodeLimits {
    pub max_nodes: usize,
    pub max_signatures: usize,
    /// Conservative expansion charge, including the root probe, over all scans.
    pub max_expansions: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_nodes: 32_000_000,
            max_signatures: 262_144,
            max_expansions: 2_000_000_000,
        }
    }
}

/// A budget fallback leaves the original design unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeStatus {
    Unchanged,
    Rewritten,
    BudgetReached,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Signature {
    paths: [([Lit; DEPTH], u8); MAX_PATHS],
    len: u8,
}

impl Signature {
    fn of(paths: &[Path]) -> Self {
        let mut result = Self {
            paths: [([Lit::FALSE; DEPTH], 0); MAX_PATHS],
            len: paths.len() as u8,
        };
        for (slot, (selects, _)) in result.paths.iter_mut().zip(paths) {
            slot.0[..selects.len()].copy_from_slice(selects);
            slot.1 = selects.len() as u8;
        }
        result
    }
}

struct Work {
    used: u64,
    limit: u64,
}

impl Work {
    fn tree(&mut self, reader: &Reader<'_>, id: u32) -> Result<(Vec<Path>, Vec<u32>), ()> {
        // One mux probe plus a depth-three binary expansion has <=16 visits.
        self.used = self.used.checked_add((2 * MAX_PATHS) as u64).ok_or(())?;
        if self.used > self.limit {
            return Err(());
        }
        let l = Lit::new(id, false);
        if reader.mux(l).is_none() {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut paths = Vec::with_capacity(MAX_PATHS);
        let mut inner = Vec::with_capacity(MAX_PATHS - 1);
        reader.expand(l, 0, &mut Vec::with_capacity(DEPTH), &mut paths, &mut inner);
        Ok((paths, inner))
    }
}

/// One root-to-leaf path: the select literals along it and the leaf literal.
type Path = (Vec<Lit>, Lit);

struct Reader<'a> {
    aig: &'a Aig,
    fanout: Vec<u32>,
}

impl Reader<'_> {
    /// A node used once, so rebuilding its function elsewhere duplicates nothing.
    fn single(&self, l: Lit) -> bool {
        let m = l.node();
        self.aig.is_and(m) && self.fanout[m as usize] == 1
    }

    /// The multiplexer reading of `l` with a regular select, when both AND nodes under its
    /// top node feed nothing else.
    fn mux(&self, l: Lit) -> Option<(Lit, Lit, Lit)> {
        let n = l.node();
        if !self.aig.is_and(n) {
            return None;
        }
        let (p, q) = self.aig.fanins(n);
        if !self.single(p) || !self.single(q) {
            return None;
        }
        self.aig
            .mux_readings(l)
            .into_iter()
            .find(|(s, _, _)| !s.is_compl())
    }

    /// `x & y` read as `x ? y : 0` (or its complement `x ? !y : 1`) when exactly one fanin is
    /// shared: the shared one is the select.
    fn gate(&self, l: Lit) -> Option<(Lit, Lit, Lit)> {
        let n = l.node();
        if !self.single(l) || !self.aig.mux_readings(l).is_empty() {
            return None;
        }
        let (a, b) = self.aig.fanins(n);
        let (fa, fb) = (
            self.fanout[a.node() as usize],
            self.fanout[b.node() as usize],
        );
        let (s, data) = if fa >= 2 && fb < 2 && !a.is_const() {
            (a, b)
        } else if fb >= 2 && fa < 2 && !b.is_const() {
            (b, a)
        } else {
            return None;
        };
        Some(if l.is_compl() {
            (s, !data, Lit::TRUE)
        } else {
            (s, data, Lit::FALSE)
        })
    }

    /// Paths of the tree under `l`, the inner nodes it covers, and whether it is a tree at all.
    fn expand(
        &self,
        l: Lit,
        depth: usize,
        prefix: &mut Vec<Lit>,
        paths: &mut Vec<Path>,
        inner: &mut Vec<u32>,
    ) {
        let reading = if depth == 0 {
            self.mux(l)
        } else if depth < DEPTH && self.single(l) {
            self.mux(l).or_else(|| self.gate(l))
        } else {
            None
        };
        let Some((s, t, e)) = reading else {
            paths.push((prefix.clone(), l));
            return;
        };
        if depth > 0 {
            inner.push(l.node());
        }
        for (sel, data) in [(s, t), (!s, e)] {
            prefix.push(sel);
            self.expand(data, depth + 1, prefix, paths, inner);
            prefix.pop();
        }
    }
}

pub fn mux_decode(d: &mut Design) {
    let _ = mux_decode_with_limits(d, DecodeLimits::default());
}

pub fn mux_decode_with_limits(d: &mut Design, limits: DecodeLimits) -> DecodeStatus {
    if d.aig.num_nodes() > limits.max_nodes {
        return DecodeStatus::BudgetReached;
    }
    let mut work = Work {
        used: 0,
        limit: limits.max_expansions,
    };
    let backup = d.clone();
    let aig = &backup.aig;
    let n = aig.num_nodes();
    let mut fanout = vec![0u32; n];
    for id in 1..n as u32 {
        if aig.is_and(id) {
            let (a, b) = aig.fanins(id);
            fanout[a.node() as usize] += 1;
            fanout[b.node() as usize] += 1;
        }
    }
    let roots = design_roots(&backup);
    for r in &roots {
        fanout[r.node() as usize] += 2;
    }
    let reader = Reader { aig, fanout };
    let cone = aig.cone(&roots);
    // Count shared signatures without retaining millions of candidate trees.
    let mut uses: FxHashMap<Signature, u32> = FxHashMap::default();
    for &id in cone.iter().rev() {
        if !aig.is_and(id) {
            continue;
        }
        let Ok((paths, _)) = work.tree(&reader, id) else {
            return DecodeStatus::BudgetReached;
        };
        if paths.len() < 3 {
            continue;
        }
        let sig = Signature::of(&paths);
        if let Some(count) = uses.get_mut(&sig) {
            *count += 1;
        } else {
            if uses.len() >= limits.max_signatures {
                return DecodeStatus::BudgetReached;
            }
            uses.insert(sig, 1);
        }
    }
    // Accept trees top-down; the inner nodes of an accepted tree are not trees of their own.
    let mut claimed = vec![false; n];
    let mut accepted = vec![false; n];
    let mut any_accepted = false;
    for &id in cone.iter().rev() {
        if claimed[id as usize] {
            continue;
        }
        if !aig.is_and(id) {
            continue;
        }
        let Ok((paths, inner)) = work.tree(&reader, id) else {
            return DecodeStatus::BudgetReached;
        };
        if paths.len() < 3 {
            continue;
        }
        let sig = Signature::of(&paths);
        if uses[&sig] < 2 {
            continue;
        }
        for &m in &inner {
            claimed[m as usize] = true;
        }
        accepted[id as usize] = true;
        any_accepted = true;
    }
    drop(uses);
    drop(claimed);
    if !any_accepted {
        return DecodeStatus::Unchanged;
    }
    let mut ng = Aig::new();
    ng.parallel_carry = aig.parallel_carry;
    let mut img = vec![Lit::FALSE; n];
    for &i in aig.inputs() {
        img[i as usize] = ng.input();
    }
    let im = |img: &[Lit], l: Lit| img[l.node() as usize].xor(l.is_compl());
    for &id in &cone {
        if !aig.is_and(id) {
            continue;
        }
        img[id as usize] = if accepted[id as usize] {
            let Ok((paths, _)) = work.tree(&reader, id) else {
                return DecodeStatus::BudgetReached;
            };
            // The complement of a tree is the tree of complemented leaves: build the sum of
            // products in the phase whose leaves are mostly regular literals.
            let compl = paths
                .iter()
                .filter(|(_, l)| !l.is_const() && l.is_compl())
                .count();
            let regular = paths
                .iter()
                .filter(|(_, l)| !l.is_const() && !l.is_compl())
                .count();
            let flip = compl > regular;
            // Equal leaves share one term: the OR of their path products.
            let mut groups: Vec<(Lit, Lit)> = Vec::new();
            for (sels, leaf) in &paths {
                let leaf = leaf.xor(flip);
                let mut product = Lit::TRUE;
                for &s in sels {
                    product = ng.and(product, im(&img, s));
                }
                let leaf = im(&img, leaf);
                match groups.iter_mut().find(|(l, _)| *l == leaf) {
                    Some((_, g)) => *g = ng.or(*g, product),
                    None => groups.push((leaf, product)),
                }
            }
            let mut terms: Vec<Lit> = groups
                .into_iter()
                .map(|(leaf, g)| ng.and(g, leaf))
                .filter(|&t| t != Lit::FALSE)
                .collect();
            while terms.len() > 1 {
                let mut next = Vec::with_capacity(terms.len().div_ceil(2));
                for pair in terms.chunks(2) {
                    next.push(if pair.len() == 2 {
                        ng.or(pair[0], pair[1])
                    } else {
                        pair[0]
                    });
                }
                terms = next;
            }
            terms.pop().unwrap_or(Lit::FALSE).xor(flip)
        } else {
            let (a, b) = aig.fanins(id);
            let (a, b) = (im(&img, a), im(&img, b));
            ng.and(a, b)
        };
        if ng.num_nodes() > limits.max_nodes {
            return DecodeStatus::BudgetReached;
        }
    }
    let new_roots: Vec<Lit> = roots.iter().map(|&r| im(&img, r)).collect();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in p.bits.iter_mut() {
            *b = im(&img, *b);
        }
    }
    for s in d.states.iter_mut() {
        s.q = im(&img, s.q);
    }
    d.aig = ng;
    set_roots(d, &new_roots);
    sweep(d);
    if d.aig.num_ands() > backup.aig.num_ands() {
        *d = backup;
        DecodeStatus::Unchanged
    } else {
        DecodeStatus::Rewritten
    }
}
