// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The reference model of a gate-level netlist (CHECK-REQ-0103): read with the checker's own
//! netlist model over the cells of its libraries, its ports and sequential instances become the
//! inputs, outputs and states that the synthesized netlist is compared with.

use rusteda_formats::liberty::Library;
use rusteda_formats::verilog::SourceMap;
use rusteda_formats::verilog::ast::Item;

use crate::Input;
use crate::circ::{B, Circuit};
use crate::liberty::Unread;
use crate::netlist;
use crate::rtl::{RtlModel, RtlState};
use crate::sources::{CELL, SOURCE, SourceModel};

/// Builds the reference model of the netlist `input` into `c` (CHECK-REQ-0103).
pub(crate) fn build(
    c: &mut Circuit,
    input: &Input,
    libs: &[Library],
    unread: &[Unread],
    top: Option<&str>,
    supply_ports: &[String],
) -> Result<SourceModel, String> {
    let mut sm = SourceMap::new();
    let units = netlist::read(&mut sm, input)?;
    let modules: Vec<&str> = units
        .iter()
        .flat_map(|u| &u.items)
        .filter_map(|i| match i {
            Item::Module(m) => Some(m.name.name.as_str()),
            _ => None,
        })
        .collect();
    let name = match top {
        Some(t) => t.to_string(),
        None if modules.len() == 1 => modules[0].to_string(),
        None => {
            return Err(format!(
                "error[{SOURCE}]: the gate-level reference holds {} modules; --top names one",
                modules.len()
            ));
        }
    };
    if !modules.contains(&name.as_str()) {
        return Err(format!(
            "error[{SOURCE}]: the gate-level reference has no module {name}"
        ));
    }
    let nm = netlist::build(c, &sm, &units, libs, &name, None, supply_ports, unread)?;
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut port_order = Vec::new();
    for (pname, is_in, width) in &nm.ports {
        if *is_in {
            port_order.push((true, inputs.len()));
            inputs.push((pname.clone(), nm.nets[pname].clone(), width.is_none()));
        } else {
            port_order.push((false, outputs.len()));
            outputs.push((pname.clone(), nm.outputs[pname].clone(), width.is_none()));
        }
    }
    let mut states = Vec::with_capacity(nm.seq.len());
    for s in &nm.seq {
        let arst = match (s.clear, s.preset) {
            (B::F, B::F) => None,
            (x, B::F) => Some((x, false)),
            (B::F, y) => Some((y, true)),
            _ => {
                return Err(format!(
                    "error[{CELL}]: instance {} of cell {} uses both its clear and its preset; \
                     a state element has one asynchronous control",
                    s.inst, s.cell
                ));
            }
        };
        states.push(RtlState {
            name: s.inst.clone(),
            latch: s.latch,
            q: s.iq,
            next: s.next,
            clk: s.clk,
            arst,
        });
    }
    let roots: Vec<B> = outputs
        .iter()
        .flat_map(|(_, b, _)| b.iter().copied())
        .chain(
            states
                .iter()
                .flat_map(|s| [s.next, s.clk].into_iter().chain(s.arst.map(|(r, _)| r))),
        )
        .collect();
    if c.order(&roots).is_err() {
        return Err(format!(
            "error[{SOURCE}]: the gate-level reference has a combinational cycle"
        ));
    }
    Ok(SourceModel {
        model: RtlModel {
            top: nm.name.clone(),
            inputs,
            outputs,
            port_order,
            states,
            observations: Vec::new(),
            warnings: Vec::new(),
        },
        initial_values: Vec::new(),
        warnings: Vec::new(),
    })
}
