// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Library cells of gate-level and BLIF sources (SYNTH-REQ-0909, SYNTH-REQ-0911): which cells
//! the lowering interprets, and their functions and state elements as AND-inverter logic.

use rustc_hash::FxHashMap;
use rusteda_core::netlist::PinDir;
use rusteda_formats::liberty::{Cell, Direction, Func, Library, SeqKind};

use crate::aig::{Aig, Lit};
use crate::elab::StateKind;

/// The pins of every cell for the structural reader: signal pins in library order, then power
/// and ground pins, the first library that defines a cell giving them (SYNTH-REQ-0910).
pub(crate) fn pins(libs: &[Library]) -> FxHashMap<String, Vec<(String, PinDir)>> {
    let mut out: FxHashMap<String, Vec<(String, PinDir)>> = FxHashMap::default();
    for l in libs {
        for c in &l.cells {
            out.entry(c.name.clone()).or_insert_with(|| {
                c.pins
                    .iter()
                    .map(|p| {
                        let d = match p.direction {
                            Direction::Input => PinDir::Input,
                            Direction::Output => PinDir::Output,
                            Direction::Inout => PinDir::Inout,
                            Direction::Internal => PinDir::Other,
                        };
                        (p.name.clone(), d)
                    })
                    .chain(c.pg_pins.iter().map(|p| (p.clone(), PinDir::Other)))
                    .collect()
            });
        }
    }
    out
}

/// The cells of the libraries by name, the first library that defines a name giving the cell
/// (SYNTH-REQ-0909).
#[derive(Debug, Default)]
pub(crate) struct Cells<'l> {
    by_name: FxHashMap<&'l str, &'l Cell>,
}

impl<'l> Cells<'l> {
    pub fn new(libs: &'l [Library]) -> Cells<'l> {
        let mut by_name = FxHashMap::default();
        for l in libs {
            for c in &l.cells {
                by_name.entry(c.name.as_str()).or_insert(c);
            }
        }
        Cells { by_name }
    }

    pub fn get(&self, name: &str) -> Option<&'l Cell> {
        self.by_name.get(name).copied()
    }
}

/// The `ff` or `latch` group of a sequential cell.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Seq<'l> {
    pub latch: bool,
    pub state: &'l str,
    pub state_n: &'l str,
    pub data: &'l Func,
    pub clock: &'l Func,
    pub clear: Option<&'l Func>,
    pub preset: Option<&'l Func>,
}

/// Checks that `cell` is inside the model of SYNTH-REQ-0911 and returns its sequential group, if
/// any; the message of an error names the reason (`S0917`).
pub(crate) fn model(cell: &Cell) -> Result<Option<Seq<'_>>, String> {
    let why = |w: String| Err(format!("cell {}: {w}", cell.name));
    if let Some(p) = cell
        .pins
        .iter()
        .find(|p| p.direction != Direction::Input && p.three_state.is_some())
    {
        return why(format!("pin {} is three-state", p.name));
    }
    if cell.has_statetable {
        return why("a state table describes it".into());
    }
    if cell.has_bus {
        return why("it has bus pins".into());
    }
    if cell.sequentials.len() > 1 {
        return why(format!(
            "{} ff and latch groups; one state element per cell is supported",
            cell.sequentials.len()
        ));
    }
    if !cell.problems.is_empty() {
        return why(format!(
            "its description has problems: {}",
            cell.problems.join("; ")
        ));
    }
    let Some(s) = cell.sequentials.first() else {
        return Ok(None);
    };
    if let Some(also) = &s.clocked_on_also {
        return why(format!(
            "clocked_on_also \"{also}\" makes it a dual-edge or master-slave element"
        ));
    }
    Ok(Some(Seq {
        latch: s.kind == SeqKind::Latch,
        state: &s.state,
        state_n: &s.state_n,
        data: &s.data,
        clock: &s.clock,
        clear: s.clear.as_ref(),
        preset: s.preset.as_ref(),
    }))
}

/// AND-inverter logic of a Liberty function over the names `look` resolves, built with an
/// explicit stack (SYNTH-REQ-0911).
pub(crate) fn func(
    aig: &mut Aig,
    f: &Func,
    look: &dyn Fn(&str) -> Option<Lit>,
) -> Result<Lit, String> {
    enum Op<'f> {
        Visit(&'f Func),
        Not,
        And,
        Or,
        Xor,
    }
    let mut ops = vec![Op::Visit(f)];
    let mut vals: Vec<Lit> = Vec::new();
    let pop2 = |v: &mut Vec<Lit>| {
        let b = v.pop().unwrap_or(Lit::FALSE);
        let a = v.pop().unwrap_or(Lit::FALSE);
        (a, b)
    };
    while let Some(op) = ops.pop() {
        match op {
            Op::Visit(Func::Const(b)) => vals.push(Lit::from_bool(*b)),
            Op::Visit(Func::Var(n)) => vals.push(look(n).ok_or_else(|| {
                format!("its function reads {n}, which is neither an input pin nor its state")
            })?),
            Op::Visit(Func::Not(a)) => {
                ops.push(Op::Not);
                ops.push(Op::Visit(a));
            }
            Op::Visit(Func::And(a, b)) => {
                ops.push(Op::And);
                ops.push(Op::Visit(b));
                ops.push(Op::Visit(a));
            }
            Op::Visit(Func::Or(a, b)) => {
                ops.push(Op::Or);
                ops.push(Op::Visit(b));
                ops.push(Op::Visit(a));
            }
            Op::Visit(Func::Xor(a, b)) => {
                ops.push(Op::Xor);
                ops.push(Op::Visit(b));
                ops.push(Op::Visit(a));
            }
            Op::Not => {
                let a = vals.pop().unwrap_or(Lit::FALSE);
                vals.push(!a);
            }
            Op::And => {
                let (a, b) = pop2(&mut vals);
                vals.push(aig.and(a, b));
            }
            Op::Or => {
                let (a, b) = pop2(&mut vals);
                vals.push(aig.or(a, b));
            }
            Op::Xor => {
                let (a, b) = pop2(&mut vals);
                vals.push(aig.xor(a, b));
            }
        }
    }
    Ok(vals.pop().unwrap_or(Lit::FALSE))
}

/// The pin names of a cell that a function may read: its input pins, resolved by `pin`, and the
/// state of its sequential group and the complement of it.
pub(crate) fn resolver<'a>(
    seq: Option<(&'a Seq<'a>, Lit)>,
    pin: &'a dyn Fn(&str) -> Option<Lit>,
) -> impl Fn(&str) -> Option<Lit> + 'a {
    move |n: &str| {
        if let Some((s, q)) = seq {
            if n == s.state {
                return Some(q);
            }
            if !s.state_n.is_empty() && n == s.state_n {
                return Some(!q);
            }
        }
        pin(n)
    }
}

/// The state element of a sequential cell instance (SYNTH-REQ-0911): kind, data, clock or
/// enable, and the one asynchronous control, which `init` (the initial-value control of
/// SYNTH-REQ-0906) adds to. An error is its code (`S0917` or `S0919` for two controls) and text.
pub(crate) fn state(
    aig: &mut Aig,
    seq: &Seq<'_>,
    q: Lit,
    pin: &dyn Fn(&str) -> Option<Lit>,
    init: Option<(Lit, bool)>,
) -> Result<(StateKind, Lit, Lit, Option<(Lit, bool)>), (&'static str, String)> {
    let look = resolver(Some((seq, q)), pin);
    let model = |m: String| ("S0917", m);
    let d = func(aig, seq.data, &look).map_err(model)?;
    let clk = func(aig, seq.clock, &look).map_err(model)?;
    let clear = match seq.clear {
        Some(f) => func(aig, f, &look).map_err(model)?,
        None => Lit::FALSE,
    };
    let preset = match seq.preset {
        Some(f) => func(aig, f, &look).map_err(model)?,
        None => Lit::FALSE,
    };
    let mut controls: Vec<(Lit, bool)> = [(clear, false), (preset, true)]
        .into_iter()
        .filter(|(c, _)| *c != Lit::FALSE)
        .collect();
    controls.extend(init);
    if controls.len() > 1 {
        return Err((
            "S0919",
            "more than one asynchronous control (clear, preset or the initial-value port); a \
             state element has at most one"
                .into(),
        ));
    }
    let kind = if seq.latch {
        StateKind::Latch
    } else {
        StateKind::Flop
    };
    Ok((kind, d, clk, controls.first().copied()))
}
