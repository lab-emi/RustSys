// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Re-encoded registers (state-map export): an RTL register whose implementation elements carry
//! a different encoding, typically an FSM state register recoded to one-hot. This module has the
//! candidate variables and code helpers; `explore` finds the elements and the code table.

use std::collections::BTreeMap;

use super::Setting;
use crate::circ::B;
use crate::corr::{Corr, Encoding};

/// Bits of a candidate variable: at most `2^MAX_BITS` codes.
pub(super) const MAX_BITS: usize = 5;

/// The RTL variable and bit index of a state name.
fn split(name: &str) -> (&str, Option<usize>) {
    match name.rfind('[') {
        Some(i) if name.ends_with(']') => (
            &name[..i],
            name[i + 1..name.len() - 1].parse::<usize>().ok(),
        ),
        _ => (name, None),
    }
}

/// RTL variables that may carry a re-encoded register: bits `var[0]`..`var[k-1]` with
/// `2 <= k <= MAX_BITS`, flip-flops with one clock and one asynchronous reset condition, none of
/// them restricted (CHECK-REQ-0048), a constant or a member of a shared pair.
pub(super) fn variables(s: &Setting<'_>, corr: &Corr) -> Vec<(String, Vec<usize>)> {
    let model = s.model;
    let mut groups: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for (si, st) in model.states.iter().enumerate() {
        if let (var, Some(index)) = split(&st.name) {
            groups.entry(var).or_default().push((index, si));
        }
    }
    let mut out = Vec::new();
    for (var, mut members) in groups {
        members.sort_unstable();
        let k = members.len();
        if !(2..=MAX_BITS).contains(&k) || members.iter().enumerate().any(|(b, (i, _))| *i != b) {
            continue;
        }
        let bits: Vec<usize> = members.iter().map(|(_, si)| *si).collect();
        let first = &model.states[bits[0]];
        let Some((cond, _)) = first.arst else {
            continue;
        };
        if bits.iter().any(|&si| {
            let st = &model.states[si];
            st.latch
                || s.restricted(si)
                || corr.constants[si].is_some()
                || st.clk != first.clk
                || st.arst.map(|(c, _)| c) != Some(cond)
                || corr.equivalents.iter().any(|e| e.0 == si || e.1 == si)
        }) {
            continue;
        }
        out.push((var.to_string(), bits));
    }
    out
}

/// The re-encoded register a hint names (see `crate::EncodingHint`).
pub(super) fn from_hint(s: &Setting<'_>, hint: &crate::EncodingHint) -> Result<Encoding, String> {
    let mut rtl = Vec::new();
    while let Some(si) = s
        .model
        .states
        .iter()
        .position(|st| st.name == format!("{}[{}]", hint.variable, rtl.len()))
    {
        rtl.push(si);
        if rtl.len() > MAX_BITS {
            return Err(format!(
                "encoding hint {}: more than {MAX_BITS} bits",
                hint.variable
            ));
        }
    }
    if rtl.is_empty() {
        return Err(format!(
            "encoding hint {}: no RTL state bits",
            hint.variable
        ));
    }
    let gate = hint
        .elements
        .iter()
        .map(|name| {
            s.nm.seq
                .iter()
                .position(|x| x.inst == *name)
                .ok_or_else(|| format!("encoding hint {}: no element {name}", hint.variable))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if gate.is_empty() || gate.len() > 64 || hint.codes.is_empty() {
        return Err(format!(
            "encoding hint {}: 1 to 64 elements and a code needed",
            hint.variable
        ));
    }
    if hint
        .codes
        .iter()
        .any(|(r, g)| *r >> rtl.len() != 0 || (gate.len() < 64 && *g >> gate.len() != 0))
    {
        return Err(format!(
            "encoding hint {}: code out of range",
            hint.variable
        ));
    }
    Ok(Encoding {
        variable: hint.variable.clone(),
        rtl,
        gate,
        codes: hint.codes.clone(),
    })
}

/// The RTL code an asynchronous reset establishes, when every bit has one.
pub(crate) fn reset_code(s: &Setting<'_>, bits: &[usize]) -> Option<u64> {
    let mut code = 0u64;
    for (b, &si) in bits.iter().enumerate() {
        let (_, v) = s.model.states[si].arst?;
        if v {
            code |= 1 << b;
        }
    }
    Some(code)
}

/// The element-side clock, clear and preset words a consistent encoding must show, per element.
pub(super) fn async_ok(enc: &Encoding, s: &Setting<'_>, v: &dyn Fn(B) -> u64) -> bool {
    let first = &s.model.states[enc.rtl[0]];
    let clk = v(first.clk);
    let (cond, reset) = match first.arst {
        Some((cnd, _)) => (v(cnd), reset_code(s, &enc.rtl)),
        None => (0, None),
    };
    let reset_gate = reset.and_then(|r| enc.encode(r));
    if first.arst.is_some() && reset_gate.is_none() {
        return false;
    }
    enc.gate.iter().enumerate().all(|(i, &ci)| {
        let x = &s.nm.seq[ci];
        let one = reset_gate.is_some_and(|d| (d >> i) & 1 == 1);
        v(x.clk) == clk
            && v(x.clear) == if one { 0 } else { cond }
            && v(x.preset) == if one { cond } else { 0 }
    })
}

/// Per-lane codes of a set of words.
pub(super) fn lane_codes(words: &[u64]) -> [u64; 64] {
    let mut out = [0u64; 64];
    for (b, w) in words.iter().enumerate() {
        for (lane, code) in out.iter_mut().enumerate() {
            *code |= ((w >> lane) & 1) << b;
        }
    }
    out
}
