// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Inductive constant states under explicit reset or initial-state assumptions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::aig::{Aig, Lit};
use crate::elab::{Design, StateKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstantState {
    pub name: String,
    pub value: bool,
    /// None denotes an explicitly selected, otherwise unspecified initial value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_input: Option<String>,
}

/// Only scalar primary inputs can describe a pulse. The pulse is an initial condition;
/// these inputs remain unrestricted during all subsequent operation.
pub fn constants(
    d: &mut Design,
    after_reset: &[(String, bool)],
) -> Result<Vec<ConstantState>, String> {
    constants_with_initial(d, after_reset, false)
}

/// Optional initial-state refinement. The zero seed is only a candidate: every retained
/// candidate must be an invariant for arbitrary inputs and arbitrary remaining state.
pub fn constants_with_initial(
    d: &mut Design,
    after_reset: &[(String, bool)],
    uninitialized_dont_care: bool,
) -> Result<Vec<ConstantState>, String> {
    if after_reset.is_empty() && !uninitialized_dont_care {
        return Ok(Vec::new());
    }
    let mut pulse = BTreeMap::new();
    for (name, value) in after_reset {
        let p = d
            .port(name)
            .filter(|p| p.input && p.scalar)
            .ok_or_else(|| format!("after-reset input {name} must be a scalar primary input"))?;
        let l = p.bits[0];
        if let Some((old, _)) = pulse.insert(l.node(), (*value ^ l.is_compl(), name.clone())) {
            if old != (*value ^ l.is_compl()) {
                return Err(format!("conflicting after-reset values for {name}"));
            }
        }
    }
    // Replacing a state before the pulse must not introduce/remove a clock or reset event.
    // Retain every state observed by any sequential control, including latch enables.
    let controls: Vec<_> = d
        .states
        .iter()
        .flat_map(|s| std::iter::once(s.clk).chain(s.arst.map(|(r, _)| r)))
        .collect();
    let protected: std::collections::BTreeSet<_> = d.aig.cone(&controls).into_iter().collect();
    let mut candidates: Vec<bool> = d
        .states
        .iter()
        .map(|s| {
            s.kind == StateKind::Flop
                && !protected.contains(&s.q.node())
                && s.arst.map_or(uninitialized_dont_care, |(r, _)| {
                    pulse.get(&r.node()).is_some_and(|(v, _)| *v ^ r.is_compl())
                })
        })
        .collect();
    // Start with pulse-established reset values and authorized unreset zero choices. Repeatedly
    // discard states whose data is not invariant when that state's reset is inactive.
    // All other inputs and all noncandidate states remain independent variables.
    loop {
        let mut groups: BTreeMap<Option<Lit>, Vec<usize>> = BTreeMap::new();
        for (i, s) in d.states.iter().enumerate().filter(|(i, _)| candidates[*i]) {
            groups.entry(s.arst.map(|(r, _)| r)).or_default().push(i);
        }
        let mut keep = candidates.clone();
        for (reset, states) in groups {
            let mut ng = Aig::new();
            let mut img = vec![Lit::FALSE; d.aig.num_nodes()];
            for &n in d.aig.inputs() {
                img[n as usize] = ng.input();
            }
            if let Some(reset) = reset {
                img[reset.node() as usize] = Lit::from_bool(reset.is_compl());
            }
            for (_, s) in d.states.iter().enumerate().filter(|(i, _)| candidates[*i]) {
                img[s.q.node() as usize] =
                    Lit::from_bool(s.arst.is_some_and(|(_, v)| v) ^ s.q.is_compl());
            }
            for n in 1..d.aig.num_nodes() {
                if d.aig.is_and(n as u32) {
                    let (a, b) = d.aig.fanins(n as u32);
                    img[n] = ng.and(
                        img[a.node() as usize].xor(a.is_compl()),
                        img[b.node() as usize].xor(b.is_compl()),
                    );
                }
            }
            // Simulation only rejects candidate invariants. A non-structural constant
            // is accepted only after an UNSAT proof with all remaining inputs free.
            let mut seed = 0xd784_691e_c705_1badu64;
            let inputs: Vec<_> = (0..ng.num_inputs())
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed & !3) | 2
                })
                .collect();
            let simulated = ng.simulate(&inputs);
            let mut cnf = crate::sat::AigCnf::new();
            let mut proofs = 0;
            for i in states {
                let s = &d.states[i];
                let value = s.arst.is_some_and(|(_, v)| v);
                let data = img[s.d.node() as usize].xor(s.d.is_compl());
                keep[i] = if let Some(v) = data.const_value() {
                    v == value
                } else if proofs < 256
                    && Aig::lit_value(&simulated, data) == if value { u64::MAX } else { 0 }
                {
                    proofs += 1;
                    let lit = cnf.lit(&ng, data) ^ u32::from(value);
                    cnf.sat.solve(&[lit], 64) == Some(false)
                } else {
                    false
                };
            }
        }
        if keep == candidates {
            break;
        }
        candidates = keep;
    }
    let mut removed = Vec::new();
    let mut substitutions = BTreeMap::new();
    for (_, s) in d.states.iter().enumerate().filter(|(i, _)| candidates[*i]) {
        let value = s.arst.is_some_and(|(_, v)| v);
        substitutions.insert(s.q.node(), Lit::from_bool(value ^ s.q.is_compl()));
        removed.push(ConstantState {
            name: s.name.clone(),
            value,
            reset_input: s.arst.map(|(r, _)| pulse[&r.node()].1.clone()),
        });
    }
    if removed.is_empty() {
        return Ok(removed);
    }
    d.states = d
        .states
        .iter()
        .enumerate()
        .filter(|(i, _)| !candidates[*i])
        .map(|(_, s)| s.clone())
        .collect();
    let mut ng = Aig::new();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for b in &mut p.bits {
            let l = ng.input();
            substitutions.insert(b.node(), l.xor(b.is_compl()));
            *b = l;
        }
    }
    for s in &mut d.states {
        let l = ng.input();
        substitutions.insert(s.q.node(), l.xor(s.q.is_compl()));
        s.q = l;
    }
    let roots = super::design_roots(d);
    let roots = d
        .aig
        .copy_into(&mut ng, &roots, &mut |_, n| substitutions[&n]);
    d.aig = ng;
    super::set_roots(d, &roots);
    super::sweep(d);
    Ok(removed)
}

/// Logical state names implemented by an already retained equivalent state after reset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivalentState {
    pub name: String,
    pub representative: String,
    #[serde(default)]
    pub inverted: bool,
}

/// Refine reset-compatible state classes until their next-state functions agree under the
/// class relation. Class refinement can split candidates, never create an unproved relation.
/// Every representative is retained as a physical state witness, even if a merged output
/// becomes unobservable. No arbitrary-initial-state merging is performed.
pub fn merge(
    d: &mut Design,
    after_reset: &[(String, bool)],
) -> Result<Vec<EquivalentState>, String> {
    if after_reset.is_empty() {
        return Ok(Vec::new());
    }
    let mut pulse = BTreeMap::new();
    for (name, value) in after_reset {
        let p = d
            .port(name)
            .filter(|p| p.input && p.scalar)
            .ok_or_else(|| format!("after-reset input {name} must be scalar"))?;
        let value = *value ^ p.bits[0].is_compl();
        if pulse
            .insert(p.bits[0].node(), value)
            .is_some_and(|old| old != value)
        {
            return Err(format!("conflicting after-reset values for {name}"));
        }
    }
    let controls: Vec<_> = d
        .states
        .iter()
        .flat_map(|s| std::iter::once(s.clk).chain(s.arst.map(|(r, _)| r)))
        .collect();
    let protected: std::collections::BTreeSet<_> = d.aig.cone(&controls).into_iter().collect();
    let mut groups = BTreeMap::new();
    let mut phases = vec![false; d.states.len()];
    let mut classes: Vec<_> = d
        .states
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if s.kind == StateKind::Flop && !protected.contains(&s.q.node()) {
                if let Some((r, value)) = s.arst {
                    if pulse.get(&r.node()).is_some_and(|v| *v ^ r.is_compl()) {
                        phases[i] = value;
                        return *groups.entry((s.clk, r)).or_insert(i);
                    }
                }
            }
            i
        })
        .collect();
    let mut ng = Aig::new();
    let mut img = vec![Lit::FALSE; d.aig.num_nodes()];
    for p in d.ports.iter().filter(|p| p.input) {
        for &l in &p.bits {
            img[l.node() as usize] = ng.input().xor(l.is_compl());
        }
    }
    let vars: Vec<_> = d.states.iter().map(|_| ng.input()).collect();
    for (i, s) in d.states.iter().enumerate() {
        img[s.q.node() as usize] = vars[classes[i]].xor(phases[i] ^ s.q.is_compl());
    }
    let mut fanouts = vec![Vec::new(); d.aig.num_nodes()];
    for n in 1..d.aig.num_nodes() {
        if d.aig.is_and(n as u32) {
            let (a, b) = d.aig.fanins(n as u32);
            fanouts[a.node() as usize].push(n);
            fanouts[b.node() as usize].push(n);
        }
    }
    let mut dirty = vec![true; d.aig.num_nodes()];
    loop {
        for n in 1..d.aig.num_nodes() {
            if dirty[n] && d.aig.is_and(n as u32) {
                let (a, b) = d.aig.fanins(n as u32);
                img[n] = ng.and(
                    img[a.node() as usize].xor(a.is_compl()),
                    img[b.node() as usize].xor(b.is_compl()),
                );
            }
        }
        let data: Vec<_> = d
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| img[s.d.node() as usize].xor(s.d.is_compl() ^ phases[i]))
            .collect();
        let mut seed = 0xa513_7692_f409_dcb7u64;
        let simulated: Vec<_> = (0..2)
            .map(|_| {
                let inputs: Vec<_> = (0..ng.num_inputs())
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        (seed & !3) | 2
                    })
                    .collect();
                ng.simulate(&inputs)
            })
            .collect();
        let mut refined: BTreeMap<(usize, u64, u64), Vec<usize>> = BTreeMap::new();
        let mut proofs = 0;
        let next: Vec<_> = data
            .iter()
            .enumerate()
            .map(|(i, &value)| {
                let key = (
                    classes[i],
                    Aig::lit_value(&simulated[0], value),
                    Aig::lit_value(&simulated[1], value),
                );
                let representatives = refined.entry(key).or_default();
                if let Some(&owner) = representatives.iter().find(|&&r| {
                    if data[r] == value {
                        true
                    } else if phases[i] != phases[r] && proofs < 128 {
                        proofs += 1;
                        super::merge::local_equal(&ng, value, data[r])
                    } else {
                        false
                    }
                }) {
                    owner
                } else {
                    representatives.push(i);
                    i
                }
            })
            .collect();
        if next == classes {
            break;
        }
        dirty.fill(false);
        let mut todo = Vec::new();
        for (i, s) in d.states.iter().enumerate() {
            if next[i] != classes[i] {
                let n = s.q.node() as usize;
                img[n] = vars[next[i]].xor(phases[i] ^ s.q.is_compl());
                todo.push(n);
            }
        }
        while let Some(n) = todo.pop() {
            for &f in &fanouts[n] {
                if !std::mem::replace(&mut dirty[f], true) {
                    todo.push(f);
                }
            }
        }
        classes = next;
    }
    let aliases: Vec<_> = classes
        .iter()
        .enumerate()
        .filter(|(i, r)| *i != **r)
        .map(|(i, &r)| EquivalentState {
            name: d.states[i].name.clone(),
            representative: d.states[r].name.clone(),
            inverted: phases[i] ^ phases[r],
        })
        .collect();
    if aliases.is_empty() {
        return Ok(aliases);
    }
    drop(ng);
    drop(img);
    drop(vars);
    drop(fanouts);
    drop(dirty);
    let mut ng = Aig::new();
    let mut image = BTreeMap::new();
    for p in d.ports.iter_mut().filter(|p| p.input) {
        for l in &mut p.bits {
            let q = ng.input();
            image.insert(l.node(), q.xor(l.is_compl()));
            *l = q;
        }
    }
    let mut vars = BTreeMap::new();
    let mut states = Vec::new();
    for (i, s) in d.states.iter().enumerate() {
        let q = *vars.entry(classes[i]).or_insert_with(|| ng.input());
        image.insert(
            s.q.node(),
            q.xor(s.q.is_compl() ^ phases[i] ^ phases[classes[i]]),
        );
        if classes[i] == i {
            let mut s = s.clone();
            s.q = q;
            states.push(s);
        }
    }
    d.states = states;
    let roots = super::design_roots(d);
    let roots = d.aig.copy_into(&mut ng, &roots, &mut |_, n| image[&n]);
    d.aig = ng;
    super::set_roots(d, &roots);
    Ok(aliases)
}
