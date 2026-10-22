// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Independent sequential choices on a shared immutable graph/library.
//! <=32M states, <=2GiB retained choice records, <=4B bounded candidate trials.
//! At1.5M states the choice vector is <128MiB; an8192-state temporary batch
//! is <1MiB. No per-thread graph/library clone or unbounded cone traversal.

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use smallvec::{SmallVec, smallvec};

use super::library::{CellLib, SeqCell};
use super::{QSource, SeqChoice};
use crate::aig::{Aig, Lit};
use crate::elab::{Design, State, StateKind};

#[derive(Debug, Clone, Copy)]
pub struct SeqLimits {
    pub max_states: usize,
    pub max_choice_bytes: usize,
    pub max_trials: u64,
}

impl Default for SeqLimits {
    fn default() -> Self {
        Self {
            max_states: 32_000_000,
            max_choice_bytes: 2 * 1024 * 1024 * 1024,
            max_trials: 4_000_000_000,
        }
    }
}

fn cost(l: Lit, inverter: f64) -> f64 {
    if l.is_compl() && !l.is_const() {
        inverter
    } else {
        0.0
    }
}

fn signature(c: &SeqCell) -> u32 {
    let flags = [
        c.latch,
        c.data_inverted,
        c.clock.active_low,
        c.q.is_some(),
        c.qn.is_some(),
        c.clear.is_some(),
        c.clear.as_ref().is_some_and(|p| p.active_low),
        c.preset.is_some(),
        c.preset.as_ref().is_some_and(|p| p.active_low),
        c.enable.is_some(),
        c.enable.as_ref().is_some_and(|p| p.active_low),
        c.hold.is_some(),
        c.hold.as_ref().is_some_and(|p| p.active_low),
    ];
    flags
        .into_iter()
        .enumerate()
        .fold(0, |value, (bit, set)| value | (u32::from(set) << bit))
}

/// Selection's cost uses area and role/polarity, not pin spelling or drive
/// strength. A larger-area cell with the same role signature cannot win.
fn candidates(lib: &CellLib) -> Vec<usize> {
    let mut best: FxHashMap<u32, usize> = FxHashMap::default();
    for (i, c) in lib.seq.iter().enumerate() {
        let slot = best.entry(signature(c)).or_insert(i);
        if c.area < lib.seq[*slot].area - 1e-12 {
            *slot = i;
        }
    }
    let mut out: Vec<_> = best.into_values().collect();
    out.sort_unstable();
    out
}

pub(super) fn feedback(aig: &Aig, s: &State) -> SmallVec<[(Lit, Lit); 8]> {
    let mut out = SmallVec::new();
    if s.kind != StateKind::Flop {
        return out;
    }
    for (select, t, e) in aig.mux_readings(s.d) {
        let pair = if e == s.q {
            Some((select, t))
        } else if t == s.q {
            Some((!select, e))
        } else {
            None
        };
        if let Some(pair) = pair {
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
    }
    out
}

fn choose(
    d: &Design,
    s: &State,
    lib: &CellLib,
    choices: &[usize],
    mux_area: f64,
) -> Result<SeqChoice, String> {
    let a_inv = lib.comb[lib.inverter].area;
    let feedback = feedback(&d.aig, s);
    let mut best: Option<(f64, SeqChoice)> = None;
    for &ci in choices {
        let c = &lib.seq[ci];
        if c.latch != (s.kind == StateKind::Latch) {
            continue;
        }
        let mut variants: SmallVec<[(Lit, Lit, f64); 9]> = smallvec![(Lit::TRUE, s.d, 0.0)];
        if c.enable.is_some() {
            variants.extend(
                feedback
                    .iter()
                    .map(|&(enable, data)| (enable, data, mux_area)),
            );
        }
        for inverted in [false, true] {
            for &(load, source, saved) in &variants {
                let data = source.xor(inverted ^ c.data_inverted);
                let clock = s.clk.xor(c.clock.active_low);
                let enable = c.enable.as_ref().map(|p| load.xor(p.active_low));
                let hold = c.hold.as_ref().map(|p| s.q.xor(inverted ^ p.active_low));
                let (need_clear, need_preset) = match s.arst {
                    None => (None, None),
                    Some((cond, value)) => {
                        if value != inverted {
                            (None, Some(cond))
                        } else {
                            (Some(cond), None)
                        }
                    }
                };
                let clear = match (&c.clear, need_clear) {
                    (None, Some(_)) => continue,
                    (None, None) => None,
                    (Some(pin), Some(cond)) => Some(cond.xor(pin.active_low)),
                    (Some(pin), None) => Some(Lit::from_bool(pin.active_low)),
                };
                let preset = match (&c.preset, need_preset) {
                    (None, Some(_)) => continue,
                    (None, None) => None,
                    (Some(pin), Some(cond)) => Some(cond.xor(pin.active_low)),
                    (Some(pin), None) => Some(Lit::from_bool(pin.active_low)),
                };
                let (q_true, q_cost) = match (inverted, c.q.is_some(), c.qn.is_some()) {
                    (false, true, _) => (QSource::Q, 0.0),
                    (false, false, true) => (QSource::InvQn, a_inv),
                    (true, _, true) => (QSource::Qn, 0.0),
                    (true, true, false) => (QSource::InvQ, a_inv),
                    _ => continue,
                };
                let q_free_neg = match q_true {
                    QSource::Q => c.qn.is_some(),
                    QSource::Qn => c.q.is_some(),
                    QSource::InvQ | QSource::InvQn => true,
                };
                let logic_inv = |l: Lit| if d.aig.is_and(l.node()) { 1e-6 } else { a_inv };
                let score = c.area + q_cost - saved
                    + cost(data, logic_inv(data))
                    + cost(clock, a_inv)
                    + enable.map_or(0.0, |l| cost(l, logic_inv(l)))
                    + hold.map_or(0.0, |l| cost(l, if q_free_neg { 0.0 } else { a_inv }))
                    + clear.map_or(0.0, |l| cost(l, a_inv))
                    + preset.map_or(0.0, |l| cost(l, a_inv))
                    + if s.arst.is_none() && (c.clear.is_some() || c.preset.is_some()) {
                        1e-6
                    } else {
                        0.0
                    };
                if best.as_ref().is_none_or(|(old, _)| score < *old - 1e-12) {
                    best = Some((
                        score,
                        SeqChoice {
                            cell: ci,
                            data,
                            enable,
                            hold,
                            clock,
                            clear,
                            preset,
                            q_true,
                            q_free_neg,
                        },
                    ));
                }
            }
        }
    }
    best.map(|(_, choice)| choice).ok_or_else(|| {
        format!(
            "no usable library cell implements state '{}' (SYNTH-REQ-0520)",
            s.name
        )
    })
}

pub(super) fn select(
    d: &Design,
    lib: &CellLib,
    limits: SeqLimits,
) -> Result<Vec<SeqChoice>, String> {
    let choices = candidates(lib);
    let bytes = d.states.len().saturating_mul(size_of::<SeqChoice>());
    let trials = d.states.len() as u128 * choices.len() as u128 * 18;
    if d.states.len() > limits.max_states.min(32_000_000)
        || bytes > limits.max_choice_bytes
        || trials > u128::from(limits.max_trials)
    {
        return Err("sequential selection storage/work budget reached (SYNTH-REQ-0520)".into());
    }
    let mux = 0xD8u64;
    let mux_area = lib
        .matches(3, mux)
        .iter()
        .map(|m| lib.comb[m.cell as usize].area)
        .min_by(f64::total_cmp)
        .unwrap_or_else(|| {
            3.0 * lib
                .comb
                .iter()
                .filter(|c| c.n_inputs == 2)
                .map(|c| c.area)
                .min_by(f64::total_cmp)
                .unwrap_or(0.0)
        });
    let mut out = Vec::with_capacity(d.states.len());
    for batch in d.states.chunks(8192) {
        let selected: Vec<_> = if rayon::current_num_threads() == 1 || batch.len() < 64 {
            batch
                .iter()
                .map(|s| choose(d, s, lib, &choices, mux_area))
                .collect()
        } else {
            batch
                .par_iter()
                .with_min_len(64)
                .map(|s| choose(d, s, lib, &choices, mux_area))
                .collect()
        };
        for choice in selected {
            out.push(choice?);
        }
    }
    Ok(out)
}
