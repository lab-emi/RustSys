// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! One proof attempt under a fixed state correspondence: sequential simulation
//! (CHECK-REQ-0045), the joint induction hypothesis, the obligations of CHECK-REQ-0040 and
//! CHECK-REQ-0041, random simulation (CHECK-REQ-0043), sweeping (CHECK-REQ-0046) and SAT
//! (CHECK-REQ-0042) with counterexample replay (CHECK-REQ-0044).

use std::collections::HashMap;

use rusteda_core::job::{Cancelled, Ctx};

use crate::circ::{B, Circuit};
use crate::corr::Corr;
use crate::netlist::NetModel;
use crate::prove::{Proof, Rng};
use crate::rtl::RtlModel;
use crate::{CheckOptions, Mismatch, Stop, seqsim, sweep};

mod export;

/// One proof obligation: `a == b` whenever `assume`.
struct Obligation {
    kind: &'static str,
    name: String,
    a: B,
    b: B,
    assume: B,
}

/// Results of a completed attempt.
pub struct Proved {
    pub outputs_checked: usize,
    pub pairs: usize,
    pub mismatches: Vec<Mismatch>,
    /// Obligations whose proof ran out of its conflict budget (CHECK-REQ-0002).
    pub undecided: Vec<String>,
    pub sweep: sweep::SweepStats,
    pub seq_sim: seqsim::SeqSimReport,
}

/// How an attempt ended.
pub enum Outcome {
    /// Random simulation refuted an obligation while RTL state without a counterpart was still
    /// cut free and discovery had not run yet (CHECK-REQ-0032); nothing was proved.
    CutPoints,
    Proved(Proved),
}

/// Inputs of an attempt that do not change between attempts.
pub struct Models<'a> {
    pub model: &'a RtlModel,
    pub nm: &'a NetModel,
    pub pulse: &'a HashMap<u32, bool>,
}

/// Runs one attempt on `c`, which it constrains with the correspondence. `mark` ends a phase and
/// starts the next (CHECK-REQ-0080); `phase` writes a verbose note.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn prove(
    c: &mut Circuit,
    m: &Models<'_>,
    corr: &Corr,
    opts: &CheckOptions,
    stop_at_cut_points: bool,
    mark: &mut dyn FnMut(&'static str, Option<&'static str>) -> Result<(), Cancelled>,
    phase: &dyn Fn(&str),
    ctx: Ctx<'_>,
) -> Result<Outcome, Stop> {
    let (model, nm) = (m.model, m.nm);
    let mut mismatches = Vec::new();
    // Sequential simulation while both models still have their own state (CHECK-REQ-0045).
    let (seq_sim, seq_mismatches) = seqsim::simulate(
        c,
        model,
        nm,
        &corr.paired,
        m.pulse,
        &corr.initial_choices,
        opts.cycles,
        opts.seed,
    );
    mismatches.extend(seq_mismatches);
    if opts.cycles > 0 {
        phase(&format!("sequential simulation: {seq_sim:?}"));
    }
    // Shared-state reset bases were checked on the original circuit; unreset sharing is an
    // explicit initial-state choice. Each member still has clock, async and next obligations.
    for &(si, ri, inv) in &corr.equivalents {
        let value = if inv {
            c.not(B::V(model.states[ri].q))
        } else {
            B::V(model.states[ri].q)
        };
        assert!(c.define(model.states[si].q, value));
    }
    // From here on, a netlist state value is the corresponding RTL state value.
    for (si, st) in model.states.iter().enumerate() {
        if let Some(value) = corr.constants[si] {
            // The pulse or the authorized, otherwise unspecified initial choice establishes
            // each base case before the joint induction hypothesis.
            assert!(c.define(st.q, B::from_bool(value)));
        }
        if let Some((ci, parity)) = corr.paired[si] {
            if corr.owners[ci] != Some(si) {
                continue;
            }
            let val = if parity {
                c.not(B::V(st.q))
            } else {
                B::V(st.q)
            };
            c.define(nm.seq[ci].iq, val);
        }
    }
    // Obligations.
    let mut obs: Vec<Obligation> = Vec::new();
    let mut outputs_checked = 0;
    for (name, bits, scalar) in &model.outputs {
        let Some(nb) = nm.outputs.get(name) else {
            continue;
        };
        if nb.len() != bits.len() {
            continue;
        }
        for (k, (a, b)) in bits.iter().zip(nb).enumerate() {
            let n = if *scalar {
                name.clone()
            } else {
                format!("{name}[{k}]")
            };
            obs.push(Obligation {
                kind: "output",
                name: n,
                a: *a,
                b: *b,
                assume: B::T,
            });
            outputs_checked += 1;
        }
    }
    let mut pairs = 0;
    for (si, st) in model.states.iter().enumerate() {
        if let Some(value) = corr.constants[si] {
            obs.push(Obligation {
                kind: if st.arst.is_some() {
                    "reset-invariant"
                } else {
                    "initial-invariant"
                },
                name: st.name.clone(),
                a: st.next,
                b: B::from_bool(value),
                assume: st.arst.map_or(B::T, |(r, _)| c.not(r)),
            });
            continue;
        }
        let Some((ci, parity)) = corr.paired[si] else {
            continue;
        };
        pairs += 1;
        let cell = nm.seq[ci].clone();
        if cell.latch != st.latch {
            mismatches.push(Mismatch {
                kind: "state-kind".into(),
                name: st.name.clone(),
                counterexample: vec![],
            });
            continue;
        }
        obs.push(Obligation {
            kind: if st.latch { "enable" } else { "clock" },
            name: st.name.clone(),
            a: st.clk,
            b: cell.clk,
            assume: B::T,
        });
        // Asynchronous behavior in terms of the true value.
        let (force0, force1) = if parity {
            (cell.preset, cell.clear)
        } else {
            (cell.clear, cell.preset)
        };
        let (want0, want1) = match st.arst {
            None => (B::F, B::F),
            Some((cond, false)) => (cond, B::F),
            Some((cond, true)) => (B::F, cond),
        };
        obs.push(Obligation {
            kind: "async-clear",
            name: st.name.clone(),
            a: want0,
            b: force0,
            assume: B::T,
        });
        obs.push(Obligation {
            kind: "async-preset",
            name: st.name.clone(),
            a: want1,
            b: force1,
            assume: B::T,
        });
        let net_next = if parity { c.not(cell.next) } else { cell.next };
        let mut assume = match st.arst {
            Some((cond, _)) => c.not(cond),
            None => B::T,
        };
        if st.latch {
            assume = c.and(assume, st.clk);
        }
        obs.push(Obligation {
            kind: if st.latch { "data" } else { "next-state" },
            name: st.name.clone(),
            a: st.next,
            b: net_next,
            assume,
        });
    }
    // These supplement every original obligation; no observed equality is
    // assumed. A later external composition must prove every observation.
    for (probe, bits) in &model.observations {
        let gate = nm.nets.get(&probe.name).ok_or_else(|| {
            Stop::Undecided(format!("netlist observation {} not found", probe.name))
        })?;
        let end = probe
            .offset
            .checked_add(probe.width)
            .ok_or("observation offset overflow")?;
        let gate = gate.get(probe.offset..end).ok_or_else(|| {
            Stop::Undecided(format!(
                "netlist observation {} slice is unavailable",
                probe.name
            ))
        })?;
        for (index, (&a, &b)) in bits.iter().zip(gate).enumerate() {
            obs.push(Obligation {
                kind: "observation",
                name: format!("{}[{}]", probe.name, probe.offset + index),
                a,
                b: B::V(b),
                assume: B::T,
            });
        }
    }
    // Structure: every free variable in the compared cones must be an input or a state value.
    let mut roots = Vec::new();
    for o in &obs {
        roots.extend([o.a, o.b, o.assume]);
    }
    let order = c.order(&roots).map_err(|cy| {
        format!(
            "combinational cycle (CHECK-REQ-0020) through circuit variable {}",
            cy.0
        )
    })?;
    let mut names: HashMap<u32, String> = HashMap::new();
    for (n, v, s) in &model.inputs {
        for (k, x) in v.iter().enumerate() {
            names.insert(*x, if *s { n.clone() } else { format!("{n}[{k}]") });
        }
    }
    for st in &model.states {
        names.insert(st.q, st.name.clone());
    }
    // An element without a counterpart is cut free: its obligations' outcomes are only
    // conservative, and the attempt serves to reject discovered relations (CHECK-REQ-0032).
    for &ci in &corr.unused {
        names.insert(nm.seq[ci].iq, nm.seq[ci].inst.clone());
    }
    for v in &order {
        if c.is_free(*v) && !names.contains_key(v) {
            let net = nm
                .nets
                .iter()
                .find(|(_, vs)| vs.contains(v))
                .map(|(n, _)| n.clone());
            return Err(Stop::from(match net {
                Some(n) => format!("netlist net {n} is used but not driven (CHECK-REQ-0020)"),
                None => format!("undriven value in the compared logic (circuit variable {v})"),
            }));
        }
    }
    let mut frees: Vec<u32> = order.iter().copied().filter(|v| c.is_free(*v)).collect();
    if let Some(options) = &opts.obligation_export {
        if !opts.after_reset.is_empty()
            || opts.uninitialized_dont_care
            || opts.assume_full_case
            || opts.assume_parallel_case
            || !corr.unused.is_empty()
            || corr.constants.iter().any(Option::is_some)
            || !corr.equivalents.is_empty()
            || corr
                .discovered
                .iter()
                .any(|(_, relation)| !matches!(relation, crate::corr::Relation::Cell(..)))
            || !mismatches.is_empty()
            || pairs != nm.seq.len()
        {
            return Err(Stop::Undecided("obligation export requires a complete one-to-one state correspondence with no reset, initial-state choices, constants or sharing relations".into()));
        }
        // Quantify even unused RTL inputs/state in the exported interface.
        // Each implementation state has its own named RTL representative.
        frees.extend(
            model
                .inputs
                .iter()
                .flat_map(|(_, bits, _)| bits.iter())
                .copied(),
        );
        frees.extend(model.states.iter().map(|state| state.q));
        frees.sort_unstable();
        frees.dedup();
        if frees.iter().any(|&v| !c.is_free(v)) {
            return Err(Stop::Internal(
                "export interface contains a constrained RTL input/state".into(),
            ));
        }
        export::write(
            c,
            &obs,
            &frees,
            options,
            outputs_checked,
            pairs,
            model.states.len(),
            nm.seq.len(),
            model,
            nm,
            corr,
        )?;
        return Err(Stop::Undecided(
            "all output/control/next-state obligations exported; external CEC has not been run"
                .into(),
        ));
    }
    phase(&format!(
        "{} obligations over {} circuit variables, {} free",
        obs.len(),
        order.len(),
        frees.len()
    ));
    // Random simulation (CHECK-REQ-0043).
    let mut rng = Rng(opts.seed);
    let mut failed = vec![false; obs.len()];
    let mut vals = Vec::new();
    for _ in 0..opts.sim_rounds {
        let words: HashMap<u32, u64> = frees.iter().map(|v| (*v, rng.next())).collect();
        c.eval_dense(&order, &|v| words.get(&v).copied().unwrap_or(0), &mut vals);
        for (i, o) in obs.iter().enumerate() {
            if failed[i] {
                continue;
            }
            let bad = Circuit::value_dense(o.assume, &vals)
                & (Circuit::value_dense(o.a, &vals) ^ Circuit::value_dense(o.b, &vals));
            if bad != 0 {
                failed[i] = true;
                let k = bad.trailing_zeros();
                let cex = frees
                    .iter()
                    .map(|v| (names[v].clone(), (words[v] >> k) & 1 == 1))
                    .collect();
                mismatches.push(Mismatch {
                    kind: o.kind.into(),
                    name: o.name.clone(),
                    counterexample: cex,
                });
            }
        }
    }
    phase(&format!(
        "simulated; {} obligations failed",
        failed.iter().filter(|f| **f).count()
    ));
    if stop_at_cut_points && failed.iter().any(|f| *f) {
        return Ok(Outcome::CutPoints);
    }
    // SAT proofs (CHECK-REQ-0042) on the swept circuit (CHECK-REQ-0046) with counterexample
    // replay (CHECK-REQ-0044).
    mark("simulate", Some("sweep"))?;
    let open: Vec<usize> = (0..obs.len())
        .filter(|i| !failed[*i] && obs[*i].a != obs[*i].b)
        .collect();
    let mut sweep = if open.is_empty() {
        None
    } else {
        Some(sweep::Sweep::run(
            c,
            &order,
            opts.sweep_rounds,
            opts.seed,
            opts.lemma_budget,
            opts.sweep_max_candidates,
            opts.verbose,
            ctx,
        )?)
    };
    phase(&format!(
        "swept: {:?}",
        sweep.as_ref().map(|s| s.stats).unwrap_or_default()
    ));
    mark("sweep", Some("prove"))?;
    let mut undecided = Vec::new();
    for (done, &i) in open.iter().enumerate() {
        // A cancel stops between proofs (JOB-REQ-0006).
        ctx.checkpoint()?;
        ctx.progress("prove", done as u64, open.len() as u64);
        let o = &obs[i];
        if opts.verbose && (done < 4 || done % (open.len() / 8).max(1) == 0) {
            phase(&format!(
                "prove {done}/{}: {} {}",
                open.len(),
                o.kind,
                o.name
            ));
        }
        let sw = sweep.as_mut().expect("swept");
        match sw.equal(o.a, o.b, o.assume, opts.budget, &frees) {
            Proof::Holds => {}
            Proof::Fails(m) => {
                let assign: HashMap<u32, bool> = m.iter().copied().collect();
                // The obligations were ordered above, so a cycle here is a defect of the
                // checker, as is a counterexample that does not replay (CHECK-REQ-0044).
                let ord = c.order(&[o.a, o.b, o.assume]).map_err(|cy| {
                    Stop::Internal(format!(
                        "internal: cycle through circuit variable {} while replaying the \
                         counterexample for {} {}",
                        cy.0, o.kind, o.name
                    ))
                })?;
                let mut vals = HashMap::new();
                c.eval(
                    &ord,
                    &|v| {
                        if assign.get(&v).copied().unwrap_or(false) {
                            u64::MAX
                        } else {
                            0
                        }
                    },
                    &mut vals,
                );
                let bad = Circuit::value(o.assume, &vals)
                    & (Circuit::value(o.a, &vals) ^ Circuit::value(o.b, &vals))
                    & 1;
                if bad == 0 {
                    return Err(Stop::Internal(format!(
                        "internal: counterexample for {} {} does not replay (CHECK-REQ-0044)",
                        o.kind, o.name
                    )));
                }
                let cex = m
                    .iter()
                    .map(|(v, b)| (names.get(v).cloned().unwrap_or_else(|| format!("v{v}")), *b))
                    .collect();
                mismatches.push(Mismatch {
                    kind: o.kind.into(),
                    name: o.name.clone(),
                    counterexample: cex,
                });
            }
            // Undecided, not an error: the budget that ran out is named (CHECK-REQ-0002).
            Proof::Unknown => {
                let swept = sw.stats.undecided;
                undecided.push(format!(
                    "{} {}: the conflict budget of {} per proof ran out{}",
                    o.kind,
                    o.name,
                    opts.budget,
                    if swept > 0 {
                        format!(
                            "; sweeping left {swept} candidates undecided at {} conflicts each",
                            opts.lemma_budget
                        )
                    } else {
                        String::new()
                    }
                ));
            }
        }
    }
    Ok(Outcome::Proved(Proved {
        outputs_checked,
        pairs,
        mismatches,
        undecided,
        sweep: sweep.as_ref().map(|s| s.stats).unwrap_or_default(),
        seq_sim,
    }))
}
