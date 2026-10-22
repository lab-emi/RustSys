// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Structural netlists and the core netlist model: conversion between the writer model
//! ([`NlModule`], VERILOG-REQ-0300) and [`GateNetlist`], and a reader of flat structural modules.
//!
//! [`read_structural`] (VERILOG-REQ-0310) accepts one module with input, output and inout ports
//! (scalar or with a constant range, ANSI or non-ANSI style), `wire` declarations, cell
//! instances with named connections, and continuous assignments (VERILOG-REQ-0311). A connection
//! or assignment side is an identifier, a bit-select with a constant index, or the constant
//! `1'b0` or `1'b1`; an identifier of a vector stands for all its bits. Range bounds and bit
//! indices are integer literals, negated only when they are plain decimal numbers (a negated
//! based literal wraps at its width). A range has at most [`MAX_RANGE_WIDTH`] bits, and all
//! ranges together at most one bit per byte of the text (at least [`MAX_RANGE_WIDTH`]): the
//! reader makes a net per bit, and a short text must not make it exhaust memory
//! (VERILOG-REQ-0315). An undeclared identifier in a connection or on the left side of an
//! assignment declares an implicit scalar wire (IEEE 1800-2017 clause 6.10), unless
//! `` `default_nettype none`` is in effect. Under a `` `default_nettype`` other than `wire`, a
//! port needs the net type `wire` in its declaration (or, non-ANSI style, in a `wire`
//! declaration of its name), since it would take the default type otherwise (clause 23.2.2).
//! A non-ANSI port whose declaration names a net type is completely declared, and so is one
//! that a `wire` declaration repeats: another `wire` declaration of its name is an error
//! (clause 23.2.2.1; VERILOG-REQ-0312).
//!
//! Names (VERILOG-REQ-0318, 0319): the core netlist keeps port, net and instance names as
//! hierarchical names ([`rusteda_core::names`]). The reader stores the text of an identifier,
//! for an escaped identifier without its backslash and terminating white space, as one segment
//! ([`escape_segment`]: `\` becomes `\\` and `/` becomes `\/`), and so does
//! [`to_gate_netlist`] with the names of the writer model; module, cell and pin names are kept
//! verbatim. [`from_gate_netlist`] gives the writer the raw text back ([`unescape`]), which it
//! writes as an escaped identifier unless it is a simple identifier. So a flat netlist reads back
//! exactly, while the separators of a hierarchical name come back escaped: `u1/n2` is written
//! `\u1/n2 ` and read as `u1\/n2`. Escaped identifiers hold printable ASCII only (IEEE
//! 1364-2005 clause 3.7.1); the lexer reads any other character up to white space, and the core
//! netlist refuses the name.
//!
//! Everything else is rejected with a located diagnostic that names the construct, never
//! skipped (VERILOG-REQ-0313, 0316): other declarations and procedures, expressions
//! (parenthesized and min:typ:max ones too, which the parser reads as their operand and their
//! typical value, so the reader finds them in the tokens), concatenations, x/z literals, gate
//! primitives, instance arrays, positional or implicit connections, parameter values, delays and
//! drive strengths, a second module or an instance of the module itself (hierarchy), the
//! directives `` `include``, `` `unconnected_drive`` and `` `pragma``, and `` `default_nettype``
//! inside the module (clause 22.8 places it outside design elements). Attribute instances, which
//! the lexer skips (VERILOG-REQ-0002), are found by a scan of the text (VERILOG-REQ-0314),
//! conditional branches and macro bodies included, and read strictly: names (simple or escaped
//! identifiers) separated by commas, each without a value or with a string value. Only `src`
//! attributes, which name source locations, are accepted; another attribute, or an instance of
//! another form (a macro inside it, a missing comma), is rejected. Macros with arguments are
//! rejected too, since substituting an argument can assemble an attribute instance that the text
//! does not show, and so are macros that use macros and macro uses beyond the length of the text
//! (at least 1 MiB), since the preprocessor bounds only the nesting of expansion, not its size.
//!
//! Both directions number entities alike (VERILOG-REQ-0317): ports first (bits from msb to lsb),
//! then wires in declaration order, then implicit nets at their first use; cells in order of
//! first use; instances, connections and assignments in order. So [`to_gate_netlist`] of a
//! module and [`read_structural`] of its written text freeze to the same tables
//! (VERILOG-REQ-0301). The core netlist keeps the order of connections, so [`from_gate_netlist`]
//! gives back the connections of [`to_gate_netlist`] in their order, whatever the library's pin
//! order.
//!
//! The module uses only its parent module's reader and writer through `super::` paths, so that
//! it moves with them unchanged.

mod freeze_writer;
mod read;
mod scan;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_read;

use std::collections::{HashMap, HashSet};
use std::fmt;

use rusteda_core::names::{escape_segment, unescape};
use rusteda_core::netlist::{
    CellId, Conn, GateNetlist, InstId, MAX_RANGE_WIDTH, NetId, PinDir, PinRef, PortDir,
};

use super::diag::Diagnostic;
use super::span::{SourceMap, Span};
use super::write::{NlDir, NlInstance, NlModule, NlPort, NlRef};

pub use freeze_writer::{FrozenBuildLimits, to_frozen_netlist};

pub use read::read_structural;

/// A conversion that the target model cannot represent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertError {
    pub message: String,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConvertError {}

fn convert_error(message: impl Into<String>) -> ConvertError {
    ConvertError {
        message: message.into(),
    }
}

/// The rejection of [`read_structural`]: the diagnostics and the source text they locate.
pub struct ReadError {
    pub diagnostics: Vec<Diagnostic>,
    pub sources: SourceMap,
}

impl fmt::Display for ReadError {
    /// One rendered diagnostic per line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (k, d) in self.diagnostics.iter().enumerate() {
            if k > 0 {
                f.write_str("\n")?;
            }
            f.write_str(&d.render(&self.sources))?;
        }
        Ok(())
    }
}

/// The rendered diagnostics, without the source text.
impl fmt::Debug for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered: Vec<String> = self
            .diagnostics
            .iter()
            .map(|d| d.render(&self.sources))
            .collect();
        f.debug_struct("ReadError")
            .field("diagnostics", &rendered)
            .finish_non_exhaustive()
    }
}

impl std::error::Error for ReadError {}

/// Number of bits of the range `[msb:lsb]`.
fn range_width(msb: i32, lsb: i32) -> u64 {
    (i64::from(msb) - i64::from(lsb)).unsigned_abs() + 1
}

/// Bit indices of a range in declaration order, from `msb` to `lsb`.
fn range_bits(msb: i32, lsb: i32) -> Vec<i32> {
    if msb >= lsb {
        (lsb..=msb).rev().collect()
    } else {
        (msb..=lsb).collect()
    }
}

/// How a module-scope name was declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetDecl {
    /// A port; `typed`: a net or variable type is given for it, which completes its declaration
    /// (IEEE 1800-2017 clause 23.2.2.1), so no further net declaration may repeat it.
    Port {
        typed: bool,
    },
    Wire,
    Implicit,
}

/// A name of the module scope: nets and instances share it.
#[derive(Debug, Clone)]
enum Decl {
    /// Bits in declaration order (msb to lsb); `range == None` for a scalar.
    Net {
        range: Option<(i32, i32)>,
        bits: Vec<NetId>,
        kind: NetDecl,
        at: Span,
    },
    Instance {
        at: Span,
    },
}

/// Builds a [`GateNetlist`] under the declaration rules shared by both inputs, so that a module
/// and its written text give the same netlist. It takes names as raw text, as the writer model
/// and the identifiers of a text hold them, and stores port, net and instance names escaped
/// ([`escape_segment`], VERILOG-REQ-0318).
struct Builder<'c, 's> {
    g: GateNetlist,
    /// Declarations by raw name.
    names: HashMap<String, Decl>,
    cells: HashMap<String, Result<CellId, String>>,
    cell_pins: &'c dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
    /// `` `default_nettype`` other than `wire`.
    nettype: Option<String>,
    /// Locates earlier declarations in messages (reader only).
    sources: Option<&'s SourceMap>,
}

impl<'c, 's> Builder<'c, 's> {
    fn new(
        module: &str,
        cell_pins: &'c dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
        nettype: Option<String>,
        sources: Option<&'s SourceMap>,
    ) -> Result<Builder<'c, 's>, String> {
        Ok(Builder {
            g: GateNetlist::new(module).map_err(|e| e.to_string())?,
            names: HashMap::new(),
            cells: HashMap::new(),
            cell_pins,
            nettype,
            sources,
        })
    }

    /// ` at <file:line:col>` of a declaration, for the reader.
    fn at(&self, d: &Decl) -> String {
        let span = match d {
            Decl::Net { at, .. } | Decl::Instance { at } => *at,
        };
        match self.sources {
            Some(sm) => format!(" at {}", sm.describe(span)),
            None => String::new(),
        }
    }

    fn redeclared(&self, name: &str, d: &Decl) -> String {
        let what = match d {
            Decl::Net {
                kind: NetDecl::Port { .. },
                ..
            } => "a port",
            Decl::Net {
                kind: NetDecl::Wire,
                ..
            } => "a wire",
            Decl::Net {
                kind: NetDecl::Implicit,
                ..
            } => "an implicit net",
            Decl::Instance { .. } => "an instance",
        };
        format!("'{name}' is already declared as {what}{}", self.at(d))
    }

    /// Declares a port; `typed`: its declaration gives it a net or variable type.
    fn port(
        &mut self,
        name: &str,
        dir: PortDir,
        range: Option<(i32, i32)>,
        typed: bool,
        at: Span,
    ) -> Result<(), String> {
        if let Some(d) = self.names.get(name) {
            return Err(self.redeclared(name, d));
        }
        let k = self
            .g
            .add_port(&escape_segment(name), dir, range)
            .map_err(|e| e.to_string())?;
        let bits = self.g.ports()[k].bits.clone();
        let kind = NetDecl::Port { typed };
        self.names.insert(
            name.to_string(),
            Decl::Net {
                range,
                bits,
                kind,
                at,
            },
        );
        Ok(())
    }

    /// Declares a wire. With `port_redeclaration`, a wire may repeat a port of the same range
    /// (non-ANSI style) that has no net or variable type yet; it gives the port its net type
    /// and declares nothing new.
    fn wire(
        &mut self,
        name: &str,
        range: Option<(i32, i32)>,
        at: Span,
        port_redeclaration: bool,
    ) -> Result<(), String> {
        if port_redeclaration
            && let Some(Decl::Net {
                kind: NetDecl::Port { typed },
                range: r,
                ..
            }) = self.names.get_mut(name)
        {
            if *typed {
                return Err(format!(
                    "port '{name}' already has a net or variable type, which completes its \
                     declaration, so it cannot be declared again as a wire (IEEE 1800-2017 \
                     clause 23.2.2.1)"
                ));
            }
            if *r != range {
                return Err(format!(
                    "wire '{name}' does not repeat the range of port '{name}'"
                ));
            }
            *typed = true;
            return Ok(());
        }
        match self.names.get(name) {
            None => {}
            Some(
                d @ Decl::Net {
                    kind: NetDecl::Implicit,
                    ..
                },
            ) => {
                return Err(format!(
                    "'{name}' is declared after its implicit declaration{}",
                    self.at(d)
                ));
            }
            Some(d) => return Err(self.redeclared(name, d)),
        }
        let indices = match range {
            None => vec![None],
            Some((msb, lsb)) if range_width(msb, lsb) > u64::from(MAX_RANGE_WIDTH) => {
                return Err(format!(
                    "range [{msb}:{lsb}] of '{name}' is wider than {MAX_RANGE_WIDTH} bits"
                ));
            }
            Some((msb, lsb)) => range_bits(msb, lsb).into_iter().map(Some).collect(),
        };
        let stored = escape_segment(name);
        let mut bits = Vec::with_capacity(indices.len());
        for k in indices {
            bits.push(
                self.g
                    .add_net(Some(&stored), k)
                    .map_err(|e| e.to_string())?,
            );
        }
        let kind = NetDecl::Wire;
        self.names.insert(
            name.to_string(),
            Decl::Net {
                range,
                bits,
                kind,
                at,
            },
        );
        Ok(())
    }

    /// The cell type of a library cell, added at its first use.
    fn cell(&mut self, name: &str) -> Result<CellId, String> {
        if let Some(r) = self.cells.get(name) {
            return r.clone();
        }
        let r = match (self.cell_pins)(name) {
            None => Err(format!("unknown cell '{name}'")),
            Some(pins) => self.g.add_cell_type(name, &pins).map_err(|e| e.to_string()),
        };
        self.cells.insert(name.to_string(), r.clone());
        r
    }

    fn instance(&mut self, name: &str, cell: CellId, at: Span) -> Result<InstId, String> {
        if let Some(d) = self.names.get(name) {
            return Err(self.redeclared(name, d));
        }
        let id = self
            .g
            .add_instance(&escape_segment(name), cell)
            .map_err(|e| e.to_string())?;
        self.names.insert(name.to_string(), Decl::Instance { at });
        Ok(id)
    }

    /// Index of a pin of a cell, by name.
    fn pin_index(&self, cell: CellId, pin: &str) -> Option<u32> {
        let c = self.g.cell(cell).ok()?;
        let s = self.g.symbols().get(pin)?;
        c.pins.iter().position(|p| p.name == s).map(|k| k as u32)
    }

    /// All bits of a name. An undeclared name becomes an implicit scalar wire when `implicit`.
    fn whole(&mut self, name: &str, implicit: bool, at: Span) -> Result<Vec<Conn>, String> {
        match self.names.get(name) {
            Some(Decl::Net { bits, .. }) => {
                return Ok(bits.iter().map(|&b| Conn::Net(b)).collect());
            }
            Some(Decl::Instance { .. }) => {
                return Err(format!("'{name}' is an instance, not a net"));
            }
            None if !implicit => return Err(format!("'{name}' is not declared")),
            None => {}
        }
        match self.nettype.as_deref() {
            None | Some("wire") => {}
            Some("none") => {
                return Err(format!(
                    "'{name}' is not declared, and `default_nettype none forbids implicit nets"
                ));
            }
            Some(t) => {
                return Err(format!(
                    "implicit net '{name}' would have net type '{t}'; only wire nets are supported"
                ));
            }
        }
        let net = self
            .g
            .add_net(Some(&escape_segment(name)), None)
            .map_err(|e| e.to_string())?;
        let kind = NetDecl::Implicit;
        let decl = Decl::Net {
            range: None,
            bits: vec![net],
            kind,
            at,
        };
        self.names.insert(name.to_string(), decl);
        Ok(vec![Conn::Net(net)])
    }

    /// Bit `k` of a vector.
    fn bit(&self, name: &str, k: i64) -> Result<Conn, String> {
        match self.names.get(name) {
            Some(Decl::Net {
                range: Some((msb, lsb)),
                bits,
                ..
            }) => {
                let (msb, lsb) = (i64::from(*msb), i64::from(*lsb));
                let pos = if msb >= lsb { msb - k } else { k - msb };
                if (msb.min(lsb)..=msb.max(lsb)).contains(&k) {
                    Ok(Conn::Net(bits[pos as usize]))
                } else {
                    Err(format!(
                        "bit {k} is outside the range [{msb}:{lsb}] of '{name}'"
                    ))
                }
            }
            Some(Decl::Net { range: None, .. }) => {
                Err(format!("'{name}' is a scalar and has no bit {k}"))
            }
            Some(Decl::Instance { .. }) => Err(format!("'{name}' is an instance, not a net")),
            None => Err(format!("'{name}' is not declared")),
        }
    }
}

fn port_dir(d: NlDir) -> PortDir {
    match d {
        NlDir::Input => PortDir::Input,
        NlDir::Output => PortDir::Output,
    }
}

/// `[w-1:0]` of a writer width, at most [`MAX_RANGE_WIDTH`].
fn writer_range(name: &str, width: Option<u32>) -> Result<Option<(i32, i32)>, ConvertError> {
    match width {
        None => Ok(None),
        Some(0) => Err(convert_error(format!("'{name}' has width 0"))),
        Some(w) if w > MAX_RANGE_WIDTH => Err(convert_error(format!(
            "'{name}' has width {w}, more than the {MAX_RANGE_WIDTH} bits of a range"
        ))),
        Some(w) => Ok(Some((w as i32 - 1, 0))),
    }
}

/// Converts the writer model to the core netlist (VERILOG-REQ-0317). `cell_pins` gives the pins
/// of a library cell in library order, or `None` for an unknown cell. Ports come first, then the
/// wires, then the implicit nets of connections and assignment left sides; a reference to a
/// whole vector stands for all its bits; connections keep their order. Port, net and instance
/// names are raw text, stored escaped (VERILOG-REQ-0319). Unknown cells or pins, a cell with
/// the module's name (the written instance would instantiate the module), pins connected twice,
/// bits outside a range, ranges wider than [`MAX_RANGE_WIDTH`], connections whose widths differ
/// and names that the core netlist refuses (outside printable ASCII) are errors.
pub fn to_gate_netlist(
    m: &NlModule,
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
) -> Result<GateNetlist, ConvertError> {
    let mut b = Builder::new(&m.name, cell_pins, None, None).map_err(convert_error)?;
    let at = Span::default();
    for p in &m.ports {
        let range = writer_range(&p.name, p.width)?;
        b.port(&p.name, port_dir(p.dir), range, true, at)
            .map_err(convert_error)?;
    }
    for (w, width) in &m.wires {
        let range = writer_range(w, *width)?;
        b.wire(w, range, at, false).map_err(convert_error)?;
    }
    for inst in &m.instances {
        let ctx = |e: String| convert_error(format!("instance '{}': {e}", inst.name));
        if inst.cell == m.name {
            return Err(ctx(format!(
                "cell '{}' has the name of the module, so the written instance would instantiate \
                 the module itself",
                inst.cell
            )));
        }
        let cell = b.cell(&inst.cell).map_err(ctx)?;
        let id = b.instance(&inst.name, cell, at).map_err(ctx)?;
        let mut connected = HashSet::new();
        for (pin, r) in &inst.conns {
            let k = b
                .pin_index(cell, pin)
                .ok_or_else(|| ctx(format!("cell '{}' has no pin '{pin}'", inst.cell)))?;
            if !connected.insert(k) {
                return Err(ctx(format!("pin '{pin}' is connected twice")));
            }
            let bits = writer_bits(&mut b, r, true).map_err(ctx)?;
            let [c] = bits[..] else {
                let n = bit_count(bits.len());
                return Err(ctx(format!("pin '{pin}' is connected to {n}")));
            };
            b.g.connect(id, PinRef::Index(k), c)
                .map_err(|e| ctx(e.to_string()))?;
        }
    }
    for (l, r) in &m.assigns {
        let ctx = |e: String| convert_error(format!("assignment to {}: {e}", ref_text(l)));
        let lhs = writer_bits(&mut b, l, true).map_err(ctx)?;
        let rhs = writer_bits(&mut b, r, false).map_err(ctx)?;
        assign_bits(&mut b.g, &lhs, &rhs).map_err(ctx)?;
    }
    Ok(b.g)
}

/// A reference as the writer writes it, for messages.
fn ref_text(r: &NlRef) -> String {
    match r {
        NlRef::Net(n) => n.clone(),
        NlRef::Bit(n, k) => format!("{n}[{k}]"),
        NlRef::Const(v) => format!("1'b{}", u8::from(*v)),
    }
}

fn writer_bits(b: &mut Builder<'_, '_>, r: &NlRef, implicit: bool) -> Result<Vec<Conn>, String> {
    match r {
        NlRef::Net(n) => b.whole(n, implicit, Span::default()),
        NlRef::Bit(n, k) => Ok(vec![b.bit(n, i64::from(*k))?]),
        NlRef::Const(v) => Ok(vec![Conn::Const(*v)]),
    }
}

/// `1 bit` or `<n> bits`.
fn bit_count(n: usize) -> String {
    if n == 1 {
        "1 bit".into()
    } else {
        format!("{n} bits")
    }
}

/// Adds `lhs = rhs` bit by bit, in declaration order.
fn assign_bits(g: &mut GateNetlist, lhs: &[Conn], rhs: &[Conn]) -> Result<(), String> {
    if lhs.len() != rhs.len() {
        return Err(format!(
            "the right side has {}, the left side {}",
            bit_count(rhs.len()),
            bit_count(lhs.len())
        ));
    }
    for (l, r) in lhs.iter().zip(rhs) {
        let Conn::Net(n) = *l else {
            return Err("a constant cannot be assigned to".into());
        };
        g.add_assign(n, *r).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The names of the module scope, which nets, ports and instances share, as the writer writes
/// them: each raw name with the kind and stored name that took it. Distinct stored names can
/// have the same raw text, `u1/n2` and `u1\/n2`.
#[derive(Default)]
struct Scope<'g> {
    taken: HashMap<String, (&'static str, &'g str)>,
}

impl<'g> Scope<'g> {
    /// The raw name of a stored port, net or instance name, unless another one takes it.
    fn declare(&mut self, kind: &'static str, stored: &'g str) -> Result<String, ConvertError> {
        let raw = unescape(stored);
        match self.taken.get(&raw) {
            None => {
                self.taken.insert(raw.clone(), (kind, stored));
                Ok(raw)
            }
            Some(&(k, s)) if s == stored => Err(convert_error(format!(
                "{kind} '{stored}' has the name of a {k}; Verilog needs distinct names"
            ))),
            Some(&(k, s)) => Err(convert_error(format!(
                "{kind} '{stored}' and {k} '{s}' are both written as '{raw}'; Verilog needs \
                 distinct names"
            ))),
        }
    }
}

/// Converts the core netlist to the writer model (VERILOG-REQ-0317). Ports are scalar or
/// `[w-1:0]` buses of input or output direction; the other nets become wires, grouped by name in
/// order of first appearance: one scalar net, or bits that form `[w-1:0]`. Connections keep
/// their order. Port, net and instance names become their raw text, which the writer escapes as
/// needed; module, cell and pin names are taken as they are (VERILOG-REQ-0319). The core netlist
/// holds printable ASCII names only, so every name can be written. Anonymous nets, other ranges,
/// inout ports, two names of the module scope with the same raw text (an instance named like a
/// net, or `u1/n2` and `u1\/n2`), and instances of a cell named like the module (Verilog would
/// read them as instances of the module itself) are errors.
pub fn from_gate_netlist(g: &GateNetlist) -> Result<NlModule, ConvertError> {
    let mut scope = Scope::default();
    let mut refs: HashMap<NetId, NlRef> = HashMap::new();
    let mut ports = Vec::new();
    for p in g.ports() {
        let stored = g.resolve(p.name);
        let name = scope.declare("port", stored)?;
        let dir = match p.dir {
            PortDir::Input => NlDir::Input,
            PortDir::Output => NlDir::Output,
            PortDir::Inout => {
                return Err(convert_error(format!(
                    "inout port '{stored}' cannot be written: the writer has input and output \
                     ports only"
                )));
            }
        };
        let width = match p.range {
            None => None,
            Some((msb, 0)) if msb >= 0 => Some(msb as u32 + 1),
            Some((msb, lsb)) => {
                return Err(convert_error(format!(
                    "port '{stored}' has range [{msb}:{lsb}]; the writer declares buses as \
                     [w-1:0]"
                )));
            }
        };
        for &bit in &p.bits {
            let r = match g.net(bit).map_err(|e| convert_error(e.to_string()))?.index {
                None => NlRef::Net(name.clone()),
                Some(k) => NlRef::Bit(name.clone(), k as u32),
            };
            refs.insert(bit, r);
        }
        ports.push(NlPort { name, dir, width });
    }
    let mut groups: Vec<(&str, Vec<(NetId, Option<i32>)>)> = Vec::new();
    let mut group_of = HashMap::new();
    for (id, n) in g.nets() {
        if refs.contains_key(&id) {
            continue;
        }
        let Some(s) = n.name else {
            return Err(convert_error(format!(
                "net #{} has no name; the writer needs a name for every net",
                id.index()
            )));
        };
        let k = *group_of.entry(s).or_insert_with(|| {
            groups.push((g.resolve(s), Vec::new()));
            groups.len() - 1
        });
        groups[k].1.push((id, n.index));
    }
    let mut wires = Vec::new();
    for (stored, bits) in groups {
        let name = scope.declare("net", stored)?;
        if let [(id, None)] = bits[..] {
            refs.insert(id, NlRef::Net(name.clone()));
            wires.push((name, None));
            continue;
        }
        // Indices are distinct, so they form [w-1:0] when all lie below the bit count.
        let w = bits.len();
        for &(id, index) in &bits {
            match index {
                Some(k) if (k as usize) < w && k >= 0 => {
                    refs.insert(id, NlRef::Bit(name.clone(), k as u32));
                }
                _ => {
                    return Err(convert_error(format!(
                        "the {w} bits of net '{stored}' do not form the range [{}:0]",
                        w - 1
                    )));
                }
            }
        }
        wires.push((name, Some(w as u32)));
    }
    let reference = |c: Conn| -> Result<NlRef, ConvertError> {
        match c {
            Conn::Const(v) => Ok(NlRef::Const(v)),
            Conn::Net(n) => refs
                .get(&n)
                .cloned()
                .ok_or_else(|| convert_error(format!("net #{} is missing", n.index()))),
        }
    };
    let mut instances = Vec::new();
    for (_, inst) in g.instances() {
        let stored = g.resolve(inst.name);
        let name = scope.declare("instance", stored)?;
        let cell = g
            .cell(inst.cell)
            .map_err(|e| convert_error(e.to_string()))?;
        let cell_name = g.resolve(cell.name);
        if cell_name == g.name() {
            return Err(convert_error(format!(
                "instance '{stored}' has the module's name '{cell_name}' as its cell; Verilog \
                 would read it as an instance of the module itself"
            )));
        }
        let mut conns = Vec::new();
        for &p in &inst.connected {
            let pin = g.pin(p).map_err(|e| convert_error(e.to_string()))?;
            if let Some(c) = pin.conn {
                let pin_name = g.resolve(cell.pins[pin.cell_pin as usize].name);
                conns.push((pin_name.to_string(), reference(c)?));
            }
        }
        instances.push(NlInstance {
            cell: cell_name.to_string(),
            name,
            conns,
        });
    }
    let mut assigns = Vec::new();
    for a in g.assigns() {
        assigns.push((reference(Conn::Net(a.lhs))?, reference(a.rhs)?));
    }
    Ok(NlModule {
        name: g.name().to_string(),
        ports,
        wires,
        instances,
        assigns,
    })
}
