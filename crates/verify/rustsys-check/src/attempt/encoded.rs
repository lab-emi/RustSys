// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Re-encoded registers under a state map (obligation export only). Synthesis may recode an RTL
//! register `V` (for example a binary FSM state into one-hot elements) and drop codes that are
//! unreachable from reset. Each element is then defined from the RTL register: the OR, over the
//! valid codes whose element code has its bit set, of `V` holding that RTL code. The map covers
//! only valid RTL codes, so the valid-code condition `V ∈ C` is an invariant that the exported
//! obligations prove, never an assumption:
//! - base: the RTL reset code is valid (`invariant-base`) and each element's asynchronous
//!   controls force the element code of the reset code (`encoded-async-*`);
//! - step: from a valid code, one transition outside reset reaches a valid code
//!   (`invariant-step`), and each element's next value is its bit of the next code
//!   (`encoded-next-state`).
//!
//! Every obligation quantifies the register only over its valid codes: the register's RTL bits
//! are defined as the canonical code `valid(p) ? p : reset code` of fresh inputs `p`, which the
//! export quantifies instead. "For all `p`" over the canonical code is exactly "for all valid
//! codes" (the map is onto the valid codes and the identity on them), and the logic stays equal
//! everywhere, so an external CEC can merge internal equivalences. The step obligation reads the
//! canonical code too, so it proves "valid code and one transition give a valid code".
//!
//! By induction from reset, each implementation run from reset is then the image of the RTL run
//! from reset under the map; both reset deterministically, so their outputs agree for every input
//! sequence from reset (equivalence from reset). Implementation states outside the image
//! (invalid element codes) are not covered: power-up without reset is out of scope.

use std::collections::{HashMap, HashSet};

use super::Obligation;
use crate::Stop;
use crate::circ::{B, Circuit};
use crate::corr::{Corr, Encoding};
use crate::netlist::NetModel;
use crate::rtl::RtlModel;

/// The literal that bits `bits` of `values` hold RTL code `code`.
fn holds(c: &mut Circuit, values: &[B], code: u64) -> B {
    let mut out = B::T;
    for (b, &v) in values.iter().enumerate() {
        let lit = if (code >> b) & 1 == 1 { v } else { c.not(v) };
        out = c.and(out, lit);
    }
    out
}

/// The valid-code condition of `enc` over RTL register values `values`.
fn valid(c: &mut Circuit, enc: &Encoding, values: &[B]) -> B {
    let mut out = B::F;
    for &(rtl, _) in &enc.codes {
        let m = holds(c, values, rtl);
        out = c.or(out, m);
    }
    out
}

/// Element `i` of `enc` as a function of RTL register values `values`.
fn element(c: &mut Circuit, enc: &Encoding, values: &[B], i: usize) -> B {
    let mut out = B::F;
    for &(rtl, gate) in &enc.codes {
        if (gate >> i) & 1 == 1 {
            let m = holds(c, values, rtl);
            out = c.or(out, m);
        }
    }
    out
}

/// The RTL reset code of `enc` and its reset condition; `None` if a bit is not asynchronously
/// reset by one shared condition (no base case without a reset).
pub(super) fn reset(model: &RtlModel, enc: &Encoding) -> Option<(B, u64)> {
    let (cond, _) = model.states[*enc.rtl.first()?].arst?;
    let mut code = 0;
    for (b, &si) in enc.rtl.iter().enumerate() {
        let (c, v) = model.states[si].arst?;
        if c != cond {
            return None;
        }
        code |= u64::from(v) << b;
    }
    Some((cond, code))
}

/// Defines each register with a valid-code invariant as the canonical code of fresh inputs, and
/// every element of a re-encoded register from it, and pushes the base, step and element
/// obligations. Returns the fresh inputs of each register (in `corr.encodings` order).
pub(super) fn define(
    c: &mut Circuit,
    model: &RtlModel,
    nm: &NetModel,
    corr: &Corr,
    obs: &mut Vec<Obligation>,
) -> Result<Vec<Vec<u32>>, Stop> {
    let mut inputs = Vec::with_capacity(corr.encodings.len());
    for enc in &corr.encodings {
        if enc.rtl.is_empty() || enc.rtl.len() > 16 || enc.gate.len() > 64 || enc.codes.is_empty() {
            return Err(Stop::Internal(format!(
                "re-encoded register {} outside its size bounds",
                enc.variable
            )));
        }
        let Some((cond, code0)) = reset(model, enc) else {
            return Err(Stop::Undecided(format!(
                "re-encoded register {} has no shared asynchronous reset: no base case",
                enc.variable
            )));
        };
        let first = &model.states[enc.rtl[0]];
        if enc.rtl.iter().any(|&si| {
            let st = &model.states[si];
            st.latch || st.clk != first.clk
        }) {
            return Err(Stop::Undecided(format!(
                "re-encoded register {} is not one clocked register",
                enc.variable
            )));
        }
        // The register's RTL bits become the canonical code of fresh inputs.
        let fresh: Vec<u32> = enc.rtl.iter().map(|_| c.free()).collect();
        let fresh_values: Vec<B> = fresh.iter().map(|&p| B::V(p)).collect();
        let fresh_valid = valid(c, enc, &fresh_values);
        for (b, &si) in enc.rtl.iter().enumerate() {
            let canonical = c.mux(
                fresh_valid,
                fresh_values[b],
                B::from_bool((code0 >> b) & 1 == 1),
            );
            if !c.define(model.states[si].q, canonical) {
                return Err(Stop::Internal(format!(
                    "bit {} of register {} with a valid-code invariant is already defined",
                    model.states[si].name, enc.variable
                )));
            }
        }
        let now: Vec<B> = enc.rtl.iter().map(|&si| B::V(model.states[si].q)).collect();
        let next: Vec<B> = enc.rtl.iter().map(|&si| model.states[si].next).collect();
        let reset_values: Vec<B> = (0..enc.rtl.len())
            .map(|b| B::from_bool((code0 >> b) & 1 == 1))
            .collect();
        let valid_next = valid(c, enc, &next);
        let valid_reset = valid(c, enc, &reset_values);
        let outside_reset = c.not(cond);
        // Base: the reset code is valid; the elements' asynchronous controls force its code.
        obs.push(Obligation {
            kind: "invariant-base",
            name: enc.variable.clone(),
            a: valid_reset,
            b: B::T,
            assume: B::T,
        });
        // Step: a valid code stays valid over one transition outside reset.
        obs.push(Obligation {
            kind: "invariant-step",
            name: enc.variable.clone(),
            a: valid_next,
            b: B::T,
            assume: outside_reset,
        });
        let gate0 = enc.encode(code0).unwrap_or(0);
        for (i, &ci) in enc.gate.iter().enumerate() {
            let cell = nm.seq[ci].clone();
            if cell.latch {
                return Err(Stop::Undecided(format!(
                    "re-encoded register {} has a latch element {}",
                    enc.variable, cell.inst
                )));
            }
            let value = element(c, enc, &now, i);
            if !c.define(cell.iq, value) {
                return Err(Stop::Internal(format!(
                    "element {} of re-encoded register {} is already defined",
                    cell.inst, enc.variable
                )));
            }
            let name = format!("{} element {}", enc.variable, cell.inst);
            obs.push(Obligation {
                kind: "encoded-clock",
                name: name.clone(),
                a: first.clk,
                b: cell.clk,
                assume: B::T,
            });
            let (want0, want1) = if (gate0 >> i) & 1 == 1 {
                (B::F, cond)
            } else {
                (cond, B::F)
            };
            obs.push(Obligation {
                kind: "encoded-async-clear",
                name: name.clone(),
                a: want0,
                b: cell.clear,
                assume: B::T,
            });
            obs.push(Obligation {
                kind: "encoded-async-preset",
                name: name.clone(),
                a: want1,
                b: cell.preset,
                assume: B::T,
            });
            let next_value = element(c, enc, &next, i);
            obs.push(Obligation {
                kind: "encoded-next-state",
                name,
                a: next_value,
                b: cell.next,
                assume: outside_reset,
            });
        }
        inputs.push(fresh);
    }
    Ok(inputs)
}

/// Checks that each element of each re-encoded register in `c` is its bit of the element code
/// for every valid RTL code (the record the export writes is the definition it proves).
pub(super) fn check(c: &Circuit, nm: &NetModel, enc: &Encoding, inputs: &[u32]) -> bool {
    let rtl = inputs;
    let roots: Vec<B> = enc.gate.iter().map(|&ci| B::V(nm.seq[ci].iq)).collect();
    let Ok(order) = c.order(&roots) else {
        return false;
    };
    // Lane k holds valid code k (at most 64 codes are checked in one evaluation).
    if enc.codes.len() > 64 {
        return false;
    }
    let mut words = vec![0u64; rtl.len()];
    for (k, &(code, _)) in enc.codes.iter().enumerate() {
        for (b, w) in words.iter_mut().enumerate() {
            *w |= ((code >> b) & 1) << k;
        }
    }
    let mut vals = HashMap::new();
    c.eval(
        &order,
        &|v| rtl.iter().position(|&q| q == v).map_or(0, |b| words[b]),
        &mut vals,
    );
    enc.gate.iter().enumerate().all(|(i, &ci)| {
        let w = Circuit::value(B::V(nm.seq[ci].iq), &vals);
        enc.codes
            .iter()
            .enumerate()
            .all(|(k, &(_, gate))| (w >> k) & 1 == (gate >> i) & 1)
    })
}

/// Simulation rounds of 64 lanes that look for obligations needing a candidate invariant.
const ROUNDS: usize = 256;

/// The candidate invariants (indices, ascending) that refuted obligations need: random,
/// mostly-one and mostly-zero values of the free variables, as in discovery. A candidate
/// explains an obligation when every lane that refutes it violates the candidate (the lanes lie
/// outside a code set that exploration found closed); the fewest candidates that explain the
/// explained obligations are chosen greedily. Each joins the proof with its own base and step
/// obligations, so a wrong choice fails a proof, never passes one.
pub(super) fn needed(
    c: &Circuit,
    obs: &[Obligation],
    order: &[u32],
    frees: &[u32],
    model: &RtlModel,
    candidates: &[Encoding],
    seed: u64,
) -> Vec<usize> {
    let index: HashMap<u32, usize> = frees.iter().enumerate().map(|(i, v)| (*v, i)).collect();
    let ordered: HashSet<u32> = order.iter().copied().collect();
    // Candidates whose bits are all quantified (a bit that the state map defines cannot be
    // restricted to a code set) and read by some obligation.
    let usable: Vec<usize> = (0..candidates.len())
        .filter(|&k| {
            candidates[k].rtl.iter().all(|&si| {
                let q = model.states[si].q;
                index.contains_key(&q) && c.is_free(q)
            }) && candidates[k]
                .rtl
                .iter()
                .any(|&si| ordered.contains(&model.states[si].q))
        })
        .collect();
    let mut rng = crate::prove::Rng(seed ^ 0x1a7a_11d5);
    // Per refuted obligation: which usable candidates are violated in all its refuting lanes.
    let mut explains: HashMap<usize, Vec<bool>> = HashMap::new();
    let mut vals = Vec::new();
    let mut words = vec![0u64; frees.len()];
    for round in 0..ROUNDS {
        for w in &mut words {
            let (a, b, c) = (rng.next(), rng.next(), rng.next());
            *w = match round % 4 {
                1 => a | b | c,
                3 => a & b & c,
                _ => a,
            };
        }
        c.eval_dense(
            order,
            &|v| index.get(&v).map_or(0, |&i| words[i]),
            &mut vals,
        );
        let inside: Vec<u64> = usable
            .iter()
            .map(|&k| {
                let enc = &candidates[k];
                let bits: Vec<u64> = enc
                    .rtl
                    .iter()
                    .map(|&si| {
                        let q = model.states[si].q;
                        index
                            .get(&q)
                            .map_or_else(|| Circuit::value_dense(B::V(q), &vals), |&i| words[i])
                    })
                    .collect();
                enc.codes.iter().fold(0u64, |acc, &(code, _)| {
                    acc | bits.iter().enumerate().fold(u64::MAX, |m, (b, w)| {
                        m & if (code >> b) & 1 == 1 { *w } else { !*w }
                    })
                })
            })
            .collect();
        for (i, o) in obs.iter().enumerate() {
            let bad = Circuit::value_dense(o.assume, &vals)
                & (Circuit::value_dense(o.a, &vals) ^ Circuit::value_dense(o.b, &vals));
            if bad == 0 {
                continue;
            }
            let entry = explains
                .entry(i)
                .or_insert_with(|| vec![true; usable.len()]);
            for (j, w) in inside.iter().enumerate() {
                if bad & w != 0 {
                    entry[j] = false;
                }
            }
        }
    }
    // Greedy cover of the explained obligations.
    let mut open: Vec<&Vec<bool>> = explains.values().filter(|e| e.iter().any(|x| *x)).collect();
    let mut chosen = Vec::new();
    while !open.is_empty() {
        let best = (0..usable.len())
            .max_by_key(|&j| (open.iter().filter(|e| e[j]).count(), std::cmp::Reverse(j)))
            .expect("an explained obligation has a candidate");
        chosen.push(usable[best]);
        open.retain(|e| !e[best]);
    }
    chosen.sort_unstable();
    chosen
}
