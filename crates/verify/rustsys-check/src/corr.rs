// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! State correspondence: by name (CHECK-REQ-0031, CHECK-REQ-0048 to CHECK-REQ-0050) and by
//! applying discovered relations (CHECK-REQ-0032) under the same base-case rules.

use std::collections::{HashMap, HashSet};

use crate::circ::{B, Circuit};
use crate::netlist::{self, NetModel};
use crate::reset;
use crate::rtl::RtlModel;

/// A relation of one RTL state bit found without a name (CHECK-REQ-0032).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Relation {
    /// The state is this constant.
    Constant(bool),
    /// The state shares the netlist state element of another RTL state bit (its index), with
    /// odd parity when `true`.
    Shares(usize, bool),
    /// The state corresponds to a netlist state element (its index) that has no name match,
    /// with odd parity when `true`.
    Cell(usize, bool),
}

/// Everything correspondence and discovery read.
pub struct Setting<'a> {
    pub c: &'a Circuit,
    pub model: &'a RtlModel,
    pub nm: &'a NetModel,
    pub pulse: &'a HashMap<u32, bool>,
    /// Variables of the RTL clock and asynchronous-control cones.
    pub control_vars: &'a HashSet<u32>,
    pub uninitialized_dont_care: bool,
}

impl Setting<'_> {
    /// The value the after-reset pulse establishes for RTL state `si`, if any.
    pub fn base(&self, si: usize) -> Option<bool> {
        let st = &self.model.states[si];
        let (_, v) = st.arst?;
        reset::establishes(self.c, st.arst, v, self.pulse).then_some(v)
    }

    /// Latches and state in a sequential-control cone may not become constants or share an
    /// element (CHECK-REQ-0048, CHECK-REQ-0049).
    pub fn restricted(&self, si: usize) -> bool {
        let st = &self.model.states[si];
        st.latch || self.control_vars.contains(&st.q)
    }
}

/// The correspondence of one proof attempt.
#[derive(Debug, Clone)]
pub struct Corr {
    /// Netlist state element of each RTL state bit, and odd parity.
    pub paired: Vec<Option<(usize, bool)>>,
    /// The RTL state bit that defines each netlist state element's value.
    pub owners: Vec<Option<usize>>,
    pub constants: Vec<Option<bool>>,
    /// `(state, representative, inverted)`: states that share their representative's element.
    pub equivalents: Vec<(usize, usize, bool)>,
    /// Chosen initial values of unreset state (CHECK-REQ-0050).
    pub initial_choices: HashMap<u32, bool>,
    /// RTL state bits without a counterpart.
    pub unmatched: Vec<usize>,
    /// Netlist state elements without a counterpart.
    pub unused: Vec<usize>,
    /// Discovered relations in force (CHECK-REQ-0032).
    pub discovered: Vec<(usize, Relation)>,
}

/// Correspondence by name. Netlist state elements without a named counterpart are returned in
/// `unused`, not rejected; the caller decides.
pub fn by_name(s: &Setting<'_>) -> Result<Corr, String> {
    named(s, false)
}

/// Export only a one-to-one subset. Removed/aliased RTL state remains FREE;
/// no constant or sharing relation is asserted. External obligations must prove
/// outputs and every retained transition for ALL values of those extra states.
pub fn by_name_for_export(s: &Setting<'_>) -> Result<Corr, String> {
    named(s, true)
}

/// Match bounded explicit hints by exact public names. A hint can only bind an
/// unused implementation element to an unpaired RTL variable with optional
/// inversion. No constants, sharing or initial values can be expressed.
/// Scratch is O(hints), with two linear model scans; <=8192 pairs /8 MiB names.
/// External CEC must still prove every output and event/transition obligation.
pub(crate) fn explicit_export_proposals(
    s: &Setting<'_>,
    corr: &Corr,
    hints: &[crate::StateProposal],
) -> Result<Vec<(usize, Relation)>, String> {
    if hints.is_empty() {
        return Ok(Vec::new());
    }
    if hints.len() > 8192
        || hints
            .iter()
            .try_fold(0usize, |n, h| {
                n.checked_add(h.rtl_state.len())?
                    .checked_add(h.gate_instance.len())
            })
            .is_none_or(|n| n > 8 * 1024 * 1024)
    {
        return Err("state proposal count/name byte budget reached".into());
    }
    let mut rtl = HashMap::with_capacity(hints.len());
    let mut gate = HashMap::with_capacity(hints.len());
    for hint in hints {
        if hint.rtl_state.is_empty()
            || hint.gate_instance.is_empty()
            || rtl.insert(hint.rtl_state.as_str(), None).is_some()
            || gate.insert(hint.gate_instance.as_str(), None).is_some()
        {
            return Err("empty or duplicate state proposal name".into());
        }
    }
    for (si, state) in s.model.states.iter().enumerate() {
        if let Some(slot) = rtl.get_mut(state.name.as_str()) {
            if slot.replace(si).is_some() {
                return Err("ambiguous RTL state proposal name".into());
            }
        }
    }
    for (ci, cell) in s.nm.seq.iter().enumerate() {
        if let Some(slot) = gate.get_mut(cell.inst.as_str()) {
            if slot.replace(ci).is_some() {
                return Err("ambiguous implementation state proposal name".into());
            }
        }
    }
    let mut result = Vec::with_capacity(hints.len());
    for hint in hints {
        let si = rtl[hint.rtl_state.as_str()]
            .ok_or_else(|| format!("unknown proposed RTL state: {}", hint.rtl_state))?;
        let ci = gate[hint.gate_instance.as_str()].ok_or_else(|| {
            format!(
                "unknown proposed implementation state: {}",
                hint.gate_instance
            )
        })?;
        if corr.paired[si] == Some((ci, hint.inverted)) && corr.owners[ci] == Some(si) {
            continue;
        }
        if corr.paired[si].is_some()
            || corr.owners[ci].is_some()
            || s.model.states[si].latch != s.nm.seq[ci].latch
        {
            return Err(format!(
                "state proposal conflicts with existing one-to-one correspondence: {} -> {}",
                hint.rtl_state, hint.gate_instance
            ));
        }
        result.push((si, Relation::Cell(ci, hint.inverted)));
    }
    Ok(result)
}

fn named(s: &Setting<'_>, leave_unpaired_free: bool) -> Result<Corr, String> {
    let (c, model, nm) = (s.c, s.model, s.nm);
    let iq_to_cell: HashMap<u32, usize> =
        nm.seq.iter().enumerate().map(|(i, x)| (x.iq, i)).collect();
    let mut corr = Corr {
        paired: vec![None; model.states.len()],
        owners: vec![None; nm.seq.len()],
        constants: vec![None; model.states.len()],
        equivalents: Vec::new(),
        initial_choices: HashMap::new(),
        unmatched: Vec::new(),
        unused: Vec::new(),
        discovered: Vec::new(),
    };
    let mut cell_used = vec![false; nm.seq.len()];
    for (si, st) in model.states.iter().enumerate() {
        let Some(net) = nm.state_bit(&st.name)? else {
            corr.unmatched.push(si);
            continue;
        };
        if let Some(value) = reset::constant(c, B::V(net)) {
            if leave_unpaired_free {
                corr.unmatched.push(si);
                continue;
            }
            // Constant state aliases need an explicit base case (CHECK-REQ-0048, CHECK-REQ-0050).
            if s.pulse.is_empty() && !s.uninitialized_dont_care {
                return Err(format!(
                    "state net {} is not driven by a state element through inverters or buffers",
                    st.name
                ));
            }
            if !constant_allowed(s, si, value) {
                return Err(format!(
                    "state {}: the specified reset pulse does not establish constant {value}",
                    st.name
                ));
            }
            if st.arst.is_none() {
                corr.initial_choices.insert(st.q, value);
            }
            corr.constants[si] = Some(value);
            continue;
        }
        // A wire that a state element does not drive through inverters and buffers (undriven,
        // or driven by other logic) gives no correspondence (CHECK-REQ-0031).
        let Some((ci, parity)) = netlist::trace(c, net)
            .and_then(|(free, parity)| Some((*iq_to_cell.get(&free)?, parity)))
        else {
            corr.unmatched.push(si);
            continue;
        };
        if let Some(ri) = corr.owners[ci] {
            if leave_unpaired_free {
                corr.unmatched.push(si);
                continue;
            }
            let inv = parity ^ corr.paired[ri].expect("state owner").1;
            if !sharing_allowed(s, si, ri, inv) {
                return Err(format!(
                    "state element {} drives two state nets without an established reset relation",
                    nm.seq[ci].inst
                ));
            }
            corr.equivalents.push((si, ri, inv));
        } else {
            corr.owners[ci] = Some(si);
        }
        cell_used[ci] = true;
        corr.paired[si] = Some((ci, parity));
    }
    corr.unused = (0..nm.seq.len()).filter(|ci| !cell_used[*ci]).collect();
    Ok(corr)
}

/// Base case of a constant state (CHECK-REQ-0048, CHECK-REQ-0050).
fn constant_allowed(s: &Setting<'_>, si: usize, value: bool) -> bool {
    let st = &s.model.states[si];
    (!s.pulse.is_empty() || s.uninitialized_dont_care)
        && !s.restricted(si)
        && (reset::establishes(s.c, st.arst, value, s.pulse)
            || (s.uninitialized_dont_care && st.arst.is_none()))
}

/// Base case of two RTL states sharing one element (CHECK-REQ-0049).
fn sharing_allowed(s: &Setting<'_>, si: usize, ri: usize, inv: bool) -> bool {
    let (st, rep) = (&s.model.states[si], &s.model.states[ri]);
    if s.restricted(si) || s.restricted(ri) {
        return false;
    }
    if s.uninitialized_dont_care && st.arst.is_none() && rep.arst.is_none() {
        return true;
    }
    !s.pulse.is_empty()
        && rep.arst.map(|(_, v)| v).is_some_and(|v| {
            reset::establishes(s.c, rep.arst, v, s.pulse)
                && reset::establishes(s.c, st.arst, v ^ inv, s.pulse)
        })
}

/// Applies discovered relations that pass the same base-case rules as named ones; returns the
/// relations applied.
pub fn apply(
    s: &Setting<'_>,
    corr: &mut Corr,
    rels: &[(usize, Relation)],
) -> Vec<(usize, Relation)> {
    let mut applied = Vec::new();
    // Elements first, so that a shared state may refer to a state paired by discovery.
    let mut rels = rels.to_vec();
    rels.sort_by_key(|(si, r)| {
        let kind = match r {
            Relation::Cell(..) => 0,
            Relation::Constant(_) => 1,
            Relation::Shares(..) => 2,
        };
        (kind, *si)
    });
    for &(si, rel) in &rels {
        if !corr.unmatched.contains(&si) {
            continue;
        }
        let st = &s.model.states[si];
        let ok = match rel {
            Relation::Constant(v) => {
                let ok = constant_allowed(s, si, v);
                if ok {
                    if st.arst.is_none() {
                        corr.initial_choices.insert(st.q, v);
                    }
                    corr.constants[si] = Some(v);
                }
                ok
            }
            Relation::Shares(ri, inv) => {
                let rep = corr.paired[ri].filter(|(ci, _)| corr.owners[*ci] == Some(ri));
                match rep {
                    Some((ci, parity)) if sharing_allowed(s, si, ri, inv) => {
                        corr.equivalents.push((si, ri, inv));
                        corr.paired[si] = Some((ci, parity ^ inv));
                        true
                    }
                    _ => false,
                }
            }
            Relation::Cell(ci, parity) => {
                let ok = corr.unused.contains(&ci) && s.nm.seq[ci].latch == st.latch;
                if ok {
                    corr.paired[si] = Some((ci, parity));
                    corr.owners[ci] = Some(si);
                    corr.unused.retain(|x| *x != ci);
                }
                ok
            }
        };
        if ok {
            corr.unmatched.retain(|x| *x != si);
            corr.discovered.push((si, rel));
            applied.push((si, rel));
        }
    }
    applied
}

/// Report text of a discovered relation (CHECK-REQ-0032).
pub fn describe(model: &RtlModel, nm: &NetModel, rel: Relation) -> String {
    let inv = |b: bool| if b { " inverted" } else { "" };
    match rel {
        Relation::Constant(v) => format!("constant {}", u8::from(v)),
        Relation::Shares(ri, i) => format!("shares {}{}", model.states[ri].name, inv(i)),
        Relation::Cell(ci, i) => format!("cell {}{}", nm.seq[ci].inst, inv(i)),
    }
}
