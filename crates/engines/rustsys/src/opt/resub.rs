// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `resub`: resubstitution (SYNTH-REQ-0411). Within a window of at most 8 leaves around a node,
//! the node is re-expressed as an existing node (or its complement), or as the AND or OR of two or
//! three existing nodes, when that frees more nodes of its fanout-free cone than it adds. Nodes are
//! visited in topological order; reference counts follow every planned replacement, so gains are
//! exact on the graph that results [@mishchenko2006scalable].

use std::sync::Mutex;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::{SmallVec, smallvec};

use super::refactor::cut_of_bounded;
use super::{design_roots, set_roots, sweep};
use crate::aig::{Aig, Lit};
use crate::elab::Design;

/// Leaves of a window.
const K: usize = 8;
/// Divisors considered per window.
const MAX_DIVS: usize = 150;
/// Candidate literals kept for the pair and triple searches.
const MAX_CAND: usize = 24;

/// Bounds on graph storage and unsuccessful side-fanout exploration. The
/// divisor count alone does not bound visits when most fanouts are unsuitable.
#[derive(Debug, Clone, Copy)]
pub struct ResubLimits {
    pub max_nodes: usize,
    pub max_fanout_visits_per_window: usize,
    pub max_window_nodes: usize,
    pub max_read_records: usize,
    pub max_proposal_bytes: usize,
    pub batch_nodes: usize,
    pub max_workers: usize,
}

impl Default for ResubLimits {
    fn default() -> Self {
        Self {
            max_nodes: 32_000_000,
            max_fanout_visits_per_window: 256,
            max_window_nodes: 8192,
            max_read_records: 65_536,
            max_proposal_bytes: 256 * 1024 * 1024,
            batch_nodes: 8192,
            max_workers: 16,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ResubStats {
    /// Planning attempts, including discarded speculation and ordered replays.
    pub windows: u64,
    pub fanout_visits: u64,
    pub bounded_windows: u64,
    pub node_budget_reached: bool,
    pub speculative_replays: u64,
}

impl ResubStats {
    fn add(&mut self, other: Self) {
        self.windows += other.windows;
        self.fanout_visits += other.fanout_visits;
        self.bounded_windows += other.bounded_windows;
    }
}

const REFS: u32 = 0;
const DEAD: u32 = 1;
const PLAN: u32 = 2;
const NODE_MASK: u32 = (1 << 30) - 1;

/// Per-batch changes are property-specific, so a fanout-count change does not
/// invalidate a candidate that only tested a divisor's liveness.
struct Changes {
    marks: Vec<u8>,
    touched: Vec<u32>,
}

impl Changes {
    fn new(n: usize) -> Self {
        Self {
            marks: vec![0; n],
            touched: Vec::new(),
        }
    }
    fn mark(&mut self, node: u32, property: u32) {
        let mark = &mut self.marks[node as usize];
        if *mark == 0 {
            self.touched.push(node);
        }
        *mark |= 1 << property;
    }
    fn intersects(&self, reads: &[u32]) -> bool {
        reads
            .iter()
            .any(|&key| self.marks[(key & NODE_MASK) as usize] & (1 << (key >> 30)) != 0)
    }
    fn clear(&mut self) {
        for node in self.touched.drain(..) {
            self.marks[node as usize] = 0;
        }
    }
}

#[derive(Default)]
struct Reads {
    records: Vec<u32>,
    count: usize,
    limit: usize,
    track: bool,
    exhausted: bool,
}

impl Reads {
    fn reset(&mut self, limit: usize, track: bool) {
        self.records.clear();
        self.count = 0;
        self.limit = limit.min(65_536);
        self.track = track;
        self.exhausted = false;
    }
    fn add(&mut self, node: u32, property: u32) {
        if self.count >= self.limit {
            self.exhausted = true;
            return;
        }
        self.count += 1;
        if self.track {
            self.records.push(node | (property << 30));
        }
    }
}

/// Truth table over up to 8 variables.
type T = [u64; 4];

const ALL: T = [u64::MAX; 4];

fn var_tt(v: usize) -> T {
    const M: [u64; 6] = [
        0xAAAA_AAAA_AAAA_AAAA,
        0xCCCC_CCCC_CCCC_CCCC,
        0xF0F0_F0F0_F0F0_F0F0,
        0xFF00_FF00_FF00_FF00,
        0xFFFF_0000_FFFF_0000,
        0xFFFF_FFFF_0000_0000,
    ];
    let mut t = [0u64; 4];
    for (w, x) in t.iter_mut().enumerate() {
        *x = if v < 6 {
            M[v]
        } else if (w >> (v - 6)) & 1 == 1 {
            u64::MAX
        } else {
            0
        };
    }
    t
}

fn and(a: &T, b: &T) -> T {
    [a[0] & b[0], a[1] & b[1], a[2] & b[2], a[3] & b[3]]
}

fn or(a: &T, b: &T) -> T {
    [a[0] | b[0], a[1] | b[1], a[2] | b[2], a[3] | b[3]]
}

fn not(a: &T) -> T {
    [!a[0], !a[1], !a[2], !a[3]]
}

/// True when `a` implies `b`.
fn implies(a: &T, b: &T) -> bool {
    (0..4).all(|i| a[i] & !b[i] == 0)
}

/// A planned replacement over literals of existing nodes.
#[derive(Debug, Clone)]
enum Repl {
    Lit(Lit),
    And(Vec<Lit>),
    Or(Vec<Lit>),
}

impl Repl {
    fn lits(&self) -> &[Lit] {
        match self {
            Repl::Lit(l) => std::slice::from_ref(l),
            Repl::And(v) | Repl::Or(v) => v,
        }
    }
}

struct State<'a> {
    aig: &'a Aig,
    refs: Vec<u32>,
    dead: Vec<bool>,
    plan: FxHashMap<u32, Repl>,
}

impl State<'_> {
    /// Current children of a node: the literals of its replacement, or its fanins.
    fn children(&self, n: u32) -> SmallVec<[u32; 3]> {
        match self.plan.get(&n) {
            Some(r) => r.lits().iter().map(|l| l.node()).collect(),
            None => {
                let (a, b) = self.aig.fanins(n);
                smallvec![a.node(), b.node()]
            }
        }
    }

    /// Replaces the implementation of `root` by `r`: references its literals, then releases the
    /// old implementation and marks what becomes unused as dead.
    fn apply(&mut self, root: u32, r: Repl, mut changes: Option<&mut Changes>) {
        for l in r.lits() {
            self.refs[l.node() as usize] += 1;
            if self.aig.is_and(l.node()) {
                if let Some(changes) = changes.as_deref_mut() {
                    changes.mark(l.node(), REFS);
                }
            }
        }
        let old = self.children(root);
        self.plan.insert(root, r);
        if let Some(changes) = changes.as_deref_mut() {
            changes.mark(root, PLAN);
        }
        let mut stack = old;
        while let Some(f) = stack.pop() {
            if !self.aig.is_and(f) {
                continue;
            }
            self.refs[f as usize] -= 1;
            if let Some(changes) = changes.as_deref_mut() {
                changes.mark(f, REFS);
            }
            if self.refs[f as usize] == 0 {
                self.dead[f as usize] = true;
                if let Some(changes) = changes.as_deref_mut() {
                    changes.mark(f, DEAD);
                }
                stack.extend(self.children(f));
            }
        }
    }
}

/// Literal of `d` (possibly complemented) with truth table `t`.
fn lit_of(d: u32, neg: bool) -> Lit {
    Lit::new(d, neg)
}

/// Best replacement of a function `f` from divisor truth tables, if it needs fewer than `budget`
/// new nodes.
fn find(f: &T, divs: &[(u32, T)], budget: usize) -> Option<Repl> {
    // Existing node.
    for (d, t) in divs {
        if t == f {
            return Some(Repl::Lit(lit_of(*d, false)));
        }
        if not(t) == *f {
            return Some(Repl::Lit(lit_of(*d, true)));
        }
    }
    if budget < 2 {
        return None;
    }
    // Literals that contain `f` (AND candidates) and literals contained in `f` (OR candidates).
    let mut pos: Vec<(Lit, T)> = Vec::new();
    let mut neg: Vec<(Lit, T)> = Vec::new();
    for (d, t) in divs {
        for inv in [false, true] {
            let tt = if inv { not(t) } else { *t };
            if tt == ALL || tt == [0; 4] {
                continue;
            }
            if implies(f, &tt) && pos.len() < MAX_CAND {
                pos.push((lit_of(*d, inv), tt));
            }
            if implies(&tt, f) && neg.len() < MAX_CAND {
                neg.push((lit_of(*d, inv), tt));
            }
        }
    }
    for i in 0..pos.len() {
        for j in i + 1..pos.len() {
            if and(&pos[i].1, &pos[j].1) == *f {
                return Some(Repl::And(vec![pos[i].0, pos[j].0]));
            }
        }
    }
    for i in 0..neg.len() {
        for j in i + 1..neg.len() {
            if or(&neg[i].1, &neg[j].1) == *f {
                return Some(Repl::Or(vec![neg[i].0, neg[j].0]));
            }
        }
    }
    if budget < 3 {
        return None;
    }
    for i in 0..pos.len() {
        for j in i + 1..pos.len() {
            let ij = and(&pos[i].1, &pos[j].1);
            for k in j + 1..pos.len() {
                if and(&ij, &pos[k].1) == *f {
                    return Some(Repl::And(vec![pos[i].0, pos[j].0, pos[k].0]));
                }
            }
        }
    }
    for i in 0..neg.len() {
        for j in i + 1..neg.len() {
            let ij = or(&neg[i].1, &neg[j].1);
            for k in j + 1..neg.len() {
                if or(&ij, &neg[k].1) == *f {
                    return Some(Repl::Or(vec![neg[i].0, neg[j].0, neg[k].0]));
                }
            }
        }
    }
    None
}

/// A batch retains at most 256 MiB of read records, divided among workers. The
/// shared graph/CSR is never cloned per worker. At most 16 scratch shards
/// each hold an 8192-node window/MFFC and its truth tables (<8 MiB/shard).
#[derive(Default)]
struct Scratch {
    remaining: FxHashMap<u32, u32>,
    stack: Vec<u32>,
    freed: FxHashSet<u32>,
    tt: FxHashMap<u32, T>,
    divs: Vec<u32>,
    seen: FxHashSet<u32>,
    dts: Vec<(u32, T)>,
    reads: Reads,
    proposal_bytes_left: usize,
}

struct Proposal {
    replacement: Option<Repl>,
    reads: Box<[u32]>,
    replay_required: bool,
    stats: ResubStats,
}

impl Scratch {
    fn mffc(&mut self, st: &State<'_>, root: u32, limit: usize) -> bool {
        self.remaining.clear();
        self.stack.clear();
        self.freed.clear();
        if limit == 0 {
            return false;
        }
        self.freed.insert(root);
        self.stack.push(root);
        while let Some(node) = self.stack.pop() {
            self.reads.add(node, PLAN);
            for f in st.children(node) {
                if !st.aig.is_and(f) {
                    continue;
                }
                self.reads.add(f, REFS);
                if self.remaining.len() >= limit.saturating_mul(3) {
                    return false;
                }
                let count = self.remaining.entry(f).or_insert(st.refs[f as usize]);
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

    fn finish(
        &mut self,
        replacement: Option<Repl>,
        mut stats: ResubStats,
        bounded: bool,
    ) -> Proposal {
        self.reads.records.sort_unstable();
        self.reads.records.dedup();
        let bytes = self.reads.records.len() * size_of::<u32>();
        let spill = self.reads.track && bytes > self.proposal_bytes_left;
        stats.bounded_windows += u64::from(bounded || self.reads.exhausted || spill);
        let reads = if spill {
            Vec::new()
        } else {
            if self.reads.track {
                self.proposal_bytes_left -= bytes;
            }
            self.reads.records.clone()
        };
        Proposal {
            replacement: if self.reads.exhausted {
                None
            } else {
                replacement
            },
            reads: reads.into_boxed_slice(),
            replay_required: self.reads.exhausted || spill,
            stats,
        }
    }
}

struct Context<'a> {
    aig: &'a Aig,
    in_cone: &'a [bool],
    levels: &'a [u32],
    offsets: &'a [usize],
    fanouts: &'a [u32],
    limits: ResubLimits,
}

impl Context<'_> {
    fn usable(
        &self,
        st: &State<'_>,
        x: u32,
        root: u32,
        freed: &FxHashSet<u32>,
        reads: &mut Reads,
    ) -> bool {
        if x >= root {
            return false;
        }
        // Input liveness/plans never change; their fanout counters are not
        // consulted by an MFFC, so they need no speculative dependency record.
        if self.aig.is_and(x) {
            reads.add(x, DEAD);
        }
        !st.dead[x as usize] && !freed.contains(&x) && self.in_cone[x as usize]
    }

    fn prepare(&self, st: &State<'_>, id: u32, scratch: &mut Scratch, track: bool) -> Proposal {
        scratch.reads.reset(self.limits.max_read_records, track);
        let mut stats = ResubStats::default();
        scratch.reads.add(id, DEAD);
        scratch.reads.add(id, REFS);
        if st.dead[id as usize] || st.refs[id as usize] == 0 {
            return scratch.finish(None, stats, false);
        }
        let Some((leaves, window)) = cut_of_bounded(
            self.aig,
            id,
            self.levels,
            K,
            self.limits.max_window_nodes.min(8192),
        ) else {
            return scratch.finish(None, stats, true);
        };
        if leaves.len() < 2 {
            return scratch.finish(None, stats, false);
        }
        stats.windows = 1;
        if !scratch.mffc(st, id, self.limits.max_window_nodes.min(8192)) {
            return scratch.finish(None, stats, true);
        }
        let Scratch {
            freed,
            tt,
            divs,
            seen,
            dts,
            reads,
            ..
        } = scratch;
        tt.clear();
        tt.insert(0, [0; 4]);
        for (i, &leaf) in leaves.iter().enumerate() {
            tt.insert(leaf, var_tt(i));
        }
        let eval = |tt: &FxHashMap<u32, T>, x: u32| -> Option<T> {
            let (a, b) = self.aig.fanins(x);
            let ta = tt.get(&a.node())?;
            let tb = tt.get(&b.node())?;
            let ta = if a.is_compl() { not(ta) } else { *ta };
            let tb = if b.is_compl() { not(tb) } else { *tb };
            Some(and(&ta, &tb))
        };
        for &node in &window {
            if let Some(t) = eval(tt, node) {
                tt.insert(node, t);
            }
        }
        let Some(f) = tt.get(&id).copied() else {
            return scratch.finish(None, stats, true);
        };
        divs.clear();
        seen.clear();
        for &x in leaves.iter().chain(window.iter()) {
            if x != 0 && self.usable(st, x, id, freed, reads) {
                divs.push(x);
            }
        }
        seen.extend(divs.iter().copied());
        let mut i = 0;
        let mut visits = 0;
        let mut bounded = false;
        let visit_limit = self.limits.max_fanout_visits_per_window.min(4096);
        while i < divs.len() && divs.len() < MAX_DIVS {
            if visits >= visit_limit {
                bounded = true;
                break;
            }
            let x = divs[i];
            i += 1;
            let row = &self.fanouts[self.offsets[x as usize]..self.offsets[x as usize + 1]];
            let end = row.partition_point(|&fo| fo < id);
            let start = end.saturating_sub(visit_limit - visits);
            bounded |= start > 0;
            for &fo in &row[start..end] {
                if divs.len() >= MAX_DIVS {
                    break;
                }
                visits += 1;
                if seen.contains(&fo) || !self.usable(st, fo, id, freed, reads) {
                    continue;
                }
                reads.add(fo, PLAN);
                if st.plan.contains_key(&fo) {
                    continue;
                }
                if let Some(t) = eval(tt, fo) {
                    tt.insert(fo, t);
                    seen.insert(fo);
                    divs.push(fo);
                }
            }
        }
        stats.fanout_visits = visits as u64;
        dts.clear();
        dts.extend(divs.iter().map(|x| (*x, tt[x])));
        let replacement = find(&f, dts, freed.len()).filter(|r| {
            let cost = match r {
                Repl::Lit(_) => 0,
                Repl::And(v) | Repl::Or(v) => v.len() - 1,
            };
            cost < freed.len()
        });
        scratch.finish(replacement, stats, bounded)
    }
}

/// One resubstitution pass over the whole design.
pub fn resub(d: &mut Design) {
    let _ = resub_with_limits(d, ResubLimits::default());
}

/// Linear CSR fanout storage replaces millions of separately allocated Vecs.
/// At 15M nodes: refs/offsets/edges <=300 MiB, temporary cursors <=120 MiB;
/// each window has <=150 divisors, <=24 pair/triple literals, and a configurable
/// cap on every attempted side-fanout visit. No simple paths are enumerated.
pub fn resub_with_limits(d: &mut Design, limits: ResubLimits) -> ResubStats {
    let mut stats = ResubStats::default();
    if d.aig.num_nodes() > limits.max_nodes.min(32_000_000) {
        stats.node_budget_reached = true;
        return stats;
    }
    let roots = design_roots(d);
    let aig = &d.aig;
    let n = aig.num_nodes();
    let cone = aig.cone(&roots);
    let mut in_cone = vec![false; n];
    for id in &cone {
        in_cone[*id as usize] = true;
    }
    let mut refs = vec![0u32; n];
    for &id in &cone {
        if aig.is_and(id) {
            let (a, b) = aig.fanins(id);
            for f in [a.node(), b.node()] {
                refs[f as usize] += 1;
            }
        }
    }
    // cone is in increasing node-id order, hence each CSR row is sorted.
    let mut offsets = Vec::with_capacity(n + 1);
    offsets.push(0usize);
    for &count in &refs {
        offsets.push(offsets.last().copied().unwrap() + count as usize);
    }
    let mut fanouts = vec![0u32; offsets[n]];
    let mut cursor = offsets[..n].to_vec();
    for &id in &cone {
        if aig.is_and(id) {
            let (a, b) = aig.fanins(id);
            for f in [a.node(), b.node()] {
                let at = &mut cursor[f as usize];
                fanouts[*at] = id;
                *at += 1;
            }
        }
    }
    drop(cursor);
    for r in &roots {
        refs[r.node() as usize] += 1;
    }
    let levels = aig.levels();
    let mut st = State {
        aig,
        refs,
        dead: vec![false; n],
        plan: FxHashMap::default(),
    };
    let context = Context {
        aig,
        in_cone: &in_cone,
        levels: &levels,
        offsets: &offsets,
        fanouts: &fanouts,
        limits,
    };
    let count = rayon::current_num_threads().min(limits.max_workers.clamp(1, 16));
    let mut workers: Vec<Mutex<Scratch>> = (0..count).map(|_| Mutex::default()).collect();
    if count == 1 {
        let scratch = workers[0].get_mut().expect("worker cache poisoned");
        for &id in &cone {
            if !aig.is_and(id) {
                continue;
            }
            let proposal = context.prepare(&st, id, scratch, false);
            stats.add(proposal.stats);
            if let Some(replacement) = proposal.replacement {
                st.apply(id, replacement, None);
            }
        }
    } else {
        let mut changes = Changes::new(n);
        for batch in cone.chunks(limits.batch_nodes.clamp(1, 8192)) {
            // Sharded quotas avoid a contended per-node atomic counter. When
            // a quota fills, the candidate is replayed serially without a log.
            let quota = limits.max_proposal_bytes.min(256 * 1024 * 1024) / count;
            for worker in &mut workers {
                worker
                    .get_mut()
                    .expect("worker cache poisoned")
                    .proposal_bytes_left = quota;
            }
            let ids: Vec<_> = batch.iter().copied().filter(|&id| aig.is_and(id)).collect();
            let proposals: Vec<Vec<_>> = ids
                .par_chunks(32)
                .map(|chunk| {
                    let index = rayon::current_thread_index().unwrap_or(0) % workers.len();
                    let mut scratch = workers[index].lock().expect("worker cache poisoned");
                    chunk
                        .iter()
                        .map(|&id| context.prepare(&st, id, &mut scratch, true))
                        .collect()
                })
                .collect();
            for (id, mut proposal) in ids.into_iter().zip(proposals.into_iter().flatten()) {
                stats.add(proposal.stats);
                if proposal.replay_required || changes.intersects(&proposal.reads) {
                    // Only this ordered thread mutates State. Recompute every
                    // candidate whose observed refs/liveness/plan changed,
                    // including negative candidates, exactly as a serial pass.
                    stats.speculative_replays += 1;
                    let scratch = workers[0].get_mut().expect("worker cache poisoned");
                    proposal = context.prepare(&st, id, scratch, false);
                    stats.add(proposal.stats);
                }
                if let Some(replacement) = proposal.replacement {
                    st.apply(id, replacement, Some(&mut changes));
                }
            }
            changes.clear();
        }
    }
    drop(workers);
    drop(in_cone);
    drop(levels);
    drop(offsets);
    drop(fanouts);
    if st.plan.is_empty() {
        return stats;
    }
    let plan = st.plan;
    drop(st.refs);
    drop(st.dead);
    let backup = d.clone();
    let aig = &backup.aig;
    let mut ng = Aig::new();
    let mut img = vec![Lit::FALSE; n];
    for &i in aig.inputs() {
        img[i as usize] = ng.input();
    }
    let get = |img: &[Lit], l: Lit| img[l.node() as usize].xor(l.is_compl());
    for &id in &cone {
        if !aig.is_and(id) {
            continue;
        }
        img[id as usize] = match plan.get(&id) {
            Some(Repl::Lit(l)) => get(&img, *l),
            Some(Repl::And(ls)) => {
                let mut acc = Lit::TRUE;
                for l in ls {
                    acc = ng.and(acc, get(&img, *l));
                }
                acc
            }
            Some(Repl::Or(ls)) => {
                let mut acc = Lit::TRUE;
                for l in ls {
                    acc = ng.and(acc, !get(&img, *l));
                }
                !acc
            }
            None => {
                let (a, b) = aig.fanins(id);
                ng.and(get(&img, a), get(&img, b))
            }
        };
    }
    let new_roots: Vec<Lit> = roots.iter().map(|r| get(&img, *r)).collect();
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
    if d.aig.num_ands() >= backup.aig.num_ands() {
        *d = backup;
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_or_and_existing_nodes() {
        let (a, b, c) = (var_tt(0), var_tt(1), var_tt(2));
        let divs = vec![(1, a), (2, b), (3, c)];
        assert!(matches!(find(&a, &divs, 1), Some(Repl::Lit(l)) if l == Lit::new(1, false)));
        assert!(matches!(find(&not(&b), &divs, 1), Some(Repl::Lit(l)) if l == Lit::new(2, true)));
        assert!(matches!(
            find(&and(&a, &not(&b)), &divs, 2),
            Some(Repl::And(_))
        ));
        assert!(matches!(find(&or(&a, &c), &divs, 2), Some(Repl::Or(_))));
        assert!(
            matches!(find(&and(&and(&a, &b), &c), &divs, 3), Some(Repl::And(v)) if v.len() == 3)
        );
        // Not worth it within the budget.
        assert!(find(&and(&a, &b), &divs, 1).is_none());
    }
}
