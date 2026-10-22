// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Cell statistics, logic depth and the mapping delay estimate of a written netlist, for
//! commands that change a netlist after synthesis (SYNTH-REQ-0565).
//!
//! The delay estimate uses the per-input cell delays of SYNTH-REQ-0513 in the fixed-point units of
//! SYNTH-REQ-0514, so it is comparable with the `estimated_delay` of the synthesis report: the
//! latest arrival at a primary output or at an input of a sequential cell, with primary inputs,
//! constants and outputs of sequential and tie cells arriving at 0. It is a proxy, not timing.

use std::collections::BTreeMap;

use rustc_hash::FxHashMap;
use rusteda_formats::verilog::write::{NlDir, NlModule, NlRef};

use crate::map::library::{CellLib, DELAY_SCALE};

/// What [`netlist_estimate`] computes (SYNTH-REQ-0565).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetlistEstimate {
    /// Sequential instances.
    pub sequential_cells: usize,
    /// Single-output combinational instances.
    pub combinational_cells: usize,
    /// Inverter instances.
    pub inverters: usize,
    /// The most combinational instances on a path.
    pub logic_depth: u32,
    /// The latest arrival in library time units (SYNTH-REQ-0513).
    pub estimated_delay: f64,
    /// Instances of cells that the classified library does not describe: their outputs start
    /// paths, as a sequential output does.
    pub unmodelled_cells: usize,
    /// Combinational instances on a combinational cycle, which are not timed.
    pub cyclic_cells: usize,
    /// Instances per cell name.
    pub cells: BTreeMap<String, usize>,
}

fn intern<'a>(r: &'a NlRef, ids: &mut FxHashMap<&'a NlRef, usize>) -> usize {
    let next = ids.len();
    *ids.entry(r).or_insert(next)
}

fn root(parent: &mut [usize], mut n: usize) -> usize {
    while parent[n] != n {
        parent[n] = parent[parent[n]];
        n = parent[n];
    }
    n
}

/// Statistics, logic depth and delay estimate of `m` against the classified library `lib`
/// (SYNTH-REQ-0565).
pub fn netlist_estimate(m: &NlModule, lib: &CellLib) -> NetlistEstimate {
    let mut ids: FxHashMap<&NlRef, usize> = FxHashMap::default();
    let ports: Vec<NlRef> = m
        .ports
        .iter()
        .filter(|p| p.dir == NlDir::Output)
        .flat_map(|p| match p.width {
            Some(w) => (0..w).map(|i| NlRef::Bit(p.name.clone(), i)).collect(),
            None => vec![NlRef::Net(p.name.clone())],
        })
        .collect();
    for r in &ports {
        intern(r, &mut ids);
    }
    for inst in &m.instances {
        for (_, r) in &inst.conns {
            intern(r, &mut ids);
        }
    }
    for (a, b) in &m.assigns {
        intern(a, &mut ids);
        intern(b, &mut ids);
    }
    let mut parent: Vec<usize> = (0..ids.len()).collect();
    for (a, b) in &m.assigns {
        let (x, y) = (root(&mut parent, ids[a]), root(&mut parent, ids[b]));
        parent[x.max(y)] = x.min(y);
    }
    let node = |r: &NlRef, parent: &mut [usize]| root(parent, ids[r]);

    let comb: FxHashMap<&str, usize> = lib
        .comb
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name.as_str(), i))
        .collect();
    let seq: FxHashMap<&str, usize> = lib
        .seq
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name.as_str(), i))
        .collect();
    let ties: Vec<&str> = [&lib.tie0, &lib.tie1]
        .into_iter()
        .flatten()
        .map(|(name, _)| name.as_str())
        .collect();
    let mut out = NetlistEstimate::default();
    // Combinational gates: (inputs with their delays, output).
    let mut gates: Vec<(Vec<(usize, i64)>, Option<usize>)> = Vec::new();
    let mut endpoints: Vec<usize> = ports.iter().map(|r| node(r, &mut parent)).collect();
    for inst in &m.instances {
        *out.cells.entry(inst.cell.clone()).or_default() += 1;
        let conn = |pin: &str| inst.conns.iter().find(|(p, _)| p == pin).map(|(_, r)| r);
        if let Some(&c) = comb.get(inst.cell.as_str()) {
            let cell = &lib.comb[c];
            out.combinational_cells += 1;
            out.inverters += usize::from(cell.inputs.len() == 1 && cell.tt == 0b01);
            let inputs = cell
                .inputs
                .iter()
                .enumerate()
                .filter_map(|(i, pin)| {
                    let d = cell.delay_q.get(i).copied().unwrap_or(1);
                    conn(pin).map(|r| (node(r, &mut parent), i64::from(d)))
                })
                .collect();
            let output = conn(&cell.output).map(|r| node(r, &mut parent));
            gates.push((inputs, output));
        } else if seq.contains_key(inst.cell.as_str()) {
            out.sequential_cells += 1;
            endpoints.extend(inst.conns.iter().map(|(_, r)| node(r, &mut parent)));
        } else if !ties.contains(&inst.cell.as_str()) {
            out.unmodelled_cells += 1;
        }
    }
    // Kahn order over the combinational gates.
    let n = ids.len();
    let mut driver = vec![usize::MAX; n];
    for (g, (_, o)) in gates.iter().enumerate() {
        if let Some(o) = *o {
            driver[o] = g;
        }
    }
    let mut readers: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut pending = vec![0usize; gates.len()];
    for (g, (inputs, _)) in gates.iter().enumerate() {
        for &(i, _) in inputs {
            readers[i].push(g);
            if driver[i] != usize::MAX {
                pending[g] += 1;
            }
        }
    }
    let mut ready: Vec<usize> = (0..gates.len()).filter(|&g| pending[g] == 0).collect();
    let mut arrival = vec![0i64; n];
    let mut depth = vec![0u32; n];
    let mut done = 0usize;
    while let Some(g) = ready.pop() {
        done += 1;
        let (inputs, output) = &gates[g];
        let a = inputs
            .iter()
            .map(|&(i, d)| arrival[i].saturating_add(d))
            .max()
            .unwrap_or(0);
        let dp = 1 + inputs.iter().map(|&(i, _)| depth[i]).max().unwrap_or(0);
        out.logic_depth = out.logic_depth.max(dp);
        if let Some(o) = *output {
            arrival[o] = a;
            depth[o] = dp;
            for &r in &readers[o] {
                pending[r] -= 1;
                if pending[r] == 0 {
                    ready.push(r);
                }
            }
        }
    }
    out.cyclic_cells = gates.len() - done;
    let latest = endpoints.iter().map(|&e| arrival[e]).max().unwrap_or(0);
    out.estimated_delay = latest as f64 / DELAY_SCALE;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusteda_formats::verilog::write::{NlInstance, NlPort};

    fn lib() -> CellLib {
        let text = r#"library (t) {
  time_unit : "1ns";
  cell (inv) { area : 1; pin (A) { direction : input; capacitance : 1; }
    pin (Y) { direction : output; function : "!A";
      timing () { related_pin : "A"; cell_rise (scalar) { values ("0.5"); } cell_fall (scalar) { values ("0.5"); } } } }
  cell (and2) { area : 2; pin (A) { direction : input; capacitance : 1; } pin (B) { direction : input; capacitance : 1; }
    pin (Y) { direction : output; function : "A & B";
      timing () { related_pin : "A"; cell_rise (scalar) { values ("1.0"); } cell_fall (scalar) { values ("1.0"); } }
      timing () { related_pin : "B"; cell_rise (scalar) { values ("2.0"); } cell_fall (scalar) { values ("2.0"); } } } }
}"#;
        let l = rusteda_formats::liberty::read_text(text, "t.lib").unwrap();
        CellLib::build(&[l], &[]).unwrap()
    }

    fn inst(cell: &str, name: &str, conns: &[(&str, NlRef)]) -> NlInstance {
        NlInstance {
            cell: cell.into(),
            name: name.into(),
            conns: conns
                .iter()
                .map(|(p, r)| (p.to_string(), r.clone()))
                .collect(),
        }
    }

    // SYNTH-REQ-0565: depth, per-input delays, assigns as aliases, unmodelled cells.
    #[test]
    fn depth_and_delay_of_a_small_netlist() {
        let net = |n: &str| NlRef::Net(n.into());
        let m = NlModule {
            name: "m".into(),
            ports: vec![
                NlPort {
                    name: "a".into(),
                    dir: NlDir::Input,
                    width: None,
                },
                NlPort {
                    name: "b".into(),
                    dir: NlDir::Input,
                    width: None,
                },
                NlPort {
                    name: "y".into(),
                    dir: NlDir::Output,
                    width: None,
                },
                NlPort {
                    name: "z".into(),
                    dir: NlDir::Output,
                    width: None,
                },
            ],
            wires: vec![("n1".into(), None), ("n2".into(), None)],
            instances: vec![
                inst("inv", "u1", &[("A", net("a")), ("Y", net("n1"))]),
                inst(
                    "and2",
                    "u2",
                    &[("A", net("n1")), ("B", net("b")), ("Y", net("n2"))],
                ),
                inst("black_box", "u3", &[("I", net("a")), ("O", net("z"))]),
            ],
            assigns: vec![(net("y"), net("n2"))],
        };
        let e = netlist_estimate(&m, &lib());
        assert_eq!(e.logic_depth, 2);
        assert_eq!(
            (e.combinational_cells, e.inverters, e.sequential_cells),
            (2, 1, 0)
        );
        assert_eq!((e.unmodelled_cells, e.cyclic_cells), (1, 0));
        // a -> u1 (0.5) -> u2 pin A (1.0) = 1.5; b -> u2 pin B (2.0) = 2.0.
        assert!((e.estimated_delay - 2.0).abs() < 1e-9, "{e:?}");
        assert_eq!(e.cells["and2"], 1);
    }
}
