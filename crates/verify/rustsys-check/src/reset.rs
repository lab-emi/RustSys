// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Independent base case of reset induction. No synthesis report is consulted.

use crate::circ::{B, Circuit, Def};
use crate::rtl::RtlModel;
use std::collections::HashMap;

pub fn pulse(model: &RtlModel, values: &[(String, bool)]) -> Result<HashMap<u32, bool>, String> {
    let mut inputs = HashMap::new();
    for (name, value) in values {
        let (_, bits, _) = model
            .inputs
            .iter()
            .find(|(n, v, scalar)| n == name && *scalar && v.len() == 1)
            .ok_or_else(|| format!("after-reset input {name} must be a scalar primary input"))?;
        if let Some(old) = inputs.insert(bits[0], *value) {
            if old != *value {
                return Err(format!("conflicting after-reset values for {name}"));
            }
        }
    }
    Ok(inputs)
}

/// Resolve only aliases and inversions; arbitrary logic and cycles are not accepted.
pub fn constant(c: &Circuit, mut b: B) -> Option<bool> {
    let mut inv = false;
    for _ in 0..=c.defs.len() {
        match b {
            B::F => return Some(inv),
            B::T => return Some(!inv),
            B::V(v) => match c.defs[v as usize] {
                Def::Alias(x) => b = x,
                Def::Not(x) => {
                    inv = !inv;
                    b = B::V(x);
                }
                _ => return None,
            },
        }
    }
    None
}

/// Checked on the original RTL circuit, before any state is constrained.
pub fn establishes(
    c: &Circuit,
    reset: Option<(B, bool)>,
    value: bool,
    pulse: &HashMap<u32, bool>,
) -> bool {
    let Some((B::V(cond), init)) = reset else {
        return false;
    };
    if init != value {
        return false;
    }
    crate::netlist::trace(c, cond)
        .is_some_and(|(input, inv)| pulse.get(&input).is_some_and(|v| *v ^ inv))
}
