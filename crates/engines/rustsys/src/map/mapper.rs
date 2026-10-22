// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Dual-phase cut-based standard-cell mapping (SYNTH-REQ-0510 to SYNTH-REQ-0515): a delay-oriented
//! pass, area-flow passes and exact-local-area passes under required times. Every pass enumerates
//! its own priority cuts, ranked by the cost of that pass [@mishchenko2007combinational;
//! @chen2004daomap; @manohararajah2006heuristics]. Costs are fixed-point integers
//! (SYNTH-REQ-0514) and the cut passes run level by level (SYNTH-REQ-0515), so the nodes of a
//! level can be computed in parallel with results independent of the backend and thread count.

use rayon::prelude::*;
use rustc_hash::FxHashSet;
use rusteda_core::job::{Cancelled, Ctx, Event, JobError};

pub use super::area::ExactLimits;
use super::cuts::Cut;
use super::library::{AF_MAX, ARR_INF, CellLib, DELAY_SCALE, Match};
use crate::aig::{Aig, Lit};

mod config;
pub use config::{
    Impl, MapBackend, MapOptions, Mapped, MappedGate, TimingBoundary, effective_cut_limit,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct Choice {
    pub imp: Impl,
    /// Arrival in fixed-point delay units; `ARR_INF` when unmapped.
    pub arr: i32,
    /// Area flow in fixed-point area units, saturated at `AF_MAX`.
    pub af: u32,
}

pub(super) const NONE: Choice = Choice {
    imp: Impl::None,
    arr: ARR_INF,
    af: AF_MAX,
};

/// Arrival plus a delay; unmapped stays unmapped and finite sums stay below `ARR_INF`.
#[inline]
pub(super) fn add_arr(a: i32, d: i32) -> i32 {
    if a == ARR_INF {
        ARR_INF
    } else {
        a.saturating_add(d).min(ARR_INF - 1)
    }
}

/// Saturating area-flow sum (both operands are at most `AF_MAX`, so the sum fits in `u32`).
#[inline]
pub(super) fn af_add(a: u32, b: u32) -> u32 {
    (a + b).min(AF_MAX)
}

/// The share of an area flow carried by one of `fo2 / 2` fanouts (`fo2` counts half fanouts,
/// at least 2): `af * 2 / fo2`, rounded down.
#[inline]
pub(super) fn af_share(af: u32, fo2: u32) -> u32 {
    (af * 2) / fo2.max(2)
}

/// Where the delay and area-flow passes run (SYNTH-REQ-0516).
/// Cost that orders the priority cuts of a pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Rank {
    Delay,
    Area,
}

/// Shared state of the mapping passes.
pub(super) struct Mapper<'a> {
    pub aig: &'a Aig,
    pub lib: &'a CellLib,
    pub roots: &'a [Lit],
    pub k: usize,
    pub d_inv: i32,
    pub a_inv: u32,
    /// Structural fanout counts.
    pub fanout: Vec<u32>,
    /// AND nodes of the cones grouped by level (distance from the inputs), ascending ids.
    pub levels: Vec<Vec<u32>>,
    pub cuts: Vec<Vec<Cut>>,
    pub best: Vec<[Choice; 2]>,
    /// Area-flow shares for the current pass; refreshed when a level commits.
    shared_af: Vec<[u32; 2]>,
    pub repair_fanout: u32,
    pub repair_price: u64,
    pub primary_inputs: usize,
    pub technology_priority: bool,
    pub exact_limits: ExactLimits,
    pub launches: Vec<i32>,
    pub root_required: Option<Vec<i32>>,
    /// Intermediate passes consume reference counts, not an allocated gate netlist.
    pub(super) compact_cover: bool,
    pub(super) current_refs: Option<Vec<[u32; 2]>>,
}

/// Maps the logic cones of `roots` (literals whose phases are required). Fails only when a GPU
/// backend was requested and cannot run.
pub fn map(
    aig: &Aig,
    lib: &CellLib,
    roots: &[Lit],
    free_neg: &FxHashSet<u32>,
    opts: MapOptions,
) -> Result<Mapped, String> {
    map_with(aig, lib, roots, free_neg, opts, Ctx::NONE).map_err(|e| match e {
        JobError::Failed(m) => m,
        JobError::Cancelled => "internal: cancelled without a cancel token".into(),
    })
}

/// `map` with a run context (SYNTH-REQ-0710): steps `delay_pass`, `area_pass` and `exact_pass`,
/// level progress of the CPU passes, the metric `map_arrival` after each pass, and checkpoints
/// between levels and passes.
pub fn map_with(
    aig: &Aig,
    lib: &CellLib,
    roots: &[Lit],
    free_neg: &FxHashSet<u32>,
    opts: MapOptions,
    ctx: Ctx<'_>,
) -> Result<Mapped, JobError<String>> {
    map_with_boundary(aig, lib, roots, free_neg, opts, None, ctx)
}

pub fn map_with_boundary(
    aig: &Aig,
    lib: &CellLib,
    roots: &[Lit],
    free_neg: &FxHashSet<u32>,
    opts: MapOptions,
    boundary: Option<&TimingBoundary>,
    ctx: Ctx<'_>,
) -> Result<Mapped, JobError<String>> {
    let t0 = std::time::Instant::now();
    let requested = opts.max_cuts;
    let mut opts = opts;
    opts.max_cuts = effective_cut_limit(aig, opts).map_err(JobError::Failed)?;
    ctx.emit(|| Event::Metric {
        name: "map_requested_cuts",
        value: requested as f64,
    });
    ctx.emit(|| Event::Metric {
        name: "map_effective_cuts",
        value: opts.max_cuts as f64,
    });
    ctx.emit(|| Event::Metric {
        name: "map_cut_memory_budget_bytes",
        value: opts.cut_memory_budget as f64,
    });
    let mut m = Mapper::new(aig, lib, roots, free_neg);
    m.compact_cover = true;
    if let Some(b) = boundary {
        if b.required.len() != roots.len() {
            return Err(JobError::Failed(
                "mapping boundary/root length mismatch".into(),
            ));
        }
        m.root_required = Some(b.required.clone());
        for &(node, arrival) in &b.launches {
            if node as usize >= aig.num_nodes() || !aig.is_input(node) {
                return Err(JobError::Failed("mapping launch is not an input".into()));
            }
            m.launches[node as usize] = arrival;
            for p in 0..2 {
                m.best[node as usize][p].arr = add_arr(m.best[node as usize][p].arr, arrival);
            }
        }
    }
    if let Some(limit) = opts.fanout_recovery {
        m.repair_fanout = limit.max(2);
        m.repair_price = lib.buffer.map_or(0, |b| u64::from(lib.comb[b].area_q));
        m.primary_inputs = opts.primary_inputs;
    }
    let mut phase_s = vec![("setup", t0.elapsed().as_secs_f64())];
    let redirected = if opts.fanout_recovery.is_some() && opts.area_passes >= 4 {
        2
    } else {
        0
    };
    let mut base_options = opts;
    base_options.area_passes -= redirected;
    let (mut mapped, limit, backend) = match opts.backend {
        MapBackend::Cpu => {
            let t = std::time::Instant::now();
            let (mapped, limit) = m.cut_passes_cpu(base_options, ctx)?;
            phase_s.push(("cut_passes", t.elapsed().as_secs_f64()));
            (mapped, limit, "cpu")
        }
        b => {
            let (mapped, limit, backend, steps) = gpu_passes(&mut m, base_options, b, ctx)?;
            phase_s.extend(steps);
            (mapped, limit, backend)
        }
    };
    if opts.fanout_recovery.is_some() {
        let t = std::time::Instant::now();
        m.technology_priority = true;
        for i in 0..1 + redirected {
            let req = m.required(limit);
            let fo = m.fanout_estimate(&mapped);
            pass_step(ctx, "technology_pass", i, 1 + redirected);
            m.pass(Rank::Area, &fo, Some(&req), opts.max_cuts, ctx)?;
            mapped = m.extract_current();
        }
        phase_s.push(("technology_pass", t.elapsed().as_secs_f64()));
    }
    // Exact-local-area passes over the final cuts.
    m.exact_limits = opts.exact_limits;
    let mut exact_plan = 0.0;
    let mut exact_commit = 0.0;
    let mut exact_replay = 0.0;
    let t = std::time::Instant::now();
    for i in 0..opts.exact_passes {
        ctx.checkpoint()?;
        ctx.emit(|| Event::Step {
            phase: "map",
            step: "exact_pass",
            ordinal: i as u32 + 1,
            count: opts.exact_passes as u32,
        });
        let before = m.best.clone();
        let req = m.required(limit);
        let stats = m.exact_area(&mapped, &req, ctx)?;
        exact_plan += stats.plan_s;
        exact_commit += stats.commit_s;
        exact_replay += stats.replay_s;
        ctx.emit(|| Event::Metric {
            name: "exact_replayed_nodes",
            value: stats.replays as f64,
        });
        ctx.emit(|| Event::Metric {
            name: "exact_spilled_nodes",
            value: stats.spilled as f64,
        });
        mapped = m.extract_current();
        m.emit_arrival(ctx);
        if m.best.iter().zip(&before).all(|(a, b)| {
            (0..2).all(|p| a[p].imp == b[p].imp && a[p].arr == b[p].arr && a[p].af == b[p].af)
        }) {
            break;
        }
    }
    // Materialize cell instances and calculate depth once, after the last recovery pass.
    m.current_refs = None;
    mapped = m.extract();
    phase_s.push(("exact_passes", t.elapsed().as_secs_f64()));
    phase_s.extend([
        ("exact_planning", exact_plan),
        ("exact_commit", exact_commit),
        ("exact_replay", exact_replay),
    ]);
    mapped.max_arrival = f64::from(m.target()) / DELAY_SCALE;
    mapped.backend = backend;
    mapped.phase_s = phase_s;
    Ok(mapped)
}

fn gpu_passes(
    m: &mut Mapper,
    opts: MapOptions,
    b: MapBackend,
    ctx: Ctx<'_>,
) -> Result<GpuPasses, JobError<String>> {
    use rusteda_compute::BackendKind;
    let kind = match b {
        MapBackend::Cuda => BackendKind::Cuda,
        MapBackend::CpuTask => BackendKind::Cpu,
        _ => BackendKind::Gpu,
    };
    let (out, name, steps) = super::compute::cut_passes(m, opts, kind, ctx)
        .map_err(|e| JobError::Failed(format!("GPU mapping: {e}")))?;
    let (mapped, limit) = out.ok_or(JobError::Cancelled)?;
    Ok((mapped, limit, name, steps))
}

/// Cover, delay bound, backend name and step times of the cut passes of the task.
type GpuPasses = (Mapped, Option<i32>, &'static str, Vec<(&'static str, f64)>);

/// The `Step` event of mapping pass `i` (from 0) of `n`.
pub(super) fn pass_step(ctx: Ctx<'_>, step: &'static str, i: usize, n: usize) {
    ctx.emit(|| Event::Step {
        phase: "map",
        step,
        ordinal: i as u32 + 1,
        count: n as u32,
    });
}

/// The delay bound `target * (1 + relax)` in fixed-point units, rounded down (SYNTH-REQ-0512).
pub(super) fn relaxed(target: i32, relax: f64) -> i32 {
    (f64::from(target) * (1.0 + relax))
        .floor()
        .min(f64::from(ARR_INF - 1)) as i32
}

impl<'a> Mapper<'a> {
    /// The delay pass and the area-flow passes on the CPU; returns the cover and the delay
    /// bound of the area passes.
    pub(super) fn cut_passes_cpu(
        &mut self,
        opts: MapOptions,
        ctx: Ctx<'_>,
    ) -> Result<(Mapped, Option<i32>), Cancelled> {
        // Delay-oriented pass: area flow shared by the structural fanout.
        let fo2: Vec<u32> = self.fanout.iter().map(|f| (*f).max(1) * 2).collect();
        pass_step(ctx, "delay_pass", 0, 1);
        self.pass(Rank::Delay, &fo2, None, opts.max_cuts, ctx)?;
        if opts.delay_relax.is_none() {
            self.feasible_area_bounds(ctx);
        }
        let mut mapped = self.extract_current();
        self.emit_arrival(ctx);
        let limit = opts.delay_relax.map(|r| relaxed(self.target(), r));
        // Area-flow passes with fanout estimates from the current cover.
        for i in 0..opts.area_passes {
            ctx.checkpoint()?;
            pass_step(ctx, "area_pass", i, opts.area_passes);
            let req = self.required(limit);
            let est = self.fanout_estimate(&mapped);
            self.pass(Rank::Area, &est, Some(&req), opts.max_cuts, ctx)?;
            mapped = self.extract_current();
            self.emit_arrival(ctx);
        }
        Ok((mapped, limit))
    }

    /// The metric `map_arrival`: the worst output arrival of the current cover.
    pub(super) fn emit_arrival(&self, ctx: Ctx<'_>) {
        ctx.emit(|| Event::Metric {
            name: "map_arrival",
            value: f64::from(self.target()) / DELAY_SCALE,
        });
    }

    /// Area recovery cannot meet a required time that the initial delay cover already misses.
    /// Recover area without worsening that endpoint's fastest observed arrival, leaving the
    /// original electrical target intact for actual load-aware refinement and reporting.
    pub(super) fn feasible_area_bounds(&mut self, ctx: Ctx<'_>) {
        let Some(bounds) = &mut self.root_required else {
            return;
        };
        let mut changed = 0;
        for (root, bound) in self.roots.iter().zip(bounds) {
            let arrival = phase_arr(&self.best, *root);
            if !root.is_const() && arrival < ARR_INF && arrival > *bound {
                *bound = arrival;
                changed += 1;
            }
        }
        ctx.emit(|| Event::Metric {
            name: "map_infeasible_root_bounds",
            value: f64::from(changed),
        });
    }

    pub(super) fn new(
        aig: &'a Aig,
        lib: &'a CellLib,
        roots: &'a [Lit],
        free_neg: &FxHashSet<u32>,
    ) -> Mapper<'a> {
        let n = aig.num_nodes();
        let inv = &lib.comb[lib.inverter];
        let mut fanout = vec![0u32; n];
        for id in 1..n as u32 {
            if aig.is_and(id) {
                let (a, b) = aig.fanins(id);
                fanout[a.node() as usize] += 1;
                fanout[b.node() as usize] += 1;
            }
        }
        for r in roots {
            fanout[r.node() as usize] += 1;
        }
        let mut in_cone = vec![false; n];
        for id in aig.cone(roots) {
            in_cone[id as usize] = true;
        }
        let mut level = vec![0u32; n];
        let mut levels: Vec<Vec<u32>> = Vec::new();
        for id in 1..n as u32 {
            if aig.is_and(id) && in_cone[id as usize] {
                let (a, b) = aig.fanins(id);
                let l = 1 + level[a.node() as usize].max(level[b.node() as usize]);
                level[id as usize] = l;
                if levels.len() < l as usize {
                    levels.resize(l as usize, Vec::new());
                }
                levels[l as usize - 1].push(id);
            }
        }
        let mut m = Mapper {
            aig,
            lib,
            roots,
            k: lib.max_inputs.clamp(1, 6),
            d_inv: inv.delay_q[0],
            a_inv: inv.area_q,
            fanout,
            levels,
            cuts: vec![Vec::new(); n],
            best: vec![[NONE; 2]; n],
            shared_af: vec![[0; 2]; n],
            repair_fanout: 2,
            repair_price: 0,
            primary_inputs: 0,
            technology_priority: false,
            exact_limits: ExactLimits::default(),
            launches: vec![0; n],
            root_required: None,
            compact_cover: false,
            current_refs: None,
        };
        for id in 1..n as u32 {
            if aig.is_input(id) {
                m.cuts[id as usize] = vec![Cut::trivial(id)];
                m.best[id as usize][0] = Choice {
                    imp: Impl::Input,
                    arr: 0,
                    af: 0,
                };
                m.best[id as usize][1] = if free_neg.contains(&id) {
                    Choice {
                        imp: Impl::FreeNeg,
                        arr: 0,
                        af: 0,
                    }
                } else {
                    Choice {
                        imp: Impl::Inv,
                        arr: m.d_inv,
                        af: m.a_inv,
                    }
                };
            }
        }
        let konst = Choice {
            imp: Impl::Input,
            arr: 0,
            af: 0,
        };
        m.best[0] = [konst, konst];
        m
    }

    /// Worst arrival over the roots.
    pub(super) fn target(&self) -> i32 {
        self.roots
            .iter()
            .map(|r| phase_arr(&self.best, *r))
            .max()
            .unwrap_or(0)
    }

    /// Half-fanout estimates for area flow: structural fanout plus references in the cover
    /// (SYNTH-REQ-0512), at least 2.
    pub(super) fn fanout_estimate(&self, mapped: &Mapped) -> Vec<u32> {
        let n = self.aig.num_nodes();
        if let Some(refs) = &self.current_refs {
            let constants = self.roots.iter().filter(|r| r.is_const()).count() as u32;
            return (0..n)
                .map(|i| {
                    (refs[i][0] + refs[i][1] + self.fanout[i] + if i == 0 { constants } else { 0 })
                        .max(2)
                })
                .collect();
        }
        let mut refs = vec![0u32; n];
        for g in &mapped.gates {
            for (l, _) in &g.ins {
                refs[*l as usize] += 1;
            }
        }
        for r in self.roots {
            refs[r.node() as usize] += 1;
        }
        (0..n).map(|i| (refs[i] + self.fanout[i]).max(2)).collect()
    }

    /// One mapping pass level by level: the priority cuts of every node are enumerated from those
    /// of its fanins and both phases are chosen. Nodes of a level depend only on lower levels and
    /// are computed on the threads of the current rayon pool.
    pub(super) fn pass(
        &mut self,
        rank: Rank,
        fo2: &[u32],
        req: Option<&[[i32; 2]]>,
        max_cuts: usize,
        ctx: Ctx<'_>,
    ) -> Result<(), Cancelled> {
        // Each leaf's divisor is constant throughout a pass. Divide at most twice per
        // committed node, instead of dividing again for every cut and matching cell.
        for (i, b) in self.best.iter().enumerate() {
            self.shared_af[i] = [af_share(b[0].af, fo2[i]), af_share(b[1].af, fo2[i])];
        }
        let single_thread = rayon::current_num_threads() == 1;
        let levels = std::mem::take(&mut self.levels);
        let total = levels.len() as u64;
        for (li, level) in levels.iter().enumerate() {
            if ctx.checkpoint().is_err() {
                self.levels = levels;
                return Err(Cancelled);
            }
            if single_thread || level.len() < 16 {
                for &id in level {
                    let (c, b) = self.node_pass(id, rank, fo2, req, max_cuts);
                    self.cuts[id as usize] = c;
                    self.best[id as usize] = b;
                    self.shared_af[id as usize] = [
                        af_share(b[0].af, fo2[id as usize]),
                        af_share(b[1].af, fo2[id as usize]),
                    ];
                }
                ctx.progress("map", li as u64 + 1, total);
                continue;
            }
            let this = &*self;
            let out: Vec<(Vec<Cut>, [Choice; 2])> = level
                .par_iter()
                .with_min_len(4)
                .map(|&id| this.node_pass(id, rank, fo2, req, max_cuts))
                .collect();
            for (&id, (c, b)) in level.iter().zip(out) {
                self.cuts[id as usize] = c;
                self.best[id as usize] = b;
                self.shared_af[id as usize] = [
                    af_share(b[0].af, fo2[id as usize]),
                    af_share(b[1].af, fo2[id as usize]),
                ];
            }
            ctx.progress("map", li as u64 + 1, total);
        }
        self.levels = levels;
        Ok(())
    }

    /// Latest leaf arrival (either phase) and the leaf area flow shared by `fo2` of a cut: the
    /// cheap costs that rank priority cuts.
    pub(super) fn cut_costs(&self, c: &Cut, _fo2: &[u32]) -> (i32, u32) {
        let mut arr = 0i32;
        let mut af = 0u32;
        for &l in c.leaves() {
            let b = self.best[l as usize];
            arr = arr.max(b[0].arr.min(b[1].arr));
            let shared = self.shared_af[l as usize];
            #[cfg(test)]
            assert_eq!(
                shared[0].min(shared[1]),
                af_share(b[0].af.min(b[1].af), _fo2[l as usize])
            );
            af = af_add(af, shared[0].min(shared[1]));
        }
        (arr, af)
    }

    /// The cuts chosen by the previous pass for the phases of node `id` that the current cover
    /// uses (required time known), when implemented by a cell.
    pub(super) fn previous_cuts(&self, id: u32, req: Option<&[[i32; 2]]>) -> [Option<Cut>; 2] {
        let mut out = [None; 2];
        let Some(req) = req else {
            return out;
        };
        for (p, o) in out.iter_mut().enumerate() {
            if req[id as usize][p] == ARR_INF {
                continue;
            }
            if let Impl::Cell { cut, .. } = self.best[id as usize][p].imp {
                *o = self.cuts[id as usize].get(cut as usize).copied();
            }
        }
        out
    }

    /// Priority cuts (plus the trivial cut) and the chosen implementations of one node.
    pub(super) fn node_pass(
        &self,
        id: u32,
        rank: Rank,
        fo2: &[u32],
        req: Option<&[[i32; 2]]>,
        max_cuts: usize,
    ) -> (Vec<Cut>, [Choice; 2]) {
        let (f0, f1) = self.aig.fanins(id);
        let cuts0 = &self.cuts[f0.node() as usize];
        let cuts1 = &self.cuts[f1.node() as usize];
        // Candidates in pair order; the first of equal leaf sets is kept.
        let mut cand: Vec<(i32, u32, Cut)> = Vec::with_capacity(
            cuts0
                .len()
                .saturating_mul(cuts1.len())
                .saturating_add(2)
                .min(512),
        );
        let mut table = [u16::MAX; 512];
        let mut insert_unique = |c: &Cut, cand: &mut Vec<(i32, u32, Cut)>| -> Result<bool, ()> {
            let mut h = ((c.sign as usize).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 55) & 511;
            for _ in 0..table.len() {
                let idx = table[h];
                if idx == u16::MAX {
                    table[h] = cand.len() as u16;
                    return Ok(true);
                }
                let prev = &cand[idx as usize].2;
                if prev.sign == c.sign && prev.n == c.n && prev.leaves() == c.leaves() {
                    return Ok(false);
                }
                h = (h + 1) & 511;
            }
            Err(())
        };
        let mut saturated = false;
        'pairs: for a in cuts0 {
            for b in cuts1 {
                if let Some(c) = Cut::merge(a, f0, b, f1, self.k) {
                    if c.n == 0 {
                        continue;
                    }
                    match insert_unique(&c, &mut cand) {
                        Ok(false) => continue,
                        Err(()) => {
                            saturated = true;
                            break 'pairs;
                        }
                        Ok(true) => {}
                    }
                    let (arr, af) = self.cut_costs(&c, fo2);
                    cand.push((arr, af, c));
                }
            }
        }
        // The cuts of the previous pass's choices of the phases the cover uses follow as
        // candidates, so that the previous cover stays available (SYNTH-REQ-0515).
        let prev = self.previous_cuts(id, req);
        for c in prev.iter().flatten().take(if saturated { 0 } else { 2 }) {
            match insert_unique(c, &mut cand) {
                Ok(false) => continue,
                Err(()) => {
                    saturated = true;
                    break;
                }
                Ok(true) => {}
            }
            let (arr, af) = self.cut_costs(c, fo2);
            cand.push((arr, af, *c));
        }
        if saturated {
            // Keep a guaranteed direct implementation and both incumbent
            // phases. A full candidate table is an optional-search fallback,
            // never an unbounded probe or an unmappable node.
            cand.clear();
            let direct = Cut::merge(
                &Cut::trivial(f0.node()),
                f0,
                &Cut::trivial(f1.node()),
                f1,
                self.k,
            );
            for c in direct.iter().chain(prev.iter().flatten()) {
                if cand
                    .iter()
                    .any(|(_, _, old)| old.leaf_key() == c.leaf_key())
                {
                    continue;
                }
                let (arr, af) = self.cut_costs(c, fo2);
                cand.push((arr, af, *c));
            }
        }
        // Stable sort: ties keep candidate order.
        match rank {
            Rank::Delay => cand.sort_by_key(|(arr, af, c)| (*arr, c.n, *af)),
            Rank::Area => cand.sort_by_key(|(arr, af, c)| (*af, c.n, *arr)),
        }
        // Kept: the cut of the two fanins (so that every node remains mappable) and the previous
        // choices' cuts, then the best others up to `max_cuts`, in rank order.
        let mut two = [f0.node(), f1.node()];
        two.sort_unstable();
        let max_k = max_cuts.max(1);
        let mut forced = [0usize; 16];
        let mut n_forced = 0usize;
        let wanted = std::iter::once(&two[..]).chain(prev.iter().flatten().map(|c| c.leaves()));
        for leaves in wanted {
            if let Some(i) = cand.iter().position(|x| x.2.leaves() == leaves)
                && !forced[..n_forced].contains(&i)
                && n_forced < max_k.min(16)
            {
                forced[n_forced] = i;
                n_forced += 1;
            }
        }
        if self.technology_priority {
            // Exact recovery may prefer a different realizable cut than area flow.
            // Preserve a bounded set per phase, while retaining structural construction cuts.
            let mut keep = [[(NONE, 0usize); 4]; 2];
            let mut keep_len = [0usize; 2];
            let mut cached: Vec<[Option<Choice>; 2]> = vec![[None; 2]; cand.len()];
            for (i, (arr, af, c)) in cand.iter().enumerate() {
                for p in 0..2 {
                    let limit = req.map(|r| r[id as usize][p]);
                    if keep_len[p] == 4
                        && !can_improve(*arr, *af, &keep[p][3].0, limit, rank == Rank::Area)
                    {
                        continue;
                    }
                    let tt = if p == 1 { !c.tt } else { c.tt };
                    let mut best = NONE;
                    for mat in self.lib.matches(c.n as usize, tt) {
                        if let Some(ch) = self.eval_match(c, 0, mat, fo2) {
                            if better(&ch, &best, limit, rank == Rank::Area) {
                                best = ch;
                            }
                        }
                    }
                    cached[i][p] = Some(best);
                    if best.arr == ARR_INF {
                        continue;
                    }
                    let len = keep_len[p];
                    let pos = keep[p][..len]
                        .iter()
                        .position(|(old, _)| better(&best, old, limit, rank == Rank::Area))
                        .unwrap_or(len);
                    if pos < 4 {
                        let new_len = (len + 1).min(4);
                        for k in (pos + 1..new_len).rev() {
                            keep[p][k] = keep[p][k - 1];
                        }
                        keep[p][pos] = (best, i);
                        keep_len[p] = new_len;
                    }
                }
            }
            for p in 0..2 {
                for &(_, i) in &keep[p][..keep_len[p]] {
                    if !forced[..n_forced].contains(&i) && n_forced < max_k.min(16) {
                        forced[n_forced] = i;
                        n_forced += 1;
                    }
                }
            }
            let mut others = max_k.saturating_sub(n_forced);
            let mut cuts: Vec<Cut> = Vec::with_capacity(cand.len().min(max_k) + 1);
            let mut out = [NONE; 2];
            let r = req.map(|r| r[id as usize]);
            let area_mode = rank == Rank::Area;
            for (i, (cut_arr, cut_af, c)) in cand.iter().enumerate() {
                let take = if forced[..n_forced].contains(&i) {
                    true
                } else if others > 0 {
                    others -= 1;
                    true
                } else {
                    false
                };
                if !take {
                    continue;
                }
                let ci = cuts.len();
                cuts.push(*c);
                for p in 0..2usize {
                    let limit = r.map(|rv| rv[p]);
                    if !can_improve(*cut_arr, *cut_af, &out[p], limit, area_mode) {
                        continue;
                    }
                    if let Some(mut best_ch) = cached[i][p] {
                        if best_ch.arr != ARR_INF {
                            if let Impl::Cell { cut, .. } = &mut best_ch.imp {
                                *cut = ci as u16;
                            }
                            if better(&best_ch, &out[p], limit, area_mode) {
                                out[p] = best_ch;
                            }
                        }
                    } else {
                        let tt = if p == 1 { !c.tt } else { c.tt };
                        for m in self.lib.matches(c.n as usize, tt) {
                            if let Some(cand_ch) = self.eval_match(c, ci, m, fo2)
                                && better(&cand_ch, &out[p], limit, area_mode)
                            {
                                out[p] = cand_ch;
                            }
                        }
                    }
                }
            }
            self.add_inverters(&mut out, r, area_mode);
            cuts.push(Cut::trivial(id));
            return (cuts, out);
        }
        let mut others = max_k.saturating_sub(n_forced);
        let mut cuts: Vec<Cut> = Vec::with_capacity(cand.len().min(max_k) + 1);
        for (i, x) in cand.iter().enumerate() {
            if forced[..n_forced].contains(&i) {
                cuts.push(x.2);
            } else if others > 0 {
                others -= 1;
                cuts.push(x.2);
            }
        }
        let r = req.map(|r| r[id as usize]);
        let best = self.choose(&cuts, fo2, r, rank == Rank::Area);
        cuts.push(Cut::trivial(id));
        (cuts, best)
    }

    /// Chooses the implementation of both phases of a node over the given cuts: the first best
    /// candidate in (cut, phase, match) order, then an inverter from the other phase.
    pub(super) fn choose(
        &self,
        cuts: &[Cut],
        fo2: &[u32],
        req: Option<[i32; 2]>,
        area_mode: bool,
    ) -> [Choice; 2] {
        let mut out = [NONE; 2];
        for (ci, c) in cuts.iter().enumerate() {
            let (cut_arr, cut_af) = self.cut_costs(c, fo2);
            for p in 0..2usize {
                if !can_improve(cut_arr, cut_af, &out[p], req.map(|r| r[p]), area_mode) {
                    continue;
                }
                let tt = if p == 1 { !c.tt } else { c.tt };
                for m in self.lib.matches(c.n as usize, tt) {
                    if let Some(cand) = self.eval_match(c, ci, m, fo2)
                        && better(&cand, &out[p], req.map(|r| r[p]), area_mode)
                    {
                        out[p] = cand;
                    }
                }
            }
        }
        self.add_inverters(&mut out, req, area_mode);
        out
    }

    /// Arrival and area flow of a cell match over a cut; `None` when a leaf is unmapped.
    #[inline]
    pub(super) fn eval_match(&self, c: &Cut, ci: usize, m: &Match, _fo2: &[u32]) -> Option<Choice> {
        let cell = &self.lib.comb[m.cell as usize];
        let mut arr = 0i32;
        let mut af = cell.area_q;
        for i in 0..cell.n_inputs as usize {
            let leaf = c.leaves[m.perm[i] as usize] as usize;
            let ph = usize::from((m.phase >> i) & 1 == 1);
            let lc = self.best[leaf][ph];
            if lc.arr == ARR_INF {
                return None;
            }
            arr = arr.max(add_arr(lc.arr, cell.delay_arr[i]));
            #[cfg(test)]
            assert_eq!(self.shared_af[leaf][ph], af_share(lc.af, _fo2[leaf]));
            af = af_add(af, self.shared_af[leaf][ph]);
        }
        Some(Choice {
            imp: Impl::Cell {
                cut: ci as u16,
                m: *m,
            },
            arr,
            af,
        })
    }

    /// Inverter from the other phase (phase 0 first; an inverter never feeds an inverter).
    pub(super) fn add_inverters(
        &self,
        out: &mut [Choice; 2],
        req: Option<[i32; 2]>,
        area_mode: bool,
    ) {
        for p in 0..2usize {
            let o = out[1 - p];
            if o.arr != ARR_INF && !matches!(o.imp, Impl::Inv) {
                let cand = Choice {
                    imp: Impl::Inv,
                    arr: add_arr(o.arr, self.d_inv),
                    af: af_add(o.af, self.a_inv),
                };
                if better(&cand, &out[p], req.map(|r| r[p]), area_mode) {
                    out[p] = cand;
                }
            }
        }
    }

    /// Required times (`limit` at the roots) and the (node, phase) pairs of the current cover,
    /// swept from the roots in reverse level order (SYNTH-REQ-0515). The nodes of a level push to
    /// inputs on lower levels only, in parallel, with minimums and flags that do not depend on
    /// the order; a node's inverter phase is resolved before its cells push. Inputs come last:
    /// only their own inverter phase reads them.
    fn cover_sweep(&self, limit: Option<i32>) -> (Vec<[i32; 2]>, Vec<[bool; 2]>) {
        use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};
        let n = self.aig.num_nodes();
        let req: Vec<[AtomicI32; 2]> = (0..n)
            .map(|_| [AtomicI32::new(ARR_INF), AtomicI32::new(ARR_INF)])
            .collect();
        let used: Vec<[AtomicBool; 2]> = (0..n)
            .map(|_| [AtomicBool::new(false), AtomicBool::new(false)])
            .collect();
        let lim = limit.unwrap_or(ARR_INF);
        for (i, r) in self.roots.iter().enumerate() {
            if !r.is_const() {
                let p = usize::from(r.is_compl());
                let lim = self
                    .root_required
                    .as_ref()
                    .map_or(lim, |bounds| lim.min(bounds[i]));
                if lim < req[r.node() as usize][p].load(Relaxed) {
                    req[r.node() as usize][p].fetch_min(lim, Relaxed);
                }
                used[r.node() as usize][p].store(true, Relaxed);
            }
        }
        let sub = |r: i32, d: i32| {
            if r == ARR_INF {
                ARR_INF
            } else {
                r.saturating_sub(d)
            }
        };
        let node = |id: u32| {
            let id = id as usize;
            // Process the inverter phase first so that its input phase is updated in time.
            for p in [0usize, 1] {
                for q in [p, 1 - p] {
                    if used[id][q].load(Relaxed) && matches!(self.best[id][q].imp, Impl::Inv) {
                        let r = sub(req[id][q].load(Relaxed), self.d_inv);
                        if r < req[id][1 - q].load(Relaxed) {
                            req[id][1 - q].fetch_min(r, Relaxed);
                        }
                        used[id][1 - q].store(true, Relaxed);
                    }
                }
            }
            for q in 0..2usize {
                if !used[id][q].load(Relaxed) {
                    continue;
                }
                if let Impl::Cell { cut, m } = self.best[id][q].imp {
                    let c = &self.cuts[id][cut as usize];
                    let cell = &self.lib.comb[m.cell as usize];
                    let rq = req[id][q].load(Relaxed);
                    for i in 0..cell.n_inputs as usize {
                        let leaf = c.leaves[m.perm[i] as usize] as usize;
                        let ph = usize::from((m.phase >> i) & 1 == 1);
                        let r = sub(rq, cell.delay_arr[i]);
                        if r < req[leaf][ph].load(Relaxed) {
                            req[leaf][ph].fetch_min(r, Relaxed);
                        }
                        if !used[leaf][ph].load(Relaxed) {
                            used[leaf][ph].store(true, Relaxed);
                        }
                    }
                }
            }
        };
        for level in self.levels.iter().rev() {
            if level.len() >= 256 {
                level.par_iter().with_min_len(64).for_each(|&id| node(id));
            } else {
                level.iter().for_each(|&id| node(id));
            }
        }
        for &id in self.aig.inputs() {
            node(id);
        }
        let req = req
            .into_par_iter()
            .map(|[a, b]| [a.into_inner(), b.into_inner()])
            .collect();
        let used = used
            .into_par_iter()
            .map(|[a, b]| [a.into_inner(), b.into_inner()])
            .collect();
        (req, used)
    }

    /// Required times of every (node, phase) in the current cover; `ARR_INF` outside it.
    pub(super) fn required(&self, limit: Option<i32>) -> Vec<[i32; 2]> {
        let (mut required, used) = self.cover_sweep(limit);
        if limit.is_none() {
            // Infinity also encodes an unused phase. Keep used, unconstrained phases distinct
            // so the following cut pass retains their implementation.
            for (r, u) in required.iter_mut().zip(used) {
                for p in 0..2 {
                    if u[p] && r[p] == ARR_INF {
                        r[p] = ARR_INF - 1;
                    }
                }
            }
        }
        required
    }

    /// The gates of the current cover in (node, phase) order, with the logic depth.
    /// Intermediate-only view: counts contain the same pins/roots as `extract`, without gate
    /// strings, input Vec allocations or depth work. The placeholder never leaves mapping.
    pub(super) fn extract_current(&mut self) -> Mapped {
        if !self.compact_cover {
            return self.extract();
        }
        let (_, used) = self.cover_sweep(None);
        let mut refs = vec![[0u32; 2]; self.aig.num_nodes()];
        for (id, used) in used.into_iter().enumerate() {
            for (p, used) in used.into_iter().enumerate() {
                if !used {
                    continue;
                }
                match self.best[id][p].imp {
                    Impl::Inv => refs[id][1 - p] += 1,
                    Impl::Cell { cut, m } => {
                        let c = &self.cuts[id][cut as usize];
                        let cell = &self.lib.comb[m.cell as usize];
                        for i in 0..cell.n_inputs as usize {
                            let leaf = c.leaves[m.perm[i] as usize] as usize;
                            refs[leaf][usize::from((m.phase >> i) & 1 == 1)] += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        for r in self.roots {
            if !r.is_const() {
                refs[r.node() as usize][usize::from(r.is_compl())] += 1;
            }
        }
        self.current_refs = Some(refs);
        Mapped {
            gates: Vec::new(),
            free_neg_used: Default::default(),
            max_arrival: 0.0,
            depth: 0,
            backend: "cpu",
            phase_s: Vec::new(),
        }
    }

    /// The final gates of the cover in (node, phase) order, including exact logic depth.
    pub(super) fn extract(&self) -> Mapped {
        let (_, used) = self.cover_sweep(None);
        let n = self.aig.num_nodes();
        let gates: Vec<MappedGate> = (0..n as u32)
            .into_par_iter()
            .flat_map_iter(|id| {
                let this = &*self;
                let used = used[id as usize];
                [false, true].into_iter().filter_map(move |ph| {
                    if !used[usize::from(ph)] {
                        return None;
                    }
                    match this.best[id as usize][usize::from(ph)].imp {
                        Impl::Input | Impl::FreeNeg => None,
                        Impl::Inv => Some(MappedGate {
                            cell: this.lib.inverter,
                            out: (id, ph),
                            ins: vec![(id, !ph)],
                        }),
                        Impl::Cell { cut, m } => {
                            let c = &this.cuts[id as usize][cut as usize];
                            let cell = &this.lib.comb[m.cell as usize];
                            Some(MappedGate {
                                cell: m.cell as usize,
                                out: (id, ph),
                                ins: (0..cell.inputs.len())
                                    .map(|i| {
                                        (c.leaves[m.perm[i] as usize], (m.phase >> i) & 1 == 1)
                                    })
                                    .collect(),
                            })
                        }
                        Impl::None => {
                            // Every AND node has a two-input decomposition through the inverter
                            // and a NAND/AND/NOR-like cell (checked when the library is
                            // classified).
                            unreachable!("node {id} phase {ph} has no implementation")
                        }
                    }
                })
            })
            .collect();
        let free_neg_used = self
            .aig
            .inputs()
            .iter()
            .copied()
            .filter(|&id| {
                (0..2)
                    .any(|p| used[id as usize][p] && self.best[id as usize][p].imp == Impl::FreeNeg)
            })
            .collect();
        // Logic depth: nodes are topologically numbered; within a node the inverter of the other
        // phase comes after the phase it reads.
        let mut level = vec![[0u32; 2]; n];
        let mut depth = 0u32;
        let mut i = 0;
        while i < gates.len() {
            let id = gates[i].out.0;
            let mut j = i;
            while j < gates.len() && gates[j].out.0 == id {
                j += 1;
            }
            for inv in [false, true] {
                for g in &gates[i..j] {
                    if g.ins.first().is_some_and(|x| x.0 == id) != inv {
                        continue;
                    }
                    let d = 1 + g
                        .ins
                        .iter()
                        .map(|x| level[x.0 as usize][usize::from(x.1)])
                        .max()
                        .unwrap_or(0);
                    level[id as usize][usize::from(g.out.1)] = d;
                    depth = depth.max(d);
                }
            }
            i = j;
        }
        Mapped {
            gates,
            free_neg_used,
            max_arrival: 0.0,
            depth,
            backend: "cpu",
            phase_s: Vec::new(),
        }
    }
}

pub(super) fn phase_arr(best: &[[Choice; 2]], r: Lit) -> i32 {
    if r.is_const() {
        0
    } else {
        best[r.node() as usize][usize::from(r.is_compl())].arr
    }
}

/// True when `a` is strictly better than `b` (SYNTH-REQ-0512): in delay mode by arrival, then
/// area flow; in area mode meeting the required time first, then area flow, then arrival.
pub(super) fn better(a: &Choice, b: &Choice, req: Option<i32>, area_mode: bool) -> bool {
    if b.arr == ARR_INF {
        return a.arr != ARR_INF;
    }
    if area_mode {
        let limit = req.unwrap_or(ARR_INF);
        let (fa, fb) = (a.arr <= limit, b.arr <= limit);
        if fa != fb {
            return fa;
        }
        if !fa {
            return a.arr < b.arr;
        }
        if a.af != b.af {
            return a.af < b.af;
        }
        a.arr < b.arr
    } else {
        if a.arr != b.arr {
            return a.arr < b.arr;
        }
        a.af < b.af
    }
}

/// True when a cut with lower-bound `(arr, af)` can produce a match better than `b`.
#[inline]
fn can_improve(arr: i32, af: u32, b: &Choice, req: Option<i32>, area_mode: bool) -> bool {
    better(
        &Choice {
            imp: Impl::None,
            arr,
            af,
        },
        b,
        req,
        area_mode,
    )
}
