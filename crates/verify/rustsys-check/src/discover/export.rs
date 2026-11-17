// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! State correspondence proposals for an obligation export: under a state map, re-encoded
//! registers by exploration, name evidence, and relations under them (CHECK-REQ-0032).

use std::collections::{HashMap, HashSet};

use rusteda_core::job::Ctx;

use super::{Hyp, MAX_FAILURES, Search, discover_inner, encode, needed_limited, trace};
use crate::Stop;
use crate::corr::{Corr, Encoding, Relation, Setting};

/// What an export search proposes: relations, re-encoded registers, and the RTL states whose
/// name match a re-encoding contradicted (to release before applying the rest).
pub(crate) struct ExportProposals {
    pub relations: Vec<(usize, Relation)>,
    pub encodings: Vec<Encoding>,
    pub released: Vec<usize>,
    /// Candidate valid-code invariants of registers without a re-encoding (`gate` empty).
    pub invariants: Vec<Encoding>,
}

/// Name-free bindings are proposals only. A bijection leaves every gate bit
/// arbitrary; the exported complete event frame must still pass external CEC.
/// Memory plan, one corner/worker: <=64M definitions * (3*8+4) bytes of dense
/// scratch, <=64M support entries *4 bytes, plus bounded names/signatures;
/// Optional 16-byte/definition support sketches use at most 1 GiB;
/// <4 GiB additional RAM. Pair, round and evaluation budgets bound all work.
pub(crate) fn for_export(
    s: &Setting<'_>,
    corr: &Corr,
    seed: u64,
    limits: &crate::ObligationExport,
    excluded: &HashSet<(usize, Relation)>,
    ctx: Ctx<'_>,
) -> Result<ExportProposals, Stop> {
    if !s.pulse.is_empty()
        || s.uninitialized_dont_care
        || s.c.defs.len() > limits.max_discovery_definitions
        || limits.max_discovery_rounds == 0
        || limits.max_discovery_work == 0
    {
        return Err(Stop::Undecided(format!(
            "one-to-one export proposal scope/definition budget reached: {} definitions (max {})",
            s.c.defs.len(),
            limits.max_discovery_definitions
        )));
    }
    let mut work = Some(limits.max_discovery_work);
    let restrict = |base: &Corr| -> Result<Corr, Stop> {
        // Search only relevant unmatched states; every other original RTL state stays free in
        // the final export. This restricts proposals, not the theorem.
        let mut search_corr = base.clone();
        search_corr.unmatched =
            needed_limited(s, base, Some(limits.max_discovery_rounds)).map_err(Stop::Undecided)?;
        let candidates = search_corr.unmatched.len() as u128 * base.unused.len() as u128;
        if candidates > limits.max_discovery_candidates as u128 {
            return Err(Stop::Undecided(format!(
                "one-to-one export state-proposal budget or scope reached: {} definitions (max {}), {} candidate pairs (max {})",
                s.c.defs.len(),
                limits.max_discovery_definitions,
                candidates,
                limits.max_discovery_candidates
            )));
        }
        Ok(search_corr)
    };
    if !s.initial_state_map {
        let search_corr = restrict(corr)?;
        let (relations, _) = discover_inner(
            s,
            &search_corr,
            seed,
            excluded,
            ctx,
            Some(limits),
            None,
            (&[], None),
            &mut work,
        )?;
        if relations
            .iter()
            .any(|(_, relation)| !matches!(relation, Relation::Cell(..)))
        {
            return Err(Stop::Internal(
                "export discovery proposed a constant or shared state".into(),
            ));
        }
        return Ok(ExportProposals {
            relations,
            encodings: Vec::new(),
            released: Vec::new(),
            invariants: Vec::new(),
        });
    }
    // A state map first explores re-encoded registers under the proposals with equal changing
    // reset traces (all of them, before refinement: states that read a re-encoded register
    // cannot hold without it), from name matches whose reset traces agree. Names are hints,
    // never proof premises: a name match that a re-encoding contradicts is released.
    // Name matches that the netlist's own names contradict (another bit numbering) are released;
    // names of RTL wires that alias a state propose pairs for unused elements.
    let names = std::rc::Rc::new(crate::alias::Names::new(s));
    let (named, by_names) = names.release_contradicted(corr);
    let corr = &named;
    let aliases = &names.proposals(s, corr);
    if std::env::var_os("RUSTSYS_DISCOVERY_TRACE").is_some() {
        eprintln!(
            "names: {} name matches released, {} alias proposals",
            by_names.len(),
            aliases.len()
        );
    }
    let mut t = trace::simulate(s, corr, seed, &mut work)?;
    let mut seed_corr = corr.clone();
    for si in 0..s.model.states.len() {
        if let Some((ci, p)) = corr.paired[si] {
            if !trace::Traces::equal(&t.rtl[si], &t.gate[ci], p) {
                seed_corr.release(si);
            }
        }
    }
    let failures: HashMap<(usize, Relation), u32> =
        excluded.iter().map(|r| (*r, MAX_FAILURES)).collect();
    let mut invariants: Vec<Encoding>;
    let mut encodings = {
        let mut pre = Search::with_needed(s, &seed_corr, seed, seed_corr.unmatched.clone())
            .map_err(Stop::Undecided)?;
        pre.remaining_work = work;
        pre.normalized_own = true;
        pre.rank_supports = false;
        pre.exact_only = true;
        pre.traces = Some(t);
        pre.named_cells = aliases
            .iter()
            .filter_map(|(_, r)| match r {
                Relation::Cell(ci, _) => Some(*ci),
                _ => None,
            })
            .collect();
        let mut hyp0: Hyp = seeded(&seed_corr, aliases, excluded, &[]);
        for _ in 0..2 {
            let more = pre.propose(&hyp0, &failures)?;
            hyp0.extend(more);
        }
        // Hinted registers are used as given (and proven like any other proposal).
        for hint in &limits.encoding_hints {
            let enc = encode::from_hint(s, hint).map_err(Stop::Undecided)?;
            pre.encodings.retain(|e| e.variable != enc.variable);
            pre.hinted.insert(enc.variable.clone());
            pre.encodings.push(enc);
        }
        pre.explore_all(&hyp0)?;
        work = pre.remaining_work;
        t = pre.traces.take().expect("traces set above");
        invariants = std::mem::take(&mut pre.invariants);
        pre.encodings
    };
    // Then relations under the registers found, and exploration again under those relations
    // (state that only reaches a code through registers paired meanwhile), while it changes.
    let mut iteration = 0;
    loop {
        iteration += 1;
        let (base, mut released) = with_encodings(corr, &encodings);
        released.extend(&by_names);
        let search_corr = restrict(&base)?;
        let (relations, _) = discover_inner(
            s,
            &search_corr,
            seed,
            excluded,
            ctx,
            Some(limits),
            Some(t.clone()),
            (aliases, Some(&names)),
            &mut work,
        )?;
        if iteration >= EXPLORE_ITERATIONS {
            return Ok(ExportProposals {
                relations,
                encodings,
                released,
                invariants,
            });
        }
        let mut again =
            Search::with_needed(s, &base, seed, base.unmatched.clone()).map_err(Stop::Undecided)?;
        again.remaining_work = work;
        again.normalized_own = true;
        again.rank_supports = false;
        again.traces = Some(t);
        again.encodings = encodings.clone();
        again.encoding_failures = vec![0; encodings.len()];
        again.hinted = limits
            .encoding_hints
            .iter()
            .map(|h| h.variable.clone())
            .collect();
        again.named_cells = aliases
            .iter()
            .filter_map(|(_, r)| match r {
                Relation::Cell(ci, _) => Some(*ci),
                _ => None,
            })
            .collect();
        let hyp: Hyp = relations.iter().copied().collect();
        let changed = again.explore_all(&hyp)?;
        work = again.remaining_work;
        t = again.traces.take().expect("traces set above");
        invariants = std::mem::take(&mut again.invariants);
        if !changed {
            return Ok(ExportProposals {
                relations,
                encodings,
                released,
                invariants,
            });
        }
        encodings = again.encodings;
    }
}

/// The seed relations still applicable to `corr`: unmatched states, unused elements outside
/// re-encoded registers, never refuted.
pub(super) fn seeded(
    corr: &Corr,
    seed: &[(usize, Relation)],
    excluded: &HashSet<(usize, Relation)>,
    encodings: &[Encoding],
) -> Hyp {
    seed.iter()
        .filter(|(si, rel)| {
            corr.unmatched.contains(si)
                && !excluded.contains(&(*si, *rel))
                && match rel {
                    Relation::Cell(ci, _) => {
                        corr.unused.contains(ci) && !encodings.iter().any(|e| e.gate.contains(ci))
                    }
                    _ => true,
                }
        })
        .copied()
        .collect()
}

/// Exploration and relation search alternate at most this often.
const EXPLORE_ITERATIONS: usize = 3;

/// `corr` with re-encoded registers: their bits' name matches released, their bits and elements
/// out of `unmatched` and `unused`. Returns the RTL states whose name match was released.
fn with_encodings(corr: &Corr, encodings: &[Encoding]) -> (Corr, Vec<usize>) {
    let mut base = corr.clone();
    let mut released = Vec::new();
    for enc in encodings {
        for &si in &enc.rtl {
            if base.paired[si].is_some() {
                released.push(si);
            }
            base.release(si);
        }
    }
    for enc in encodings {
        base.unmatched.retain(|si| !enc.rtl.contains(si));
        base.unused.retain(|ci| !enc.gate.contains(ci));
    }
    base.encodings = encodings.to_vec();
    (base, released)
}
