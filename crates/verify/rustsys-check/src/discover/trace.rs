// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Reset-trace signatures that rank implementation state-map proposals.
//!
//! Both models run side by side on the same random inputs, 64 runs per word: every primary
//! input that reaches an asynchronous control is held active for the first cycles and later
//! released, flipping back in about one lane in 64 per cycle; every input that reaches a clock is
//! toggled low then high each cycle. A flip-flop updates when its own clock expression rises, so
//! gated and inverted clocks need no recognition; transparent latches and asynchronous controls
//! are settled to a bounded fixpoint in each phase. After a warm-up the value of every state is
//! recorded once per cycle.
//!
//! Signatures only rank and filter proposals. They are not reset assumptions and never a proof
//! premise: every proposal is still refined by induction from arbitrary states and then proved by
//! the exported obligations.
//!
//! Memory: one u64 per definition (values and free inputs), plus
//! `(rtl states + elements) * runs * cycles` signature words; at the default 4 runs of 96
//! recorded cycles that is 3 KiB per state. Work: every evaluation is charged to the caller's
//! evaluation budget before it runs.

use std::collections::HashSet;

use super::{Corr, Setting};
use crate::Stop;
use crate::circ::{B, Circuit};
use crate::prove::Rng;

/// Recorded runs of 64 lanes.
pub(super) const RUNS: usize = 4;
/// Cycles before recording starts, the first `RESET_CYCLES` with active asynchronous controls.
const WARMUP: usize = 12;
const RESET_CYCLES: usize = 2;
/// Recorded cycles per run.
pub(super) const CYCLES: usize = 96;
/// Latch/asynchronous settling iterations per phase.
const SETTLE: usize = 8;

/// One signature word per recorded cycle and run, by RTL state and by implementation element.
#[derive(Clone)]
pub(super) struct Traces {
    pub rtl: Vec<Vec<u64>>,
    pub gate: Vec<Vec<u64>>,
}

impl Traces {
    /// The value if the signature never changes.
    pub fn constant(sig: &[u64]) -> Option<bool> {
        if sig.iter().all(|w| *w == 0) {
            Some(false)
        } else if sig.iter().all(|w| *w == u64::MAX) {
            Some(true)
        } else {
            None
        }
    }

    /// Whether `a` equals `b` exactly, or its complement when `inverted`.
    pub fn equal(a: &[u64], b: &[u64], inverted: bool) -> bool {
        let m = if inverted { u64::MAX } else { 0 };
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| *x == (*y ^ m))
    }

    /// A parity-free hash key: the signature is complemented when its first bit is set, so a
    /// signature and its complement share a key.
    pub fn key(sig: &[u64]) -> (u64, bool) {
        let inv = sig.first().is_some_and(|w| w & 1 == 1);
        let m = if inv { u64::MAX } else { 0 };
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for w in sig {
            h = (h ^ (w ^ m)).wrapping_mul(0x0100_0000_01b3);
            h ^= h >> 29;
        }
        (h, inv)
    }
}

struct Sim<'a, 'b> {
    s: &'a Setting<'b>,
    order: Vec<u32>,
    free: Vec<u64>,
    vals: Vec<u64>,
    rq: Vec<u64>,
    nq: Vec<u64>,
    remaining: &'a mut Option<u64>,
}

fn force(q: u64, f0: u64, f1: u64) -> u64 {
    (q & !(f0 | f1)) | (f1 & !f0)
}

impl Sim<'_, '_> {
    fn eval(&mut self) -> Result<(), Stop> {
        if let Some(left) = self.remaining.as_mut() {
            let work = self.order.len() as u64;
            if work > *left {
                return Err(Stop::Undecided(
                    "state proposal trace-work budget reached".into(),
                ));
            }
            *left -= work;
        }
        let (model, nm) = (self.s.model, self.s.nm);
        for (st, q) in model.states.iter().zip(&self.rq) {
            self.free[st.q as usize] = *q;
        }
        for (x, q) in nm.seq.iter().zip(&self.nq) {
            self.free[x.iq as usize] = *q;
        }
        let free = &self.free;
        self.s
            .c
            .eval_dense(&self.order, &|v| free[v as usize], &mut self.vals);
        Ok(())
    }

    fn v(&self, b: B) -> u64 {
        Circuit::value_dense(b, &self.vals)
    }

    /// Transparent latches and active asynchronous controls, to a bounded fixpoint.
    fn settle(&mut self) -> Result<(), Stop> {
        for _ in 0..SETTLE {
            self.eval()?;
            let mut changed = 0;
            for i in 0..self.rq.len() {
                let st = &self.s.model.states[i];
                let q = self.rq[i];
                let mut n = q;
                if st.latch {
                    let en = self.v(st.clk);
                    n = (en & self.v(st.next)) | (!en & q);
                }
                if let Some((cnd, value)) = st.arst {
                    let a = self.v(cnd);
                    n = if value {
                        force(n, 0, a)
                    } else {
                        force(n, a, 0)
                    };
                }
                changed |= n ^ q;
                self.rq[i] = n;
            }
            for i in 0..self.nq.len() {
                let x = &self.s.nm.seq[i];
                let q = self.nq[i];
                let mut n = q;
                if x.latch {
                    let en = self.v(x.clk);
                    n = (en & self.v(x.next)) | (!en & q);
                }
                n = force(n, self.v(x.clear), self.v(x.preset));
                changed |= n ^ q;
                self.nq[i] = n;
            }
            if changed == 0 {
                return Ok(());
            }
        }
        self.eval()
    }
}

/// Simulates both models and records signatures (see the module documentation).
pub(super) fn simulate(
    s: &Setting<'_>,
    corr: &Corr,
    seed: u64,
    remaining: &mut Option<u64>,
) -> Result<Traces, Stop> {
    let (c, model, nm) = (s.c, s.model, s.nm);
    let mut roots = Vec::new();
    let mut conds = Vec::new();
    let mut clocks = Vec::new();
    for st in &model.states {
        roots.extend([st.next, st.clk]);
        if let Some((cnd, _)) = st.arst {
            roots.push(cnd);
            conds.push(cnd);
        }
        if !st.latch {
            clocks.push(st.clk);
        }
    }
    for x in &nm.seq {
        roots.extend([x.next, x.clk, x.clear, x.preset]);
        conds.extend([x.clear, x.preset]);
        if !x.latch {
            clocks.push(x.clk);
        }
    }
    let order = c.order(&roots).map_err(|cy| {
        Stop::Error(format!(
            "combinational cycle through circuit variable {}",
            cy.0
        ))
    })?;
    let inputs: Vec<u32> = model
        .inputs
        .iter()
        .flat_map(|(_, bits, _)| bits.iter().copied())
        .collect();
    let input_set: HashSet<u32> = inputs.iter().copied().collect();
    let support = |bits: &[B]| -> Result<Vec<u32>, Stop> {
        let mut v = c
            .support(bits)
            .map_err(|cy| Stop::Error(format!("combinational cycle through {}", cy.0)))?;
        v.retain(|x| input_set.contains(x));
        Ok(v)
    };
    let clock_inputs = support(&clocks)?;
    let mut reset_inputs = support(&conds)?;
    reset_inputs.retain(|v| !clock_inputs.contains(v));
    let data_inputs: Vec<u32> = inputs
        .iter()
        .copied()
        .filter(|v| !clock_inputs.contains(v) && !reset_inputs.contains(v))
        .collect();
    let mut sim = Sim {
        s,
        order,
        free: vec![0; c.defs.len()],
        vals: vec![0; c.defs.len()],
        rq: vec![0; model.states.len()],
        nq: vec![0; nm.seq.len()],
        remaining,
    };
    let mut rng = Rng(seed ^ 0x7ace_5eed_0bad_cafe);
    // The reset-input polarity that activates more asynchronous controls.
    let mut best = (0u32, false);
    for polarity in [false, true] {
        for &v in &data_inputs {
            sim.free[v as usize] = rng.next();
        }
        for &v in &reset_inputs {
            sim.free[v as usize] = if polarity { u64::MAX } else { 0 };
        }
        for i in 0..sim.rq.len() {
            sim.rq[i] = rng.next();
        }
        for i in 0..sim.nq.len() {
            sim.nq[i] = rng.next();
        }
        sim.eval()?;
        let active: u32 = conds.iter().map(|b| sim.v(*b).count_ones()).sum();
        if active > best.0 {
            best = (active, polarity);
        }
    }
    let active = if best.1 { u64::MAX } else { 0 };
    let mut traces = Traces {
        rtl: vec![Vec::with_capacity(RUNS * CYCLES); model.states.len()],
        gate: vec![Vec::with_capacity(RUNS * CYCLES); nm.seq.len()],
    };
    let mut prev_clk = vec![0u64; clocks.len()];
    let mut prev_d = vec![0u64; clocks.len()];
    let flop_rtl: Vec<usize> = (0..model.states.len())
        .filter(|i| !model.states[*i].latch)
        .collect();
    let flop_gate: Vec<usize> = (0..nm.seq.len()).filter(|i| !nm.seq[*i].latch).collect();
    for _ in 0..RUNS {
        // Every state starts at 0, so unreset state that the trace never writes reads the same
        // on both sides; named pairs and their map relations start consistent.
        sim.rq.iter_mut().for_each(|w| *w = 0);
        sim.nq.iter_mut().for_each(|w| *w = 0);
        for &(si, ri, inv) in &corr.equivalents {
            sim.rq[si] = sim.rq[ri] ^ if inv { u64::MAX } else { 0 };
        }
        for (si, k) in corr.constants.iter().enumerate() {
            if let Some(k) = k {
                sim.rq[si] = if *k { u64::MAX } else { 0 };
            }
        }
        for (ci, owner) in corr.owners.iter().enumerate() {
            if let Some(si) = owner {
                let parity = corr.paired[*si].is_some_and(|(_, p)| p);
                sim.nq[ci] = sim.rq[*si] ^ if parity { u64::MAX } else { 0 };
            }
        }
        prev_clk.iter_mut().for_each(|w| *w = u64::MAX);
        for cycle in 0..WARMUP + CYCLES {
            for &v in &data_inputs {
                sim.free[v as usize] = rng.next();
            }
            for &v in &reset_inputs {
                sim.free[v as usize] = if cycle < RESET_CYCLES {
                    active
                } else {
                    let flip = (0..6).fold(u64::MAX, |m, _| m & rng.next());
                    !active ^ flip
                };
            }
            for phase in [false, true] {
                for &v in &clock_inputs {
                    sim.free[v as usize] = if phase { u64::MAX } else { 0 };
                }
                sim.settle()?;
                let mut rose = false;
                let mut k = 0;
                let mut updates = Vec::new();
                for &i in &flop_rtl {
                    let now = sim.v(model.states[i].clk);
                    let edge = !prev_clk[k] & now;
                    if edge != 0 {
                        updates.push((false, i, edge, prev_d[k]));
                        rose = true;
                    }
                    k += 1;
                }
                for &i in &flop_gate {
                    let now = sim.v(nm.seq[i].clk);
                    let edge = !prev_clk[k] & now;
                    if edge != 0 {
                        updates.push((true, i, edge, prev_d[k]));
                        rose = true;
                    }
                    k += 1;
                }
                for (gate, i, edge, d) in updates {
                    let q = if gate { &mut sim.nq[i] } else { &mut sim.rq[i] };
                    *q = (*q & !edge) | (d & edge);
                }
                if rose {
                    sim.settle()?;
                }
                let mut k = 0;
                for &i in &flop_rtl {
                    prev_clk[k] = sim.v(model.states[i].clk);
                    prev_d[k] = sim.v(model.states[i].next);
                    k += 1;
                }
                for &i in &flop_gate {
                    prev_clk[k] = sim.v(nm.seq[i].clk);
                    prev_d[k] = sim.v(nm.seq[i].next);
                    k += 1;
                }
            }
            if cycle >= WARMUP {
                for (sig, q) in traces.rtl.iter_mut().zip(&sim.rq) {
                    sig.push(*q);
                }
                for (sig, q) in traces.gate.iter_mut().zip(&sim.nq) {
                    sig.push(*q);
                }
            }
        }
    }
    if std::env::var_os("RUSTSYS_DISCOVERY_TRACE").is_some() {
        let name = |v: u32| {
            model
                .inputs
                .iter()
                .find_map(|(n, bits, _)| {
                    bits.iter()
                        .position(|x| *x == v)
                        .map(|k| format!("{n}[{k}]"))
                })
                .unwrap_or_default()
        };
        eprintln!(
            "trace: clocks {:?} resets {:?} active polarity {} ({} lanes); constant RTL {} of {}, constant elements {} of {}",
            clock_inputs.iter().map(|v| name(*v)).collect::<Vec<_>>(),
            reset_inputs.iter().map(|v| name(*v)).collect::<Vec<_>>(),
            best.1,
            best.0,
            traces
                .rtl
                .iter()
                .filter(|t| Traces::constant(t).is_some())
                .count(),
            traces.rtl.len(),
            traces
                .gate
                .iter()
                .filter(|t| Traces::constant(t).is_some())
                .count(),
            traces.gate.len()
        );
    }
    if let Some(path) = std::env::var_os("RUSTSYS_DISCOVERY_TRACE_DUMP") {
        let mut text = String::new();
        for (st, t) in model.states.iter().zip(&traces.rtl) {
            let ones: u32 = t.iter().map(|w| w.count_ones()).sum();
            text.push_str(&format!(
                "rtl {} {:?} {}\n",
                st.name,
                Traces::constant(t),
                ones
            ));
        }
        for (x, t) in nm.seq.iter().zip(&traces.gate) {
            let ones: u32 = t.iter().map(|w| w.count_ones()).sum();
            text.push_str(&format!(
                "gate {} {:?} {}\n",
                x.inst,
                Traces::constant(t),
                ones
            ));
        }
        let (mut same, mut differ) = (0, 0);
        for (ci, owner) in corr.owners.iter().enumerate() {
            if let Some(si) = owner {
                let parity = corr.paired[*si].is_some_and(|(_, p)| p);
                if Traces::equal(&traces.rtl[*si], &traces.gate[ci], parity) {
                    same += 1;
                } else {
                    differ += 1;
                    if differ <= 40 {
                        let first = traces.rtl[*si]
                            .iter()
                            .zip(&traces.gate[ci])
                            .position(|(a, b)| a ^ b ^ if parity { u64::MAX } else { 0 } != 0);
                        text.push_str(&format!(
                            "pair-differs {} {} first word {:?}\n",
                            model.states[*si].name, nm.seq[ci].inst, first
                        ));
                    }
                }
            }
        }
        text.push_str(&format!("named pairs equal {same} differ {differ}\n"));
        let _ = std::fs::write(path, text);
    }
    Ok(traces)
}
