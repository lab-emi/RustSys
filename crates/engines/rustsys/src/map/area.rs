// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Exact-local-area recovery (SYNTH-REQ-0512): with reference counts of the current cover, every
//! used (node, phase) is re-implemented by the candidate that adds the least area when its
//! exclusively owned logic is counted exactly, subject to the required time
//! [@manohararajah2006heuristics; @mishchenko2007combinational].
//!
//! The pass visits the nodes in topological order and runs level by level (SYNTH-REQ-0517). A
//! node's reference walks reach only the node itself and lower levels, so the nodes of a level
//! are decided speculatively in parallel, each against the counts at the start of the level on
//! its own overlay of changed counts. The decisions are committed in level order; a decision that
//! read a count changed by an earlier node of the level is recomputed on the shared counts. The
//! result is that of the sequential order, whatever the thread count.

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use rusteda_core::job::{Cancelled, Ctx};

use super::library::ARR_INF;
use super::mapper::{Choice, Impl, Mapped, Mapper, NONE, add_arr};

/// Scratch is private to at most16 workers; retained proposals are budgeted per worker batch.
#[derive(Debug, Clone, Copy)]
pub struct ExactLimits {
    pub batch_nodes: usize,
    pub max_workers: usize,
    pub max_overlay_nodes: usize,
    pub max_plan_bytes: usize,
}

impl ExactLimits {
    pub const DEFAULT: Self = Self {
        batch_nodes: 65_536,
        max_workers: 16,
        max_overlay_nodes: 32768,
        max_plan_bytes: 256 * 1024 * 1024,
    };
}
impl Default for ExactLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Default)]
pub(super) struct ExactStats {
    pub plan_s: f64,
    pub commit_s: f64,
    pub replay_s: f64,
    pub replays: u64,
    pub spilled: u64,
}

#[derive(Clone, Copy)]
struct Read {
    node: u32,
    low: [u32; 2],
    high: [u32; 2],
    delta: [i64; 2],
}

/// Reference counts per (node, phase) of the current cover.
trait RefCounts {
    /// Adds one to the count of `(c, p)` (or subtracts one) and returns the new count.
    fn bump(&mut self, c: u32, p: usize, up: bool) -> Result<u32, ()>;
    fn get(&self, c: u32, p: usize) -> u32;
}

impl RefCounts for [[u32; 2]] {
    fn bump(&mut self, c: u32, p: usize, up: bool) -> Result<u32, ()> {
        let r = &mut self[c as usize][p];
        if up {
            *r += 1;
        } else {
            debug_assert!(*r > 0, "reference count underflow");
            *r -= 1;
        }
        Ok(*r)
    }

    fn get(&self, c: u32, p: usize) -> u32 {
        self[c as usize][p]
    }
}

/// The counts seen by one node of a level: the counts at the start of the level plus the
/// changes of this node's own walks. Per changed node: the counts and, per phase, the least
/// count after any change (`u32::MAX` while that phase is unchanged).
struct Overlay<'r> {
    base: &'r [[u32; 2]],
    changed: &'r mut FxHashMap<u32, ([u32; 2], [u32; 2], [u32; 2])>,
    limit: usize,
}

impl RefCounts for Overlay<'_> {
    fn bump(&mut self, c: u32, p: usize, up: bool) -> Result<u32, ()> {
        if self.changed.len() >= self.limit && !self.changed.contains_key(&c) {
            return Err(());
        }
        let base = self.base;
        let (r, low, high) = self
            .changed
            .entry(c)
            .or_insert_with(|| (base[c as usize], [u32::MAX; 2], [0; 2]));
        if up {
            r[p] += 1;
        } else {
            debug_assert!(r[p] > 0, "reference count underflow");
            r[p] -= 1;
        }
        low[p] = low[p].min(r[p]);
        high[p] = high[p].max(r[p]);
        Ok(r[p])
    }

    fn get(&self, c: u32, p: usize) -> u32 {
        self.changed
            .get(&c)
            .map_or(self.base[c as usize][p], |r| r.0[p])
    }
}

/// The shared counts, keeping the count at the start of the level of every node changed during
/// the level (`epoch`).
struct Tracked<'r> {
    refs: &'r mut [[u32; 2]],
    stamp: &'r mut [u32],
    start: &'r mut [[u32; 2]],
    epoch: u32,
}

impl RefCounts for Tracked<'_> {
    fn bump(&mut self, c: u32, p: usize, up: bool) -> Result<u32, ()> {
        if self.stamp[c as usize] != self.epoch {
            self.stamp[c as usize] = self.epoch;
            self.start[c as usize] = self.refs[c as usize];
        }
        self.refs.bump(c, p, up)
    }

    fn get(&self, c: u32, p: usize) -> u32 {
        RefCounts::get(&*self.refs, c, p)
    }
}

impl Tracked<'_> {
    fn apply(&mut self, read: &Read) {
        let c = read.node as usize;
        if read.delta == [0; 2] {
            return;
        }
        if self.stamp[c] != self.epoch {
            self.stamp[c] = self.epoch;
            self.start[c] = self.refs[c];
        }
        for p in 0..2 {
            let value = i64::from(self.refs[c][p]) + read.delta[p];
            self.refs[c][p] = u32::try_from(value).expect("validated reference-count delta");
        }
    }
    /// A load-price calculation depends on reference counts beyond the 0/1 boundary.
    fn changed_counts(&self, read: &[Read], fanout: u32) -> bool {
        read.iter().any(|r| {
            let c = r.node as usize;
            self.stamp[c] == self.epoch
                && (0..2).any(|p| {
                    let (was, now) = (self.start[c][p], self.refs[c][p]);
                    if r.low[p] == u32::MAX || was == now {
                        return false;
                    }
                    let low = i64::from(r.low[p].min(was));
                    let high = i64::from(r.high[p].max(was));
                    let delta = i64::from(now) - i64::from(was);
                    let shifted = low + delta;
                    if shifted < 0 {
                        return true;
                    }
                    let f = i64::from(fanout);
                    let step = f - 1;
                    // Cost uses changes of ceil(max(refs-F,0)/(F-1)). A shift preserves
                    // every charge if both ranges lie in one bucket, or have equal residues
                    // wholly above the clamp. Branch consistency is checked by stale().
                    let debt = |n: i64| ((n - f).max(0) + step - 1) / step;
                    let constant = debt(low) == debt(high) && debt(shifted) == debt(high + delta);
                    let periodic = low.min(shifted) >= f && delta % step == 0;
                    !constant && !periodic
                })
        })
    }
    /// Whether a decision whose walks changed the counts as `read` records (per node: the least
    /// count after a change, per phase) could differ on the current counts from the counts at
    /// the start of the level. It cannot when every count it changed is unchanged since the
    /// start of the level or stays at 2 or more after every change in both cases: the walks
    /// branch only on a count reaching 0 or 1, so they then branch alike.
    fn stale(&self, read: &[Read]) -> bool {
        read.iter().any(|r| {
            let c = r.node as usize;
            self.stamp[c] == self.epoch
                && (0..2).any(|p| {
                    let (was, now) = (self.start[c][p], self.refs[c][p]);
                    // Stale unless `low - was + min(was, now) >= 2`.
                    was != now
                        && r.low[p] != u32::MAX
                        && u64::from(r.low[p]) + u64::from(was.min(now)) < 2 + u64::from(was)
                })
        })
    }
}

/// The choices of the node being decided, which may differ from the mapper's until they are
/// applied.
struct Own {
    id: u32,
    best: [Choice; 2],
}

/// Reusable walk buffers.
#[derive(Default)]
struct Walk {
    stack: Vec<(u32, bool, Impl)>,
    kids: Vec<(u32, bool)>,
}

impl Mapper<'_> {
    /// A branching buffer adds at most L-1 sink slots. Charge whole buffers, including
    /// the first excess sink; a fractional lower bound underprices many mildly overloaded
    /// nets. This remains a proxy: actual pin limits and timing require legalization.
    fn repair_debt(&self, node: u32, refs: u32) -> u64 {
        if self.repair_price == 0
            || self
                .aig
                .input_of(node)
                .is_some_and(|i| (i as usize) < self.primary_inputs)
        {
            return 0;
        }
        u64::from(
            refs.saturating_sub(self.repair_fanout)
                .div_ceil(self.repair_fanout - 1),
        ) * self.repair_price
    }
    fn area_of(&self, imp: Impl) -> u64 {
        match imp {
            Impl::Cell { m, .. } => u64::from(self.lib.comb[m.cell as usize].area_q),
            Impl::Inv => u64::from(self.a_inv),
            _ => 0,
        }
    }

    fn choice(&self, own: &Own, c: u32, p: usize) -> Choice {
        if c == own.id {
            own.best[p]
        } else {
            self.best[c as usize][p]
        }
    }

    /// Inputs (node, phase) of an implementation of `(id, ph)`.
    #[inline]
    fn children(&self, id: u32, ph: bool, imp: Impl, out: &mut Vec<(u32, bool)>) {
        out.clear();
        match imp {
            Impl::Inv => out.push((id, !ph)),
            Impl::Cell { cut, m } => {
                let c = &self.cuts[id as usize][cut as usize];
                let cell = &self.lib.comb[m.cell as usize];
                for i in 0..cell.n_inputs as usize {
                    out.push((c.leaves[m.perm[i] as usize], (m.phase >> i) & 1 == 1));
                }
            }
            _ => {}
        }
    }

    /// Arrival time of `imp` for `(id, ph)` from the current arrivals of its inputs.
    #[inline]
    fn arrival(&self, own: &Own, id: u32, ph: bool, imp: Impl) -> i32 {
        match imp {
            Impl::Inv => add_arr(self.choice(own, id, usize::from(!ph)).arr, self.d_inv),
            Impl::Cell { cut, m } => {
                let c = &self.cuts[id as usize][cut as usize];
                let cell = &self.lib.comb[m.cell as usize];
                let mut arr = 0i32;
                for i in 0..cell.n_inputs as usize {
                    let leaf = c.leaves[m.perm[i] as usize];
                    let lp = usize::from((m.phase >> i) & 1 == 1);
                    arr = arr.max(add_arr(self.choice(own, leaf, lp).arr, cell.delay_arr[i]));
                }
                arr
            }
            Impl::Input | Impl::FreeNeg => self.launches[id as usize],
            Impl::None => ARR_INF,
        }
    }

    /// References the inputs of `imp` (implementing `(id, ph)`) and, transitively, every input
    /// that becomes used (`up`), or the inverse; returns the area of `imp` plus the area brought
    /// in or freed.
    #[allow(clippy::too_many_arguments)]
    fn walk<R: RefCounts + ?Sized>(
        &self,
        own: &Own,
        refs: &mut R,
        w: &mut Walk,
        id: u32,
        ph: bool,
        imp: Impl,
        up: bool,
    ) -> Result<u64, ()> {
        let mut area = 0u64;
        w.stack.clear();
        w.stack.push((id, ph, imp));
        let edge = u32::from(up);
        while let Some((i, p, im)) = w.stack.pop() {
            area += self.area_of(im);
            self.children(i, p, im, &mut w.kids);
            for &(c, cp) in &w.kids {
                let q = usize::from(cp);
                let before = if self.repair_price > 0 {
                    refs.get(c, q)
                } else {
                    0
                };
                let after = refs.bump(c, q, up)?;
                if self.repair_price > 0 {
                    area += self.repair_debt(c, before.max(after))
                        - self.repair_debt(c, before.min(after));
                }
                if after == edge {
                    w.stack.push((c, cp, self.choice(own, c, q).imp));
                }
            }
        }
        Ok(area)
    }

    /// Decides phase `ph` of `own` under the counts `refs`, which are updated to the new choice.
    fn exact_phase<R: RefCounts + ?Sized>(
        &self,
        own: &mut Own,
        refs: &mut R,
        w: &mut Walk,
        ph: bool,
        req: &[[i32; 2]],
    ) -> Result<(), ()> {
        let id = own.id;
        let p = usize::from(ph);
        if refs.get(id, p) == 0 {
            // Not in the cover: only keep its arrival current.
            own.best[p].arr = self.arrival(own, id, ph, own.best[p].imp);
            return Ok(());
        }
        let cur = own.best[p];
        self.walk(own, refs, w, id, ph, cur.imp, false)?;
        // An inverter of the other phase that reads this one (possibly chosen just now) must
        // meet the other phase's required time too.
        let mut limit = req[id as usize][p];
        let q_req = req[id as usize][1 - p];
        if matches!(own.best[1 - p].imp, Impl::Inv) && q_req != ARR_INF {
            limit = limit.min(q_req.saturating_sub(self.d_inv));
        }
        // (area, arrival, choice) of the best feasible and the fastest candidate.
        let mut feasible: Option<(u64, i32, Choice)> = None;
        let mut fastest: Option<(i32, Choice)> = None;
        let consider = |feasible: &mut Option<(u64, i32, Choice)>,
                        fastest: &mut Option<(i32, Choice)>,
                        area: u64,
                        ch: Choice| {
            if ch.arr <= limit
                && feasible
                    .as_ref()
                    .is_none_or(|(a, t, _)| area < *a || (area == *a && ch.arr < *t))
            {
                *feasible = Some((area, ch.arr, ch));
            }
            if fastest.as_ref().is_none_or(|(t, _)| ch.arr < *t) {
                *fastest = Some((ch.arr, ch));
            }
        };
        for (ci, c) in self.cuts[id as usize].iter().enumerate() {
            if c.is_trivial_of(id) {
                continue;
            }
            let tt = if ph { !c.tt } else { c.tt };
            // A drive variant or pin permutation with the same leaf polarities references
            // exactly the same subcover. Walk it once per mask, then add each cell's area.
            // The walks restore counts, and the first walk records all overlay dependencies.
            let mut leaf_cost = [u64::MAX; 64];
            for m in self.lib.matches(c.n as usize, tt) {
                let mask = m.leaf_phase as usize;
                let own_area = u64::from(self.lib.comb[m.cell as usize].area_q);
                if let Some((best_a, _, _)) = feasible
                    && leaf_cost[mask] != u64::MAX
                    && leaf_cost[mask] + own_area > best_a
                {
                    continue;
                }
                let imp = Impl::Cell {
                    cut: ci as u16,
                    m: *m,
                };
                let arr = self.arrival(own, id, ph, imp);
                if arr == ARR_INF {
                    continue;
                }
                if arr > limit {
                    // Infeasible choices compete only for the fastest fallback. Their
                    // reference-count area cannot affect that decision, so do not walk it.
                    consider(
                        &mut feasible,
                        &mut fastest,
                        0,
                        Choice {
                            imp,
                            arr,
                            af: cur.af,
                        },
                    );
                    continue;
                }
                if leaf_cost[mask] == u64::MAX {
                    leaf_cost[mask] = self.walk(own, refs, w, id, ph, imp, true)? - own_area;
                    self.walk(own, refs, w, id, ph, imp, false)?;
                }
                let a = leaf_cost[mask] + own_area;
                #[cfg(test)]
                {
                    let uncached = self.walk(own, refs, w, id, ph, imp, true)?;
                    self.walk(own, refs, w, id, ph, imp, false)?;
                    assert_eq!(a, uncached, "cached subcover cost");
                }
                consider(
                    &mut feasible,
                    &mut fastest,
                    a,
                    Choice {
                        imp,
                        arr,
                        af: cur.af,
                    },
                );
            }
        }
        // Inverter from the other phase, unless that phase is itself an inverter; the other
        // phase's arrival is taken from the current arrivals of its inputs.
        let other = own.best[1 - p];
        let other_arr = self.arrival(own, id, !ph, other.imp);
        if other_arr != ARR_INF && !matches!(other.imp, Impl::Inv) {
            let arr = add_arr(other_arr, self.d_inv);
            let area = if arr <= limit {
                let cost = self.walk(own, refs, w, id, ph, Impl::Inv, true)?;
                self.walk(own, refs, w, id, ph, Impl::Inv, false)?;
                cost
            } else {
                0
            };
            consider(
                &mut feasible,
                &mut fastest,
                area,
                Choice {
                    imp: Impl::Inv,
                    arr,
                    af: cur.af,
                },
            );
        }
        let chosen = feasible
            .map(|(_, _, c)| c)
            .or(fastest.map(|(_, c)| c))
            .unwrap_or(NONE);
        if !matches!(chosen.imp, Impl::None) {
            own.best[p] = chosen;
        }
        self.walk(own, refs, w, id, ph, own.best[p].imp, true)?;
        Ok(())
    }

    /// Both phases of node `id` decided under the counts `refs`.
    /// Recovers a phase pair together. An individually cheap inverter can otherwise prevent
    /// replacing its expensive source by a cheaper implementation of the opposite phase.
    fn exact_pair<R: RefCounts + ?Sized>(
        &self,
        own: &mut Own,
        refs: &mut R,
        w: &mut Walk,
        req: &[[i32; 2]],
    ) -> Result<(), ()> {
        let id = own.id;
        let external: [u32; 2] = std::array::from_fn(|p| {
            refs.get(id, p)
                - u32::from(refs.get(id, 1 - p) > 0 && matches!(own.best[1 - p].imp, Impl::Inv))
        });
        // Both polarities must have external users. This also ensures that pair changes
        // cannot recursively delete the partner root during sequential or parallel commit.
        if external.contains(&0) {
            return Ok(());
        }
        let old = own.best;
        let order = if matches!(old[0].imp, Impl::Inv) {
            [0, 1]
        } else {
            [1, 0]
        };
        let mut best_cost = 0;
        for p in order {
            best_cost += self.walk(own, refs, w, id, p == 1, old[p].imp, false)?;
        }
        let mut best = old;
        let mut best_arr = old[0].arr.max(old[1].arr);
        for p in 0..2 {
            let extra = self.repair_debt(id, external[p] + 1) - self.repair_debt(id, external[p]);
            for (ci, c) in self.cuts[id as usize].iter().enumerate() {
                if c.is_trivial_of(id) {
                    continue;
                }
                let tt = if p == 1 { !c.tt } else { c.tt };
                let mut leaf_cost = [u64::MAX; 64];
                for m in self.lib.matches(c.n as usize, tt) {
                    let mask = m.leaf_phase as usize;
                    let area = u64::from(self.lib.comb[m.cell as usize].area_q);
                    if leaf_cost[mask] != u64::MAX
                        && leaf_cost[mask] + area + u64::from(self.a_inv) + extra > best_cost
                    {
                        continue;
                    }
                    let imp = Impl::Cell {
                        cut: ci as u16,
                        m: *m,
                    };
                    let arr = self.arrival(own, id, p == 1, imp);
                    let inv_arr = add_arr(arr, self.d_inv);
                    if arr == ARR_INF
                        || arr > req[id as usize][p]
                        || inv_arr > req[id as usize][1 - p]
                    {
                        continue;
                    }
                    if leaf_cost[mask] == u64::MAX {
                        leaf_cost[mask] = self.walk(own, refs, w, id, p == 1, imp, true)? - area;
                        self.walk(own, refs, w, id, p == 1, imp, false)?;
                    }
                    let cost = leaf_cost[mask] + area + u64::from(self.a_inv) + extra;
                    if cost < best_cost || (cost == best_cost && inv_arr < best_arr) {
                        best = old;
                        best[p] = Choice {
                            imp,
                            arr,
                            af: old[p].af,
                        };
                        best[1 - p] = Choice {
                            imp: Impl::Inv,
                            arr: inv_arr,
                            af: old[1 - p].af,
                        };
                        best_cost = cost;
                        best_arr = inv_arr;
                    }
                }
            }
        }
        own.best = best;
        let order = if matches!(best[0].imp, Impl::Inv) {
            [1, 0]
        } else {
            [0, 1]
        };
        for p in order {
            self.walk(own, refs, w, id, p == 1, best[p].imp, true)?;
        }
        debug_assert!(
            (0..2).all(|p| refs.get(id, p)
                == external[p] + u32::from(matches!(best[1 - p].imp, Impl::Inv)))
        );
        Ok(())
    }

    /// Both phases of node `id` decided under the counts `refs`.
    fn exact_node<R: RefCounts + ?Sized>(
        &self,
        id: u32,
        refs: &mut R,
        w: &mut Walk,
        req: &[[i32; 2]],
    ) -> Result<[Choice; 2], ()> {
        let mut own = Own {
            id,
            best: self.best[id as usize],
        };
        for ph in [false, true] {
            self.exact_phase(&mut own, refs, w, ph, req)?;
        }
        self.exact_pair(&mut own, refs, w, req)?;
        // An inverter phase follows the arrival of the phase it reads.
        for q in 0..2usize {
            if matches!(own.best[q].imp, Impl::Inv) {
                own.best[q].arr = add_arr(own.best[1 - q].arr, self.d_inv);
            }
        }
        Ok(own.best)
    }

    /// Reference counts of every (node, phase) in the cover: gate inputs and outputs.
    fn cover_refs(&self, mapped: &Mapped) -> Vec<[u32; 2]> {
        if let Some(refs) = &self.current_refs {
            return refs.clone();
        }
        let mut refs = vec![[0u32; 2]; self.aig.num_nodes()];
        for g in &mapped.gates {
            for &(l, p) in &g.ins {
                refs[l as usize][usize::from(p)] += 1;
            }
        }
        for r in self.roots {
            if !r.is_const() {
                refs[r.node() as usize][usize::from(r.is_compl())] += 1;
            }
        }
        refs
    }

    /// Reference for `exact_area` in tests: the nodes in level order, each decided and committed
    /// on the shared counts before the next one (SYNTH-REQ-0517).
    #[cfg(test)]
    pub(super) fn exact_area_one_by_one(&mut self, mapped: &Mapped, req: &[[i32; 2]]) {
        let mut refs = self.cover_refs(mapped);
        let mut w = Walk::default();
        let levels = std::mem::take(&mut self.levels);
        for &id in levels.iter().flatten() {
            let b = self
                .exact_node(id, &mut refs[..], &mut w, req)
                .expect("shared counts do not spill");
            self.best[id as usize] = b;
        }
        self.levels = levels;
    }

    /// One exact-local-area pass in topological order (SYNTH-REQ-0512), computed level by level
    /// (SYNTH-REQ-0517).
    pub(super) fn exact_area(
        &mut self,
        mapped: &Mapped,
        req: &[[i32; 2]],
        ctx: Ctx<'_>,
    ) -> Result<ExactStats, Cancelled> {
        let n = self.aig.num_nodes();
        let mut stats = ExactStats::default();
        let mut refs = self.cover_refs(mapped);
        let levels = std::mem::take(&mut self.levels);
        let mut stamp = vec![0u32; n];
        let mut start = vec![[0u32; 2]; n];
        let mut w = Walk::default();
        let total = levels.len() as u64;
        let limits = self.exact_limits;
        let workers = rayon::current_num_threads().min(limits.max_workers.clamp(1, 16));
        let retained = limits.max_plan_bytes / workers / 2;
        // Hash-table spare capacity plus a geometric-capacity walk stack for both phases.
        let overlay_limit = limits
            .max_overlay_nodes
            .min(32_768)
            .min(retained / (64 + 4 * size_of::<(u32, bool, Impl)>()));
        type Scratch = (FxHashMap<u32, ([u32; 2], [u32; 2], [u32; 2])>, Walk);
        let mut scratch: Vec<Scratch> = (0..workers).map(|_| Default::default()).collect();
        let mut epoch = 0u32;
        for (index, level) in levels.iter().enumerate() {
            for batch in level.chunks(limits.batch_nodes.clamp(1, 65_536)) {
                if ctx.checkpoint().is_err() {
                    self.levels = levels;
                    return Err(Cancelled);
                }
                if workers == 1 || batch.len() < 16 {
                    for &id in batch {
                        self.best[id as usize] = self
                            .exact_node(id, &mut refs[..], &mut w, req)
                            .expect("shared counts do not spill");
                    }
                    continue;
                }
                epoch += 1;
                let started = std::time::Instant::now();
                let this = &*self;
                let base = &refs[..];
                let used_bytes = std::sync::atomic::AtomicUsize::new(0);
                let decide = |(changed, w): &mut Scratch, &id: &u32| {
                    if changed.capacity() > 256
                        && changed.capacity() > changed.len().saturating_mul(8)
                    {
                        *changed = FxHashMap::default();
                    } else {
                        changed.clear();
                    }
                    let mut ov = Overlay {
                        base,
                        changed,
                        limit: overlay_limit,
                    };
                    let best = this.exact_node(id, &mut ov, w, req).ok()?;
                    let size = ov.changed.len() * size_of::<Read>();
                    if size > 0
                        && used_bytes
                            .fetch_update(
                                std::sync::atomic::Ordering::Relaxed,
                                std::sync::atomic::Ordering::Relaxed,
                                |old| {
                                    old.checked_add(size)
                                        .filter(|&total| total <= limits.max_plan_bytes / 2)
                                },
                            )
                            .is_err()
                    {
                        return None;
                    }
                    let read: Vec<_> = ov
                        .changed
                        .iter()
                        .map(|(&c, &(r, low, high))| Read {
                            node: c,
                            low,
                            high,
                            delta: std::array::from_fn(|p| {
                                i64::from(r[p]) - i64::from(base[c as usize][p])
                            }),
                        })
                        .collect();
                    Some((best, read))
                };
                let decided: Vec<_> = if rayon::current_num_threads() <= workers {
                    batch
                        .par_iter()
                        .with_min_len(4)
                        .map_init(Default::default, decide)
                        .collect()
                } else {
                    // Limit live scratch when the caller's pool is wider than this pass budget.
                    scratch
                        .par_iter_mut()
                        .zip(batch.par_chunks(batch.len().div_ceil(workers)))
                        .flat_map_iter(|(state, ids)| ids.iter().map(|id| decide(state, id)))
                        .collect()
                };
                stats.plan_s += started.elapsed().as_secs_f64();
                let started = std::time::Instant::now();
                for (&id, decision) in batch.iter().zip(decided) {
                    let mut tr = Tracked {
                        refs: &mut refs,
                        stamp: &mut stamp,
                        start: &mut start,
                        epoch,
                    };
                    if let Some((b, read)) = decision.as_ref()
                        && !tr.stale(read)
                        && (self.repair_price == 0 || !tr.changed_counts(read, self.repair_fanout))
                    {
                        // Validated walks have identical 0/1 branches and load-price changes.
                        // Their final count deltas commute: avoid re-walking the selected cover.
                        for r in read {
                            tr.apply(r);
                        }
                        self.best[id as usize] = *b;
                    } else {
                        stats.replays += 1;
                        stats.spilled += u64::from(decision.is_none());
                        let replay = std::time::Instant::now();
                        self.best[id as usize] = self
                            .exact_node(id, &mut tr, &mut w, req)
                            .expect("shared counts do not spill");
                        stats.replay_s += replay.elapsed().as_secs_f64();
                    }
                }
                stats.commit_s += started.elapsed().as_secs_f64();
            }
            ctx.progress("map", index as u64 + 1, total);
        }
        self.levels = levels;
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::{ARR_INF, Choice, Impl, Mapper, Own, Read, Tracked, Walk, add_arr};

    #[test]
    fn accepted_price_shifts_preserve_every_discrete_charge() {
        // Exhaust all small count intervals, not merely sampled decisions of the mapper.
        for f in [2u32, 3, 8] {
            let debt = |r: u32| r.saturating_sub(f).div_ceil(f - 1);
            for was in 0..32u32 {
                for now in 0..32u32 {
                    for low in 0..=was {
                        for high in was..32u32 {
                            let mut refs = [[now, 0]];
                            let mut stamp = [1];
                            let mut start = [[was, 0]];
                            let tr = Tracked {
                                refs: &mut refs,
                                stamp: &mut stamp,
                                start: &mut start,
                                epoch: 1,
                            };
                            let read = Read {
                                node: 0,
                                low: [low, u32::MAX],
                                high: [high, 0],
                                delta: [0; 2],
                            };
                            if !tr.changed_counts(&[read], f) {
                                let shift = i64::from(now) - i64::from(was);
                                for count in low..high {
                                    let shifted = u32::try_from(i64::from(count) + shift).unwrap();
                                    assert_eq!(
                                        debt(count + 1) - debt(count),
                                        debt(shifted + 1) - debt(shifted)
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// `stale` for one changed node 0 whose phase-0 count went from `was` to `now` during the
    /// level, read by a decision whose walks took it as low as `low`.
    fn stale(was: u32, now: u32, low: u32) -> bool {
        let mut refs = [[now, 0]];
        let mut stamp = [7];
        let mut start = [[was, 0]];
        let t = Tracked {
            refs: &mut refs,
            stamp: &mut stamp,
            start: &mut start,
            epoch: 7,
        };
        t.stale(&[Read {
            node: 0,
            low: [low, u32::MAX],
            high: [low, 0],
            delta: [0; 2],
        }])
    }

    // SYNTH-REQ-0540: cover recovery considers both output phases together.
    #[test]
    fn pair_recovers_a_cheaper_opposite_polarity() {
        use super::super::cuts::Cut;
        use super::super::library::CellLib;
        use crate::aig::Aig;
        let raw=rusteda_formats::liberty::read_text(r#"library(t) {
            cell(inv) {area:1;pin(A){direction:input;}pin(Y){direction:output;function:"!A";}}
            cell(and2) {area:10;pin(A){direction:input;}pin(B){direction:input;}pin(Y){direction:output;function:"A&B";}}
            cell(nand2) {area:3;pin(A){direction:input;}pin(B){direction:input;}pin(Y){direction:output;function:"!(A&B)";}}
        }"#,"phase.lib").unwrap();
        let lib = CellLib::build(&[raw], &[]).unwrap();
        let mut aig = Aig::new();
        let a = aig.input();
        let b = aig.input();
        let y = aig.and(a, b);
        let roots = [y, !y];
        let free = Default::default();
        let mut mapper = Mapper::new(&aig, &lib, &roots, &free);
        let cut = Cut::merge(&Cut::trivial(a.node()), a, &Cut::trivial(b.node()), b, 2).unwrap();
        mapper.cuts[y.node() as usize] = vec![cut];
        let mat = *lib
            .matches(2, cut.tt)
            .iter()
            .find(|m| lib.comb[m.cell as usize].name == "and2")
            .unwrap();
        let mut own = Own {
            id: y.node(),
            best: [
                Choice {
                    imp: Impl::Cell { cut: 0, m: mat },
                    arr: 1,
                    af: 0,
                },
                Choice {
                    imp: Impl::Inv,
                    arr: 2,
                    af: 0,
                },
            ],
        };
        own.best[0].arr = mapper.arrival(&own, y.node(), false, own.best[0].imp);
        own.best[1].arr = add_arr(own.best[0].arr, mapper.d_inv);
        mapper.best[y.node() as usize] = own.best;
        let mapped = mapper.extract();
        let mut refs = mapper.cover_refs(&mapped);
        let mut walk = Walk::default();
        let req = vec![[ARR_INF; 2]; aig.num_nodes()];
        mapper
            .exact_phase(&mut own, &mut refs[..], &mut walk, false, &req)
            .unwrap();
        mapper
            .exact_phase(&mut own, &mut refs[..], &mut walk, true, &req)
            .unwrap();
        assert!(matches!(own.best[1].imp, Impl::Inv)); // Greedy phases are trapped at area 11.
        mapper
            .exact_pair(&mut own, &mut refs[..], &mut walk, &req)
            .unwrap();
        assert!(matches!(own.best[0].imp, Impl::Inv));
        let Impl::Cell { m, .. } = own.best[1].imp else {
            panic!("negative phase must be direct")
        };
        assert_eq!(lib.comb[m.cell as usize].name, "nand2");
        mapper.best[y.node() as usize] = own.best;
        assert_eq!(
            mapper
                .extract()
                .gates
                .iter()
                .map(|g| lib.comb[g.cell].area)
                .sum::<f64>(),
            4.0
        );
        assert_eq!(refs, mapper.cover_refs(&mapper.extract()));
    }

    // SYNTH-REQ-0517: a decision is recomputed exactly when its walks could branch differently.
    #[test]
    fn stale_decisions() {
        // Unchanged count: never stale.
        assert!(!stale(3, 3, 0));
        // A count its walk left at 1 or more but that started at 0 now starts at 1: the walk's
        // first reference no longer reaches 1 from 0.
        assert!(stale(0, 1, 1));
        // Counts that stay at 2 or more in both cases.
        assert!(!stale(3, 4, 2));
        assert!(!stale(5, 4, 4));
        // A decrease that brings the walk's lowest count below 2.
        assert!(stale(3, 2, 2));
        assert!(stale(2, 1, 1));
        // A walk that reached 0 depends on every change.
        assert!(stale(1, 2, 0));
        // Nodes not changed during the level, and phases the walk did not touch.
        let mut refs = [[1, 5]];
        let mut stamp = [6];
        let mut start = [[0, 4]];
        let t = Tracked {
            refs: &mut refs,
            stamp: &mut stamp,
            start: &mut start,
            epoch: 7,
        };
        assert!(!t.stale(&[Read {
            node: 0,
            low: [0, 0],
            high: [0; 2],
            delta: [0; 2]
        }]));
        let mut stamp = [7];
        let t = Tracked {
            refs: &mut refs,
            stamp: &mut stamp,
            start: &mut start,
            epoch: 7,
        };
        assert!(!t.stale(&[Read {
            node: 0,
            low: [u32::MAX, 3],
            high: [0; 2],
            delta: [0; 2]
        }]));
        assert!(t.stale(&[Read {
            node: 0,
            low: [0, 3],
            high: [0; 2],
            delta: [0; 2]
        }]));
    }
}
