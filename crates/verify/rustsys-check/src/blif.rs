// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The checker's own BLIF reader (CHECK-REQ-0101, CHECK-REQ-0104, CHECK-REQ-0107): the text,
//! models, covers, latches, cells and hierarchy of BLIF-REQ-0010 to BLIF-REQ-0042, read from the
//! BLIF specification and built straight into circuit variables, with the ports, clocks, initial
//! values and state names of SYNTH-REQ-0903 to SYNTH-REQ-0907.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusteda_formats::liberty::{Cell, Direction, Func, Library, SeqKind};

use crate::Input;
use crate::circ::{B, Circuit};
use crate::lfunc;
use crate::liberty::Unread;
use crate::rtl::{RtlModel, RtlState};
use crate::sources::{
    CELL, InitialValues, SKIPPED, SOURCE, SourceModel, UNDRIVEN, bytes, generated,
};

/// Commands skipped with a warning: no effect on the function (BLIF-REQ-0051).
const SKIP: [&str; 20] = [
    ".area",
    ".delay",
    ".wire_load_slope",
    ".wire",
    ".input_arrival",
    ".default_input_arrival",
    ".output_required",
    ".default_output_required",
    ".input_drive",
    ".default_input_drive",
    ".output_load",
    ".default_output_load",
    ".max_input_load",
    ".default_max_input_load",
    ".cycle",
    ".clock_event",
    ".attr",
    ".attrib",
    ".param",
    ".cname",
];

/// The most statements a flattened model may hold (CHECK-REQ-0107, BLIF-REQ-0060).
const MAX_ITEMS: usize = 1 << 22;
/// The most signals a flattened model may hold.
const MAX_SIGNALS: usize = 1 << 25;

/// A logical line: its file, its first line and its words (BLIF-REQ-0010).
struct Ln<'t> {
    file: usize,
    no: u32,
    w: Vec<&'t str>,
}

/// The logical lines of a text.
fn logical(text: &str, file: usize) -> Vec<Ln<'_>> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut first = 1;
    for (k, raw) in text.split('\n').enumerate() {
        let body = raw.split('#').next().unwrap_or("");
        let body = body.trim_end_matches([' ', '\t', '\r']);
        let (body, more) = match body.strip_suffix('\\') {
            Some(b) => (b, true),
            None => (body, false),
        };
        if cur.is_empty() {
            first = k as u32 + 1;
        }
        cur.extend(body.split([' ', '\t', '\r']).filter(|w| !w.is_empty()));
        if !more && !cur.is_empty() {
            out.push(Ln {
                file,
                no: first,
                w: std::mem::take(&mut cur),
            });
        }
    }
    if !cur.is_empty() {
        out.push(Ln {
            file,
            no: first,
            w: cur,
        });
    }
    out
}

/// A statement of a model.
enum St<'t> {
    Names {
        sig: Vec<&'t str>,
        rows: Vec<(&'t str, bool)>,
        at: (usize, u32),
    },
    Latch {
        w: Vec<&'t str>,
        at: (usize, u32),
    },
    Sub {
        model: &'t str,
        conns: Vec<(&'t str, &'t str)>,
        at: (usize, u32),
    },
    Cell {
        cell: &'t str,
        conns: Vec<(&'t str, &'t str)>,
        /// `.mlatch` control and initial value (empty when absent).
        ml: Option<(&'t str, &'t str)>,
        at: (usize, u32),
    },
}

/// A model.
struct Mdl<'t> {
    name: String,
    at: (usize, u32),
    ins: Vec<&'t str>,
    clocks: Vec<&'t str>,
    outs: Vec<&'t str>,
    body: Vec<St<'t>>,
    blackbox: bool,
}

/// The reader's state.
struct Rd<'t> {
    files: Vec<String>,
    models: Vec<Mdl<'t>>,
    warnings: Vec<String>,
}

impl Rd<'_> {
    fn err(&self, at: (usize, u32), msg: impl std::fmt::Display) -> String {
        format!("error[{SOURCE}]: {}:{}:1: {msg}", self.files[at.0], at.1)
    }
}

/// `formal=actual` words.
fn conns<'t>(
    rd: &Rd<'_>,
    w: &[&'t str],
    at: (usize, u32),
) -> Result<Vec<(&'t str, &'t str)>, String> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    for x in w {
        match x.split_once('=') {
            Some((f, a)) if !f.is_empty() && !a.is_empty() => {
                if out.iter().any(|(g, _)| *g == f) {
                    return Err(rd.err(at, format!("formal {f} is connected twice")));
                }
                out.push((f, a));
            }
            _ => return Err(rd.err(at, format!("'{x}' is not formal=actual"))),
        }
    }
    Ok(out)
}

/// Parses one file into models (BLIF-REQ-0020 to BLIF-REQ-0052).
fn parse<'t>(rd: &mut Rd<'t>, text: &'t str, file: usize, stem: &str) -> Result<(), String> {
    let mut cur: Option<Mdl<'t>> = None;
    let mut names_open = false;
    let mut exdc = false;
    let mut skipped: Vec<(&str, (usize, u32), usize)> = Vec::new();
    let mut any = false;
    for ln in logical(text, file) {
        let at = (ln.file, ln.no);
        let w0 = ln.w[0];
        if exdc && w0 != ".end" && w0 != ".model" {
            continue;
        }
        if !w0.starts_with('.') {
            let Some(St::Names { sig, rows, .. }) = cur
                .as_mut()
                .and_then(|m| m.body.last_mut())
                .filter(|_| names_open)
            else {
                return Err(rd.err(at, "a cover row without a .names before it"));
            };
            let n = sig.len() - 1;
            let (plane, out) = match ln.w.as_slice() {
                [o] if n == 0 => ("", *o),
                [p, o] if n > 0 => (*p, *o),
                _ => return Err(rd.err(at, "a malformed cover row")),
            };
            if plane.len() != n || plane.bytes().any(|b| !matches!(b, b'0' | b'1' | b'-')) {
                return Err(rd.err(
                    at,
                    format!("input plane '{plane}' of a cover of {n} inputs"),
                ));
            }
            let v = match out {
                "1" => true,
                "0" => false,
                _ => return Err(rd.err(at, format!("cover output '{out}'"))),
            };
            if rows.first().is_some_and(|r| r.1 != v) {
                return Err(rd.err(at, "a cover mixes the outputs 0 and 1"));
            }
            rows.push((plane, v));
            continue;
        }
        names_open = false;
        let args = &ln.w[1..];
        if w0 == ".model" {
            if let Some(m) = cur.take() {
                rd.models.push(m);
            }
            exdc = false;
            any = true;
            if args.len() > 1 {
                return Err(rd.err(at, ".model takes one name"));
            }
            cur = Some(Mdl {
                name: args
                    .first()
                    .map_or_else(|| stem.to_string(), |s| s.to_string()),
                at,
                ins: Vec::new(),
                clocks: Vec::new(),
                outs: Vec::new(),
                body: Vec::new(),
                blackbox: false,
            });
            continue;
        }
        if [
            ".search",
            ".start_kiss",
            ".end_kiss",
            ".latch_order",
            ".code",
            ".conn",
        ]
        .contains(&w0)
        {
            return Err(rd.err(
                at,
                format!("{w0} is not supported: it changes the function"),
            ));
        }
        let Some(m) = cur.as_mut() else {
            return Err(rd.err(at, format!("{w0} outside a model")));
        };
        match w0 {
            ".end" => {
                exdc = false;
                if let Some(m) = cur.take() {
                    rd.models.push(m);
                }
            }
            ".inputs" => m.ins.extend(args),
            ".clock" => m.clocks.extend(args),
            ".outputs" => m.outs.extend(args),
            ".names" => {
                if args.is_empty() {
                    return Err(rd.err(at, ".names without an output"));
                }
                m.body.push(St::Names {
                    sig: args.to_vec(),
                    rows: Vec::new(),
                    at,
                });
                names_open = true;
            }
            ".latch" => m.body.push(St::Latch {
                w: args.to_vec(),
                at,
            }),
            ".subckt" => {
                let Some((model, rest)) = args.split_first() else {
                    return Err(rd.err(at, ".subckt without a model"));
                };
                let c = conns(rd, rest, at)?;
                m.body.push(St::Sub {
                    model,
                    conns: c,
                    at,
                });
            }
            ".gate" | ".mlatch" => {
                let Some((cell, rest)) = args.split_first() else {
                    return Err(rd.err(at, format!("{w0} without a cell")));
                };
                let k = rest.iter().take_while(|x| x.contains('=')).count();
                let c = conns(rd, &rest[..k], at)?;
                let tail = &rest[k..];
                let ml = match (w0, tail) {
                    (".gate", []) => None,
                    (".mlatch", [ctl]) => Some((*ctl, "")),
                    (".mlatch", [ctl, init]) => Some((*ctl, *init)),
                    _ => return Err(rd.err(at, format!("a malformed {w0}"))),
                };
                m.body.push(St::Cell {
                    cell,
                    conns: c,
                    ml,
                    at,
                });
            }
            ".exdc" => {
                exdc = true;
                rd.warnings.push(format!(
                    "warning[{SKIPPED}]: {}:{}:1: the external don't-care network is skipped; \
                     the function is kept exactly",
                    rd.files[file], ln.no
                ));
            }
            ".blackbox" => m.blackbox = true,
            c if SKIP.contains(&c) => match skipped.iter_mut().find(|(n, _, _)| *n == c) {
                Some((_, _, k)) => *k += 1,
                None => skipped.push((c, at, 1)),
            },
            c => return Err(rd.err(at, format!("unknown command {c}"))),
        }
    }
    if let Some(m) = cur.take() {
        rd.models.push(m);
    }
    if !any {
        return Err(format!("error[{SOURCE}]: {}:1:1: no model", rd.files[file]));
    }
    for (c, at, k) in skipped {
        rd.warnings.push(format!(
            "warning[{SKIPPED}]: {}:{}:1: {c} ({k}) has no effect on the function and is skipped",
            rd.files[at.0], at.1
        ));
    }
    Ok(())
}

/// What a variable is, beside a value that a statement defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Input,
    State,
}

/// One model instance being walked.
struct Frame<'t> {
    m: usize,
    k: usize,
    bind: HashMap<&'t str, u32>,
    path: String,
    subs: usize,
}

/// The builder of the flattened model.
struct Bd<'r, 't> {
    rd: &'r Rd<'t>,
    c: &'r mut Circuit,
    by_name: HashMap<&'r str, usize>,
    names: HashSet<String>,
    var_name: HashMap<u32, String>,
    role: HashMap<u32, Role>,
    reads: Vec<(u32, (usize, u32))>,
    clock: Option<u32>,
    init: Option<u32>,
    mode: InitialValues,
    states: Vec<RtlState>,
    declared: Vec<(String, bool)>,
    cells: HashMap<&'r str, Result<&'r Cell, String>>,
    unread: &'r [Unread],
    items: usize,
    warnings: Vec<String>,
}

impl<'t> Bd<'_, 't> {
    fn err(&self, at: (usize, u32), msg: impl std::fmt::Display) -> String {
        self.rd.err(at, msg)
    }

    /// The variable of local signal `name` of frame `fr`.
    fn var(&mut self, fr: &mut Frame<'t>, name: &'t str, at: (usize, u32)) -> Result<u32, String> {
        if let Some(&v) = fr.bind.get(name) {
            return Ok(v);
        }
        if name.chars().any(|c| c < ' ' || c == '\u{7f}') {
            return Err(self.err(at, format!("name {name:?} has a control character")));
        }
        let full = if fr.path.is_empty() {
            name.to_string()
        } else {
            format!("{}.{name}", fr.path)
        };
        if !self.names.insert(full.clone()) {
            return Err(self.err(at, format!("flattened name {full} collides with a signal")));
        }
        if self.names.len() > MAX_SIGNALS {
            return Err(self.err(at, format!("more than {MAX_SIGNALS} signals")));
        }
        let v = self.c.free();
        self.var_name.insert(v, full);
        fr.bind.insert(name, v);
        Ok(v)
    }

    /// Defines `v` by `f`: a second driver is an error.
    fn drive(&mut self, v: u32, f: B, at: (usize, u32)) -> Result<(), String> {
        if self.role.contains_key(&v) || !self.c.define(v, f) {
            return Err(self.err(at, format!("signal {} has two drivers", self.var_name[&v])));
        }
        Ok(())
    }

    fn clock(&mut self) -> u32 {
        *self.clock.get_or_insert_with(|| self.c.free())
    }

    /// The asynchronous initial value of a state that declares `init` (SYNTH-REQ-0906).
    fn initial(
        &mut self,
        init: &str,
        name: &str,
        at: (usize, u32),
    ) -> Result<Option<(B, bool)>, String> {
        let v = match init {
            "0" => false,
            "1" => true,
            "" | "2" | "3" => return Ok(None),
            x => return Err(self.err(at, format!("initial value '{x}'"))),
        };
        self.declared.push((name.to_string(), v));
        if self.mode == InitialValues::Arbitrary {
            return Ok(None);
        }
        let i = *self.init.get_or_insert_with(|| self.c.free());
        Ok(Some((B::V(i), v)))
    }

    fn statement(&mut self, fr: &mut Frame<'t>, st: &St<'t>) -> Result<Option<Frame<'t>>, String> {
        self.items += 1;
        if self.items > MAX_ITEMS {
            return Err(format!(
                "error[{SOURCE}]: the flattened model exceeds {MAX_ITEMS} statements"
            ));
        }
        match st {
            St::Names { sig, rows, at } => {
                let mut vars = Vec::with_capacity(sig.len());
                for &s in sig {
                    vars.push(self.var(fr, s, *at)?);
                }
                let out = vars.pop().expect("an output");
                let mut f = B::F;
                for (plane, _) in rows {
                    let mut t = B::T;
                    for (ch, &x) in plane.bytes().zip(&vars) {
                        t = match ch {
                            b'1' => self.c.and(t, B::V(x)),
                            b'0' => {
                                let n = self.c.not(B::V(x));
                                self.c.and(t, n)
                            }
                            _ => t,
                        };
                    }
                    f = self.c.or(f, t);
                }
                if rows.first().is_some_and(|r| !r.1) {
                    f = self.c.not(f);
                }
                self.reads.extend(vars.iter().map(|&v| (v, *at)));
                self.drive(out, f, *at)?;
                Ok(None)
            }
            St::Latch { w, at } => {
                let (kind, ctl, init) = match w.as_slice() {
                    [_, _] => ("", "NIL", ""),
                    [_, _, i] => ("", "NIL", *i),
                    [_, _, k, c] => (*k, *c, ""),
                    [_, _, k, c, i] => (*k, *c, *i),
                    _ => return Err(self.err(*at, "a malformed .latch")),
                };
                let d = self.var(fr, w[0], *at)?;
                let q = self.var(fr, w[1], *at)?;
                if self.role.contains_key(&q) || !self.c.is_free(q) {
                    return Err(self.err(*at, format!("signal {} has two drivers", w[1])));
                }
                self.role.insert(q, Role::State);
                self.reads.push((d, *at));
                let c = if ctl == "NIL" {
                    B::V(self.clock())
                } else {
                    let v = self.var(fr, ctl, *at)?;
                    self.reads.push((v, *at));
                    B::V(v)
                };
                let (latch, clk) = match kind {
                    "" | "re" => (false, c),
                    "fe" => (false, self.c.not(c)),
                    "ah" => (true, c),
                    "al" => (true, self.c.not(c)),
                    k => return Err(self.err(*at, format!("latch type '{k}'"))),
                };
                let name = self.var_name[&q].clone();
                let arst = self.initial(init, &name, *at)?;
                self.states.push(RtlState {
                    name,
                    latch,
                    q,
                    next: B::V(d),
                    clk,
                    arst,
                });
                Ok(None)
            }
            St::Sub { model, conns, at } => {
                let Some(&ci) = self.by_name.get(model) else {
                    return Err(self.err(*at, format!("unknown model {model}")));
                };
                let rd = self.rd;
                let cm = &rd.models[ci];
                if cm.blackbox {
                    return Err(self.err(*at, format!("model {model} is a black box")));
                }
                let inst = format!("{model}_{}", fr.subs);
                fr.subs += 1;
                let path = if fr.path.is_empty() {
                    inst
                } else {
                    format!("{}.{inst}", fr.path)
                };
                let mut bind = HashMap::new();
                for &(f, a) in conns {
                    if !cm
                        .ins
                        .iter()
                        .chain(&cm.clocks)
                        .chain(&cm.outs)
                        .any(|p| *p == f)
                    {
                        return Err(self.err(*at, format!("{f} is not a port of model {model}")));
                    }
                    let v = self.var(fr, a, *at)?;
                    bind.insert(f, v);
                }
                if let Some(p) = cm
                    .ins
                    .iter()
                    .chain(&cm.clocks)
                    .find(|p| !bind.contains_key(*p))
                {
                    return Err(
                        self.err(*at, format!("input {p} of model {model} is not connected"))
                    );
                }
                Ok(Some(Frame {
                    m: ci,
                    k: 0,
                    bind,
                    path,
                    subs: 0,
                }))
            }
            St::Cell {
                cell,
                conns,
                ml,
                at,
            } => {
                self.cell(fr, cell, conns, *ml, *at)?;
                Ok(None)
            }
        }
    }

    /// A `.gate` or `.mlatch` instance (CHECK-REQ-0104).
    fn cell(
        &mut self,
        fr: &mut Frame<'t>,
        name: &str,
        conns: &[(&'t str, &'t str)],
        ml: Option<(&'t str, &'t str)>,
        at: (usize, u32),
    ) -> Result<(), String> {
        let loc = format!("{}:{}", self.rd.files[at.0], at.1);
        let bad = |m: String| format!("error[{CELL}]: {loc}:1: cell {name}: {m}");
        let cell = match self.cells.get(name) {
            Some(Ok(c)) => *c,
            Some(Err(e)) => return Err(bad(e.clone())),
            None if self.cells.is_empty() => {
                return Err(bad(
                    "BLIF cells need the libraries of their cells (--gate-lib)".into(),
                ));
            }
            None => return Err(bad("not in the libraries".into())),
        };
        if let Some(why) = outside(cell, self.unread) {
            return Err(bad(why));
        }
        let seq = cell.sequentials.first();
        let mut pins: HashMap<&str, B> = HashMap::new();
        let mut outs: Vec<(&Func, &str, u32)> = Vec::new();
        for &(f, a) in conns {
            let Some(p) = cell.pin(f) else {
                return Err(bad(format!("no pin {f}")));
            };
            let v = self.var(fr, a, at)?;
            match p.direction {
                Direction::Input => {
                    self.reads.push((v, at));
                    pins.insert(p.name.as_str(), B::V(v));
                }
                Direction::Output => match (&p.function, &p.function_text) {
                    (Some(func), Some(t)) => outs.push((func, t.as_str(), v)),
                    _ => return Err(bad(format!("output pin {f} has no function"))),
                },
                _ => return Err(bad(format!("pin {f} is neither an input nor an output"))),
            }
        }
        let mut init = "";
        if let Some((ctl, i)) = ml {
            let Some(s) = seq else {
                return Err(bad(".mlatch of a cell without ff or latch group".into()));
            };
            let clock_pin = match &s.clock {
                Func::Var(p) => Some(p.as_str()),
                Func::Not(x) => match x.as_ref() {
                    Func::Var(p) => Some(p.as_str()),
                    _ => None,
                },
                _ => None,
            };
            let Some(cp) =
                clock_pin.filter(|p| cell.pin(p).is_some_and(|x| x.direction == Direction::Input))
            else {
                return Err(bad(
                    "the clock or enable of an .mlatch cell is not one input pin".into(),
                ));
            };
            if pins.contains_key(cp) {
                return Err(bad(format!(
                    ".mlatch connects the clock pin {cp} among its formals"
                )));
            }
            let c = if ctl == "NIL" {
                self.clock()
            } else {
                let v = self.var(fr, ctl, at)?;
                self.reads.push((v, at));
                v
            };
            pins.insert(cp, B::V(c));
            init = i;
        }
        if let Some(p) = cell.inputs().find(|p| !pins.contains_key(p.name.as_str())) {
            return Err(bad(format!("input pin {} is not connected", p.name)));
        }
        let q = seq.map(|_| self.c.free());
        let nq = q.map(|q| self.c.not(B::V(q)));
        let look = |n: &str| -> Option<B> {
            if let (Some(s), Some(q), Some(nq)) = (seq, q, nq) {
                if n == s.state {
                    return Some(B::V(q));
                }
                if n == s.state_n {
                    return Some(nq);
                }
            }
            pins.get(n).copied()
        };
        for (_, t, v) in &outs {
            let b = lfunc::build(self.c, t, &look).map_err(&bad)?;
            self.drive(*v, b, at)?;
        }
        if let (Some(s), Some(q)) = (seq, q) {
            let Some((_, _, v)) = outs
                .iter()
                .find(|(f, _, _)| matches!(f, Func::Var(x) if *x == s.state))
            else {
                return Err(bad(
                    "its state output is not connected, which names the state".into(),
                ));
            };
            let name = self.var_name[v].clone();
            let next = lfunc::build(self.c, &s.data_text, &look).map_err(&bad)?;
            let clk = lfunc::build(self.c, &s.clock_text, &look).map_err(&bad)?;
            let clear = match &s.clear_text {
                Some(t) => lfunc::build(self.c, t, &look).map_err(&bad)?,
                None => B::F,
            };
            let preset = match &s.preset_text {
                Some(t) => lfunc::build(self.c, t, &look).map_err(&bad)?,
                None => B::F,
            };
            let mut ctl: Vec<(B, bool)> = [(clear, false), (preset, true)]
                .into_iter()
                .filter(|(b, _)| *b != B::F)
                .collect();
            ctl.extend(self.initial(init, &name, at)?);
            if ctl.len() > 1 {
                return Err(bad("more than one asynchronous control".into()));
            }
            self.states.push(RtlState {
                name,
                latch: s.kind == SeqKind::Latch,
                q,
                next,
                clk,
                arst: ctl.first().copied(),
            });
        }
        Ok(())
    }
}

/// Why a cell is outside the model of BLIF cells (SYNTH-REQ-0911), if it is.
pub(crate) fn outside(cell: &Cell, unread: &[Unread]) -> Option<String> {
    if let Some(p) = cell.pins.iter().find(|p| p.three_state.is_some()) {
        return Some(format!("pin {} is three-state", p.name));
    }
    if cell.has_statetable || cell.has_bus || cell.sequentials.len() > 1 {
        return Some("a state table, bus pins or several ff and latch groups".into());
    }
    if cell.sequentials.iter().any(|s| s.clocked_on_also.is_some()) {
        return Some("clocked_on_also".into());
    }
    if let Some(u) = unread.iter().find(|u| u.cell == cell.name) {
        return Some(u.what.clone());
    }
    if !cell.problems.is_empty() {
        return Some(format!(
            "described with errors: {}",
            cell.problems.join("; ")
        ));
    }
    None
}

/// Builds the model of the BLIF sources into `c` (CHECK-REQ-0101).
pub(crate) fn build(
    c: &mut Circuit,
    input: &Input,
    libs: &[Library],
    unread: &[Unread],
    top: Option<&str>,
    mode: InitialValues,
) -> Result<SourceModel, String> {
    let data = bytes(input)?;
    let mut texts = Vec::with_capacity(data.len());
    for (n, b) in data {
        let t = String::from_utf8(b).map_err(|_| format!("error[{SOURCE}]: {n}: not UTF-8"))?;
        texts.push((n, t));
    }
    let mut rd = Rd {
        files: texts.iter().map(|(n, _)| n.clone()).collect(),
        models: Vec::new(),
        warnings: Vec::new(),
    };
    for (k, (n, t)) in texts.iter().enumerate() {
        let stem = Path::new(n)
            .file_stem()
            .map_or_else(|| n.clone(), |s| s.to_string_lossy().into_owned());
        parse(&mut rd, t, k, &stem)?;
    }
    let mut by_name = HashMap::new();
    for (k, m) in rd.models.iter().enumerate() {
        if by_name.insert(m.name.as_str(), k).is_some() {
            return Err(rd.err(m.at, format!("model {} is defined twice", m.name)));
        }
    }
    let ti = match top {
        Some(t) => *by_name
            .get(t)
            .ok_or_else(|| format!("error[{SOURCE}]: no BLIF model is named {t}"))?,
        None => 0,
    };
    if rd.models[ti].blackbox {
        return Err(rd.err(rd.models[ti].at, "the top model is a black box"));
    }
    let mut b = Bd {
        rd: &rd,
        c,
        by_name,
        names: HashSet::new(),
        var_name: HashMap::new(),
        role: HashMap::new(),
        reads: Vec::new(),
        clock: None,
        init: None,
        mode,
        states: Vec::new(),
        declared: Vec::new(),
        cells: crate::netlist::cell_table(libs),
        unread,
        items: 0,
        warnings: Vec::new(),
    };
    let tm = &rd.models[ti];
    let mut top_frame = Frame {
        m: ti,
        k: 0,
        bind: HashMap::new(),
        path: String::new(),
        subs: 0,
    };
    let mut ins = Vec::new();
    for p in tm.ins.iter().chain(&tm.clocks) {
        if top_frame.bind.contains_key(p) {
            return Err(b.err(tm.at, format!("input {p} is declared twice")));
        }
        let v = b.var(&mut top_frame, p, tm.at)?;
        b.role.insert(v, Role::Input);
        ins.push((p.to_string(), v));
    }
    let mut outs = Vec::new();
    for p in &tm.outs {
        if outs.iter().any(|(n, _)| n == p) {
            return Err(b.err(tm.at, format!("output {p} is declared twice")));
        }
        let v = b.var(&mut top_frame, p, tm.at)?;
        b.reads.push((v, tm.at));
        outs.push((p.to_string(), v));
    }
    // The walk of BLIF-REQ-0041 with an explicit stack; a model on the stack may not be entered
    // again.
    let mut stack = vec![top_frame];
    let mut on: Vec<usize> = vec![ti];
    while let Some(mut fr) = stack.pop() {
        let body = &rd.models[fr.m].body;
        let Some(st) = body.get(fr.k) else {
            on.pop();
            continue;
        };
        fr.k += 1;
        let child = b.statement(&mut fr, st)?;
        stack.push(fr);
        if let Some(ch) = child {
            if on.contains(&ch.m) {
                return Err(format!(
                    "error[{SOURCE}]: model {} instantiates itself",
                    rd.models[ch.m].name
                ));
            }
            on.push(ch.m);
            stack.push(ch);
        }
    }
    // A read signal without a driver reads as 0, with a warning (SYNTH-REQ-0908).
    let mut undriven: Vec<(u32, (usize, u32))> = Vec::new();
    for &(v, at) in &b.reads {
        if b.c.is_free(v) && !b.role.contains_key(&v) {
            b.c.define(v, B::F);
            undriven.push((v, at));
        }
    }
    if let Some(&(_, at)) = undriven.first() {
        let names: Vec<&str> = undriven
            .iter()
            .take(8)
            .map(|(v, _)| b.var_name[v].as_str())
            .collect();
        let w = format!(
            "warning[{UNDRIVEN}]: {}:{}:1: {} signals without a driver read as 0: {}",
            rd.files[at.0],
            at.1,
            undriven.len(),
            names.join(", ")
        );
        b.warnings.push(w);
    }
    finish(b, ins, outs, tm.name.clone())
}

/// The ports, generated ports, name checks and cycle check of a built model.
fn finish(
    b: Bd<'_, '_>,
    ins: Vec<(String, u32)>,
    outs: Vec<(String, u32)>,
    top: String,
) -> Result<SourceModel, String> {
    let outputs: Vec<(String, B)> = outs.into_iter().map(|(n, v)| (n, B::V(v))).collect();
    let model = assemble(b.c, top, ins, outputs, b.states, b.clock, b.init, &|v| {
        b.var_name.get(&v).cloned()
    })?;
    let mut warnings = b.rd.warnings.clone();
    warnings.extend(b.warnings);
    Ok(SourceModel {
        model,
        initial_values: b.declared,
        warnings,
    })
}

/// The model of a BLIF or AIGER source: inputs, then the generated clock and initial-value ports,
/// then outputs (SYNTH-REQ-0903, SYNTH-REQ-0904, SYNTH-REQ-0906); distinct printable port names;
/// state names that only a one-bit output of the state itself shares (SYNTH-REQ-0907); no
/// combinational cycle.
#[allow(clippy::too_many_arguments)]
pub(crate) fn assemble(
    c: &mut Circuit,
    top: String,
    ins: Vec<(String, u32)>,
    outs: Vec<(String, B)>,
    states: Vec<RtlState>,
    clock: Option<u32>,
    init: Option<u32>,
    name_of: &dyn Fn(u32) -> Option<String>,
) -> Result<RtlModel, String> {
    let taken: HashSet<&str> = ins
        .iter()
        .map(|(n, _)| n.as_str())
        .chain(outs.iter().map(|(n, _)| n.as_str()))
        .chain(states.iter().map(|s| s.name.as_str()))
        .collect();
    let clk_name = clock.map(|_| generated("clk", &|x| taken.contains(x)));
    let init_name = init.map(|_| {
        generated("init", &|x| {
            taken.contains(x) || clk_name.as_deref() == Some(x)
        })
    });
    let mut inputs: Vec<(String, Vec<u32>, bool)> =
        ins.into_iter().map(|(n, v)| (n, vec![v], true)).collect();
    for (n, v) in clk_name
        .into_iter()
        .zip(clock)
        .chain(init_name.into_iter().zip(init))
    {
        inputs.push((n, vec![v], true));
    }
    let mut seen = HashSet::new();
    for n in inputs.iter().map(|x| &x.0).chain(outs.iter().map(|x| &x.0)) {
        if n.is_empty() || n.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
            return Err(format!(
                "error[{SOURCE}]: port name {n:?} is not printable ASCII"
            ));
        }
        if !seen.insert(n.clone()) {
            return Err(format!("error[{SOURCE}]: two ports are named {n}"));
        }
    }
    // A variable through its aliases.
    let resolved = |b: B| {
        let mut b = b;
        for _ in 0..c.defs.len() {
            match b {
                B::V(v) => match c.defs[v as usize] {
                    crate::circ::Def::Alias(x) => b = x,
                    _ => break,
                },
                _ => break,
            }
        }
        b
    };
    let mut snames = HashSet::new();
    for s in &states {
        if !snames.insert(s.name.as_str()) {
            return Err(format!("error[{SOURCE}]: two states are named {}", s.name));
        }
        if inputs.iter().any(|(n, _, _)| *n == s.name)
            || outs
                .iter()
                .any(|(n, b)| *n == s.name && resolved(*b) != B::V(s.q))
        {
            return Err(format!(
                "error[{SOURCE}]: state {} has the name of a port that is not the state",
                s.name
            ));
        }
    }
    let roots: Vec<B> = outs
        .iter()
        .map(|(_, b)| *b)
        .chain(
            states
                .iter()
                .flat_map(|s| [s.next, s.clk].into_iter().chain(s.arst.map(|(r, _)| r))),
        )
        .collect();
    if let Err(cy) = c.order(&roots) {
        let who = name_of(cy.0).unwrap_or_else(|| format!("variable {}", cy.0));
        return Err(format!(
            "error[{SOURCE}]: a combinational cycle through {who}"
        ));
    }
    let n_in = inputs.len();
    let port_order = (0..n_in)
        .map(|i| (true, i))
        .chain((0..outs.len()).map(|o| (false, o)))
        .collect();
    Ok(RtlModel {
        top,
        inputs,
        outputs: outs.into_iter().map(|(n, b)| (n, vec![b], true)).collect(),
        port_order,
        states,
        observations: Vec::new(),
        warnings: Vec::new(),
    })
}
