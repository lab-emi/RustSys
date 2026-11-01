// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Driver resolution: builds the final graph whose inputs are primary input bits and state
//! outputs (SYNTH-REQ-0213, SYNTH-REQ-0214, SYNTH-REQ-0402).

use rustc_hash::FxHashSet;
use rusteda_formats::verilog::Span;

use super::types::Ty;
use super::{EResult, Elaborator, SigId};
use crate::aig::{Aig, Lit};

/// A top-level port bit vector.
#[derive(Debug, Clone)]
pub struct Port {
    pub name: String,
    pub input: bool,
    pub width: u32,
    /// Declared without packed dimension and 1 bit wide (SYNTH-REQ-0602).
    pub scalar: bool,
    /// Input literals (inputs) or output functions (outputs), least significant bit first.
    pub bits: Vec<Lit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateKind {
    Flop,
    Latch,
}

/// A state element of the final design.
#[derive(Debug, Clone)]
pub struct State {
    /// State name (SYNTH-REQ-0603).
    pub name: String,
    pub kind: StateKind,
    /// Output literal (a combinational input of the graph).
    pub q: Lit,
    /// Data input function.
    pub d: Lit,
    /// Rising-edge clock function (flip-flops) or active-high enable (latches).
    pub clk: Lit,
    /// Asynchronous control: active-high condition and value.
    pub arst: Option<(Lit, bool)>,
}

/// An opaque macro's original port and its internal synthesis cut.
#[derive(Debug, Clone)]
pub struct OpaquePort {
    pub name: String,
    pub cut: String,
    /// Original packed range. Keep the base name once instead of allocating
    /// one copy per bit; even long identifiers have bounded linear storage.
    pub range: Option<super::types::Range>,
}

/// A preserved hard macro instance.
#[derive(Debug, Clone)]
pub struct OpaqueInstance {
    pub name: String,
    pub cell: String,
    pub ports: Vec<OpaquePort>,
    /// Filled from the validated technology library before netlist emission.
    pub area: f64,
}

/// Elaborated design. Opaque interfaces remain roots through every logic pass.
#[derive(Debug, Clone)]
pub struct Design {
    pub top: String,
    pub aig: Aig,
    pub ports: Vec<Port>,
    pub states: Vec<State>,
    pub opaque_instances: Vec<OpaqueInstance>,
}

const UNSET: Lit = Lit(u32::MAX);

impl Design {
    /// Evaluates the combinational logic for 64 patterns at once. `inputs[i]` holds the patterns
    /// of graph input i (primary input bits in port order, then state outputs in state order).
    /// Returns the simulation vector of all nodes.
    pub fn simulate(&self, inputs: &[u64]) -> Vec<u64> {
        self.aig.simulate(inputs)
    }

    /// Number of graph inputs that are primary input bits.
    pub fn num_input_bits(&self) -> usize {
        self.ports
            .iter()
            .filter(|p| p.input)
            .map(|p| p.bits.len())
            .sum()
    }

    /// Port by name.
    pub fn port(&self, name: &str) -> Option<&Port> {
        self.ports.iter().find(|p| p.name == name)
    }

    /// State by name.
    pub fn state(&self, name: &str) -> Option<&State> {
        self.states.iter().find(|s| s.name == name)
    }
}

impl Elaborator<'_> {
    pub(crate) fn resolve(&mut self) -> EResult<Design> {
        if self.aig.mul_trace_overflow {
            return Err(self.err(
                "S0398",
                Span::default(),
                "multiplier observation budget reached",
            ));
        }
        let mut ng = Aig::new();
        // Driver resolution rebuilds the graph, but later carry alternatives still need this.
        ng.has_parallel_arithmetic = self.aig.has_parallel_arithmetic;
        ng.xor_carry = self.aig.xor_carry;
        let n = self.aig.num_nodes();
        let mut img = vec![UNSET; n];
        let mut color = vec![0u8; n];
        img[0] = Lit::FALSE;
        color[0] = 2;
        let mut ports = Vec::new();
        let mut top_ports = self.top_ports.clone();
        let mut reserved: FxHashSet<String> = self.signals.iter().map(|s| s.name.clone()).collect();
        reserved.extend(top_ports.iter().map(|p| p.0.clone()));
        let mut opaque_instances = Vec::new();
        let mut cut_index = 0usize;
        for (name, cell, pins) in &self.opaque_instances {
            let mut connections = Vec::new();
            for p in pins {
                let cut = loop {
                    let n = format!("__rustsys_macro_cut_{cut_index}");
                    cut_index += 1;
                    if reserved.insert(n.clone()) {
                        break n;
                    }
                };
                top_ports.push((
                    cut.clone(),
                    p.sig,
                    p.dir == rusteda_formats::verilog::ast::Direction::Output,
                ));
                let range = match &self.signals[p.sig as usize].ty {
                    Ty::Bit { .. } => None,
                    Ty::Vector { range, .. } => Some(*range),
                    _ => unreachable!("opaque port type validated during elaboration"),
                };
                connections.push(OpaquePort {
                    name: p.name.clone(),
                    cut,
                    range,
                });
            }
            opaque_instances.push(OpaqueInstance {
                name: name.clone(),
                cell: cell.clone(),
                ports: connections,
                area: 0.0,
            });
        }
        // Inputs first, in port order, so that input numbering follows the port list.
        for (name, sig, is_in) in &top_ports {
            if !*is_in {
                continue;
            }
            let s = &self.signals[*sig as usize];
            let mut bits = Vec::new();
            for d in &s.drivers {
                let Some((l, _)) = d else {
                    unreachable!("inputs are driven")
                };
                let ni = ng.input();
                img[l.node() as usize] = ni;
                color[l.node() as usize] = 2;
                bits.push(ni);
            }
            ports.push(Port {
                name: name.clone(),
                input: true,
                width: s.elem_width,
                scalar: matches!(s.ty, Ty::Bit { .. }),
                bits,
            });
        }
        for st in &self.states {
            let ni = ng.input();
            img[st.q.node() as usize] = ni;
            color[st.q.node() as usize] = 2;
        }
        let mut undriven: FxHashSet<SigId> = FxHashSet::default();
        let mut outs = Vec::new();
        for (name, sig, is_in) in &top_ports {
            if *is_in {
                continue;
            }
            let s = self.signals[*sig as usize].clone();
            let mut bits = Vec::new();
            for l in &s.ph {
                bits.push(self.resolve_lit(&mut ng, &mut img, &mut color, *l, &mut undriven)?);
            }
            outs.push(Port {
                name: name.clone(),
                input: false,
                width: s.elem_width,
                scalar: matches!(s.ty, Ty::Bit { .. }),
                bits,
            });
        }
        // Restore header order of ports.
        let mut ordered = Vec::new();
        let mut ins = ports.into_iter();
        let mut os = outs.into_iter();
        for (_, _, is_in) in &top_ports {
            ordered.push(if *is_in {
                ins.next().expect("input")
            } else {
                os.next().expect("output")
            });
        }
        let mut states = Vec::with_capacity(self.states.len());
        let recs = self.states.clone();
        for st in &recs {
            let q = img[st.q.node() as usize];
            let d = self.resolve_lit(&mut ng, &mut img, &mut color, st.d, &mut undriven)?;
            let clk = self.resolve_lit(&mut ng, &mut img, &mut color, st.clk, &mut undriven)?;
            let arst = match st.arst {
                Some((c, v)) => Some((
                    self.resolve_lit(&mut ng, &mut img, &mut color, c, &mut undriven)?,
                    v,
                )),
                None => None,
            };
            let kind = if st.latch {
                StateKind::Latch
            } else {
                StateKind::Flop
            };
            states.push(State {
                name: self.state_name(st.sig, st.elem, st.bit),
                kind,
                q,
                d,
                clk,
                arst,
            });
        }
        if let Some(sites) = self.aig.mul_sites.clone() {
            let mut mapped = Vec::new();
            for site in sites {
                let mut vectors = Vec::new();
                for bits in [&site.a, &site.b, &site.product] {
                    let mut result = Vec::new();
                    for &bit in bits {
                        result.push(self.resolve_lit(
                            &mut ng,
                            &mut img,
                            &mut color,
                            bit,
                            &mut undriven,
                        )?);
                    }
                    vectors.push(result);
                }
                mapped.push(crate::aig::MulSite {
                    a: vectors.remove(0),
                    b: vectors.remove(0),
                    product: vectors.remove(0),
                    signed: site.signed,
                });
            }
            ng.mul_sites = Some(mapped);
        }
        let mut und: Vec<SigId> = undriven.into_iter().collect();
        und.sort_unstable();
        for s in und {
            let (name, span) = (
                self.signals[s as usize].name.clone(),
                self.signals[s as usize].span,
            );
            self.warn("S0213", span, format!("'{name}' is read but not (fully) driven; undriven bits read as 0 (SYNTH-REQ-0213)"));
        }
        let map = |l: Lit| {
            let v = img[l.node() as usize];
            (v != UNSET).then(|| v.xor(l.is_compl()))
        };
        ng.add_sites = self
            .aig
            .add_sites
            .iter()
            .map(|site| {
                let site = site.as_ref()?;
                Some(crate::aig::AddSite {
                    a: site
                        .a
                        .iter()
                        .copied()
                        .map(map)
                        .collect::<Option<Vec<_>>>()?,
                    b: site
                        .b
                        .iter()
                        .copied()
                        .map(map)
                        .collect::<Option<Vec<_>>>()?,
                    cin: map(site.cin)?,
                    sum: site
                        .sum
                        .iter()
                        .map(|&l| map(l).unwrap_or(Lit::FALSE))
                        .collect(),
                    carry: map(site.carry).unwrap_or(Lit::FALSE),
                })
            })
            .collect();
        Ok(Design {
            top: self.top_name.clone(),
            aig: ng,
            ports: ordered,
            states,
            opaque_instances,
        })
    }

    fn resolve_lit(
        &self,
        ng: &mut Aig,
        img: &mut [Lit],
        color: &mut [u8],
        root: Lit,
        undriven: &mut FxHashSet<SigId>,
    ) -> EResult<Lit> {
        let mut stack: Vec<(u32, bool)> = vec![(root.node(), false)];
        while let Some((n, post)) = stack.pop() {
            let ni = n as usize;
            if color[ni] == 2 {
                continue;
            }
            if post {
                if let Some(&id) = self.select_of.get(&n) {
                    let table = &self.deferred_selects[id];
                    let get = |l: Lit| img[l.node() as usize].xor(l.is_compl());
                    let chunks: Vec<_> = table
                        .chunks
                        .iter()
                        .map(|v| v.iter().copied().map(get).collect())
                        .collect();
                    let pos: Vec<_> = table.pos.iter().copied().map(get).collect();
                    let values =
                        super::select::lower_select(ng, &chunks, &pos, self.prefer_narrow_muxes);
                    for (old, value) in table.outputs.iter().zip(values) {
                        img[old.node() as usize] = value;
                        color[old.node() as usize] = 2;
                    }
                    continue;
                }
                let l = if let Some(&(sig, bit)) = self.placeholder_of.get(&n) {
                    match self.signals[sig as usize].drivers[bit as usize] {
                        Some((d, _)) => img[d.node() as usize].xor(d.is_compl()),
                        None => Lit::FALSE,
                    }
                } else {
                    let (a, b) = self.aig.fanins(n);
                    let x = img[a.node() as usize].xor(a.is_compl());
                    let y = img[b.node() as usize].xor(b.is_compl());
                    ng.and(x, y)
                };
                img[ni] = l;
                color[ni] = 2;
                continue;
            }
            if color[ni] == 1 {
                // Reached a node whose resolution is in progress: a combinational cycle.
                let mut names: Vec<String> = Vec::new();
                for (m, p) in &stack {
                    if *p {
                        if let Some(&(sig, bit)) = self.placeholder_of.get(m) {
                            let s = &self.signals[sig as usize];
                            let nm = self.state_name(sig, bit / s.elem_width, bit % s.elem_width);
                            if !names.contains(&nm) {
                                names.push(nm);
                            }
                        }
                    }
                }
                names.truncate(8);
                let span = self
                    .placeholder_of
                    .get(&n)
                    .map_or(Span::default(), |(s, _)| self.signals[*s as usize].span);
                return Err(self.err(
                    "S0214",
                    span,
                    format!(
                        "combinational loop through {} (SYNTH-REQ-0214)",
                        names.join(", ")
                    ),
                ));
            }
            color[ni] = 1;
            stack.push((n, true));
            if let Some(&id) = self.select_of.get(&n) {
                let table = &self.deferred_selects[id];
                for l in table.chunks.iter().flatten().chain(&table.pos) {
                    stack.push((l.node(), false));
                }
            } else if let Some(&(sig, bit)) = self.placeholder_of.get(&n) {
                match self.signals[sig as usize].drivers[bit as usize] {
                    Some((d, _)) => stack.push((d.node(), false)),
                    None => {
                        undriven.insert(sig);
                    }
                }
            } else if self.aig.is_and(n) {
                let (a, b) = self.aig.fanins(n);
                stack.push((a.node(), false));
                stack.push((b.node(), false));
            } else {
                return Err(self.err("S0900", Span::default(), "internal: unresolved graph input"));
            }
        }
        Ok(img[root.node() as usize].xor(root.is_compl()))
    }
}
