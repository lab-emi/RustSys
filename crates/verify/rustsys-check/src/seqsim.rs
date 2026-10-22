// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Sequential simulation of both models over clock cycles (CHECK-REQ-0045): 64 random runs in
//! parallel, one per bit of a word.

use serde::Serialize;

use crate::Mismatch;
use crate::circ::{B, Circuit, Def};
use crate::netlist::NetModel;
use crate::prove::Rng;
use crate::rtl::RtlModel;

/// Outcome of the sequential simulation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct SeqSimReport {
    /// Cycles simulated (0 when not requested or skipped).
    pub cycles: usize,
    /// Why the simulation was skipped.
    pub skipped: Option<String>,
}

/// The free variable behind a bit through aliases and inversions, and whether it is inverted.
fn literal(c: &Circuit, b: B) -> Option<(u32, bool)> {
    let B::V(mut v) = b else { return None };
    let mut inv = false;
    loop {
        match c.defs[v as usize] {
            Def::Free => return Some((v, inv)),
            Def::Alias(B::V(x)) => v = x,
            Def::Not(x) => {
                inv = !inv;
                v = x;
            }
            _ => return None,
        }
    }
}

fn skipped(why: &str) -> (SeqSimReport, Vec<Mismatch>) {
    (
        SeqSimReport {
            cycles: 0,
            skipped: Some(why.into()),
        },
        Vec::new(),
    )
}

/// Asynchronous forcing of a state word: `force0` and `force1` are the lane masks that force the
/// value to 0 and to 1.
fn force(q: u64, force0: u64, force1: u64) -> u64 {
    (q & !(force0 | force1)) | (force1 & !force0)
}

/// Simulates both models for `cycles` cycles. `paired[i]` is the netlist state element of RTL
/// state `i` and whether its value is inverted. Each cycle applies new random inputs with the
/// clock low, then raises the clock (rising-edge state elements update) and lowers it at the end
/// (falling-edge elements update); outputs are compared in both phases. Asynchronous controls
/// driven by a primary input are active in the first cycle and then rarely (one lane in 64 on
/// average).
pub fn simulate(
    c: &Circuit,
    model: &RtlModel,
    nm: &NetModel,
    paired: &[Option<(usize, bool)>],
    pulse: &std::collections::HashMap<u32, bool>,
    initial_choices: &std::collections::HashMap<u32, bool>,
    cycles: usize,
    seed: u64,
) -> (SeqSimReport, Vec<Mismatch>) {
    if cycles == 0 {
        return (SeqSimReport::default(), Vec::new());
    }
    if model.states.iter().any(|s| s.latch) || nm.seq.iter().any(|s| s.latch) {
        return skipped("latches are not simulated");
    }
    let inputs: Vec<(String, u32)> = model
        .inputs
        .iter()
        .flat_map(|(n, vs, scalar)| {
            vs.iter().enumerate().map(move |(k, v)| {
                (
                    if *scalar {
                        n.clone()
                    } else {
                        format!("{n}[{k}]")
                    },
                    *v,
                )
            })
        })
        .collect();
    let is_input = |v: u32| inputs.iter().any(|(_, x)| *x == v);
    // One clock domain: every clock is the same primary input, either polarity.
    let mut clock: Option<u32> = None;
    let mut rtl_inv = Vec::with_capacity(model.states.len());
    let mut net_inv = Vec::with_capacity(nm.seq.len());
    for (clk, out) in model
        .states
        .iter()
        .map(|s| (s.clk, 0))
        .chain(nm.seq.iter().map(|s| (s.clk, 1)))
    {
        let Some((v, inv)) = literal(c, clk) else {
            return skipped("a clock is not a primary input");
        };
        if !is_input(v) || clock.is_some_and(|k| k != v) {
            return skipped("more than one clock domain");
        }
        clock = Some(v);
        if out == 0 {
            rtl_inv.push(inv);
        } else {
            net_inv.push(inv);
        }
    }
    // Primary inputs that drive asynchronous controls, with their active value.
    if clock.is_some_and(|v| pulse.contains_key(&v)) {
        return skipped("the reset pulse also drives the simulation clock");
    }
    let mut async_in: Vec<(u32, bool)> = Vec::new();
    let conds = model
        .states
        .iter()
        .filter_map(|s| s.arst.map(|(b, _)| b))
        .chain(nm.seq.iter().flat_map(|s| [s.clear, s.preset]));
    for b in conds {
        if let Some((v, inv)) = literal(c, b) {
            if is_input(v) && Some(v) != clock && !async_in.iter().any(|(x, _)| *x == v) {
                async_in.push((v, !inv));
            }
        }
    }
    // Everything the simulation evaluates.
    let mut roots: Vec<B> = Vec::new();
    for (_, bits, _) in &model.outputs {
        roots.extend(bits);
    }
    for bits in nm.outputs.values() {
        roots.extend(bits);
    }
    for s in &model.states {
        roots.push(s.next);
        if let Some((b, _)) = s.arst {
            roots.push(b);
        }
    }
    for s in &nm.seq {
        roots.extend([s.next, s.clear, s.preset]);
    }
    let Ok(order) = c.order(&roots) else {
        return skipped("combinational cycle");
    };
    let mut rng = Rng(seed ^ 0x5eed_5eed_5eed_5eed);
    // Common random initial state.
    let mut rq: Vec<u64> = model.states.iter().map(|_| rng.next()).collect();
    for (s, q) in model.states.iter().zip(&mut rq) {
        if let Some(v) = initial_choices.get(&s.q) {
            *q = if *v { u64::MAX } else { 0 };
        }
    }
    let mut nq: Vec<u64> = nm.seq.iter().map(|_| rng.next()).collect();
    let mut unreset_groups = std::collections::HashMap::<usize, (usize, bool)>::new();
    for (si, p) in paired.iter().enumerate() {
        if let Some((ci, parity)) = p {
            if model.states[si].arst.is_none() {
                if let Some(&(ri, phase)) = unreset_groups.get(ci) {
                    rq[si] = rq[ri] ^ if *parity ^ phase { u64::MAX } else { 0 };
                } else {
                    unreset_groups.insert(*ci, (si, *parity));
                }
            }
        }
        if let Some((ci, parity)) = p {
            nq[*ci] = if *parity { !rq[si] } else { rq[si] };
        }
    }
    let mut free = vec![0u64; c.defs.len()];
    let mut vals: Vec<u64> = Vec::new();
    let mut mismatches = Vec::new();
    for cycle in 0..cycles {
        for (_, v) in &inputs {
            free[*v as usize] = rng.next();
        }
        for (v, active) in &async_in {
            let on = if cycle == 0 {
                u64::MAX
            } else {
                (0..6).fold(u64::MAX, |m, _| m & rng.next())
            };
            free[*v as usize] = if *active { on } else { !on };
        }
        if cycle == 0 {
            for (v, value) in pulse {
                free[*v as usize] = if *value { u64::MAX } else { 0 };
            }
        }
        for phase in 0..2 {
            if let Some(k) = clock {
                free[k as usize] = if phase == 0 { 0 } else { u64::MAX };
            }
            // State values are free variables of the circuit.
            let eval = |free: &mut Vec<u64>, rq: &[u64], nq: &[u64], vals: &mut Vec<u64>| {
                for (s, q) in model.states.iter().zip(rq) {
                    free[s.q as usize] = *q;
                }
                for (s, q) in nm.seq.iter().zip(nq) {
                    free[s.iq as usize] = *q;
                }
                c.eval_dense(&order, &|v| free[v as usize], vals);
            };
            // Asynchronous controls act while they are active.
            eval(&mut free, &rq, &nq, &mut vals);
            for (s, q) in model.states.iter().zip(rq.iter_mut()) {
                if let Some((b, val)) = s.arst {
                    let m = Circuit::value_dense(b, &vals);
                    *q = if val {
                        force(*q, 0, m)
                    } else {
                        force(*q, m, 0)
                    };
                }
            }
            for (s, q) in nm.seq.iter().zip(nq.iter_mut()) {
                let (f0, f1) = (
                    Circuit::value_dense(s.clear, &vals),
                    Circuit::value_dense(s.preset, &vals),
                );
                *q = force(*q, f0, f1);
            }
            eval(&mut free, &rq, &nq, &mut vals);
            for (name, bits, scalar) in &model.outputs {
                let Some(nb) = nm.outputs.get(name) else {
                    continue;
                };
                for (k, (a, b)) in bits.iter().zip(nb).enumerate() {
                    let diff = Circuit::value_dense(*a, &vals) ^ Circuit::value_dense(*b, &vals);
                    if diff != 0 && mismatches.len() < 8 {
                        let lane = diff.trailing_zeros();
                        let bit = if *scalar {
                            name.clone()
                        } else {
                            format!("{name}[{k}]")
                        };
                        mismatches.push(Mismatch {
                            kind: "sequential".into(),
                            name: format!("{bit} in cycle {cycle}, clock phase {phase}"),
                            counterexample: inputs
                                .iter()
                                .map(|(n, v)| (n.clone(), (free[*v as usize] >> lane) & 1 == 1))
                                .collect(),
                        });
                    }
                }
            }
            if !mismatches.is_empty() {
                return (
                    SeqSimReport {
                        cycles: cycle + 1,
                        skipped: None,
                    },
                    mismatches,
                );
            }
            // Clock edge at the end of the phase: rising after the low phase, falling after the
            // high phase. Values sampled before the edge; active asynchronous controls win.
            let edge_inv = phase == 1;
            let next_r: Vec<(usize, u64)> = model
                .states
                .iter()
                .enumerate()
                .filter(|(i, _)| rtl_inv[*i] == edge_inv)
                .map(|(i, s)| {
                    let hold = s.arst.map_or(0, |(b, _)| Circuit::value_dense(b, &vals));
                    let n = Circuit::value_dense(s.next, &vals);
                    (i, (rq[i] & hold) | (n & !hold))
                })
                .collect();
            let next_n: Vec<(usize, u64)> = nm
                .seq
                .iter()
                .enumerate()
                .filter(|(i, _)| net_inv[*i] == edge_inv)
                .map(|(i, s)| {
                    let hold = Circuit::value_dense(s.clear, &vals)
                        | Circuit::value_dense(s.preset, &vals);
                    let n = Circuit::value_dense(s.next, &vals);
                    (i, (nq[i] & hold) | (n & !hold))
                })
                .collect();
            for (i, v) in next_r {
                rq[i] = v;
            }
            for (i, v) in next_n {
                nq[i] = v;
            }
        }
    }
    (
        SeqSimReport {
            cycles,
            skipped: None,
        },
        mismatches,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forcing_gives_priority_to_clear() {
        assert_eq!(force(0b1010, 0b0011, 0b0101), 0b1100);
    }

    #[test]
    fn literal_follows_inversions() {
        let mut c = Circuit::new();
        let v = c.free();
        let n = c.not(B::V(v));
        assert_eq!(literal(&c, n), Some((v, true)));
        assert_eq!(literal(&c, B::V(v)), Some((v, false)));
        let w = c.free();
        let a = c.and(B::V(v), B::V(w));
        assert_eq!(literal(&c, a), None);
    }
}
