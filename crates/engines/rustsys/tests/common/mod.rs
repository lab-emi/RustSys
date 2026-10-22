// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Test helpers: elaborate text sources and evaluate the resulting design.

#![allow(dead_code)]

use std::collections::HashMap;

use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
use rustsys::aig::{Aig, Lit};
use rustsys::elab::{Design, ElabOptions, elaborate};

/// Elaborates sources; returns the design or the rendered diagnostics.
pub fn elab(srcs: &[&str], top: Option<&str>) -> Result<Design, String> {
    let mut sm = SourceMap::new();
    let named: Vec<(String, &str)> = srcs
        .iter()
        .enumerate()
        .map(|(i, s)| (format!("f{i}.sv"), *s))
        .collect();
    let texts: Vec<(&str, &str)> = named.iter().map(|(n, s)| (n.as_str(), *s)).collect();
    let (units, diags) = read_texts(
        &mut sm,
        &texts,
        PpOptions {
            defines: vec![("SYNTHESIS".into(), String::new())],
            ..Default::default()
        },
    );
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags
            .iter()
            .map(|d| d.render(&sm))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let opts = ElabOptions {
        parallel_carry: false,
        carry_cost_weight: None,
        xor_carry: false,
        carry_plan: Vec::new(),
        assume_full_case: false,
        assume_parallel_case: false,
        top: top.map(str::to_string),
        params: Vec::new(),
    };
    let (design, diags) = elaborate(&sm, &units, &opts);
    match design {
        Some(d) => Ok(d),
        None => Err(diags
            .iter()
            .map(|d| d.render(&sm))
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

/// Elaborates and panics with diagnostics on failure.
pub fn design(src: &str) -> Design {
    elab(&[src], None).unwrap_or_else(|e| panic!("elaboration failed:\n{e}"))
}

/// Evaluates outputs and next-state functions for one assignment of named inputs and states.
pub struct Eval<'d> {
    pub d: &'d Design,
    sim: Vec<u64>,
}

impl<'d> Eval<'d> {
    pub fn new(d: &'d Design, inputs: &[(&str, u64)], states: &[(&str, bool)]) -> Eval<'d> {
        let mut words = Vec::new();
        for p in d.ports.iter().filter(|p| p.input) {
            let v = inputs.iter().find(|(n, _)| *n == p.name).map_or(0, |x| x.1);
            for i in 0..p.bits.len() {
                words.push(if i < 64 && (v >> i) & 1 == 1 {
                    u64::MAX
                } else {
                    0
                });
            }
        }
        for s in &d.states {
            let v = states
                .iter()
                .find(|(n, _)| *n == s.name)
                .is_some_and(|x| x.1);
            words.push(if v { u64::MAX } else { 0 });
        }
        Eval {
            d,
            sim: d.simulate(&words),
        }
    }

    pub fn lit(&self, l: Lit) -> bool {
        Aig::lit_value(&self.sim, l) & 1 == 1
    }

    pub fn out(&self, name: &str) -> u64 {
        let p = self
            .d
            .port(name)
            .unwrap_or_else(|| panic!("no port {name}"));
        p.bits
            .iter()
            .enumerate()
            .fold(0u64, |a, (i, l)| a | (u64::from(self.lit(*l)) << i))
    }

    pub fn next(&self, state: &str) -> bool {
        let s = self
            .d
            .state(state)
            .unwrap_or_else(|| panic!("no state {state}"));
        self.lit(s.d)
    }
}

/// Simple cycle simulator for single-clock designs whose clock input is `clk` (rising edge)
/// and whose asynchronous controls are evaluated before each edge.
pub struct Cycle<'d> {
    pub d: &'d Design,
    pub state: HashMap<String, bool>,
}

impl<'d> Cycle<'d> {
    pub fn new(d: &'d Design) -> Cycle<'d> {
        Cycle {
            d,
            state: d.states.iter().map(|s| (s.name.clone(), false)).collect(),
        }
    }

    fn states(&self) -> Vec<(&str, bool)> {
        self.state.iter().map(|(k, v)| (k.as_str(), *v)).collect()
    }

    /// Applies inputs, performs asynchronous updates, then one clock edge; returns outputs
    /// sampled before the edge.
    pub fn step(&mut self, inputs: &[(&str, u64)]) -> HashMap<String, u64> {
        let st = self.states();
        let e = Eval::new(self.d, inputs, &st);
        let outs: HashMap<String, u64> = self
            .d
            .ports
            .iter()
            .filter(|p| !p.input)
            .map(|p| (p.name.clone(), e.out(&p.name)))
            .collect();
        let mut next = self.state.clone();
        for s in &self.d.states {
            let v = match s.arst {
                Some((c, v)) if e.lit(c) => v,
                _ => e.lit(s.d),
            };
            next.insert(s.name.clone(), v);
        }
        self.state = next;
        outs
    }

    /// Reads a multi-bit state variable `name[k]` or a scalar `name`.
    pub fn read(&self, name: &str, width: usize) -> u64 {
        if width == 1 {
            return u64::from(self.state[name]);
        }
        (0..width).fold(0, |a, k| {
            a | (u64::from(self.state[&format!("{name}[{k}]")]) << k)
        })
    }
}
