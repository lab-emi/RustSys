// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Planted defects in structural netlists (CHECK-REQ-0070).

use rusteda_formats::liberty::{Direction, Library};
use rusteda_formats::verilog::ast::{Expr, ExprKind, Item, ModuleItem, PortConn, PortList};
use rusteda_formats::verilog::number::Bit4;
use rusteda_formats::verilog::write::{NlDir, NlInstance, NlModule, NlPort, NlRef};
use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};

/// Defect classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefectClass {
    CellSubstitution,
    PinSwap,
    InvertedNet,
    StuckAt0,
    StuckAt1,
    Rewire,
    RemovedInverter,
    ClockInversion,
    AsyncPolarity,
    AsyncValue,
    SwappedStateNames,
}

impl DefectClass {
    pub const ALL: [DefectClass; 11] = [
        DefectClass::CellSubstitution,
        DefectClass::PinSwap,
        DefectClass::InvertedNet,
        DefectClass::StuckAt0,
        DefectClass::StuckAt1,
        DefectClass::Rewire,
        DefectClass::RemovedInverter,
        DefectClass::ClockInversion,
        DefectClass::AsyncPolarity,
        DefectClass::AsyncValue,
        DefectClass::SwappedStateNames,
    ];
}

fn lit_bit(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Number(n) => n.bits().ok().map(|b| b.first() == Some(&Bit4::One)),
        _ => None,
    }
}

fn nl_ref(e: &Expr) -> Result<NlRef, String> {
    match &e.kind {
        ExprKind::Ident(p) => Ok(NlRef::Net(p.name.name.clone())),
        ExprKind::Index(b, i) => {
            let n = b.as_ident().ok_or("bad net reference")?.name.clone();
            let ExprKind::Number(l) = &i.kind else {
                return Err("bad index".into());
            };
            let k = l.bits()?.iter().enumerate().fold(0u32, |a, (i, x)| {
                if *x == Bit4::One { a | 1 << i } else { a }
            });
            Ok(NlRef::Bit(n, k))
        }
        _ => lit_bit(e)
            .map(NlRef::Const)
            .ok_or_else(|| "bad netlist expression".into()),
    }
}

/// Reads a netlist in the structural subset into an editable module.
pub fn read_netlist(text: &str) -> Result<NlModule, String> {
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("netlist.v", text)], PpOptions::default());
    if let Some(d) = diags.iter().find(|d| d.is_error()) {
        return Err(d.render(&sm));
    }
    let m = units
        .iter()
        .flat_map(|u| u.items.iter())
        .find_map(|i| match i {
            Item::Module(m) => Some(m),
            _ => None,
        })
        .ok_or("no module")?;
    let mut out = NlModule {
        name: m.name.name.clone(),
        ..NlModule::default()
    };
    let width = |dims: &[rusteda_formats::verilog::ast::Dim]| -> Option<u32> {
        match dims.first() {
            Some(rusteda_formats::verilog::ast::Dim::Range(l, r)) => {
                let v = |e: &Expr| match &e.kind {
                    ExprKind::Number(n) => n.bits().ok().map(|b| {
                        b.iter().enumerate().fold(0i64, |a, (i, x)| {
                            if *x == Bit4::One { a | 1 << i } else { a }
                        })
                    }),
                    _ => None,
                };
                Some(((v(l)? - v(r)?).unsigned_abs() + 1) as u32)
            }
            _ => None,
        }
    };
    if let PortList::Ansi(ps) = &m.ports {
        for p in ps {
            out.ports.push(NlPort {
                name: p.name.name.clone(),
                dir: if p.dir == rusteda_formats::verilog::ast::Direction::Input {
                    NlDir::Input
                } else {
                    NlDir::Output
                },
                width: width(&p.ty.packed),
            });
        }
    }
    for it in &m.items {
        match it {
            ModuleItem::Data(d) => {
                for x in &d.decls {
                    out.wires.push((x.name.name.clone(), width(&d.ty.packed)));
                }
            }
            ModuleItem::Assign(v) => {
                for a in v {
                    out.assigns.push((nl_ref(&a.lhs)?, nl_ref(&a.rhs)?));
                }
            }
            ModuleItem::Instance(i) => {
                for x in &i.instances {
                    let mut conns = Vec::new();
                    for c in &x.conns {
                        let PortConn::Named(p, Some(Some(e))) = c else {
                            return Err("named connections expected".into());
                        };
                        conns.push((p.name.clone(), nl_ref(e)?));
                    }
                    out.instances.push(NlInstance {
                        cell: i.module.name.clone(),
                        name: x.name.name.clone(),
                        conns,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Small deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick(&mut self, n: usize) -> Option<usize> {
        if n == 0 {
            None
        } else {
            Some((self.next() % n as u64) as usize)
        }
    }
}

fn cell_kind(lib: &Library, name: &str) -> Option<(bool, u64, Vec<String>, String)> {
    let c = lib.cells.iter().find(|c| c.name == name)?;
    let ins: Vec<String> = c
        .pins
        .iter()
        .filter(|p| p.direction == Direction::Input)
        .map(|p| p.name.clone())
        .collect();
    let out = c.pins.iter().find(|p| p.direction == Direction::Output)?;
    let names: Vec<&str> = ins.iter().map(String::as_str).collect();
    let tt = out
        .function
        .as_ref()
        .and_then(|f| f.truth_table(&names))
        .unwrap_or(0);
    Some((!c.sequentials.is_empty(), tt, ins, out.name.clone()))
}

fn fresh(m: &NlModule, base: &str) -> String {
    let mut k = 0;
    loop {
        let n = format!("{base}{k}");
        if !m.wires.iter().any(|w| w.0 == n)
            && !m.instances.iter().any(|i| i.name == n)
            && !m.ports.iter().any(|p| p.name == n)
        {
            return n;
        }
        k += 1;
    }
}

/// Plants one defect of the given class; `None` if the netlist has no suitable site.
pub fn plant(nl: &NlModule, lib: &Library, class: DefectClass, seed: u64) -> Option<NlModule> {
    let mut m = nl.clone();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let comb: Vec<usize> = (0..m.instances.len())
        .filter(|&i| cell_kind(lib, &m.instances[i].cell).is_some_and(|k| !k.0))
        .collect();
    let seq: Vec<usize> = (0..m.instances.len())
        .filter(|&i| cell_kind(lib, &m.instances[i].cell).is_some_and(|k| k.0))
        .collect();
    let inv = lib
        .cells
        .iter()
        .find(|c| {
            cell_kind(lib, &c.name).is_some_and(|k| !k.0 && k.2.len() == 1 && k.1 == 0b01)
                && !c.dont_use
        })?
        .name
        .clone();
    let inv_pins = cell_kind(lib, &inv).map(|k| (k.2[0].clone(), k.3))?;
    let input_pins = |m: &NlModule, i: usize| -> Vec<usize> {
        let cell = &m.instances[i].cell;
        let c = lib.cells.iter().find(|c| c.name == *cell);
        (0..m.instances[i].conns.len())
            .filter(|&k| {
                c.and_then(|c| c.pin(&m.instances[i].conns[k].0))
                    .is_some_and(|p| p.direction == Direction::Input)
            })
            .collect()
    };
    match class {
        DefectClass::CellSubstitution => {
            let i = comb[rng.pick(comb.len())?];
            let (_, tt, ins, out) = cell_kind(lib, &m.instances[i].cell)?;
            let alt = lib.cells.iter().find(|c| {
                c.name != m.instances[i].cell
                    && !c.dont_use
                    && cell_kind(lib, &c.name)
                        .is_some_and(|k| !k.0 && k.2 == ins && k.3 == out && k.1 != tt)
            })?;
            m.instances[i].cell = alt.name.clone();
        }
        DefectClass::PinSwap => {
            // A cell whose function changes when two inputs are exchanged.
            for _ in 0..comb.len() * 2 {
                let i = comb[rng.pick(comb.len())?];
                let (_, tt, ins, _) = cell_kind(lib, &m.instances[i].cell)?;
                for a in 0..ins.len() {
                    for b in a + 1..ins.len() {
                        let n = ins.len();
                        let swapped = (0..1u64 << n).fold(0u64, |acc, x| {
                            let (ba, bb) = ((x >> a) & 1, (x >> b) & 1);
                            let y = (x & !(1 << a) & !(1 << b)) | (bb << a) | (ba << b);
                            acc | (((tt >> y) & 1) << x)
                        });
                        if swapped != tt {
                            let pa = m.instances[i].conns.iter().position(|c| c.0 == ins[a])?;
                            let pb = m.instances[i].conns.iter().position(|c| c.0 == ins[b])?;
                            let ra = m.instances[i].conns[pa].1.clone();
                            let rb = m.instances[i].conns[pb].1.clone();
                            if ra != rb {
                                m.instances[i].conns[pa].1 = rb;
                                m.instances[i].conns[pb].1 = ra;
                                return Some(m);
                            }
                        }
                    }
                }
            }
            return None;
        }
        DefectClass::InvertedNet
        | DefectClass::StuckAt0
        | DefectClass::StuckAt1
        | DefectClass::Rewire => {
            let i = comb[rng.pick(comb.len())?];
            let pins = input_pins(&m, i);
            let k = pins[rng.pick(pins.len())?];
            let orig = m.instances[i].conns[k].1.clone();
            match class {
                DefectClass::InvertedNet => {
                    let n = fresh(&m, "dfx_n");
                    let g = fresh(&m, "dfx_g");
                    m.wires.push((n.clone(), None));
                    m.instances.push(NlInstance {
                        cell: inv.clone(),
                        name: g,
                        conns: vec![
                            (inv_pins.0.clone(), orig),
                            (inv_pins.1.clone(), NlRef::Net(n.clone())),
                        ],
                    });
                    m.instances[i].conns[k].1 = NlRef::Net(n);
                }
                DefectClass::StuckAt0 => {
                    if orig == NlRef::Const(false) {
                        return None;
                    }
                    m.instances[i].conns[k].1 = NlRef::Const(false);
                }
                DefectClass::StuckAt1 => {
                    if orig == NlRef::Const(true) {
                        return None;
                    }
                    m.instances[i].conns[k].1 = NlRef::Const(true);
                }
                _ => {
                    // Another net driven by a combinational cell earlier in the list (no loop).
                    let others: Vec<NlRef> = m.instances[..i]
                        .iter()
                        .filter(|x| cell_kind(lib, &x.cell).is_some_and(|k| !k.0))
                        .filter_map(|x| x.conns.last().map(|c| c.1.clone()))
                        .filter(|r| *r != orig)
                        .collect();
                    let r = others.get(rng.pick(others.len())?)?.clone();
                    m.instances[i].conns[k].1 = r;
                }
            }
        }
        DefectClass::RemovedInverter => {
            let invs: Vec<usize> = comb
                .iter()
                .copied()
                .filter(|&i| {
                    cell_kind(lib, &m.instances[i].cell)
                        .is_some_and(|k| k.2.len() == 1 && k.1 == 0b01)
                })
                .collect();
            let i = invs[rng.pick(invs.len())?];
            let (inp, out) = (
                m.instances[i].conns[0].1.clone(),
                m.instances[i].conns[1].1.clone(),
            );
            m.instances.remove(i);
            // Consumers of the inverter output now read its input.
            for x in &mut m.instances {
                for c in &mut x.conns {
                    if c.1 == out {
                        c.1 = inp.clone();
                    }
                }
            }
            for a in &mut m.assigns {
                if a.1 == out {
                    a.1 = inp.clone();
                }
            }
            if m.assigns.iter().any(|a| a.0 == out)
                || m.ports.iter().any(|p| NlRef::Net(p.name.clone()) == out)
            {
                m.assigns.push((out, inp));
            }
        }
        DefectClass::ClockInversion | DefectClass::AsyncPolarity => {
            let sites: Vec<(usize, String)> = seq
                .iter()
                .filter_map(|&i| {
                    let c = lib.cells.iter().find(|c| c.name == m.instances[i].cell)?;
                    let s = c.sequentials.first()?;
                    let text = if class == DefectClass::ClockInversion {
                        Some(s.clock_text.clone())
                    } else {
                        s.clear_text.clone().or(s.preset_text.clone())
                    }?;
                    let pin = c.pins.iter().find(|p| {
                        p.direction == Direction::Input && text.contains(p.name.as_str())
                    })?;
                    Some((i, pin.name.clone()))
                })
                .collect();
            let (i, pin) = sites.get(rng.pick(sites.len())?)?.clone();
            let k = m.instances[i].conns.iter().position(|c| c.0 == pin)?;
            let orig = m.instances[i].conns[k].1.clone();
            let n = fresh(&m, "dfx_n");
            let g = fresh(&m, "dfx_g");
            m.wires.push((n.clone(), None));
            m.instances.push(NlInstance {
                cell: inv.clone(),
                name: g,
                conns: vec![
                    (inv_pins.0.clone(), orig),
                    (inv_pins.1.clone(), NlRef::Net(n.clone())),
                ],
            });
            m.instances[i].conns[k].1 = NlRef::Net(n);
        }
        DefectClass::AsyncValue => {
            // Exchange a clear flip-flop for a preset flip-flop (or vice versa), its control pin
            // renamed.
            for &i in &seq {
                let c = lib.cells.iter().find(|c| c.name == m.instances[i].cell)?;
                let s = c.sequentials.first()?;
                let ((Some(t), None) | (None, Some(t))) = (&s.clear_text, &s.preset_text) else {
                    continue;
                };
                let want_clear = s.clear_text.is_none();
                let alt = lib.cells.iter().find(|a| {
                    a.name != c.name
                        && !a.dont_use
                        && a.sequentials.first().is_some_and(|x| {
                            x.clock_text == s.clock_text
                                && x.data_text == s.data_text
                                && (x.clear_text.is_some() == want_clear)
                                && (x.preset_text.is_some() != want_clear)
                                && x.clear_text
                                    .as_deref()
                                    .or(x.preset_text.as_deref())
                                    .is_some_and(|tt| tt.starts_with('!') == t.starts_with('!'))
                        })
                });
                let Some(alt) = alt else { continue };
                // The control pin of each cell.
                let old_pin = c
                    .pins
                    .iter()
                    .find(|p| p.direction == Direction::Input && t.contains(p.name.as_str()))?
                    .name
                    .clone();
                let at = alt.sequentials[0]
                    .clear_text
                    .clone()
                    .or(alt.sequentials[0].preset_text.clone())?;
                let new_pin = alt
                    .pins
                    .iter()
                    .find(|p| p.direction == Direction::Input && at.contains(p.name.as_str()))?
                    .name
                    .clone();
                // The outputs that read the state of each cell, by function.
                let state_out = |cell: &rusteda_formats::liberty::Cell, var: &str| {
                    cell.pins
                        .iter()
                        .find(|p| {
                            p.direction == Direction::Output
                                && p.function_text
                                    .as_deref()
                                    .is_some_and(|f| f.trim().trim_matches('"').trim() == var)
                        })
                        .map(|p| p.name.clone())
                };
                // A connected output the other cell lacks is rebuilt, so that only the
                // asynchronous value differs: the complement output by an inverter from the true
                // output. Another missing pin, or an input of the other cell that the instance
                // does not connect, rules the instance out.
                let comp = state_out(c, &s.state_n).filter(|_| !s.state_n.is_empty());
                let lost: Vec<&(String, NlRef)> = m.instances[i]
                    .conns
                    .iter()
                    .filter(|cn| cn.0 != old_pin && alt.pin(&cn.0).is_none())
                    .collect();
                let unconnected = alt.pins.iter().any(|p| {
                    p.direction == Direction::Input
                        && p.name != new_pin
                        && !m.instances[i].conns.iter().any(|cn| cn.0 == p.name)
                });
                if unconnected || lost.iter().any(|cn| Some(&cn.0) != comp.as_ref()) {
                    continue;
                }
                let rebuilt: Option<NlRef> = lost.first().map(|cn| cn.1.clone());
                let Some(q_pin) = state_out(alt, &alt.sequentials[0].state) else {
                    continue;
                };
                let q_net = m.instances[i]
                    .conns
                    .iter()
                    .find(|cn| cn.0 == q_pin)
                    .map(|cn| cn.1.clone());
                let (q_wire, gate) = (fresh(&m, "dfx_q"), fresh(&m, "dfx_g"));
                let x = &mut m.instances[i];
                x.cell = alt.name.clone();
                for cn in &mut x.conns {
                    if cn.0 == old_pin {
                        cn.0 = new_pin.clone();
                    }
                }
                x.conns.retain(|cn| alt.pin(&cn.0).is_some());
                if let Some(qn) = rebuilt {
                    let q = match q_net {
                        Some(q) => q,
                        None => {
                            x.conns.push((q_pin, NlRef::Net(q_wire.clone())));
                            m.wires.push((q_wire.clone(), None));
                            NlRef::Net(q_wire)
                        }
                    };
                    m.instances.push(NlInstance {
                        cell: inv.clone(),
                        name: gate,
                        conns: vec![(inv_pins.0.clone(), q), (inv_pins.1.clone(), qn)],
                    });
                }
                return Some(m);
            }
            return None;
        }
        DefectClass::SwappedStateNames => {
            // Exchange the names of two state wires: their references are swapped.
            let states: Vec<String> = m
                .wires
                .iter()
                .filter(|w| w.1.is_none() && (w.0.contains('[') || w.0.contains('.')))
                .map(|w| w.0.clone())
                .collect();
            if states.len() < 2 {
                return None;
            }
            let a = states[rng.pick(states.len())?].clone();
            let mut b = states[rng.pick(states.len())?].clone();
            if a == b {
                b = states.iter().find(|s| **s != a)?.clone();
            }
            let tmp = "\u{1}swap".to_string();
            let ren = |r: &mut NlRef, from: &str, to: &str| {
                if let NlRef::Net(n) = r {
                    if n == from {
                        *n = to.to_string();
                    }
                }
            };
            for (from, to) in [
                (a.as_str(), tmp.as_str()),
                (b.as_str(), a.as_str()),
                (tmp.as_str(), b.as_str()),
            ] {
                for x in &mut m.instances {
                    for c in &mut x.conns {
                        ren(&mut c.1, from, to);
                    }
                }
                for asg in &mut m.assigns {
                    ren(&mut asg.0, from, to);
                    ren(&mut asg.1, from, to);
                }
            }
        }
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_reader() {
        let text = "module m(input wire [1:0] a, output wire y);\n  wire n0;\n  nd2 g0 (.A(a[0]), .B(a[1]), .Y(n0));\n  assign y = n0;\nendmodule\n";
        let m = read_netlist(text).unwrap();
        assert_eq!(m.ports.len(), 2);
        assert_eq!(m.instances.len(), 1);
        assert_eq!(m.assigns.len(), 1);
        let again = read_netlist(&rusteda_formats::verilog::write::write_netlist(&m)).unwrap();
        assert_eq!(again, m);
    }
}
