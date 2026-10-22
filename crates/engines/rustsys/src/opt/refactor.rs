// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `refactor`: collapse-and-refactor over reconvergence-driven cuts of up to 10 leaves
//! (SYNTH-REQ-0411). The cone of a node above its cut is collapsed into a truth table, an
//! irredundant cover of the function or of its complement is factored, and the node is replaced
//! when the factored form needs fewer new nodes than the fanout-free cone it frees. New nodes are
//! counted on the current graph, so logic that already exists outside the freed cone is reused
//! [@mishchenko2006dag; @minato1992fast; @brayton1987factoring].

use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use super::factor::{Cube, Form, factor};
use super::parallel::{Mffc, WorkLimits, WorkStats};
use super::{design_roots, set_roots, sweep};
use crate::aig::{Aig, Lit};
use crate::elab::Design;

/// Largest cut.
const K: usize = 10;

/// Truth tables of up to `K` variables, 64 minterms per word.
type Tt = smallvec::SmallVec<[u64; 16]>;

fn nwords(n: usize) -> usize {
    if n <= 6 { 1 } else { 1 << (n - 6) }
}

fn tail_mask(n: usize) -> u64 {
    if n >= 6 {
        u64::MAX
    } else {
        (1u64 << (1u32 << n)) - 1
    }
}

/// Truth table of variable `v` over `n` variables.
fn var_tt(v: usize, n: usize) -> Tt {
    const M: [u64; 6] = [
        0xAAAA_AAAA_AAAA_AAAA,
        0xCCCC_CCCC_CCCC_CCCC,
        0xF0F0_F0F0_F0F0_F0F0,
        0xFF00_FF00_FF00_FF00,
        0xFFFF_0000_FFFF_0000,
        0xFFFF_FFFF_0000_0000,
    ];
    (0..nwords(n))
        .map(|w| {
            if v < 6 {
                M[v] & tail_mask(n)
            } else if (w >> (v - 6)) & 1 == 1 {
                u64::MAX
            } else {
                0
            }
        })
        .collect()
}

/// Cofactors with respect to `v`, each replicated over both halves.
fn cofactors(t: &[u64], v: usize) -> (Tt, Tt) {
    if v < 6 {
        let m = crate::map::cuts::var_mask(v);
        let s = 1u32 << v;
        let lo = t.iter().map(|x| {
            let l = x & !m;
            l | (l << s)
        });
        let hi = t.iter().map(|x| {
            let h = x & m;
            h | (h >> s)
        });
        (lo.collect(), hi.collect())
    } else {
        let step = 1usize << (v - 6);
        let mut lo = Tt::from_slice(t);
        let mut hi = Tt::from_slice(t);
        for w in 0..t.len() {
            let base = w & !step;
            lo[w] = t[base];
            hi[w] = t[base | step];
        }
        (lo, hi)
    }
}

fn is_zero(t: &[u64]) -> bool {
    t.iter().all(|x| *x == 0)
}

fn is_full(t: &[u64], n: usize) -> bool {
    t.iter().all(|x| *x == tail_mask(n))
}

/// Irredundant cover of a function in the interval `on <= f <= upper`; returns the cubes and the
/// function they cover [@minato1992fast].
fn isop(on: &[u64], upper: &[u64], n: usize, v_hi: usize) -> (Vec<Cube>, Tt) {
    if is_zero(on) {
        return (Vec::new(), smallvec::smallvec![0; on.len()]);
    }
    if is_full(upper, n) {
        return (
            vec![Cube { pos: 0, neg: 0 }],
            smallvec::smallvec![tail_mask(n); on.len()],
        );
    }
    let mut v = v_hi;
    while v > 0 {
        let (a0, a1) = cofactors(on, v - 1);
        let (b0, b1) = cofactors(upper, v - 1);
        if a0 != a1 || b0 != b1 {
            break;
        }
        v -= 1;
    }
    if v == 0 {
        return (
            vec![Cube { pos: 0, neg: 0 }],
            smallvec::smallvec![tail_mask(n); on.len()],
        );
    }
    let x = v - 1;
    let (on0, on1) = cofactors(on, x);
    let (up0, up1) = cofactors(upper, x);
    let a: Tt = on0.iter().zip(&up1).map(|(p, q)| p & !q).collect();
    let (c0, f0) = isop(&a, &up0, n, x);
    let b: Tt = on1.iter().zip(&up0).map(|(p, q)| p & !q).collect();
    let (c1, f1) = isop(&b, &up1, n, x);
    let rest: Tt = (0..on.len())
        .map(|w| (on0[w] & !f0[w]) | (on1[w] & !f1[w]))
        .collect();
    let both: Tt = up0.iter().zip(&up1).map(|(p, q)| p & q).collect();
    let (c2, f2) = isop(&rest, &both, n, x);
    let m = var_tt(x, n);
    let f: Tt = (0..on.len())
        .map(|w| ((f0[w] & !m[w]) | (f1[w] & m[w]) | f2[w]) & tail_mask(n))
        .collect();
    let mut cubes = Vec::with_capacity(c0.len() + c1.len() + c2.len());
    cubes.extend(c0.into_iter().map(|c| Cube {
        pos: c.pos,
        neg: c.neg | (1 << x),
    }));
    cubes.extend(c1.into_iter().map(|c| Cube {
        pos: c.pos | (1 << x),
        neg: c.neg,
    }));
    cubes.extend(c2);
    (cubes, f)
}

/// Reconvergence-driven cut of `root`: starting from its fanins, the leaf whose expansion adds the
/// fewest new leaves is expanded while the cut has at most `k` leaves. Returns the leaves and the
/// cone above them (including `root`), both sorted.
pub(super) fn cut_of_bounded(
    aig: &Aig,
    root: u32,
    levels: &[u32],
    k: usize,
    max_nodes: usize,
) -> Option<(Vec<u32>, Vec<u32>)> {
    if max_nodes == 0 {
        return None;
    }
    let mut inside: FxHashSet<u32> = FxHashSet::default();
    inside.insert(root);
    let (a, b) = aig.fanins(root);
    let mut leaves: Vec<u32> = vec![a.node(), b.node()];
    leaves.sort_unstable();
    leaves.dedup();
    loop {
        let mut pick: Option<(usize, u32, usize)> = None;
        for (i, &l) in leaves.iter().enumerate() {
            if !aig.is_and(l) {
                continue;
            }
            let (x, y) = aig.fanins(l);
            let cost = [x.node(), y.node()]
                .iter()
                .filter(|f| !leaves.contains(f) && !inside.contains(f))
                .count();
            let better = pick.is_none_or(|(_, pl, pc)| {
                cost < pc || (cost == pc && levels[l as usize] > levels[pl as usize])
            });
            if better {
                pick = Some((i, l, cost));
            }
        }
        let Some((i, l, cost)) = pick else { break };
        if leaves.len() - 1 + cost > k {
            break;
        }
        if inside.len() >= max_nodes {
            return None;
        }
        leaves.swap_remove(i);
        inside.insert(l);
        let (x, y) = aig.fanins(l);
        for f in [x.node(), y.node()] {
            if !leaves.contains(&f) && !inside.contains(&f) {
                leaves.push(f);
            }
        }
    }
    leaves.sort_unstable();
    let mut cone: Vec<u32> = inside.into_iter().collect();
    cone.sort_unstable();
    Some((leaves, cone))
}

/// Truth table of `root` over `leaves` (cone nodes in topological order).
fn cone_tt(aig: &Aig, leaves: &[u32], cone: &[u32]) -> Tt {
    let n = leaves.len();
    let words = nwords(n);
    let mask = tail_mask(n);
    // One dense allocation per cone, rather than cloning heap-backed truth tables for
    // both fanins of every internal node. The map stores offsets, not owned tables.
    let mut offsets: FxHashMap<u32, usize> = FxHashMap::default();
    let mut values = vec![0u64; (leaves.len() + 1 + cone.len()) * words];
    for (i, &leaf) in leaves.iter().enumerate() {
        offsets.insert(leaf, i * words);
        values[i * words..(i + 1) * words].copy_from_slice(&var_tt(i, n));
    }
    offsets.insert(0, leaves.len() * words);
    let mut last = 0;
    for (index, &id) in cone.iter().enumerate() {
        let (a, b) = aig.fanins(id);
        let ao = offsets[&a.node()];
        let bo = offsets[&b.node()];
        let out = (leaves.len() + 1 + index) * words;
        let am = if a.is_compl() { mask } else { 0 };
        let bm = if b.is_compl() { mask } else { 0 };
        for w in 0..words {
            values[out + w] = (values[ao + w] ^ am) & (values[bo + w] ^ bm);
        }
        offsets.insert(id, out);
        last = out;
    }
    Tt::from_slice(&values[last..last + words])
}

/// New nodes needed to build `f` over `leaves` on the current graph, reusing existing nodes that
/// are not about to be freed; `None` literals stand for nodes that do not exist yet.
pub(super) fn dry_cost(
    aig: &Aig,
    f: &Form,
    leaves: &[Lit],
    freed: &FxHashSet<u32>,
    cost: &mut usize,
) -> Option<Lit> {
    let and = |x: Option<Lit>, y: Option<Lit>, cost: &mut usize| -> Option<Lit> {
        if let (Some(p), Some(q)) = (x, y) {
            if let Some(r) = aig.lookup_and(p, q) {
                if r.is_const() || !freed.contains(&r.node()) || !aig.is_and(r.node()) {
                    return Some(r);
                }
            }
        }
        *cost += 1;
        None
    };
    match f {
        Form::Const(c) => Some(Lit::from_bool(*c)),
        Form::Lit(v, neg) => Some(leaves[*v].xor(*neg)),
        Form::And(xs) => {
            let mut acc = Some(Lit::TRUE);
            for x in xs {
                let y = dry_cost(aig, x, leaves, freed, cost);
                acc = and(acc, y, cost);
            }
            acc
        }
        Form::Or(xs) => {
            let mut acc = Some(Lit::TRUE);
            for x in xs {
                let y = dry_cost(aig, x, leaves, freed, cost).map(|l| !l);
                acc = and(acc, y, cost);
            }
            acc.map(|l| !l)
        }
    }
}

/// Graph-independent forms can be reused across passes in one synthesis invocation.
#[derive(Debug, Default)]
pub struct FormCache {
    workers: Vec<Mutex<RefactorScratch>>,
}

#[derive(Debug, Default)]
struct RefactorScratch {
    forms: FxHashMap<(usize, Tt), Option<Arc<Form>>>,
    mffc: Mffc,
}

type Replacement = (Vec<u32>, Arc<Form>, bool);

/// Actual heap capacities of a bounded factored tree; no path enumeration.
fn form_heap_bytes(form: &Form) -> Option<usize> {
    let mut stack = vec![form];
    let mut visited = 0usize;
    let mut bytes = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > 4096 {
            return None;
        }
        if let Form::And(children) | Form::Or(children) = node {
            if visited + stack.len() + children.len() > 4096 {
                return None;
            }
            bytes = bytes.checked_add(children.capacity().checked_mul(size_of::<Form>())?)?;
            stack.extend(children);
        }
    }
    Some(bytes)
}

struct PlanView<'a> {
    aig: &'a Aig,
    refs: &'a [u32],
    levels: &'a [u32],
    max_window_nodes: usize,
    cache_entries: usize,
}

impl PlanView<'_> {
    fn candidate(&self, id: u32, scratch: &mut RefactorScratch) -> (Option<Replacement>, bool) {
        let Some((leaves, cone)) =
            cut_of_bounded(self.aig, id, self.levels, K, self.max_window_nodes)
        else {
            return (None, true);
        };
        if leaves.len() < 3 {
            return (None, false);
        }
        if !scratch
            .mffc
            .compute(self.aig, id, &leaves, self.refs, self.max_window_nodes)
        {
            return (None, true);
        }
        let freed = &scratch.mffc.freed;
        if freed.len() < 2 {
            return (None, false);
        }
        let nv = leaves.len();
        let t = cone_tt(self.aig, &leaves, &cone);
        let nt: Tt = t.iter().map(|x| !x & tail_mask(nv)).collect();
        let leaf_lits: Vec<Lit> = leaves.iter().map(|l| Lit::new(*l, false)).collect();
        let mut best: Option<(usize, Arc<Form>, bool)> = None;
        for (tab, inv) in [(&t, false), (&nt, true)] {
            // Fixed total entry budget is divided among worker shards; caching
            // changes computation reuse only, never the chosen Boolean form.
            if scratch.forms.len() >= self.cache_entries {
                scratch.forms.clear();
            }
            let form = scratch.forms.entry((nv, tab.clone())).or_insert_with(|| {
                let (cubes, _) = isop(tab, tab, nv, nv);
                if cubes.len() > 64 {
                    return None;
                }
                let form = factor(&cubes);
                (form_heap_bytes(&form)? <= 64 * 1024).then(|| Arc::new(form))
            });
            let Some(form) = form else { continue };
            let mut cost = 0usize;
            dry_cost(self.aig, form, &leaf_lits, freed, &mut cost);
            if cost < freed.len() && best.as_ref().is_none_or(|b| cost < b.0) {
                best = Some((cost, form.clone(), inv));
            }
        }
        (best.map(|(_, form, inv)| (leaves, form, inv)), false)
    }
}

/// One refactoring pass over the whole design.
pub fn refactor(d: &mut Design) {
    refactor_cached(d, &mut FormCache::default());
}

pub fn refactor_cached(d: &mut Design, cache: &mut FormCache) {
    let _ = refactor_cached_with_limits(d, cache, WorkLimits::default());
}

pub fn refactor_cached_with_limits(
    d: &mut Design,
    cache: &mut FormCache,
    limits: WorkLimits,
) -> WorkStats {
    let mut stats = WorkStats::default();
    if d.aig.num_nodes() > limits.max_nodes.min(32_000_000) {
        stats.node_budget_reached = true;
        return stats;
    }
    let roots = design_roots(d);
    let aig = &d.aig;
    let n = aig.num_nodes();
    let mut refs = vec![0u32; n];
    for id in 1..n as u32 {
        if aig.is_and(id) {
            let (a, b) = aig.fanins(id);
            refs[a.node() as usize] += 1;
            refs[b.node() as usize] += 1;
        }
    }
    for r in &roots {
        refs[r.node() as usize] += 1;
    }
    let levels = aig.levels();
    let mut plan: FxHashMap<u32, Replacement> = FxHashMap::default();
    let mut plan_bytes = 1024usize;
    cache.workers.resize_with(limits.workers(), Mutex::default);
    let workers = &mut cache.workers;
    let view = PlanView {
        aig,
        refs: &refs,
        levels: &levels,
        max_window_nodes: limits.max_window_nodes.min(16_384),
        cache_entries: 16_384 / workers.len(),
    };
    for start in (1..n).step_by(limits.batch()) {
        let end = (start + limits.batch()).min(n);
        let ids: Vec<_> = (start as u32..end as u32)
            .filter(|&id| aig.is_and(id) && refs[id as usize] > 0)
            .collect();
        let candidates: Vec<_> = if workers.len() == 1 || ids.len() < 64 {
            let scratch = workers[0].get_mut().expect("worker cache poisoned");
            ids.iter().map(|&id| view.candidate(id, scratch)).collect()
        } else {
            ids.par_iter()
                .with_min_len(32)
                .map(|&id| {
                    let index = rayon::current_thread_index().unwrap_or(0) % workers.len();
                    let mut scratch = workers[index].lock().expect("worker cache poisoned");
                    view.candidate(id, &mut scratch)
                })
                .collect()
        };
        for (id, (candidate, bounded)) in ids.into_iter().zip(candidates) {
            stats.windows += 1;
            stats.bounded_windows += u64::from(bounded);
            if let Some(replacement) = candidate {
                // Charge the complete template per root even when Arc shares
                // it. This conservative budget is independent of scheduling,
                // cache eviction and worker count.
                let Some(bytes) = form_heap_bytes(&replacement.1) else {
                    stats.storage_budget_reached = true;
                    return stats;
                };
                plan_bytes = plan_bytes.saturating_add(256 + replacement.0.capacity() * 4 + bytes);
                if plan_bytes > limits.max_plan_bytes {
                    stats.storage_budget_reached = true;
                    return stats;
                }
                plan.insert(id, replacement);
            }
        }
    }
    drop(refs);
    drop(levels);
    if plan.is_empty() {
        return stats;
    }
    let backup = d.clone();
    let aig = &backup.aig;
    let mut ng = Aig::new();
    let mut img = vec![Lit::FALSE; n];
    for &i in aig.inputs() {
        img[i as usize] = ng.input();
    }
    for id in aig.cone(&roots) {
        if !aig.is_and(id) {
            continue;
        }
        img[id as usize] = match plan.get(&id) {
            Some((leaves, form, inv)) => {
                let ls: Vec<Lit> = leaves.iter().map(|l| img[*l as usize]).collect();
                super::factor::build(&mut ng, form, &ls).xor(*inv)
            }
            None => {
                let (a, b) = aig.fanins(id);
                let x = img[a.node() as usize].xor(a.is_compl());
                let y = img[b.node() as usize].xor(b.is_compl());
                ng.and(x, y)
            }
        };
    }
    let new_roots: Vec<Lit> = roots
        .iter()
        .map(|r| img[r.node() as usize].xor(r.is_compl()))
        .collect();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in p.bits.iter_mut() {
            *b = img[b.node() as usize];
        }
    }
    for s in d.states.iter_mut() {
        s.q = img[s.q.node() as usize];
    }
    d.aig = ng;
    set_roots(d, &new_roots);
    sweep(d);
    if d.aig.num_ands() >= backup.aig.num_ands() {
        *d = backup;
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_form(f: &Form, m: u32) -> bool {
        match f {
            Form::Const(c) => *c,
            Form::Lit(v, n) => ((m >> v) & 1 == 1) != *n,
            Form::And(xs) => xs.iter().all(|x| eval_form(x, m)),
            Form::Or(xs) => xs.iter().any(|x| eval_form(x, m)),
        }
    }

    // SYNTH-REQ-0411, SYNTH-REQ-0415, SYNTH-REQ-0416, SYNTH-REQ-0417, SYNTH-REQ-0418:
    // covers of random functions of 7 to 10 variables are exact and use dense word storage.
    #[test]
    fn multiword_isop_is_exact() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for nv in [3usize, 6, 7, 8, 10] {
            for _ in 0..20 {
                // Sparse functions keep the cover small, dense ones exercise many cubes.
                let t: Tt = (0..nwords(nv))
                    .map(|_| (rnd() & rnd() & rnd()) & tail_mask(nv))
                    .collect();
                let (cubes, f) = isop(&t, &t, nv, nv);
                assert_eq!(f, t);
                let form = factor(&cubes);
                for m in 0..(1u32 << nv) {
                    let bit = (t[(m >> 6) as usize] >> (m & 63)) & 1 == 1;
                    assert_eq!(eval_form(&form, m), bit, "nv={nv} m={m}");
                }
            }
        }
    }
}
