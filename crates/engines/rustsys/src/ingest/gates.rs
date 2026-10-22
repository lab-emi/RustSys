// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Lowering of a gate-level netlist of library cells (SYNTH-REQ-0902, SYNTH-REQ-0903,
//! SYNTH-REQ-0910 to SYNTH-REQ-0912): its logic is derived from the Liberty descriptions of its
//! cells.

use rusteda_core::names::unescape;
use rusteda_core::netlist::{Conn, GateNetlist, PortDir};
use rusteda_formats::liberty::{Cell, Direction, Func, Library};

use super::cells::{self, Cells, Seq};
use super::{Comb, Info, Lowered, UNSET, check_ports, check_states};
use crate::aig::{Aig, Lit};
use crate::elab::{Design, Port, State};

/// What an input pin or an assignment reads.
#[derive(Debug, Clone, Copy)]
enum Src {
    Net(u32),
    Const(bool),
}

/// A connected output pin: its function, its net and the inputs (indices) the function reads.
struct Out<'l> {
    func: &'l Func,
    net: u32,
    reads: Vec<usize>,
}

/// A cell instance of the netlist.
struct Inst<'l> {
    name: String,
    cell: &'l Cell,
    seq: Option<Seq<'l>>,
    inputs: Vec<(&'l str, Src)>,
    outputs: Vec<Out<'l>>,
}

impl Inst<'_> {
    /// The nets that inputs `idx` read, in order.
    fn fanins(&self, idx: &[usize]) -> Vec<u32> {
        idx.iter()
            .filter_map(|&k| match self.inputs[k].1 {
                Src::Net(n) => Some(n),
                Src::Const(_) => None,
            })
            .collect()
    }

    /// The values of inputs `idx`, given the values of [`Inst::fanins`] of them in order.
    fn values(&self, idx: &[usize], ins: &[Lit]) -> Vec<(&str, Lit)> {
        let mut k = 0;
        idx.iter()
            .map(|&i| {
                let (p, s) = self.inputs[i];
                let v = match s {
                    Src::Net(_) => {
                        k += 1;
                        ins[k - 1]
                    }
                    Src::Const(b) => Lit::from_bool(b),
                };
                (p, v)
            })
            .collect()
    }

    /// Every input, in order.
    fn all(&self) -> Vec<usize> {
        (0..self.inputs.len()).collect()
    }
}

/// What drives a computed net.
enum Kind {
    Cell(usize, usize),
    Assign(Src),
}

/// Lowers `g` over the cells of `libs` (SYNTH-REQ-0910 to SYNTH-REQ-0912).
pub(super) fn lower(
    g: &GateNetlist,
    libs: &[Library],
    top: Option<&str>,
) -> Result<Lowered, Vec<String>> {
    lower_inner(g, libs, top).map_err(|m| vec![m])
}

fn lower_inner(g: &GateNetlist, libs: &[Library], top: Option<&str>) -> Result<Lowered, String> {
    let module = g.name().to_string();
    if let Some(t) = top.filter(|t| *t != module) {
        return Err(format!(
            "error[S0920]: the gate-level netlist holds module {module}, not {t}"
        ));
    }
    let cells = Cells::new(libs);
    let n = g
        .nets()
        .map(|(id, _)| id.index() as usize + 1)
        .max()
        .unwrap_or(0);
    let label = |k: u32| {
        g.nets()
            .find(|(id, _)| id.index() == k)
            .map_or_else(|| format!("#{k}"), |(id, _)| unescape(&g.net_label(id)))
    };
    let mut insts = Vec::new();
    for (_, inst) in g.instances() {
        let name = unescape(g.resolve(inst.name));
        let ct = g
            .cell(inst.cell)
            .map_err(|e| format!("error[S0900]: internal: {e}"))?;
        let cname = g.resolve(ct.name);
        let e = |m: String| format!("error[S0917]: instance {name} of cell {cname}: {m}");
        let Some(cell) = cells.get(cname) else {
            return Err(e("the cell is not in the --gate-lib libraries".into()));
        };
        let seq = cells::model(cell).map_err(e)?;
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for pid in &inst.pins {
            let pin = g
                .pin(*pid)
                .map_err(|x| format!("error[S0900]: internal: {x}"))?;
            let pname = g.resolve(ct.pins[pin.cell_pin as usize].name);
            // Power and ground pins are not logic (SYNTH-REQ-0910).
            let Some(lp) = cell.pin(pname) else {
                continue;
            };
            match (lp.direction, pin.conn) {
                (Direction::Input, Some(Conn::Net(x))) => {
                    inputs.push((lp.name.as_str(), Src::Net(x.index())));
                }
                (Direction::Input, Some(Conn::Const(b))) => {
                    inputs.push((lp.name.as_str(), Src::Const(b)));
                }
                (Direction::Input, None) => {
                    return Err(e(format!("input pin {pname} is not connected")));
                }
                (Direction::Output, Some(Conn::Net(x))) => {
                    let Some(f) = &lp.function else {
                        return Err(e(format!("output pin {pname} has no function")));
                    };
                    outputs.push((f, x.index()));
                }
                (Direction::Output, Some(Conn::Const(_))) => {
                    return Err(format!(
                        "error[S0918]: instance {name}: output pin {pname} drives a constant"
                    ));
                }
                (Direction::Output, None) => {}
                (_, Some(_)) => {
                    return Err(e(format!(
                        "pin {pname} is {} and connected",
                        if lp.direction == Direction::Inout {
                            "bidirectional"
                        } else {
                            "internal"
                        }
                    )));
                }
                (_, None) => {}
            }
        }
        // An output reads only the input pins its function names, so that the state output of
        // a sequential cell does not depend on its data input.
        let outputs = outputs
            .into_iter()
            .map(|(func, net)| {
                let vars = func.vars();
                let reads = (0..inputs.len())
                    .filter(|&k| vars.iter().any(|v| v == inputs[k].0))
                    .collect();
                Out { func, net, reads }
            })
            .collect();
        insts.push(Inst {
            name,
            cell,
            seq,
            inputs,
            outputs,
        });
    }
    // Drivers (SYNTH-REQ-0910).
    let mut comb = Comb::new(n);
    let mut kinds = Vec::new();
    let mut driven = vec![false; n];
    let mut ports = Vec::new();
    for p in g.ports() {
        let pname = unescape(g.resolve(p.name));
        if p.dir == PortDir::Inout {
            return Err(format!(
                "error[S0916]: port {pname} is inout; gate-level sources take input and output \
                 ports"
            ));
        }
        if p.dir == PortDir::Input {
            for b in &p.bits {
                driven[b.index() as usize] = true;
            }
        }
        ports.push((pname, p.dir == PortDir::Input));
    }
    check_ports(&ports)?;
    let mut drive = |comb: &mut Comb, net: u32, fanins: Vec<u32>| {
        if driven[net as usize] {
            return Err(format!(
                "error[S0918]: net {} has more than one driver",
                label(net)
            ));
        }
        driven[net as usize] = true;
        comb.driver[net as usize] = comb.node(fanins);
        Ok(())
    };
    for (ii, inst) in insts.iter().enumerate() {
        for (o, out) in inst.outputs.iter().enumerate() {
            drive(&mut comb, out.net, inst.fanins(&out.reads))?;
            kinds.push(Kind::Cell(ii, o));
        }
    }
    for a in g.assigns() {
        let src = match a.rhs {
            Conn::Net(x) => Src::Net(x.index()),
            Conn::Const(b) => Src::Const(b),
        };
        let fanins = match src {
            Src::Net(x) => vec![x],
            Src::Const(_) => Vec::new(),
        };
        drive(&mut comb, a.lhs.index(), fanins)?;
        kinds.push(Kind::Assign(src));
    }
    // Every read net is driven (SYNTH-REQ-0910).
    let reads = insts
        .iter()
        .flat_map(|i| i.fanins(&i.all()))
        .chain(g.assigns().iter().filter_map(|a| match a.rhs {
            Conn::Net(x) => Some(x.index()),
            Conn::Const(_) => None,
        }))
        .chain(
            g.ports()
                .iter()
                .filter(|p| p.dir == PortDir::Output)
                .flat_map(|p| p.bits.iter().map(|b| b.index())),
        );
    for net in reads {
        if !driven[net as usize] {
            return Err(format!(
                "error[S0918]: net {} is read but has no driver",
                label(net)
            ));
        }
    }
    // Graph inputs: input port bits from the least significant, then the states.
    let mut aig = Aig::new();
    let mut vals = vec![UNSET; n];
    let mut design_ports = Vec::new();
    for p in g.ports().iter().filter(|p| p.dir == PortDir::Input) {
        let mut bits = Vec::with_capacity(p.bits.len());
        for b in p.bits.iter().rev() {
            let x = aig.input();
            vals[b.index() as usize] = x;
            bits.push(x);
        }
        design_ports.push(port(g, p, bits));
    }
    let mut qs = vec![Lit::FALSE; insts.len()];
    for (ii, inst) in insts.iter().enumerate() {
        if inst.seq.is_some() {
            qs[ii] = aig.input();
        }
    }
    let mut open = vec![false; n];
    let mut build = |aig: &mut Aig, k: u32, ins: &[Lit]| -> Result<Lit, String> {
        match &kinds[k as usize] {
            Kind::Assign(Src::Net(_)) => Ok(ins[0]),
            Kind::Assign(Src::Const(b)) => Ok(Lit::from_bool(*b)),
            Kind::Cell(ii, o) => {
                let inst = &insts[*ii];
                let out = &inst.outputs[*o];
                let values = inst.values(&out.reads, ins);
                let pin = |p: &str| values.iter().find(|(x, _)| *x == p).map(|(_, l)| *l);
                let seq = inst.seq.as_ref().map(|s| (s, qs[*ii]));
                let look = cells::resolver(seq, &pin);
                cells::func(aig, out.func, &look).map_err(|m| {
                    format!(
                        "error[S0917]: instance {} of cell {}: {m}",
                        inst.name, inst.cell.name
                    )
                })
            }
        }
    };
    let name = |k: u32| label(k);
    let mut value = |aig: &mut Aig, vals: &mut Vec<Lit>, net: u32| {
        comb.value(net, vals, &mut open, aig, &mut build, &name)
    };
    let mut states = Vec::new();
    for (ii, inst) in insts.iter().enumerate() {
        let Some(seq) = &inst.seq else {
            continue;
        };
        let all = inst.all();
        let mut ins = Vec::new();
        for net in inst.fanins(&all) {
            ins.push(value(&mut aig, &mut vals, net)?);
        }
        let values = inst.values(&all, &ins);
        let pin = |p: &str| values.iter().find(|(x, _)| *x == p).map(|(_, l)| *l);
        let (kind, d, clk, arst) =
            cells::state(&mut aig, seq, qs[ii], &pin, None).map_err(|(code, m)| {
                format!(
                    "error[{code}]: instance {} of cell {}: {m}",
                    inst.name, inst.cell.name
                )
            })?;
        states.push(State {
            name: inst.name.clone(),
            kind,
            q: qs[ii],
            d,
            clk,
            arst,
        });
    }
    for p in g.ports().iter().filter(|p| p.dir == PortDir::Output) {
        let mut bits = Vec::with_capacity(p.bits.len());
        for b in p.bits.iter().rev() {
            bits.push(value(&mut aig, &mut vals, b.index())?);
        }
        design_ports.push(port(g, p, bits));
    }
    // Ports in the order of the module (SYNTH-REQ-0903).
    let order: Vec<String> = ports.iter().map(|(n, _)| n.clone()).collect();
    design_ports.sort_by_key(|p| order.iter().position(|n| *n == p.name));
    let design = Design {
        top: module,
        aig,
        ports: design_ports,
        states,
    };
    check_states(&design)?;
    Ok(Lowered {
        design,
        messages: Vec::new(),
        warnings: 0,
        info: Info {
            format: "gate-netlist",
            ..Info::default()
        },
    })
}

/// The design port of a netlist port with its bits from the least significant.
fn port(g: &GateNetlist, p: &rusteda_core::netlist::Port, bits: Vec<Lit>) -> Port {
    Port {
        name: unescape(g.resolve(p.name)),
        input: p.dir == PortDir::Input,
        width: bits.len() as u32,
        scalar: p.range.is_none(),
        bits,
    }
}
