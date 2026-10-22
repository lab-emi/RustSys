// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Implement a state and its complement on compatible Q/QN pins while retaining both net names.

use super::{model::Model, network::Network};

fn same_state(old: &super::model::CellModel<'_>, new: &super::model::CellModel<'_>) -> bool {
    let a = &old.raw.sequentials[0];
    let b = &new.raw.sequentials[0];
    // Internal state-variable identifiers have no electrical or behavioral meaning.
    // Output polarities have already been normalized by the library classifier.
    a.kind == b.kind
        && a.data == b.data
        && a.clock == b.clock
        && a.clocked_on_also == b.clocked_on_also
        && a.clear == b.clear
        && a.preset == b.preset
        && a.clear_preset_var1 == b.clear_preset_var1
        && a.clear_preset_var2 == b.clear_preset_var2
}

/// Absorbs an inverter on a state output into a cell that provides both phases (SYNTH-REQ-0547).
pub(super) fn fold(net: &mut Network, lib: &Model<'_>) -> usize {
    let alternatives: Vec<Vec<usize>> = lib
        .cells
        .iter()
        .map(|old| {
            if old.clock.is_none() {
                return Vec::new();
            }
            let mut cells: Vec<_> = lib
                .cells
                .iter()
                .enumerate()
                .filter(|(_, new)| {
                    new.clock.is_some()
                        && new.q_pins.iter().all(Option::is_some)
                        && same_state(old, new)
                        && old.inputs.len() == new.inputs.len()
                        && old
                            .inputs
                            .iter()
                            .zip(&new.inputs)
                            .all(|(&a, &b)| old.raw.pins[a].name == new.raw.pins[b].name)
                })
                .map(|(i, _)| i)
                .collect();
            cells.sort_by(|&a, &b| {
                lib.cells[a]
                    .raw
                    .area
                    .total_cmp(&lib.cells[b].raw.area)
                    .then(a.cmp(&b))
            });
            cells
        })
        .collect();
    let mut removed = 0;
    for g in 0..net.gates.len() {
        let cell = net.gates[g].cell;
        let old = &lib.cells[cell];
        if !net.gates[g].active || alternatives[cell].is_empty() || net.gates[g].outputs.len() != 1
        {
            continue;
        }
        let (pin, source) = net.gates[g].outputs[0];
        if net.nodes[source].protected {
            continue;
        }
        let Some(phase) = old.q_pins.iter().position(|p| *p == Some(pin)) else {
            continue;
        };
        let Some((inv, dest)) = net.nodes[source].sinks.iter().find_map(|&(g, _)| {
            let gate = &net.gates[g];
            let c = &lib.cells[gate.cell];
            if gate.active && c.comb && c.inputs.len() == 1 && c.truth == Some(1) {
                let dest = gate.outputs[0].1;
                (!net.nodes[dest].protected).then_some((g, dest))
            } else {
                None
            }
        }) else {
            continue;
        };
        let available = old.raw.area + lib.cells[net.gates[inv].cell].raw.area;
        let Some(&replacement) = alternatives[cell]
            .iter()
            .find(|&&c| lib.cells[c].raw.area <= available + 1e-8)
        else {
            continue;
        };
        let c = &lib.cells[replacement];
        net.gates[g].cell = replacement;
        net.gates[g].outputs = vec![
            (c.q_pins[phase].unwrap(), source),
            (c.q_pins[1 - phase].unwrap(), dest),
        ];
        net.gates[inv].active = false;
        removed += 1;
    }
    net.rebuild(lib)
        .expect("compatible Q/QN substitution preserves the graph");
    removed
}
