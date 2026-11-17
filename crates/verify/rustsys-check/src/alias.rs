// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Name evidence for a state map (obligation export only). Synthesis names a flip-flop's output
//! net after a public RTL wire it drives and may number vector bits by declared index, while RTL
//! states are named by bit position (SYNTH-REQ-0603). Names stay proposals: refinement and the
//! exported obligations decide, so a wrong name costs a proof attempt, never soundness.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::corr::{Corr, Relation, Setting};

/// Bit names recorded at most (signal bits and netlist net bits each).
const MAX_NAMES: usize = 1 << 22;

/// What the netlist's names say about its state elements.
pub(crate) struct Names {
    /// Per element: the RTL states (with inversion) its output nets name, and whether one of its
    /// names is ambiguous between the two bit-numbering conventions.
    elements: HashMap<usize, (BTreeSet<(usize, bool)>, bool)>,
    /// Per element: the names of its output nets.
    labels: HashMap<usize, Vec<String>>,
}

/// Characters two names share at their start and at their end (at most the shorter length).
fn shared(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let tail = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (head + tail).min(a.len().min(b.len()))
}

/// The vector name of a bit name `v[i]`.
fn signal(name: &str) -> Option<&str> {
    name.strip_suffix(']')
        .and_then(|n| n.rsplit_once('['))
        .map(|(base, _)| base)
}

/// Bit names of the netlist's nets with their variables: scalar nets as written (an escaped
/// name may carry an index), vector bits with their declared index.
fn netlist_bits(s: &Setting<'_>) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for (name, vars) in &s.nm.nets {
        if out.len() >= MAX_NAMES {
            break;
        }
        match (vars.as_slice(), s.nm.ranges.get(name)) {
            ([v], _) => out.push((name.clone(), *v)),
            (_, Some(&(l, r))) => {
                for (k, &v) in vars.iter().enumerate() {
                    let d = if l >= r { r + k as i64 } else { r - k as i64 };
                    out.push((format!("{name}[{d}]"), v));
                }
            }
            _ => {}
        }
    }
    out
}

impl Names {
    pub(crate) fn new(s: &Setting<'_>) -> Names {
        let bits = netlist_bits(s);
        let present: HashSet<&str> = bits.iter().map(|(n, _)| n.as_str()).collect();
        // Per vector with a declared range not numbered from 0: which convention the netlist
        // uses, judged only by names that exist under one convention.
        let aliases = &s.model.state_aliases;
        let mut by_position: HashMap<&str, HashSet<&str>> = HashMap::new();
        let mut by_declared: HashMap<&str, HashSet<&str>> = HashMap::new();
        for a in aliases {
            if let (Some(d), Some(sig)) = (a.declared.as_deref(), signal(&a.name)) {
                by_position.entry(sig).or_default().insert(&a.name);
                by_declared.entry(sig).or_default().insert(d);
            }
        }
        // (position names used, declared names used)
        let convention: HashMap<&str, (bool, bool)> = by_position
            .iter()
            .map(|(sig, pos)| {
                let decl = &by_declared[sig];
                let hit = |a: &HashSet<&str>, b: &HashSet<&str>| {
                    a.iter().any(|n| !b.contains(n) && present.contains(n))
                };
                (*sig, (hit(pos, decl), hit(decl, pos)))
            })
            .collect();
        let mut keys: Vec<(&str, (usize, bool))> = Vec::new();
        for a in aliases {
            let value = (a.state, a.inverted);
            match (
                a.declared.as_deref(),
                signal(&a.name).and_then(|g| convention.get(g)),
            ) {
                (Some(d), Some((false, true))) => keys.push((d, value)),
                (Some(d), Some((pos, decl))) if pos == decl => {
                    keys.push((&a.name, value));
                    keys.push((d, value));
                }
                _ => keys.push((&a.name, value)),
            }
        }
        let mut by_name: HashMap<&str, Option<(usize, bool)>> = HashMap::new();
        for (key, value) in keys {
            let slot = by_name.entry(key).or_insert(Some(value));
            if *slot != Some(value) {
                *slot = None;
            }
        }
        let iq: HashMap<u32, usize> =
            s.nm.seq
                .iter()
                .enumerate()
                .map(|(ci, x)| (x.iq, ci))
                .collect();
        let mut elements: HashMap<usize, (BTreeSet<(usize, bool)>, bool)> = HashMap::new();
        let mut labels: HashMap<usize, Vec<String>> = HashMap::new();
        for (name, v) in &bits {
            let Some((x, ginv)) = crate::netlist::trace(s.c, *v) else {
                continue;
            };
            let Some(&ci) = iq.get(&x) else {
                continue;
            };
            let list = labels.entry(ci).or_default();
            if list.len() < 8 {
                list.push(name.clone());
            }
            match by_name.get(name.as_str()) {
                Some(Some((si, rinv))) => {
                    elements.entry(ci).or_default().0.insert((*si, rinv ^ ginv));
                }
                Some(None) => elements.entry(ci).or_default().1 = true,
                None => {}
            }
        }
        Names { elements, labels }
    }

    /// How much of RTL state `name` a name of element `ci` shares at its start and end: a tie
    /// break among candidates that simulation cannot tell apart (high bits of a counter,
    /// registers that rarely change).
    pub(crate) fn likeness(&self, ci: usize, name: &str) -> usize {
        self.labels.get(&ci).map_or(0, |list| {
            list.iter().map(|l| shared(l, name)).max().unwrap_or(0)
        })
    }

    /// Whether the names of element `ci` speak against its pairing with RTL state `si` under
    /// `parity`: they name other states only, or one of them is ambiguous.
    pub(crate) fn contradicts(&self, si: usize, ci: usize, parity: bool) -> bool {
        self.elements.get(&ci).is_some_and(|(states, ambiguous)| {
            !states.contains(&(si, parity)) && (*ambiguous || !states.is_empty())
        })
    }

    /// Unambiguous one-to-one proposals: an unused element whose names name exactly one
    /// unmatched RTL state of the same kind that no other unused element names.
    pub(crate) fn proposals(&self, s: &Setting<'_>, corr: &Corr) -> Vec<(usize, Relation)> {
        let unmatched: HashSet<usize> = corr.unmatched.iter().copied().collect();
        let hints: Vec<(usize, usize, bool)> = corr
            .unused
            .iter()
            .filter_map(|&ci| {
                let (states, ambiguous) = self.elements.get(&ci)?;
                let mut it = states.iter();
                match (it.next(), it.next(), ambiguous) {
                    (Some(&(si, p)), None, false) if unmatched.contains(&si) => Some((ci, si, p)),
                    _ => None,
                }
            })
            .collect();
        let mut count: HashMap<usize, usize> = HashMap::new();
        for (_, si, _) in &hints {
            *count.entry(*si).or_default() += 1;
        }
        let mut out: Vec<(usize, Relation)> = hints
            .into_iter()
            .filter(|(ci, si, _)| {
                count[si] == 1 && s.model.states[*si].latch == s.nm.seq[*ci].latch
            })
            .map(|(ci, si, p)| (si, Relation::Cell(ci, p)))
            .collect();
        out.sort_by_key(|x| x.0);
        out
    }

    /// `corr` without the name matches that the names contradict, and their RTL states.
    pub(crate) fn release_contradicted(&self, corr: &Corr) -> (Corr, Vec<usize>) {
        let mut out = corr.clone();
        let mut released = Vec::new();
        for (si, pair) in corr.paired.iter().enumerate() {
            if let Some((ci, parity)) = *pair {
                if self.contradicts(si, ci, parity) {
                    out.release(si);
                    released.push(si);
                }
            }
        }
        (out, released)
    }
}
