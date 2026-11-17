// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Proposals of shared (b) and cell (c) relations for states without one (CHECK-REQ-0032).

use std::collections::{HashMap, HashSet};

use super::{Hyp, MAX_DISAGREEMENT, MAX_FAILURES, PROPOSE_ROUNDS, Search, mask, support, trace};
use crate::Stop;
use crate::circ::B;
use crate::corr::Relation;

impl Search<'_, '_> {
    /// Proposes shared (b) and cell (c) relations for states without one.
    pub(super) fn propose(
        &mut self,
        hyp: &Hyp,
        failures: &HashMap<(usize, Relation), u32>,
    ) -> Result<Vec<(usize, Relation)>, Stop> {
        let (model, nm) = (self.s.model, self.s.nm);
        let free_sharers: Vec<usize> = self
            .sharers
            .iter()
            .copied()
            .filter(|si| !hyp.contains_key(si))
            .collect();
        let taken: HashSet<usize> = hyp
            .values()
            .filter_map(|r| match r {
                Relation::Cell(ci, _) => Some(*ci),
                _ => None,
            })
            .collect();
        let free_cells: Vec<usize> = self
            .unused
            .iter()
            .copied()
            .filter(|ci| !taken.contains(ci))
            .collect();
        let free_rtl: Vec<usize> = self
            .unmatched
            .iter()
            .copied()
            .filter(|si| !matches!(hyp.get(si), Some(Relation::Shares(..) | Relation::Cell(..))))
            .collect();
        let mut anchors = self.anchors.clone();
        anchors.extend(hyp.iter().filter_map(|(si, r)| {
            (matches!(r, Relation::Cell(..))
                && !self.s.restricted(*si)
                && (self.s.initial_state_map || self.s.base(*si).is_some()))
            .then_some(*si)
        }));
        if (free_sharers.is_empty() || anchors.is_empty())
            && (free_cells.is_empty() || free_rtl.is_empty())
        {
            return Ok(Vec::new());
        }
        // Items: for every RTL state of interest next, clock and asynchronous condition and the
        // state value itself; for every free element its four functions.
        let mut rtl_ids: Vec<usize> = free_sharers.clone();
        rtl_ids.extend(&anchors);
        rtl_ids.extend(&free_rtl);
        rtl_ids.sort_unstable();
        rtl_ids.dedup();
        let mut items = Vec::new();
        for &si in &rtl_ids {
            let st = &model.states[si];
            items.extend([
                st.next,
                st.clk,
                st.arst.map_or(B::F, |(cnd, _)| cnd),
                B::V(st.q),
            ]);
        }
        for &ci in &free_cells {
            let x = &nm.seq[ci];
            items.extend([x.next, x.clk, x.clear, x.preset]);
        }
        let w = self.record(hyp, &items)?;
        let at: HashMap<usize, usize> = rtl_ids
            .iter()
            .enumerate()
            .map(|(k, si)| (*si, 4 * k))
            .collect();
        let cell_at = |k: usize| 4 * rtl_ids.len() + 4 * k;
        let key = |a: &[u64], b: &[u64]| -> Vec<u64> { a.iter().chain(b).copied().collect() };
        let ok = |bad: u32, active: u32| f64::from(bad) <= MAX_DISAGREEMENT * f64::from(active);
        let tried = |si: usize, rel: Relation| {
            failures.get(&(si, rel)).copied().unwrap_or(0) >= MAX_FAILURES
        };
        let mut out: Vec<(usize, Relation)> = Vec::new();
        // (c) Cells: an unused element and an unmatched RTL state of the same kind with equal
        // clock and asynchronous behavior whose next values agree under one parity in most lanes.
        let support_hints = self
            .rank_supports
            .then(|| support::signatures(self.s, self.corr, hyp, &free_rtl, &free_cells));
        let sketch_hints = if !self.rank_supports && self.sketch_bytes > 0 {
            let work = (self.order.len() as u64).saturating_mul(4);
            if let Some(left) = &mut self.remaining_work {
                if work > *left {
                    return Err(Stop::Undecided(
                        "state proposal sketch-work budget reached".into(),
                    ));
                }
                *left -= work;
            }
            support::sketches(
                self.s,
                self.corr,
                &free_rtl,
                &free_cells,
                &self.order,
                self.sketch_bytes,
            )
        } else {
            None
        };
        let mut used: HashSet<usize> = out.iter().map(|x| x.0).collect();
        // Clock words equal and no asynchronous difference between RTL state `si` (word offset
        // `r`) and element `ci` (word offset `k`) under parity `p`.
        let compatible = |si: usize, ci: usize, r: usize, k: usize, p: bool| -> bool {
            let (st, x) = (&model.states[si], &nm.seq[ci]);
            if st.latch != x.latch || w[r + 1].rnd != w[k + 1].rnd {
                return false;
            }
            (0..PROPOSE_ROUNDS).all(|t| {
                let (clear, preset) = (w[k + 2].rnd[t], w[k + 3].rnd[t]);
                let (f0, f1) = if p { (preset, clear) } else { (clear, preset) };
                let cnd = w[r + 2].rnd[t];
                let (want0, want1) = match st.arst {
                    None => (0, 0),
                    Some((_, false)) => (cnd, 0),
                    Some((_, true)) => (0, cnd),
                };
                f0 == want0 && f1 == want1
            })
        };
        // (c0) State map: an element and an RTL state whose changing reset traces are equal
        // under one parity. Next functions that read other state still without a relation cannot
        // agree one pair at a time; induction then refines all such pairs together.
        let mut taken_cells: HashSet<usize> = HashSet::new();
        if let Some(t) = &self.traces {
            let mut by_sig: HashMap<u64, Vec<usize>> = HashMap::new();
            for &si in free_rtl.iter().filter(|si| !used.contains(si)) {
                if trace::Traces::constant(&t.rtl[si]).is_none() {
                    by_sig
                        .entry(trace::Traces::key(&t.rtl[si]).0)
                        .or_default()
                        .push(si);
                }
            }
            for (n, &ci) in free_cells.iter().enumerate() {
                let sig = &t.gate[ci];
                if trace::Traces::constant(sig).is_some() {
                    continue;
                }
                let (h, ginv) = trace::Traces::key(sig);
                let Some(list) = by_sig.get(&h) else {
                    continue;
                };
                let k = cell_at(n);
                let mut best: Option<(u32, usize, bool)> = None;
                for &si in list {
                    let p = ginv ^ trace::Traces::key(&t.rtl[si]).1;
                    if used.contains(&si)
                        || !trace::Traces::equal(&t.rtl[si], sig, p)
                        || tried(si, Relation::Cell(ci, p))
                        || !compatible(si, ci, at[&si], k, p)
                    {
                        continue;
                    }
                    let r = at[&si];
                    let parity = mask(p);
                    let bad: u32 = (0..PROPOSE_ROUNDS)
                        .map(|t| {
                            ((w[k].zero[t] ^ parity ^ w[r].zero[t])
                                | (w[k].one[t] ^ parity ^ w[r].one[t]))
                                .count_ones()
                        })
                        .sum();
                    if best.is_none_or(|(b, s0, _)| bad < b || (bad == b && si < s0)) {
                        best = Some((bad, si, p));
                    }
                }
                if let Some((_, si, p)) = best {
                    used.insert(si);
                    taken_cells.insert(ci);
                    out.push((si, Relation::Cell(ci, p)));
                }
            }
        }
        // (b) Shares: same clock and asynchronous condition; the candidate's next value with its
        // own value set to the representative's (through `zero` and `one`) agrees in most lanes.
        // All candidates are proposed together, so states that depend on each other's relation
        // can hold together under refinement.
        // Run after (c0), so a state can share an element proposed there.
        anchors.extend(
            out.iter()
                .filter_map(|(si, r)| matches!(r, Relation::Cell(..)).then_some(*si)),
        );
        let mut by_key: HashMap<Vec<u64>, Vec<usize>> = HashMap::new();
        for &ri in &anchors {
            if !at.contains_key(&ri) {
                continue;
            }
            let k = at[&ri];
            by_key
                .entry(key(&w[k + 1].rnd, &w[k + 2].rnd))
                .or_default()
                .push(ri);
        }
        for &si in &free_sharers {
            if used.contains(&si) {
                continue;
            }
            let k = at[&si];
            let Some(group) = by_key.get(&key(&w[k + 1].rnd, &w[k + 2].rnd)) else {
                continue;
            };
            let base = self.s.base(si);
            let passing: Vec<(u32, usize, bool)> = group
                .iter()
                .copied()
                .flat_map(|ri| {
                    // Reset bases fix the parity; a state map tries both.
                    let parities: &[bool] = match (base, self.s.base(ri)) {
                        (Some(a), Some(b)) if a ^ b => &[true],
                        (Some(_), Some(_)) => &[false],
                        _ => &[false, true],
                    };
                    parities.iter().map(move |&inv| (ri, inv))
                })
                .filter_map(|(ri, inv)| {
                    if tried(si, Relation::Shares(ri, inv)) {
                        return None;
                    }
                    // A state map shares only on equal reset traces that change: a constant
                    // trace is no evidence of sharing and must stay open for an element.
                    if let Some(t) = &self.traces {
                        if trace::Traces::constant(&t.rtl[si]).is_some()
                            || !trace::Traces::equal(&t.rtl[si], &t.rtl[ri], inv)
                        {
                            return None;
                        }
                    }
                    let r = at[&ri];
                    let (mut bad, mut active) = (0, 0);
                    for t in 0..PROPOSE_ROUNDS {
                        let own = w[r + 3].rnd[t] ^ mask(inv);
                        let next = (own & w[k].one[t]) | (!own & w[k].zero[t]);
                        let live = !w[k + 2].rnd[t];
                        bad += ((next ^ mask(inv) ^ w[r].rnd[t]) & live).count_ones();
                        active += live.count_ones();
                    }
                    // Under a state map equal changing reset traces are the evidence; the
                    // functional check reads states still without a relation.
                    (self.traces.is_some() || ok(bad, active)).then_some((bad, ri, inv))
                })
                .collect();
            // Rank: bits of the same RTL variable first, then structural similarity, then
            // simulation agreement.
            let var = variable(&model.states[si].name);
            let mut best: Option<(bool, f64, u32, usize, bool)> = None;
            for (bad, ri, inv) in passing {
                let same = variable(&model.states[ri].name) == var;
                let sim = self.similarity(si, ri);
                let better = match best {
                    None => true,
                    Some((s0, m0, b0, r0, _)) => {
                        (same, sim) > (s0, m0) || ((same, sim) == (s0, m0) && (bad, ri) < (b0, r0))
                    }
                };
                if better {
                    best = Some((same, sim, bad, ri, inv));
                }
            }
            let best = best.map(|(_, _, b, r, i)| (b, r, i));
            if let Some((_, ri, inv)) = best {
                used.insert(si);
                out.push((si, Relation::Shares(ri, inv)));
            }
        }
        for (n, &ci) in free_cells.iter().enumerate() {
            if taken_cells.contains(&ci) || self.exact_only {
                continue;
            }
            let x = &nm.seq[ci];
            let k = cell_at(n);
            // A state map ranks reset-trace agreement first; two non-constant traces that
            // differ rule a pair out (cf. `trace`), so a functional near-match never wins.
            let trace_rank = |si: usize, p: bool| -> Option<bool> {
                let Some(t) = &self.traces else {
                    return Some(true);
                };
                let (a, b) = (&t.rtl[si], &t.gate[ci]);
                if trace::Traces::equal(a, b, p) {
                    Some(true)
                } else if trace::Traces::constant(a).is_none()
                    && trace::Traces::constant(b).is_none()
                {
                    None
                } else {
                    Some(false)
                }
            };
            let mut best: Option<(bool, u32, usize, f64, usize, bool)> = None;
            for &si in free_rtl.iter().filter(|si| !used.contains(si)) {
                let st = &model.states[si];
                let r = at[&si];
                if st.latch != x.latch || w[r + 1].rnd != w[k + 1].rnd {
                    continue;
                }
                for p in [false, true] {
                    if tried(si, Relation::Cell(ci, p)) {
                        continue;
                    }
                    if let (Some(bc), Some(br)) = (self.cell_base[ci], self.s.base(si)) {
                        if bc != br ^ p {
                            continue;
                        }
                    }
                    let (mut bad, mut active, mut forced) = (0, 0, false);
                    let parity = mask(p);
                    let cross = p ^ self.rtl_phase(si) ^ self.cell_phase(ci);
                    for t in 0..PROPOSE_ROUNDS {
                        let (clear, preset) = (w[k + 2].rnd[t], w[k + 3].rnd[t]);
                        let (f0, f1) = if p { (preset, clear) } else { (clear, preset) };
                        let cnd = w[r + 2].rnd[t];
                        let (want0, want1) = match st.arst {
                            None => (0, 0),
                            Some((_, false)) => (cnd, 0),
                            Some((_, true)) => (0, cnd),
                        };
                        forced |= f0 != want0 || f1 != want1;
                        let mut live = !cnd;
                        if st.latch {
                            live &= w[r + 1].rnd[t];
                        }
                        let (c0, c1) = (w[k].zero[t], w[k].one[t]);
                        let (s0, s1) = (w[r].zero[t], w[r].one[t]);
                        let differ = if cross {
                            (c1 ^ parity ^ s0) | (c0 ^ parity ^ s1)
                        } else {
                            (c0 ^ parity ^ s0) | (c1 ^ parity ^ s1)
                        };
                        bad += (differ & live).count_ones();
                        active += live.count_ones();
                    }
                    let similarity = support_hints.as_ref().map_or_else(
                        || {
                            sketch_hints
                                .as_ref()
                                .map_or(0.0, |(rtl_support, cell_support)| {
                                    support::sketch_similarity(rtl_support[&si], cell_support[&ci])
                                })
                        },
                        |(rtl_support, cell_support)| {
                            support::similarity(&rtl_support[&si], &cell_support[&ci])
                        },
                    );
                    let Some(agrees) = trace_rank(si, p) else {
                        continue;
                    };
                    let index = self.names.as_ref().map_or(0, |n| n.likeness(ci, &st.name));
                    if !forced
                        && ok(bad, active)
                        && best.is_none_or(|(a, b, i, sim, _, _)| {
                            (agrees, std::cmp::Reverse(bad), index, similarity)
                                > (a, std::cmp::Reverse(b), i, sim)
                        })
                    {
                        best = Some((agrees, bad, index, similarity, si, p));
                    }
                }
            }
            if let Some((_, _, _, _, si, p)) = best {
                used.insert(si);
                out.push((si, Relation::Cell(ci, p)));
            }
        }
        Ok(out)
    }
}

/// The RTL variable of a state name: the name without its last bit index.
fn variable(name: &str) -> &str {
    match name.rfind('[') {
        Some(i) if name.ends_with(']') => &name[..i],
        _ => name,
    }
}
