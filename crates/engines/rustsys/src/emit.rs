// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Netlist emission per the output contract (SYNTH-REQ-0601 to SYNTH-REQ-0606, SYNTH-REQ-0522).

use std::fmt::Write as _;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use rusteda_formats::verilog::write::{NlDir, NlInstance, NlModule, NlPort, NlRef};

use crate::aig::Lit;
use crate::elab::Design;
use crate::map::library::CellLib;
use crate::map::{Mapped, QSource, SeqChoice};

/// Allocates generated names that avoid reserved names.
struct Names<'a> {
    reserved: FxHashSet<&'a str>,
    counters: [u32; 4],
}

impl Names<'_> {
    fn fresh(&mut self, prefix: &'static str) -> String {
        // These private namespaces are disjoint. A monotonic counter already
        // guarantees uniqueness among generated names, so only original
        // design names need storage and collision checks.
        let index = match prefix {
            "n" => 0,
            "g" => 1,
            "r" => 2,
            "t" => 3,
            _ => unreachable!("unknown generated namespace"),
        };
        loop {
            let c = &mut self.counters[index];
            let mut name = String::with_capacity(prefix.len() + 10);
            write!(name, "{prefix}{c}").expect("String write");
            *c = c.checked_add(1).expect("generated-name index limit");
            if !self.reserved.contains(name.as_str()) {
                return name;
            }
        }
    }
}

/// Statistics of an emitted netlist.
#[derive(Debug, Clone, Default)]
pub struct EmitStats {
    pub cells: FxHashMap<String, usize>,
    /// Area of each used cell (SYNTH-REQ-0707).
    pub cell_area: FxHashMap<String, f64>,
    pub area: f64,
    pub sequential: usize,
    pub combinational: usize,
    pub inverters: usize,
}

impl EmitStats {
    fn cell(&mut self, name: &str, area: f64) {
        if let Some(count) = self.cells.get_mut(name) {
            *count += 1;
            *self.cell_area.get_mut(name).expect("cell area recorded") = area;
        } else {
            self.cells.insert(name.to_owned(), 1);
            self.cell_area.insert(name.to_owned(), area);
        }
        // Preserve the exact serial accumulation order; no parallel float sum.
        self.area += area;
    }
}

const EMIT_BATCH: usize = 8192;

enum InstancePlan {
    Comb(usize, String),
    Seq(usize, String),
    Ready(NlInstance),
}

fn literal(l: Lit, net: &FxHashMap<(u32, bool), NlRef>, consts: &[Option<NlRef>; 2]) -> NlRef {
    if let Some(value) = l.const_value() {
        return consts[usize::from(value)]
            .as_ref()
            .expect("constant planned")
            .clone();
    }
    net.get(&(l.node(), l.is_compl()))
        .cloned()
        .unwrap_or_else(|| panic!("internal: literal {l:?} has no net"))
}

fn constant_ref(
    v: bool,
    lib: &CellLib,
    m: &mut NlModule,
    names: &mut Names<'_>,
    stats: &mut EmitStats,
    consts: &mut [Option<NlRef>; 2],
) -> NlRef {
    let slot = usize::from(v);
    if let Some(r) = &consts[slot] {
        return r.clone();
    }
    let tie = if v { &lib.tie1 } else { &lib.tie0 };
    let r = match tie {
        Some((cell, pin)) => {
            let n = names.fresh("n");
            m.wires.push((n.clone(), None));
            m.instances.push(NlInstance {
                cell: cell.clone(),
                name: names.fresh("t"),
                conns: vec![(pin.clone(), NlRef::Net(n.clone()))],
            });
            stats.cell(cell, lib.tie_area[slot]);
            NlRef::Net(n)
        }
        None => NlRef::Const(v),
    };
    consts[slot] = Some(r.clone());
    r
}

fn plan_constant(
    l: Lit,
    lib: &CellLib,
    m: &mut NlModule,
    names: &mut Names<'_>,
    stats: &mut EmitStats,
    consts: &mut [Option<NlRef>; 2],
    plans: &mut Vec<InstancePlan>,
) {
    if let Some(v) = l.const_value() {
        let old = m.instances.len();
        constant_ref(v, lib, m, names, stats, consts);
        // A tie cell stays at its original first-use position, even if earlier
        // cells of this batch have not yet had their connections materialized.
        plans.extend(m.instances.drain(old..).map(InstancePlan::Ready));
    }
}

struct EmitView<'a> {
    d: &'a Design,
    lib: &'a CellLib,
    mapped: &'a Mapped,
    seq: &'a [SeqChoice],
    net: &'a FxHashMap<(u32, bool), NlRef>,
    seq_neg: &'a FxHashMap<usize, String>,
    consts: &'a [Option<NlRef>; 2],
}

impl EmitView<'_> {
    fn build(&self, plan: InstancePlan) -> NlInstance {
        let get = |l| literal(l, self.net, self.consts);
        match plan {
            InstancePlan::Ready(instance) => instance,
            InstancePlan::Comb(index, name) => {
                let gate = &self.mapped.gates[index];
                let cell = &self.lib.comb[gate.cell];
                let mut conns = Vec::with_capacity(cell.inputs.len() + 1);
                for (pin, &(node, phase)) in cell.inputs.iter().zip(&gate.ins) {
                    conns.push((pin.clone(), get(Lit::new(node, phase))));
                }
                conns.push((cell.output.clone(), get(Lit::new(gate.out.0, gate.out.1))));
                NlInstance {
                    cell: cell.name.clone(),
                    name,
                    conns,
                }
            }
            InstancePlan::Seq(index, name) => {
                let s = &self.d.states[index];
                let choice = &self.seq[index];
                let cell = &self.lib.seq[choice.cell];
                let mut conns = vec![
                    (cell.data.clone(), get(choice.data)),
                    (cell.clock.pin.clone(), get(choice.clock)),
                ];
                if let (Some(pin), Some(l)) = (&cell.clear, choice.clear) {
                    conns.push((pin.pin.clone(), get(l)));
                }
                if let (Some(pin), Some(l)) = (&cell.preset, choice.preset) {
                    conns.push((pin.pin.clone(), get(l)));
                }
                if let (Some(pin), Some(l)) = (&cell.enable, choice.enable) {
                    conns.push((pin.pin.clone(), get(l)));
                }
                if let (Some(pin), Some(l)) = (&cell.hold, choice.hold) {
                    conns.push((pin.pin.clone(), get(l)));
                }
                let state = self.net[&(s.q.node(), false)].clone();
                let neg = self.seq_neg.get(&index).map(|n| NlRef::Net(n.clone()));
                let (q, qn) = match choice.q_true {
                    QSource::Q => (Some(state), neg),
                    QSource::Qn => (neg, Some(state)),
                    QSource::InvQ => (neg, None),
                    QSource::InvQn => (None, neg),
                };
                if let (Some(pin), Some(r)) = (&cell.q, q) {
                    conns.push((pin.clone(), r));
                }
                if let (Some(pin), Some(r)) = (&cell.qn, qn) {
                    conns.push((pin.clone(), r));
                }
                NlInstance {
                    cell: cell.name.clone(),
                    name,
                    conns,
                }
            }
        }
    }

    fn append(&self, m: &mut NlModule, plans: Vec<InstancePlan>) {
        // At most 8192 gates or 16384 sequential/inverter instances per batch.
        // Indexed collection preserves order and does not duplicate the graph.
        let instances: Vec<_> = if plans.len() < 64 || rayon::current_num_threads() == 1 {
            plans.into_iter().map(|p| self.build(p)).collect()
        } else {
            plans
                .into_par_iter()
                .with_min_len(64)
                .map(|p| self.build(p))
                .collect()
        };
        m.instances.extend(instances);
    }
}

/// Logical state probes are part of the checker contract, even when they have no cell sink.
pub fn state_name_ref(d: &Design, name: &str) -> NlRef {
    match d
        .ports
        .iter()
        .find(|p| !p.input && p.name == name && p.width == 1)
    {
        Some(p) if !p.scalar => NlRef::Bit(p.name.clone(), 0),
        _ => NlRef::Net(name.into()),
    }
}
pub fn state_nets(d: &Design) -> Vec<NlRef> {
    let vector_outputs: FxHashSet<_> = d
        .ports
        .iter()
        .filter(|p| !p.input && p.width == 1 && !p.scalar)
        .map(|p| p.name.as_str())
        .collect();
    d.states
        .iter()
        .map(|s| {
            if vector_outputs.contains(s.name.as_str()) {
                NlRef::Bit(s.name.clone(), 0)
            } else {
                NlRef::Net(s.name.clone())
            }
        })
        .collect()
}

/// Recompute final cell statistics after equivalent-cell substitutions or buffering.
pub fn recount(m: &NlModule, lib: &CellLib) -> EmitStats {
    let mut types = FxHashMap::default();
    for c in &lib.comb {
        types.insert(
            c.name.as_str(),
            (c.area, false, true, c.inputs.len() == 1 && c.tt == 0b01),
        );
    }
    for c in &lib.seq {
        types.insert(c.name.as_str(), (c.area, true, false, false));
    }
    for (i, tie) in [&lib.tie0, &lib.tie1].iter().enumerate() {
        if let Some((name, _)) = tie {
            types.insert(name.as_str(), (lib.tie_area[i], false, false, false));
        }
    }
    let mut s = EmitStats::default();
    for inst in &m.instances {
        let &(area, seq, comb, inv) = types
            .get(inst.cell.as_str())
            .expect("refinement uses eligible cells");
        s.cell(&inst.cell, area);
        s.sequential += usize::from(seq);
        s.combinational += usize::from(comb);
        s.inverters += usize::from(inv);
    }
    s
}

/// Builds the netlist module.
pub fn emit(
    d: &Design,
    lib: &CellLib,
    mapped: &Mapped,
    seq: &[SeqChoice],
) -> (NlModule, EmitStats) {
    emit_with_constants(d, lib, mapped, seq, &[])
}

/// Keeps named constant-state aliases for independent reset-invariant checking.
pub fn emit_with_constants(
    d: &Design,
    lib: &CellLib,
    mapped: &Mapped,
    seq: &[SeqChoice],
    constants: &[crate::opt::reset::ConstantState],
) -> (NlModule, EmitStats) {
    emit_with_states(d, lib, mapped, seq, constants, &[])
}

pub fn emit_with_states(
    d: &Design,
    lib: &CellLib,
    mapped: &Mapped,
    seq: &[SeqChoice],
    constants: &[crate::opt::reset::ConstantState],
    merged: &[crate::opt::reset::EquivalentState],
) -> (NlModule, EmitStats) {
    let mut names = Names {
        reserved: FxHashSet::default(),
        counters: [0; 4],
    };
    for p in &d.ports {
        names.reserved.insert(p.name.as_str());
    }
    for s in &d.states {
        names.reserved.insert(s.name.as_str());
    }
    for s in constants {
        names.reserved.insert(s.name.as_str());
    }
    for s in merged {
        names.reserved.insert(s.name.as_str());
    }
    let mut m = NlModule {
        name: d.top.clone(),
        ..NlModule::default()
    };
    let mut stats = EmitStats::default();
    // Net of every (node, phase).
    let mut net: FxHashMap<(u32, bool), NlRef> = FxHashMap::default();
    for p in d.ports.iter().filter(|p| p.input) {
        for (k, l) in p.bits.iter().enumerate() {
            let r = if p.scalar {
                NlRef::Net(p.name.clone())
            } else {
                NlRef::Bit(p.name.clone(), k as u32)
            };
            net.insert((l.node(), false), r);
        }
    }
    for p in &d.ports {
        m.ports.push(NlPort {
            name: p.name.clone(),
            dir: if p.input { NlDir::Input } else { NlDir::Output },
            width: if p.scalar { None } else { Some(p.width) },
        });
    }
    // State wires carry the true state value (SYNTH-REQ-0604); a one-bit output port with the
    // state's name is its own state wire.
    let state_ports: FxHashMap<_, _> = d
        .ports
        .iter()
        .filter(|p| !p.input && p.width == 1)
        .map(|p| (p.name.as_str(), p))
        .collect();
    for s in &d.states {
        let port = state_ports.get(s.name.as_str()).copied();
        let r = match port {
            Some(p) if p.scalar => NlRef::Net(p.name.clone()),
            Some(p) => NlRef::Bit(p.name.clone(), 0),
            None => {
                m.wires.push((s.name.clone(), None));
                NlRef::Net(s.name.clone())
            }
        };
        net.insert((s.q.node(), false), r);
    }
    // Gate outputs.
    for g in &mapped.gates {
        if let std::collections::hash_map::Entry::Vacant(e) = net.entry(g.out) {
            let n = names.fresh("n");
            m.wires.push((n.clone(), None));
            e.insert(NlRef::Net(n));
        }
    }
    // Free complemented state outputs used by the logic, and inverted-output nets.
    let mut seq_neg: FxHashMap<usize, String> = FxHashMap::default();
    for (i, (s, c)) in d.states.iter().zip(seq).enumerate() {
        let needs_internal = matches!(c.q_true, QSource::InvQ | QSource::InvQn);
        if needs_internal || mapped.free_neg_used.contains(&s.q.node()) {
            let n = names.fresh("n");
            m.wires.push((n.clone(), None));
            net.insert((s.q.node(), true), NlRef::Net(n.clone()));
            seq_neg.insert(i, n);
        }
    }
    // Constants and instance names are planned in original first-use order.
    let mut consts: [Option<NlRef>; 2] = [None, None];
    let inverted_states = seq
        .iter()
        .filter(|c| matches!(c.q_true, QSource::InvQ | QSource::InvQn))
        .count();
    m.instances.reserve(
        mapped
            .gates
            .len()
            .saturating_add(seq.len())
            .saturating_add(inverted_states)
            .saturating_add(2),
    );
    for start in (0..mapped.gates.len()).step_by(EMIT_BATCH) {
        let end = (start + EMIT_BATCH).min(mapped.gates.len());
        let mut plans = Vec::with_capacity(end - start + 2);
        for index in start..end {
            let g = &mapped.gates[index];
            let cell = &lib.comb[g.cell];
            for &(node, phase) in g.ins.iter().take(cell.inputs.len()) {
                plan_constant(
                    Lit::new(node, phase),
                    lib,
                    &mut m,
                    &mut names,
                    &mut stats,
                    &mut consts,
                    &mut plans,
                );
            }
            plan_constant(
                Lit::new(g.out.0, g.out.1),
                lib,
                &mut m,
                &mut names,
                &mut stats,
                &mut consts,
                &mut plans,
            );
            plans.push(InstancePlan::Comb(index, names.fresh("g")));
            stats.cell(&cell.name, cell.area);
            stats.combinational += 1;
            stats.inverters += usize::from(g.cell == lib.inverter);
        }
        EmitView {
            d,
            lib,
            mapped,
            seq,
            net: &net,
            seq_neg: &seq_neg,
            consts: &consts,
        }
        .append(&mut m, plans);
    }
    let inv = &lib.comb[lib.inverter];
    let sequential = d.states.len().min(seq.len());
    for start in (0..sequential).step_by(EMIT_BATCH) {
        let end = (start + EMIT_BATCH).min(sequential);
        let mut plans = Vec::with_capacity(2 * (end - start) + 2);
        for i in start..end {
            let s = &d.states[i];
            let choice = &seq[i];
            let cell = &lib.seq[choice.cell];
            plan_constant(
                choice.data,
                lib,
                &mut m,
                &mut names,
                &mut stats,
                &mut consts,
                &mut plans,
            );
            plan_constant(
                choice.clock,
                lib,
                &mut m,
                &mut names,
                &mut stats,
                &mut consts,
                &mut plans,
            );
            if let (Some(_), Some(l)) = (&cell.clear, choice.clear) {
                plan_constant(
                    l,
                    lib,
                    &mut m,
                    &mut names,
                    &mut stats,
                    &mut consts,
                    &mut plans,
                );
            }
            if let (Some(_), Some(l)) = (&cell.preset, choice.preset) {
                plan_constant(
                    l,
                    lib,
                    &mut m,
                    &mut names,
                    &mut stats,
                    &mut consts,
                    &mut plans,
                );
            }
            for l in [choice.enable, choice.hold].into_iter().flatten() {
                plan_constant(
                    l,
                    lib,
                    &mut m,
                    &mut names,
                    &mut stats,
                    &mut consts,
                    &mut plans,
                );
            }
            plans.push(InstancePlan::Seq(i, names.fresh("r")));
            stats.cell(&cell.name, cell.area);
            stats.sequential += 1;
            if matches!(choice.q_true, QSource::InvQ | QSource::InvQn) {
                let state = net[&(s.q.node(), false)].clone();
                let source = NlRef::Net(seq_neg.get(&i).expect("internal net").clone());
                plans.push(InstancePlan::Ready(NlInstance {
                    cell: inv.name.clone(),
                    name: names.fresh("g"),
                    conns: vec![(inv.inputs[0].clone(), source), (inv.output.clone(), state)],
                }));
                stats.cell(&inv.name, inv.area);
                stats.combinational += 1;
                stats.inverters += 1;
            }
        }
        EmitView {
            d,
            lib,
            mapped,
            seq,
            net: &net,
            seq_neg: &seq_neg,
            consts: &consts,
        }
        .append(&mut m, plans);
    }
    // Output ports.
    for p in d.ports.iter().filter(|p| !p.input) {
        for (k, l) in p.bits.iter().enumerate() {
            let lhs = if p.scalar {
                NlRef::Net(p.name.clone())
            } else {
                NlRef::Bit(p.name.clone(), k as u32)
            };
            let rhs = if let Some(v) = l.const_value() {
                constant_ref(v, lib, &mut m, &mut names, &mut stats, &mut consts)
            } else {
                literal(*l, &net, &consts)
            };
            if lhs != rhs {
                m.assigns.push((lhs, rhs));
            }
        }
    }
    for s in constants {
        if !d.ports.iter().any(|p| p.name == s.name) {
            m.wires.push((s.name.clone(), None));
            m.assigns
                .push((NlRef::Net(s.name.clone()), NlRef::Const(s.value)));
        }
    }
    for s in merged {
        if !d.ports.iter().any(|p| p.name == s.name) {
            m.wires.push((s.name.clone(), None));
            if s.inverted {
                let owner = d
                    .states
                    .iter()
                    .find(|q| q.name == s.representative)
                    .expect("retained state representative");
                if let Some(src) = net.get(&(owner.q.node(), true)) {
                    m.assigns.push((NlRef::Net(s.name.clone()), src.clone()));
                } else {
                    m.instances.push(NlInstance {
                        cell: inv.name.clone(),
                        name: names.fresh("g"),
                        conns: vec![
                            (inv.inputs[0].clone(), state_name_ref(d, &s.representative)),
                            (inv.output.clone(), NlRef::Net(s.name.clone())),
                        ],
                    });
                    stats.cell(&inv.name, inv.area);
                    stats.combinational += 1;
                    stats.inverters += 1;
                }
            } else {
                m.assigns.push((
                    NlRef::Net(s.name.clone()),
                    state_name_ref(d, &s.representative),
                ));
            }
        }
    }
    (m, stats)
}
