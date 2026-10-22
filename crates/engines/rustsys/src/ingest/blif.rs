// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Lowering of a BLIF network (SYNTH-REQ-0903 to SYNTH-REQ-0909): ports, the global clock and
//! the initial-value port, latches and sequential cells as states, covers and cells as
//! AND-inverter logic.

use rusteda_formats::blif::{Cover, Init, LatchType, Network};
use rusteda_formats::liberty::{Cell, Direction, Func, Library};

use super::cells::{self, Cells, Seq};
use super::{
    Comb, Info, InitialValues, Lowered, UNSET, and_all, check_ports, check_states, fresh_name,
    or_all,
};
use crate::aig::{Aig, Lit};
use crate::elab::{Design, Port, State, StateKind};

/// A resolved `.gate` or `.mlatch` instance.
struct Inst<'l> {
    cell: &'l Cell,
    seq: Option<Seq<'l>>,
    /// Input pins and their signals.
    inputs: Vec<(&'l str, u32)>,
    /// Connected output pins: function, signal, and the inputs (indices) the function reads.
    outputs: Vec<(&'l Func, u32, Vec<usize>)>,
    /// The clock pin of an `.mlatch` and its control (`None`: the global clock).
    mlatch: Option<(&'l str, Option<u32>, Init)>,
}

/// What drives a computed signal.
enum Kind {
    Node(usize),
    Cell(usize, usize),
}

/// The value of a cover of input values (SYNTH-REQ-0908, BLIF-REQ-0030).
fn cover(aig: &mut Aig, c: &Cover, ins: &[Lit]) -> Lit {
    if c.is_empty() {
        return Lit::FALSE;
    }
    let mut rows = Vec::with_capacity(c.len());
    for r in 0..c.len() {
        let lits: Vec<Lit> = c
            .row(r)
            .iter()
            .zip(ins)
            .filter_map(|(ch, &l)| match ch {
                b'1' => Some(l),
                b'0' => Some(!l),
                _ => None,
            })
            .collect();
        rows.push(and_all(aig, lits));
    }
    let f = or_all(aig, rows);
    if c.onset { f } else { !f }
}

/// Resolves the cell instances of `net` (SYNTH-REQ-0909, SYNTH-REQ-0911).
fn resolve<'l>(
    net: &Network,
    cells: &Cells<'l>,
    libs: &[Library],
) -> Result<Vec<Inst<'l>>, String> {
    let mut out = Vec::with_capacity(net.cells.len());
    for c in &net.cells {
        let at = net.describe(c.at);
        let e = |m: String| format!("error[S0917]: {at}: {m}");
        if libs.is_empty() {
            return Err(e(format!(
                "instance of library cell {}: .gate and .mlatch need the libraries of their \
                 cells (--gate-lib)",
                c.cell
            )));
        }
        let Some(cell) = cells.get(&c.cell) else {
            return Err(e(format!("unknown cell {}", c.cell)));
        };
        let seq = cells::model(cell).map_err(e)?;
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for (formal, s) in &c.conns {
            let Some(p) = cell.pin(formal) else {
                return Err(e(format!("cell {} has no pin {formal}", cell.name)));
            };
            match p.direction {
                Direction::Input => inputs.push((p.name.as_str(), *s)),
                Direction::Output => {
                    let Some(f) = &p.function else {
                        return Err(e(format!(
                            "output pin {} of cell {} has no function",
                            p.name, cell.name
                        )));
                    };
                    outputs.push((f, *s, Vec::new()));
                }
                Direction::Inout | Direction::Internal => {
                    return Err(e(format!(
                        "pin {} of cell {} is {}",
                        p.name,
                        cell.name,
                        if p.direction == Direction::Inout {
                            "bidirectional"
                        } else {
                            "internal"
                        }
                    )));
                }
            }
        }
        let mlatch = match (c.mlatch, &seq) {
            (None, _) => None,
            (Some((ctl, init)), Some(s)) => {
                let pin = match s.clock {
                    Func::Var(p) => Some(p),
                    Func::Not(x) => match x.as_ref() {
                        Func::Var(p) => Some(p),
                        _ => None,
                    },
                    _ => None,
                }
                .filter(|p| cell.pin(p).is_some_and(|x| x.direction == Direction::Input));
                let Some(pin) = pin else {
                    return Err(e(format!(
                        ".mlatch of cell {}, whose clock or enable is not a single input pin",
                        cell.name
                    )));
                };
                if c.conns.iter().any(|(f, _)| f == pin) {
                    return Err(e(format!(
                        ".mlatch connects the clock pin {pin} of cell {} among its formals",
                        cell.name
                    )));
                }
                Some((pin.as_str(), ctl, init))
            }
            (Some(_), None) => {
                return Err(e(format!(
                    ".mlatch of cell {}, which has no ff or latch group",
                    cell.name
                )));
            }
        };
        if let Some(p) = cell.inputs().find(|p| {
            !inputs.iter().any(|(n, _)| *n == p.name)
                && mlatch.as_ref().is_none_or(|(m, _, _)| *m != p.name)
        }) {
            return Err(e(format!(
                "input pin {} of cell {} is not connected",
                p.name, cell.name
            )));
        }
        // An output reads only the input pins its function names (SYNTH-REQ-0911).
        for o in &mut outputs {
            let vars = o.0.vars();
            o.2 = (0..inputs.len())
                .filter(|&k| vars.iter().any(|v| v == inputs[k].0))
                .collect();
        }
        out.push(Inst {
            cell,
            seq,
            inputs,
            outputs,
            mlatch,
        });
    }
    Ok(out)
}

/// A state of the source before lowering.
enum Src {
    Latch(usize),
    Cell(usize),
}

/// Lowers `net` (SYNTH-REQ-0903 to SYNTH-REQ-0909).
pub(super) fn lower(
    net: &Network,
    libs: &[Library],
    mode: InitialValues,
) -> Result<Lowered, Vec<String>> {
    lower_inner(net, libs, mode).map_err(|m| vec![m])
}

fn lower_inner(net: &Network, libs: &[Library], mode: InitialValues) -> Result<Lowered, String> {
    let cells = Cells::new(libs);
    let insts = resolve(net, &cells, libs)?;
    let n = net.signals.len();
    let name = |s: u32| net.name(s).to_string();
    // Drivers of computed signals: nodes and cell output pins (SYNTH-REQ-0909).
    let mut comb = Comb::new(n);
    let mut kinds = Vec::new();
    let mut driven = vec![false; n];
    for &s in &net.inputs {
        driven[s as usize] = true;
    }
    for l in &net.latches {
        driven[l.output as usize] = true;
    }
    let mut drive = |comb: &mut Comb, s: u32, fanins: Vec<u32>, at: String| {
        if driven[s as usize] {
            return Err(format!(
                "error[S0918]: {at}: signal {} has more than one driver",
                net.name(s)
            ));
        }
        driven[s as usize] = true;
        comb.driver[s as usize] = comb.node(fanins);
        Ok(())
    };
    for (i, nd) in net.nodes.iter().enumerate() {
        drive(&mut comb, nd.output, nd.inputs.clone(), net.describe(nd.at))?;
        kinds.push(Kind::Node(i));
    }
    for (ci, inst) in insts.iter().enumerate() {
        for (k, (_, s, reads)) in inst.outputs.iter().enumerate() {
            let fanins = reads.iter().map(|&j| inst.inputs[j].1).collect();
            drive(&mut comb, *s, fanins, net.describe(net.cells[ci].at))?;
            kinds.push(Kind::Cell(ci, k));
        }
    }
    // A signal that a cell input reads and nothing drives reads as 0 with a warning
    // (BLIF-REQ-0034, SYNTH-REQ-0909); the reader warned about the other undriven signals.
    let mut cell_undriven: Vec<(u32, usize)> = Vec::new();
    let mut warned = vec![false; n];
    for (ci, inst) in insts.iter().enumerate() {
        let ctl = inst.mlatch.as_ref().and_then(|(_, c, _)| *c);
        for s in inst.inputs.iter().map(|(_, s)| *s).chain(ctl) {
            if !driven[s as usize] && !warned[s as usize] {
                warned[s as usize] = true;
                cell_undriven.push((s, ci));
            }
        }
    }
    // States and their names (SYNTH-REQ-0907).
    let mut srcs = Vec::new();
    let mut state_names = Vec::new();
    let mut declared = Vec::new();
    for (i, l) in net.latches.iter().enumerate() {
        srcs.push(Src::Latch(i));
        state_names.push(name(l.output));
        if let Some(v) = l.init.value() {
            declared.push((name(l.output), v));
        }
    }
    for (ci, inst) in insts.iter().enumerate() {
        let Some(seq) = &inst.seq else {
            continue;
        };
        let out = inst
            .outputs
            .iter()
            .find(|(f, _, _)| matches!(f, Func::Var(v) if v == seq.state));
        let Some((_, s, _)) = out else {
            return Err(format!(
                "error[S0917]: {}: sequential cell {} needs its state output connected to name \
                 its state",
                net.describe(net.cells[ci].at),
                inst.cell.name
            ));
        };
        srcs.push(Src::Cell(ci));
        state_names.push(name(*s));
        if let Some(v) = inst.mlatch.as_ref().and_then(|(_, _, i)| i.value()) {
            declared.push((name(*s), v));
        }
    }
    // Generated ports (SYNTH-REQ-0904, SYNTH-REQ-0906).
    let needs_clock = net.latches.iter().any(|l| l.control.is_none())
        || insts.iter().any(|i| matches!(i.mlatch, Some((_, None, _))));
    let needs_init = mode == InitialValues::Reset && !declared.is_empty();
    let mut ports: Vec<(String, bool)> = net.inputs.iter().map(|&s| (name(s), true)).collect();
    let outputs: Vec<(String, bool)> = net.outputs.iter().map(|&s| (name(s), false)).collect();
    let taken = |x: &str, extra: &[String]| {
        ports.iter().chain(&outputs).any(|(p, _)| p == x)
            || state_names.iter().chain(extra).any(|p| p == x)
    };
    let clock_port = needs_clock.then(|| fresh_name("clk", &|x| taken(x, &[])));
    let init_port = needs_init.then(|| {
        let extra: Vec<String> = clock_port.iter().cloned().collect();
        fresh_name("init", &|x| taken(x, &extra))
    });
    ports.extend(clock_port.iter().map(|p| (p.clone(), true)));
    ports.extend(init_port.iter().map(|p| (p.clone(), true)));
    let mut all = ports.clone();
    all.extend(outputs.iter().cloned());
    check_ports(&all)?;
    // Graph inputs: ports, then states (SYNTH-REQ-0402).
    let mut aig = Aig::new();
    let mut vals = vec![UNSET; n];
    let mut design_ports = Vec::new();
    for &s in &net.inputs {
        let l = aig.input();
        vals[s as usize] = l;
        design_ports.push(scalar(&name(s), true, l));
    }
    let clk = clock_port.as_ref().map(|p| {
        let l = aig.input();
        design_ports.push(scalar(p, true, l));
        l
    });
    let init = init_port.as_ref().map(|p| {
        let l = aig.input();
        design_ports.push(scalar(p, true, l));
        l
    });
    // Signals without a driver read as 0 (SYNTH-REQ-0908).
    for (k, v) in vals.iter_mut().enumerate() {
        if !driven[k] {
            *v = Lit::FALSE;
        }
    }
    let mut qs = Vec::with_capacity(srcs.len());
    let mut cell_q = vec![Lit::FALSE; insts.len()];
    for src in &srcs {
        let q = aig.input();
        qs.push(q);
        match src {
            Src::Latch(i) => vals[net.latches[*i].output as usize] = q,
            Src::Cell(ci) => cell_q[*ci] = q,
        }
    }
    // Combinational logic on demand (SYNTH-REQ-0908).
    let mut open = vec![false; n];
    let mut build = |aig: &mut Aig, k: u32, ins: &[Lit]| -> Result<Lit, String> {
        match kinds[k as usize] {
            Kind::Node(i) => Ok(cover(aig, &net.nodes[i].cover, ins)),
            Kind::Cell(ci, o) => {
                let inst = &insts[ci];
                let reads = &inst.outputs[o].2;
                let pin = |p: &str| {
                    reads
                        .iter()
                        .position(|&j| inst.inputs[j].0 == p)
                        .map(|k| ins[k])
                };
                let seq = inst.seq.as_ref().map(|s| (s, cell_q[ci]));
                let look = cells::resolver(seq, &pin);
                cells::func(aig, inst.outputs[o].0, &look).map_err(|m| {
                    format!(
                        "error[S0917]: {}: cell {}: {m}",
                        net.describe(net.cells[ci].at),
                        inst.cell.name
                    )
                })
            }
        }
    };
    let mut value = |aig: &mut Aig, vals: &mut Vec<Lit>, s: u32| {
        comb.value(s, vals, &mut open, aig, &mut build, &name)
    };
    let mut states = Vec::with_capacity(srcs.len());
    let init_of = |v: Init| init.zip(v.value());
    for (src, (q, nm)) in srcs.iter().zip(qs.iter().zip(&state_names)) {
        let st = match src {
            Src::Latch(i) => {
                let l = &net.latches[*i];
                let d = value(&mut aig, &mut vals, l.input)?;
                let ctl = match l.control {
                    Some(c) => value(&mut aig, &mut vals, c)?,
                    None => clk.unwrap_or(Lit::FALSE),
                };
                let (kind, c) = match l.kind {
                    None | Some(LatchType::RisingEdge) => (StateKind::Flop, ctl),
                    Some(LatchType::FallingEdge) => (StateKind::Flop, !ctl),
                    Some(LatchType::ActiveHigh) => (StateKind::Latch, ctl),
                    Some(LatchType::ActiveLow) => (StateKind::Latch, !ctl),
                };
                let arst = init_of(l.init);
                State {
                    name: nm.clone(),
                    kind,
                    q: *q,
                    d,
                    clk: c,
                    arst,
                }
            }
            Src::Cell(ci) => {
                let inst = &insts[*ci];
                let mut pins = Vec::with_capacity(inst.inputs.len() + 1);
                for (p, s) in &inst.inputs {
                    pins.push((*p, value(&mut aig, &mut vals, *s)?));
                }
                if let Some((p, ctl, _)) = &inst.mlatch {
                    let c = match ctl {
                        Some(c) => value(&mut aig, &mut vals, *c)?,
                        None => clk.unwrap_or(Lit::FALSE),
                    };
                    pins.push((p, c));
                }
                let pin = |p: &str| pins.iter().find(|(x, _)| *x == p).map(|(_, l)| *l);
                let Some(seq) = inst.seq.as_ref() else {
                    return Err("error[S0900]: internal: a state cell without a group".into());
                };
                let init = inst.mlatch.as_ref().and_then(|(_, _, i)| init_of(*i));
                let (kind, d, c, arst) =
                    cells::state(&mut aig, seq, *q, &pin, init).map_err(|(code, m)| {
                        format!(
                            "error[{code}]: {}: cell {}: {m}",
                            net.describe(net.cells[*ci].at),
                            inst.cell.name
                        )
                    })?;
                State {
                    name: nm.clone(),
                    kind,
                    q: *q,
                    d,
                    clk: c,
                    arst,
                }
            }
        };
        states.push(st);
    }
    for &s in &net.outputs {
        let l = value(&mut aig, &mut vals, s)?;
        design_ports.push(scalar(&name(s), false, l));
    }
    let design = Design {
        top: net.name.clone(),
        aig,
        ports: design_ports,
        states,
    };
    check_states(&design)?;
    let mut messages = Vec::new();
    if let Some(&(_, ci)) = cell_undriven.first() {
        let names: Vec<&str> = cell_undriven
            .iter()
            .take(8)
            .map(|&(s, _)| net.name(s))
            .collect();
        messages.push(format!(
            "warning[S0921]: {}: {} signals that cell inputs read have no driver and read as 0: \
             {}{}",
            net.describe(net.cells[ci].at),
            cell_undriven.len(),
            names.join(", "),
            if cell_undriven.len() > names.len() {
                ", ..."
            } else {
                ""
            }
        ));
    }
    if mode == InitialValues::Arbitrary && !declared.is_empty() {
        messages.push(format!(
            "warning[S0913]: the declared initial values of {} states are not implemented \
             (--initial-values arbitrary); every state starts in an arbitrary state",
            declared.len()
        ));
    }
    Ok(Lowered {
        design,
        warnings: messages.len(),
        messages,
        info: Info {
            format: "blif",
            initial_values: declared,
            clock_port,
            init_port,
        },
    })
}

/// A scalar port.
pub(super) fn scalar(name: &str, input: bool, l: Lit) -> Port {
    Port {
        name: name.to_string(),
        input,
        width: 1,
        scalar: true,
        bits: vec![l],
    }
}
