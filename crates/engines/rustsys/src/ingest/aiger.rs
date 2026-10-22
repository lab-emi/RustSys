// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Lowering of an AIGER graph (SYNTH-REQ-0903, SYNTH-REQ-0904, SYNTH-REQ-0906 to
//! SYNTH-REQ-0908): named ports, latches on the global clock, AND gates one to one.

use rusteda_formats::aiger::{Aig as Graph, Reset};

use super::blif::scalar;
use super::{Info, InitialValues, Lowered, UNSET, check_ports, check_states, fresh_name};
use crate::aig::{Aig, Lit};
use crate::elab::{Design, State, StateKind};

/// Lowers `g` as module `module` (SYNTH-REQ-0902 to SYNTH-REQ-0908).
pub(super) fn lower(g: &Graph, module: &str, mode: InitialValues) -> Result<Lowered, Vec<String>> {
    lower_inner(g, module, mode).map_err(|m| vec![m])
}

fn lower_inner(g: &Graph, module: &str, mode: InitialValues) -> Result<Lowered, String> {
    let named = |t: &std::collections::BTreeMap<u32, String>, k: usize, p: &str| {
        t.get(&(k as u32))
            .cloned()
            .unwrap_or_else(|| format!("{p}{k}"))
    };
    let inputs: Vec<String> = (0..g.inputs.len())
        .map(|k| named(&g.symbols.inputs, k, "i"))
        .collect();
    let outputs: Vec<String> = (0..g.outputs.len())
        .map(|k| named(&g.symbols.outputs, k, "o"))
        .collect();
    let latches: Vec<String> = (0..g.latches.len())
        .map(|k| named(&g.symbols.latches, k, "l"))
        .collect();
    let declared: Vec<(String, bool)> = g
        .latches
        .iter()
        .zip(&latches)
        .filter_map(|(l, n)| match l.reset {
            Reset::Zero => Some((n.clone(), false)),
            Reset::One => Some((n.clone(), true)),
            Reset::Uninitialized => None,
        })
        .collect();
    let needs_init = mode == InitialValues::Reset && !declared.is_empty();
    let taken = |x: &str, extra: &[String]| {
        inputs
            .iter()
            .chain(&outputs)
            .chain(&latches)
            .chain(extra)
            .any(|p| p == x)
    };
    let clock_port = (!g.latches.is_empty()).then(|| fresh_name("clk", &|x| taken(x, &[])));
    let init_port = needs_init.then(|| {
        let extra: Vec<String> = clock_port.iter().cloned().collect();
        fresh_name("init", &|x| taken(x, &extra))
    });
    let mut all: Vec<(String, bool)> = inputs.iter().map(|n| (n.clone(), true)).collect();
    all.extend(
        clock_port
            .iter()
            .chain(&init_port)
            .map(|n| (n.clone(), true)),
    );
    all.extend(outputs.iter().map(|n| (n.clone(), false)));
    check_ports(&all)?;
    let mut aig = Aig::new();
    let mut var = vec![UNSET; g.max_var as usize + 1];
    var[0] = Lit::FALSE;
    let mut ports = Vec::with_capacity(all.len());
    for (&l, n) in g.inputs.iter().zip(&inputs) {
        let x = aig.input();
        var[(l >> 1) as usize] = x;
        ports.push(scalar(n, true, x));
    }
    let mut generated = |aig: &mut Aig, p: &Option<String>| {
        p.as_ref().map(|n| {
            let x = aig.input();
            ports.push(scalar(n, true, x));
            x
        })
    };
    let clk = generated(&mut aig, &clock_port);
    let init = generated(&mut aig, &init_port);
    let mut qs = Vec::with_capacity(g.latches.len());
    for l in &g.latches {
        let q = aig.input();
        var[(l.lit >> 1) as usize] = q;
        qs.push(q);
    }
    let lit = |var: &[Lit], l: u32| var[(l >> 1) as usize].xor(l & 1 == 1);
    for &[lhs, a, b] in &g.ands {
        let x = aig.and(lit(&var, a), lit(&var, b));
        var[(lhs >> 1) as usize] = x;
    }
    let mut states = Vec::with_capacity(g.latches.len());
    for ((l, q), n) in g.latches.iter().zip(&qs).zip(&latches) {
        let value = match l.reset {
            Reset::Zero => Some(false),
            Reset::One => Some(true),
            Reset::Uninitialized => None,
        };
        states.push(State {
            name: n.clone(),
            kind: StateKind::Flop,
            q: *q,
            d: lit(&var, l.next),
            clk: clk.unwrap_or(Lit::FALSE),
            arst: init.zip(value),
        });
    }
    for (&o, n) in g.outputs.iter().zip(&outputs) {
        ports.push(scalar(n, false, lit(&var, o)));
    }
    let design = Design {
        top: module.to_string(),
        aig,
        ports,
        states,
    };
    check_states(&design)?;
    let mut messages = Vec::new();
    if mode == InitialValues::Arbitrary && !declared.is_empty() {
        messages.push(format!(
            "warning[S0913]: the declared initial values of {} latches are not implemented \
             (--initial-values arbitrary); every state starts in an arbitrary state",
            declared.len()
        ));
    }
    Ok(Lowered {
        design,
        warnings: messages.len(),
        messages,
        info: Info {
            format: "aiger",
            initial_values: declared,
            clock_port,
            init_port,
        },
    })
}
