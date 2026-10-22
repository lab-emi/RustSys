// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use rustc_hash::{FxHashMap, FxHashSet};
use rusteda_formats::verilog::write::{NlDir, NlInstance, NlModule, NlRef};

use super::Options;
use super::model::Model;

#[derive(Debug, Clone)]
pub(super) struct Gate {
    pub active: bool,
    pub cell: usize,
    pub inputs: Vec<usize>,
    pub outputs: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Node {
    pub driver: Option<(usize, usize)>,
    pub sinks: Vec<(usize, usize)>,
    pub primary: bool,
    pub outputs: usize,
    pub protected: bool,
    /// Carries a clock to a sequential clock or enable pin (SYNTH-REQ-0552).
    pub clock: bool,
    pub state_alias: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Network {
    pub nodes: Vec<Node>,
    pub gates: Vec<Gate>,
    pub order: Vec<usize>,
    /// Kahn FIFO layer ends in `order`; absent after the bounded layer budget.
    pub level_ends: Option<Vec<usize>>,
    pub refs: Vec<NlRef>,
    pub original_gates: usize,
    pub known_state_aliases: bool,
}

fn intern(r: &NlRef, map: &mut FxHashMap<NlRef, usize>, refs: &mut Vec<NlRef>) -> usize {
    if let Some(&id) = map.get(r) {
        return id;
    }
    let id = refs.len();
    refs.push(r.clone());
    map.insert(r.clone(), id);
    id
}

fn root(parent: &mut [usize], mut n: usize) -> usize {
    while parent[n] != n {
        parent[n] = parent[parent[n]];
        n = parent[n];
    }
    n
}

fn port_refs(m: &NlModule, dir: NlDir) -> impl Iterator<Item = NlRef> + '_ {
    m.ports.iter().filter(move |p| p.dir == dir).flat_map(|p| {
        if let Some(w) = p.width {
            (0..w)
                .map(|i| NlRef::Bit(p.name.clone(), i))
                .collect::<Vec<_>>()
        } else {
            vec![NlRef::Net(p.name.clone())]
        }
    })
}

/// Number of full branches at this level of a minimum-size bounded-fanout tree.
/// Each buffer replaces up to `width` sink edges by one, freeing `width - 1` slots.
/// Keep the remaining direct edges instead of creating a partly occupied final branch.
pub(super) fn branch_count(count: usize, width: usize, force: bool) -> usize {
    if count > width {
        (count - width).div_ceil(width - 1).min(count / width)
    } else {
        usize::from(force && count > 1)
    }
}

pub(super) fn tree_size(mut count: usize, width: usize, mut force: bool) -> usize {
    let mut total = 0;
    loop {
        let branches = branch_count(count, width, force);
        if branches == 0 {
            break;
        }
        let moved = (branches * width).min(count);
        count = count - moved + branches;
        total += branches;
        force = false;
    }
    total
}

impl Network {
    #[cfg(test)]
    pub fn new(m: &NlModule, lib: &Model<'_>) -> Result<Self, String> {
        Self::with_state_aliases(m, lib, None)
    }

    pub fn with_state_aliases(
        m: &NlModule,
        lib: &Model<'_>,
        aliases: Option<&[NlRef]>,
    ) -> Result<Self, String> {
        let mut ids = FxHashMap::default();
        let mut refs = Vec::new();
        for r in port_refs(m, NlDir::Input).chain(port_refs(m, NlDir::Output)) {
            intern(&r, &mut ids, &mut refs);
        }
        for inst in &m.instances {
            for (_, r) in &inst.conns {
                intern(r, &mut ids, &mut refs);
            }
        }
        for (a, b) in &m.assigns {
            intern(a, &mut ids, &mut refs);
            intern(b, &mut ids, &mut refs);
        }
        let mut parents: Vec<usize> = (0..refs.len()).collect();
        for (a, b) in &m.assigns {
            let (a, b) = (root(&mut parents, ids[a]), root(&mut parents, ids[b]));
            parents[a.max(b)] = a.min(b);
        }
        let mut compact = FxHashMap::default();
        let mut node_refs = Vec::new();
        for (i, r) in refs.iter().enumerate() {
            let k = root(&mut parents, i);
            let n = *compact.entry(k).or_insert_with(|| {
                let n = node_refs.len();
                node_refs.push(refs[k].clone());
                n
            });
            ids.insert(r.clone(), n);
        }
        let mut net = Self {
            nodes: vec![Node::default(); node_refs.len()],
            gates: Vec::new(),
            order: Vec::new(),
            level_ends: None,
            refs: node_refs,
            original_gates: m.instances.len(),
            known_state_aliases: aliases.is_some(),
        };
        if let Some(aliases) = aliases {
            for a in aliases {
                let n = ids
                    .get(a)
                    .ok_or_else(|| format!("missing logical state probe {a:?}"))?;
                net.nodes[*n].state_alias = true;
            }
        }
        for r in port_refs(m, NlDir::Input) {
            let n = ids[&r];
            net.nodes[n].primary = true;
            net.refs[n] = r;
        }
        for r in &refs {
            if matches!(r, NlRef::Const(_)) {
                net.refs[ids[r]] = r.clone();
            }
        }
        for r in port_refs(m, NlDir::Output) {
            net.nodes[ids[&r]].outputs += 1;
        }
        let mut protected = Vec::new();
        for inst in &m.instances {
            let &cell = lib
                .by_name
                .get(&inst.cell)
                .ok_or_else(|| format!("no electrical model for '{}'", inst.cell))?;
            let c = &lib.cells[cell];
            let mut inputs = Vec::new();
            for &pin in &c.inputs {
                let r = inst
                    .conns
                    .iter()
                    .find(|(p, _)| *p == c.raw.pins[pin].name)
                    .ok_or_else(|| {
                        format!("unconnected input {}.{}", inst.name, c.raw.pins[pin].name)
                    })?;
                let n = ids[&r.1];
                inputs.push(n);
                if !c.comb && !c.data.is_empty() && !c.data.contains(&pin) {
                    protected.push(n);
                }
            }
            let outputs = c
                .outputs
                .iter()
                .filter_map(|&pin| {
                    inst.conns
                        .iter()
                        .find(|(p, _)| *p == c.raw.pins[pin].name)
                        .map(|(_, r)| {
                            net.refs[ids[r]] = r.clone();
                            (pin, ids[r])
                        })
                })
                .collect();
            net.gates.push(Gate {
                active: true,
                cell,
                inputs,
                outputs,
            });
        }
        net.rebuild(lib)?;
        while let Some(n) = protected.pop() {
            if std::mem::replace(&mut net.nodes[n].protected, true) {
                continue;
            }
            if let Some((g, _)) = net.nodes[n].driver {
                if lib.cells[net.gates[g].cell].comb {
                    protected.extend(&net.gates[g].inputs);
                }
            }
        }
        net.mark_clocks(lib);
        Ok(net)
    }

    /// Clock nets (SYNTH-REQ-0552): within the combinational fan-in of a sequential clock or
    /// enable pin, a primary input, or the output of a combinational cell with an input that is
    /// itself a clock net. Clock-tree construction owns these nets; every other net, including
    /// asynchronous controls and enables derived from state, follows the data-net rules.
    fn mark_clocks(&mut self, lib: &Model<'_>) {
        let mut cone = vec![false; self.nodes.len()];
        let mut stack = Vec::new();
        for gate in self.gates.iter().filter(|g| g.active) {
            let c = &lib.cells[gate.cell];
            if let Some(i) = c.inputs.iter().position(|&p| Some(p) == c.clock) {
                stack.push(gate.inputs[i]);
            }
        }
        while let Some(n) = stack.pop() {
            if std::mem::replace(&mut cone[n], true) {
                continue;
            }
            if let Some((g, _)) = self.nodes[n].driver {
                if lib.cells[self.gates[g].cell].comb {
                    stack.extend(&self.gates[g].inputs);
                }
            }
        }
        for (n, node) in self.nodes.iter_mut().enumerate() {
            node.clock = cone[n] && node.primary;
        }
        for &g in &self.order {
            if self.gates[g].inputs.iter().any(|&n| self.nodes[n].clock) {
                for &(_, n) in &self.gates[g].outputs {
                    self.nodes[n].clock |= cone[n];
                }
            }
        }
    }

    pub fn rebuild(&mut self, lib: &Model<'_>) -> Result<(), String> {
        for node in &mut self.nodes {
            node.driver = None;
            node.sinks.clear();
        }
        for (g, gate) in self.gates.iter().enumerate() {
            if !gate.active {
                continue;
            }
            for (pin, &n) in gate.inputs.iter().enumerate() {
                self.nodes[n].sinks.push((g, pin));
            }
            for &(pin, n) in &gate.outputs {
                if self.nodes[n].driver.replace((g, pin)).is_some() {
                    return Err("multiple drivers in electrical refinement".into());
                }
            }
        }
        let mut pending = vec![0; self.gates.len()];
        let mut ready = Vec::new();
        for (g, gate) in self.gates.iter().enumerate() {
            if !gate.active || !lib.cells[gate.cell].comb {
                continue;
            }
            pending[g] = gate
                .inputs
                .iter()
                .filter(|&&n| {
                    self.nodes[n]
                        .driver
                        .is_some_and(|(p, _)| lib.cells[self.gates[p].cell].comb)
                })
                .count();
            if pending[g] == 0 {
                ready.push(g);
            }
        }
        self.order.clear();
        self.level_ends = Some(vec![0]);
        let mut k = 0;
        let mut end = ready.len();
        while k < ready.len() {
            let g = ready[k];
            k += 1;
            self.order.push(g);
            for &(_, n) in &self.gates[g].outputs {
                for &(h, _) in &self.nodes[n].sinks {
                    if lib.cells[self.gates[h].cell].comb {
                        pending[h] -= 1;
                        if pending[h] == 0 {
                            ready.push(h);
                        }
                    }
                }
            }
            if k == end {
                if let Some(ends) = &mut self.level_ends {
                    if ends.len() > 65_536 {
                        self.level_ends = None;
                    } else {
                        ends.push(self.order.len());
                    }
                }
                end = ready.len();
            }
        }
        if self.order.len()
            != self
                .gates
                .iter()
                .filter(|g| g.active && lib.cells[g.cell].comb)
                .count()
        {
            return Err("combinational cycle in electrical refinement".into());
        }
        Ok(())
    }

    pub fn area(&self, lib: &Model<'_>) -> f64 {
        self.gates
            .iter()
            .filter(|g| g.active)
            .map(|g| lib.cells[g.cell].raw.area)
            .sum()
    }

    pub fn change_cell(&mut self, g: usize, cell: usize, lib: &Model<'_>) {
        let old = self.gates[g].cell;
        self.gates[g].cell = cell;
        for (pin, n) in &mut self.gates[g].outputs {
            *pin = if lib.cells[cell].comb {
                lib.cells[cell].outputs[0]
            } else {
                let name = &lib.cells[old].raw.pins[*pin].name;
                lib.cells[cell]
                    .raw
                    .pins
                    .iter()
                    .position(|p| &p.name == name)
                    .expect("compatible sequential output")
            };
            self.nodes[*n].driver = Some((g, *pin));
        }
    }

    /// Split one fanout into bounded branches. The caller has reserved the full tree area.
    pub fn split_fanout(
        &mut self,
        n: usize,
        buffer: usize,
        width: usize,
        force: bool,
        lib: &Model<'_>,
    ) -> usize {
        let before = self.gates.len();
        let mut sinks = self.nodes[n].sinks.clone();
        let mut first = force;
        loop {
            let branches = branch_count(sinks.len(), width, first);
            if branches == 0 {
                break;
            }
            first = false;
            let moved = (branches * width).min(sinks.len());
            let mut upper = Vec::new();
            for chunk in sinks[..moved].chunks(width) {
                let out = self.nodes.len();
                self.nodes.push(Node::default());
                self.refs.push(NlRef::Net(String::new()));
                for &(g, p) in chunk {
                    self.gates[g].inputs[p] = out;
                }
                let g = self.gates.len();
                self.gates.push(Gate {
                    active: true,
                    cell: buffer,
                    inputs: vec![n],
                    outputs: vec![(lib.cells[buffer].outputs[0], out)],
                });
                upper.push((g, 0));
            }
            upper.extend_from_slice(&sinks[moved..]);
            sinks = upper;
        }
        self.gates.len() - before
    }

    /// Isolate all internal sinks behind one noninverting buffer. Unlike a fanout
    /// tree, load isolation can be useful even for one large sink capacitance.
    pub fn isolate_load(&mut self, n: usize, buffer: usize, lib: &Model<'_>) -> usize {
        let sinks = self.nodes[n].sinks.clone();
        if sinks.is_empty() {
            return 0;
        }
        let out = self.nodes.len();
        self.nodes.push(Node::default());
        self.refs.push(NlRef::Net(String::new()));
        for (g, p) in sinks {
            self.gates[g].inputs[p] = out;
        }
        self.gates.push(Gate {
            active: true,
            cell: buffer,
            inputs: vec![n],
            outputs: vec![(lib.cells[buffer].outputs[0], out)],
        });
        1
    }

    pub fn buffer(&mut self, lib: &Model<'_>, opts: &Options) -> usize {
        let Some(&buffer) = lib.buffers.first() else {
            return 0;
        };
        let mut area = self.area(lib);
        // Keep headroom for gate sizing. Rank loads before spending the bounded budget.
        let buffer_limit = opts
            .area_limit
            .map(|limit| area + (limit - area).max(0.0) * 0.65);
        let timing = super::timing::Timing::new(self, lib, opts);
        let mut candidates: Vec<_> = (0..self.nodes.len())
            .filter(|&n| {
                self.nodes[n].sinks.len() > opts.buffer_fanout
                    && timing.slack(n) < opts.period_ns * 0.15
            })
            .map(|n| {
                let critical = (1.0 - timing.slack(n) / opts.period_ns).clamp(0.1, 10.0);
                (critical * timing.slope[n] * timing.load[n], n)
            })
            .collect();
        candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let price = lib.cells[buffer].raw.area;
        let before = self.gates.len();
        for (_, n) in candidates {
            if self.nodes[n].primary || self.nodes[n].protected || self.nodes[n].driver.is_none() {
                continue;
            }
            let (driver, _) = self.nodes[n].driver.expect("checked");
            let c = &lib.cells[self.gates[driver].cell];
            if !c.comb && c.clock.is_none() {
                continue;
            }
            let count = tree_size(self.nodes[n].sinks.len(), opts.buffer_fanout, false);
            let cost = count as f64 * price;
            if buffer_limit.is_none_or(|lim| area + cost <= lim + 1e-8) {
                let mut sinks: Vec<_> = self.nodes[n]
                    .sinks
                    .iter()
                    .map(|&(g, p)| (timing.sink_required(self, lib, g, p, n, opts), (g, p)))
                    .collect();
                sinks.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
                self.nodes[n].sinks = sinks.into_iter().map(|(_, s)| s).collect();
                let added = self.split_fanout(n, buffer, opts.buffer_fanout, false, lib);
                debug_assert_eq!(added, count);
                area += cost;
            }
        }
        // Buffer insertion preserves acyclicity by splitting existing fanout edges.
        self.rebuild(lib).expect("buffer insertion preserves DAG");
        self.gates.len() - before
    }

    pub fn write(&mut self, m: &mut NlModule, lib: &Model<'_>) {
        let mut used: FxHashSet<String> = m
            .wires
            .iter()
            .map(|(s, _)| s.clone())
            .chain(m.ports.iter().map(|p| p.name.clone()))
            .chain(m.instances.iter().map(|i| i.name.clone()))
            .collect();
        let mut net_serial = 0usize;
        let mut cell_serial = 0usize;
        for r in &mut self.refs {
            if *r == NlRef::Net(String::new()) {
                let name = loop {
                    let s = format!("rs_net{net_serial}");
                    net_serial += 1;
                    if used.insert(s.clone()) {
                        break s;
                    }
                };
                m.wires.push((name.clone(), None));
                *r = NlRef::Net(name);
            }
        }
        let mut instances = Vec::new();
        for (g, gate) in self.gates.iter().enumerate() {
            if !gate.active {
                continue;
            }
            let c = &lib.cells[gate.cell];
            let mut conns: Vec<_> = c
                .inputs
                .iter()
                .zip(&gate.inputs)
                .map(|(&pin, &n)| (c.raw.pins[pin].name.clone(), self.refs[n].clone()))
                .collect();
            conns.extend(
                gate.outputs
                    .iter()
                    .map(|&(pin, n)| (c.raw.pins[pin].name.clone(), self.refs[n].clone())),
            );
            let name = if g < self.original_gates {
                m.instances[g].name.clone()
            } else {
                loop {
                    let s = format!("rs_buf{cell_serial}");
                    cell_serial += 1;
                    if used.insert(s.clone()) {
                        break s;
                    }
                }
            };
            instances.push(NlInstance {
                cell: c.raw.name.clone(),
                name,
                conns,
            });
        }
        m.instances = instances;
    }
}
