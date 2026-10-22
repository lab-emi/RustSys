// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The delay and area-flow passes on the cut-pass task of `rusteda-compute` (SYNTH-REQ-0516):
//! the mapper is lowered once into a [`CutPassProblem`], one session runs every pass, and after
//! each pass the choices and the chosen cuts (after the last pass before exact-area recovery or a
//! technology pass, the whole cut table) are decoded back into the mapper. The task computes
//! exactly what `Mapper::node_pass` computes without technology priority (integer costs, same
//! orders), so the netlist does not depend on the backend. Required times, fanout estimates,
//! cover extraction, technology passes and exact-area passes stay on the CPU between passes.

use rayon::prelude::*;
use rusteda_compute::cut::{
    self, CutPassProblem, CutPassSession, CutRow, Imp, MatchTable, PassCosts, PassOutput, Readback,
};
use rusteda_compute::{BackendKind, ComputeError};

use super::cuts::Cut;
use super::library::{AF_MAX, ARR_INF, Match};
use super::mapper::{Choice, Impl, MapOptions, Mapped, Mapper, Rank, pass_step, relaxed};
use rusteda_core::job::Ctx;

/// The problem of a mapper, owned, and the matches in table order to decode implementations.
struct Lowered {
    fanins: Vec<[u32; 2]>,
    inputs: Vec<u32>,
    level_ptr: Vec<u32>,
    level_nodes: Vec<u32>,
    init: Vec<[cut::Choice; 2]>,
    key_vars: Vec<u8>,
    key_tt: Vec<u64>,
    key_first: Vec<u32>,
    m_cell: Vec<u32>,
    m_perm: Vec<[u8; 6]>,
    m_phase: Vec<u8>,
    c_inputs: Vec<u8>,
    c_area: Vec<u32>,
    c_delay: Vec<[i32; 6]>,
    max_leaves: u32,
    max_cuts: u32,
    inv_delay: i32,
    inv_area: u32,
    matches: Vec<Match>,
}

impl Lowered {
    fn new(m: &Mapper, max_cuts: usize) -> Lowered {
        let n = m.aig.num_nodes();
        let mut fanins = vec![[0u32; 2]; n];
        let mut inputs = Vec::new();
        for id in 1..n as u32 {
            if m.aig.is_and(id) {
                let (a, b) = m.aig.fanins(id);
                fanins[id as usize] = [a.0, b.0];
            } else if m.aig.is_input(id) {
                inputs.push(id);
            }
        }
        let mut level_ptr = vec![0u32];
        let mut level_nodes = Vec::new();
        for l in &m.levels {
            level_nodes.extend_from_slice(l);
            level_ptr.push(level_nodes.len() as u32);
        }
        let init = m
            .best
            .iter()
            .map(|b| {
                b.map(|c| cut::Choice {
                    imp: match c.imp {
                        Impl::None | Impl::Cell { .. } => Imp::NONE,
                        Impl::Input => Imp::INPUT,
                        Impl::FreeNeg => Imp::FREE_NEG,
                        Impl::Inv => Imp::INV,
                    },
                    arr: c.arr,
                    af: c.af,
                })
            })
            .collect();
        let mut keys: Vec<(u8, u64)> = m.lib.table.keys().copied().collect();
        keys.sort_unstable();
        let mut key_first = Vec::with_capacity(keys.len() + 1);
        let mut matches: Vec<Match> = Vec::new();
        for k in &keys {
            key_first.push(matches.len() as u32);
            matches.extend_from_slice(&m.lib.table[k]);
        }
        key_first.push(matches.len() as u32);
        Lowered {
            fanins,
            inputs,
            level_ptr,
            level_nodes,
            init,
            key_vars: keys.iter().map(|k| k.0).collect(),
            key_tt: keys.iter().map(|k| k.1).collect(),
            key_first,
            m_cell: matches.iter().map(|x| x.cell).collect(),
            m_perm: matches.iter().map(|x| x.perm).collect(),
            m_phase: matches.iter().map(|x| x.phase).collect(),
            c_inputs: m.lib.comb.iter().map(|c| c.n_inputs).collect(),
            c_area: m.lib.comb.iter().map(|c| c.area_q).collect(),
            c_delay: m.lib.comb.iter().map(|c| c.delay_arr).collect(),
            max_leaves: m.k as u32,
            max_cuts: u32::try_from(max_cuts).unwrap_or(u32::MAX),
            inv_delay: m.d_inv,
            inv_area: m.a_inv,
            matches,
        }
    }

    fn problem(&self) -> CutPassProblem<'_> {
        CutPassProblem {
            fanins: &self.fanins,
            inputs: &self.inputs,
            level_ptr: &self.level_ptr,
            level_nodes: &self.level_nodes,
            init: &self.init,
            max_leaves: self.max_leaves,
            max_cuts: self.max_cuts,
            inv_delay: self.inv_delay,
            inv_area: self.inv_area,
            lib: MatchTable {
                key_vars: &self.key_vars,
                key_tt: &self.key_tt,
                key_first: &self.key_first,
                m_cell: &self.m_cell,
                m_perm: &self.m_perm,
                m_phase: &self.m_phase,
                c_inputs: &self.c_inputs,
                c_area: &self.c_area,
                c_delay: &self.c_delay,
            },
        }
    }

    /// A mapper choice from a task choice.
    fn choice(&self, c: &cut::Choice) -> Choice {
        let imp = match c.imp.kind() {
            1 => Impl::Input,
            2 => Impl::FreeNeg,
            3 => Impl::Inv,
            4 => Impl::Cell {
                cut: c.imp.cut() as u16,
                m: self.matches[c.imp.match_index() as usize],
            },
            _ => Impl::None,
        };
        Choice {
            imp,
            arr: c.arr,
            af: c.af,
        }
    }

    /// Updates the mapper's cuts and choices of the AND nodes from a whole cut table.
    fn decode(&self, m: &mut Mapper, out: &PassOutput) {
        let slots = out.slots as usize;
        let decoded: Vec<(Vec<Cut>, [Choice; 2])> = self
            .level_nodes
            .par_iter()
            .map(|&id| {
                let id = id as usize;
                let count = out.cut_count[id] as usize;
                let rows = out.rows[id * slots..id * slots + count]
                    .iter()
                    .map(cut_of)
                    .collect();
                let b = &out.choices[id];
                (rows, [self.choice(&b[0]), self.choice(&b[1])])
            })
            .collect();
        for (&id, (c, b)) in self.level_nodes.iter().zip(decoded) {
            m.cuts[id as usize] = c;
            m.best[id as usize] = b;
        }
    }

    /// Updates the mapper's choices of the AND nodes and only their chosen cuts, renumbered as
    /// cuts 0 and 1: all that the cover extraction and the required times read between passes.
    fn decode_chosen(&self, m: &mut Mapper, out: &PassOutput) {
        let decoded: Vec<(Vec<Cut>, [Choice; 2])> = self
            .level_nodes
            .par_iter()
            .map(|&id| {
                let id = id as usize;
                let b = &out.choices[id];
                let mut ch = [self.choice(&b[0]), self.choice(&b[1])];
                let mut cuts = Vec::with_capacity(2);
                for (p, c) in ch.iter_mut().enumerate() {
                    if let Impl::Cell { cut, .. } = &mut c.imp {
                        *cut = cuts.len() as u16;
                        cuts.push(cut_of(&out.rows[2 * id + p]));
                    }
                }
                (cuts, ch)
            })
            .collect();
        for (&id, (c, b)) in self.level_nodes.iter().zip(decoded) {
            m.cuts[id as usize] = c;
            m.best[id as usize] = b;
        }
    }
}

fn cut_of(r: &CutRow) -> Cut {
    Cut {
        leaves: r.leaves,
        n: r.n as u8,
        tt: r.tt,
        sign: r.sign,
    }
}

/// Accumulated seconds per step of the task passes.
#[derive(Default)]
struct Steps(Vec<(&'static str, f64)>);

impl Steps {
    fn add_s(&mut self, name: &'static str, d: f64) {
        match self.0.iter_mut().find(|x| x.0 == name) {
            Some(x) => x.1 += d,
            None => self.0.push((name, d)),
        }
    }
    fn add(&mut self, name: &'static str, t: std::time::Instant) {
        self.add_s(name, t.elapsed().as_secs_f64());
    }
}

/// One pass on the session; the mapper's choices are updated from the result, with all cuts
/// (`full`) or only the chosen ones.
fn pass(
    s: &mut dyn CutPassSession,
    l: &Lowered,
    m: &mut Mapper,
    rank: Rank,
    fo2: &[u32],
    req: Option<&[[i32; 2]]>,
    full: bool,
    steps: &mut Steps,
) -> Result<(), ComputeError> {
    let mut out = PassOutput::default();
    let st = s.run(
        &PassCosts {
            rank: match rank {
                Rank::Delay => cut::Rank::Delay,
                Rank::Area => cut::Rank::AreaFlow,
            },
            fo2,
            required: req,
        },
        if full {
            Readback::All
        } else {
            Readback::Chosen
        },
        &mut out,
    )?;
    steps.add_s("gpu_kernels", st.kernel_s);
    steps.add_s("gpu_readback", st.readback_s);
    let t = std::time::Instant::now();
    if full {
        l.decode(m, &out);
    } else {
        l.decode_chosen(m, &out);
    }
    steps.add("gpu_decode", t);
    Ok(())
}

/// The delay pass and the area-flow passes on one session (SYNTH-REQ-0512, SYNTH-REQ-0516);
/// `None` when the run was cancelled between two passes (SYNTH-REQ-0711).
fn passes(
    s: &mut dyn CutPassSession,
    l: &Lowered,
    m: &mut Mapper,
    opts: MapOptions,
    steps: &mut Steps,
    ctx: Ctx<'_>,
) -> Result<Option<(Mapped, Option<i32>)>, ComputeError> {
    let fo2: Vec<u32> = m.fanout.iter().map(|f| (*f).max(1) * 2).collect();
    // All cuts are read back only after the last pass, for the exact-area passes (a technology
    // pass recomputes the fanins' cuts level by level and reads only a node's chosen cuts).
    let full = |last: bool| last && opts.exact_passes > 0;
    if ctx.checkpoint().is_err() {
        return Ok(None);
    }
    pass_step(ctx, "delay_pass", 0, 1);
    pass(
        s,
        l,
        m,
        Rank::Delay,
        &fo2,
        None,
        full(opts.area_passes == 0),
        steps,
    )?;
    let t = std::time::Instant::now();
    if opts.delay_relax.is_none() {
        m.feasible_area_bounds(ctx);
    }
    let mut mapped = m.extract_current();
    let limit = opts.delay_relax.map(|r| relaxed(m.target(), r));
    steps.add("cpu_between", t);
    m.emit_arrival(ctx);
    for i in 0..opts.area_passes {
        if ctx.checkpoint().is_err() {
            return Ok(None);
        }
        pass_step(ctx, "area_pass", i, opts.area_passes);
        let t = std::time::Instant::now();
        let req = m.required(limit);
        let est = m.fanout_estimate(&mapped);
        steps.add("cpu_between", t);
        let last = i + 1 == opts.area_passes;
        pass(s, l, m, Rank::Area, &est, Some(&req), full(last), steps)?;
        let t = std::time::Instant::now();
        mapped = m.extract_current();
        steps.add("cpu_between", t);
        m.emit_arrival(ctx);
    }
    Ok(Some((mapped, limit)))
}

/// Cover and delay bound (`None` when cancelled), the backend used and the step times.
pub(super) type CutPassesOut = (
    Option<(Mapped, Option<i32>)>,
    &'static str,
    Vec<(&'static str, f64)>,
);

/// Runs the delay and area-flow passes on backend `kind` of the cut-pass task; returns the cover,
/// the delay bound and the backend used.
pub(super) fn cut_passes(
    m: &mut Mapper,
    opts: MapOptions,
    kind: BackendKind,
    ctx: Ctx<'_>,
) -> Result<CutPassesOut, ComputeError> {
    let mut steps = Steps::default();
    // Backend selection and, once per process, the device self-test.
    let t = std::time::Instant::now();
    let backend = cut::backend(kind)?;
    steps.add("gpu_start", t);
    let t = std::time::Instant::now();
    let l = Lowered::new(m, opts.max_cuts);
    let mut s = backend.open(&l.problem())?;
    steps.add("gpu_setup", t);
    let out = passes(&mut *s, &l, m, opts, &mut steps, ctx)?;
    Ok((out, backend.name(), steps.0))
}

/// One pass of the task's CPU backend on a fresh lowering of `m` with all cuts read back, for
/// tests that compare it with `Mapper::pass` pass by pass.
#[cfg(test)]
pub(super) struct TaskPasses {
    l: Lowered,
    s: Box<dyn CutPassSession>,
}

#[cfg(test)]
impl TaskPasses {
    pub(super) fn new(m: &Mapper, max_cuts: usize, kind: BackendKind) -> TaskPasses {
        let l = Lowered::new(m, max_cuts);
        let s = cut::backend(kind)
            .and_then(|b| b.open(&l.problem()))
            .unwrap_or_else(|e| panic!("{e}"));
        TaskPasses { l, s }
    }

    pub(super) fn pass(
        &mut self,
        m: &mut Mapper,
        rank: Rank,
        fo2: &[u32],
        req: Option<&[[i32; 2]]>,
        full: bool,
    ) -> Vec<(&'static str, f64)> {
        let mut steps = Steps::default();
        pass(&mut *self.s, &self.l, m, rank, fo2, req, full, &mut steps)
            .unwrap_or_else(|e| panic!("{e}"));
        steps.0
    }
}

// Constants shared with the task.
const _: () = assert!(ARR_INF == cut::ARR_INF && AF_MAX == cut::AF_MAX);
