// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The structural reader: the walk over the syntax tree of one module, after the scans of
//! [`super::scan`] for what the lexer and the parser skip.

use std::collections::{HashMap, HashSet};
use std::fmt;

use rusteda_core::netlist::{Conn, GateNetlist, MAX_RANGE_WIDTH, PinDir, PinRef, PortDir};

use super::super::ast::{
    AnsiPort, ContAssign, DataDecl, DataType, Dim, Direction, Expr, ExprKind, GateKind,
    Instantiation, Item, Module, ModuleItem, NetKind, NonAnsiPort, PortConn, PortList, TypeKind,
    UnOp, UnitKind,
};
use super::super::diag::Diagnostic;
use super::super::number::{Bit4, Literal};
use super::super::parse_tokens;
use super::super::pp::{PpOptions, Preprocessor};
use super::super::span::{SourceMap, Span};
use super::super::token::Token;
use super::scan::{ansi_net_types, scan_text, scan_tokens};
use super::{Builder, Decl, NetDecl, ReadError, assign_bits, bit_count, range_width};

/// A construct outside the structural subset.
pub(super) const UNSUPPORTED: &str = "V0300";
/// A declaration problem: redeclared, undeclared or implicit names.
const DECLARATION: &str = "V0301";
/// An unknown cell or pin, or a pin connected twice.
const CELL: &str = "V0302";
/// A bit outside a range, or connected widths that differ.
const WIDTH: &str = "V0303";

/// Reads one flat structural module (see the module documentation) from `text`, located as
/// `file` in diagnostics. `cell_pins` gives the pins of a library cell in library order, or
/// `None` for an unknown cell. The text is read alone: `` `include`` is rejected, so no file
/// is opened.
pub fn read_structural(
    text: &str,
    file: &str,
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
) -> Result<GateNetlist, ReadError> {
    let budget = (text.len() as u64).max(u64::from(MAX_RANGE_WIDTH));
    read_with_budget(text, file, cell_pins, budget)
}

/// [`read_structural`] with `budget` bits for all ranges together: a net is made per bit, so the
/// budget, one bit per byte of text and at least [`MAX_RANGE_WIDTH`], keeps the memory of a
/// netlist in proportion to its text.
pub(super) fn read_with_budget(
    text: &str,
    file: &str,
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
    budget: u64,
) -> Result<GateNetlist, ReadError> {
    let mut sources = SourceMap::new();
    let id = sources.add(file, text);
    let mut diagnostics = scan_text(text, id);
    if diagnostics.is_empty() {
        let mut pp = Preprocessor::new(PpOptions::default());
        let toks = pp.run(&mut sources, id);
        if pp.diags.iter().any(Diagnostic::is_error) {
            diagnostics = pp.diags;
        } else {
            diagnostics = scan_tokens(&toks);
            if diagnostics.is_empty() {
                match parse_tokens(&toks) {
                    Err(d) => diagnostics.push(d),
                    Ok(unit) => match read_unit(&sources, &toks, &unit.items, cell_pins, budget) {
                        Ok(g) => return Ok(g),
                        Err(d) => diagnostics = d,
                    },
                }
            }
        }
    }
    Err(ReadError {
        diagnostics,
        sources,
    })
}

fn unsupported(span: Span, what: impl fmt::Display) -> Diagnostic {
    Diagnostic::error(
        UNSUPPORTED,
        span,
        format!("{what} is not supported in a structural netlist"),
    )
}

fn net_kind_name(k: NetKind) -> &'static str {
    match k {
        NetKind::Wire => "wire",
        NetKind::Tri => "tri",
        NetKind::Uwire => "uwire",
        NetKind::Wand => "wand",
        NetKind::Wor => "wor",
        NetKind::Triand => "triand",
        NetKind::Trior => "trior",
        NetKind::Tri0 => "tri0",
        NetKind::Tri1 => "tri1",
        NetKind::Trireg => "trireg",
        NetKind::Supply0 => "supply0",
        NetKind::Supply1 => "supply1",
        NetKind::Interconnect => "interconnect",
    }
}

fn gate_name(k: GateKind) -> &'static str {
    match k {
        GateKind::And => "and",
        GateKind::Nand => "nand",
        GateKind::Or => "or",
        GateKind::Nor => "nor",
        GateKind::Xor => "xor",
        GateKind::Xnor => "xnor",
        GateKind::Not => "not",
        GateKind::Buf => "buf",
    }
}

/// Location and name, with its article, of a module item that the reader does not accept.
fn item_kind(it: &ModuleItem) -> (Span, String) {
    fn first<T>(v: &[T], span: impl Fn(&T) -> Span) -> Span {
        v.first().map_or(Span::default(), span)
    }
    let (span, what): (Span, String) = match it {
        ModuleItem::PortDecl(d) => (d.span, "port declaration".into()),
        ModuleItem::Data(d) => (d.span, "declaration".into()),
        ModuleItem::Param(v) => (first(v, |p| p.span), "parameter".into()),
        ModuleItem::Typedef(t) => (t.span, "typedef".into()),
        ModuleItem::Import(v) => (first(v, |p| p.span), "package import".into()),
        ModuleItem::Assign(v) => (first(v, |a| a.span), "continuous assignment".into()),
        ModuleItem::Always(a) => (a.span, "always procedure".into()),
        ModuleItem::Initial(s) => (s.span, "initial procedure".into()),
        ModuleItem::Final(s) => (s.span, "final procedure".into()),
        ModuleItem::Instance(i) => (i.span, "instance".into()),
        ModuleItem::Gate(v) => {
            let kind = v.first().map_or("", |g| gate_name(g.kind));
            (first(v, |g| g.span), format!("gate primitive '{kind}'"))
        }
        ModuleItem::Genvar(v) => (first(v, |g| g.span), "genvar declaration".into()),
        ModuleItem::GenFor(g) => (g.span, "generate loop".into()),
        ModuleItem::GenIf(g) => (g.span, "generate if".into()),
        ModuleItem::GenCase(g) => (g.span, "generate case".into()),
        ModuleItem::GenBlock(g) => (g.span, "generate block".into()),
        ModuleItem::GenRegion(_, s) => (*s, "generate region".into()),
        ModuleItem::Function(f) => (f.span, "function".into()),
        ModuleItem::Task(t) => (t.span, "task".into()),
        ModuleItem::ElabTask { name, span, .. } => (*span, format!("elaboration task '{name}'")),
        ModuleItem::Defparam(s) => (*s, "defparam".into()),
        ModuleItem::Modport(v) => (first(v, |m| m.span), "modport".into()),
        ModuleItem::Ignored { what, span } => (*span, what.to_string()),
        ModuleItem::Unsupported { what, span } => (*span, what.clone()),
    };
    let article = if what.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    (span, format!("{article} {what}"))
}

/// Value of an integer literal without x/z bits, if it fits 62 bits; based signed literals
/// are refused (their value depends on the width).
fn literal_value(l: &Literal) -> Option<i64> {
    if l.signed && !l.plain {
        return None;
    }
    let bits = l.bits().ok()?;
    let mut v = 0i64;
    for (k, b) in bits.iter().enumerate() {
        match b {
            Bit4::Zero => {}
            Bit4::One if k < 62 => v |= 1 << k,
            _ => return None,
        }
    }
    Some(v)
}

/// The walk over the syntax tree of one module.
struct Reader<'c, 's> {
    b: Builder<'c, 's>,
    sources: &'s SourceMap,
    diags: Vec<Diagnostic>,
    /// For each ANSI port, whether a net type is written for it (see [`ansi_net_types`]).
    ansi_net_types: Vec<bool>,
    /// The bits that ranges may still declare ([`read_with_budget`]).
    bits_left: u64,
    budget: u64,
}

fn read_unit(
    sources: &SourceMap,
    toks: &[Token],
    items: &[Item],
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
    budget: u64,
) -> Result<GateNetlist, Vec<Diagnostic>> {
    let mut diags = Vec::new();
    let mut module: Option<&Module> = None;
    for item in items {
        match item {
            Item::Module(m) if module.is_none() => module = Some(m),
            Item::Module(m) => diags.push(unsupported(
                m.name.span,
                format!(
                    "a second module '{}' (hierarchy; a structural netlist is one flat module)",
                    m.name.name
                ),
            )),
            Item::Package(p) => diags.push(unsupported(p.span, "a package")),
            Item::Decl(it) => {
                let (span, what) = item_kind(it);
                diags.push(unsupported(span, format!("{what} outside the module")));
            }
            Item::Unsupported { what, span } => diags.push(unsupported(*span, what)),
        }
    }
    let Some(m) = module else {
        diags.push(Diagnostic::error(
            DECLARATION,
            Span::default(),
            "no module found",
        ));
        return Err(diags);
    };
    let builder = Builder::new(
        &m.name.name,
        cell_pins,
        m.default_nettype.clone(),
        Some(sources),
    );
    let b = builder.map_err(|e| vec![Diagnostic::error(DECLARATION, m.name.span, e)])?;
    let mut r = Reader {
        b,
        sources,
        diags,
        ansi_net_types: ansi_net_types(toks),
        bits_left: budget,
        budget,
    };
    r.module(m);
    if r.diags.is_empty() {
        Ok(r.b.g)
    } else {
        Err(r.diags)
    }
}

impl Reader<'_, '_> {
    fn error(&mut self, code: &'static str, span: Span, message: impl Into<String>) {
        self.diags.push(Diagnostic::error(code, span, message));
    }

    /// The source text of a span, shortened, for messages.
    fn quote(&self, span: Span) -> String {
        let s = self.sources.snippet(span).trim();
        match s.char_indices().nth(40) {
            Some((k, _)) => format!("'{}...'", &s[..k]),
            None => format!("'{s}'"),
        }
    }

    fn module(&mut self, m: &Module) {
        match m.kind {
            UnitKind::Module => {}
            UnitKind::Interface => self.diags.push(unsupported(m.span, "an interface")),
            UnitKind::Program => self.diags.push(unsupported(m.span, "a program")),
        }
        if let Some(i) = m.imports.first() {
            self.diags.push(unsupported(i.span, "a package import"));
        }
        if let Some(p) = m.params.first() {
            self.diags.push(unsupported(p.span, "a parameter"));
        }
        let non_ansi = match &m.ports {
            PortList::None => false,
            PortList::Ansi(ports) => {
                for (k, p) in ports.iter().enumerate() {
                    // Without its tokens, a port is taken to have no net type.
                    let named = self.ansi_net_types.get(k).copied().unwrap_or(false);
                    self.ansi_port(p, named);
                }
                false
            }
            PortList::NonAnsi(ports) => {
                self.non_ansi_ports(m, ports);
                true
            }
        };
        for item in &m.items {
            match item {
                ModuleItem::PortDecl(d) if !non_ansi => self.diags.push(unsupported(
                    d.span,
                    "a port declaration in a module with an ANSI port list",
                )),
                ModuleItem::PortDecl(_) => {}
                ModuleItem::Data(d) => self.wires(d, non_ansi),
                ModuleItem::Instance(i) => self.instantiation(i),
                ModuleItem::Assign(v) => {
                    for a in v {
                        self.assign(a);
                    }
                }
                other => {
                    let (span, what) = item_kind(other);
                    self.diags.push(unsupported(span, what));
                }
            }
        }
    }

    /// The type rule of nets and ports: implicit or `logic`, without signing.
    fn type_problem(&self, ty: &DataType) -> Option<Diagnostic> {
        if !matches!(ty.kind, TypeKind::Implicit | TypeKind::Logic) {
            let what = format!("the data type {}", self.quote(ty.span));
            return Some(unsupported(ty.span, what));
        }
        ty.signing
            .map(|_| unsupported(ty.span, "a signing keyword"))
    }

    /// The range of a type: none, or one packed range with constant bounds and at most
    /// [`MAX_RANGE_WIDTH`] bits, checked before any net is made.
    fn range(&self, ty: &DataType) -> Result<Option<(i32, i32)>, Diagnostic> {
        match &ty.packed[..] {
            [] => Ok(None),
            [Dim::Range(msb, lsb)] => {
                let (m, l) = (self.bound(msb)?, self.bound(lsb)?);
                if range_width(m, l) > u64::from(MAX_RANGE_WIDTH) {
                    let msg = format!("range [{m}:{l}] is wider than {MAX_RANGE_WIDTH} bits");
                    return Err(Diagnostic::error(WIDTH, msb.span.to(lsb.span), msg));
                }
                Ok(Some((m, l)))
            }
            [Dim::Size(e)] => Err(unsupported(e.span, "a size dimension")),
            _ => Err(unsupported(ty.span, "a multi-dimensional type")),
        }
    }

    /// Reports the first problem of a declaration, if any.
    fn report(&mut self, problem: Option<String>, span: Span, ty: &DataType) {
        match problem {
            Some(what) => self.diags.push(unsupported(span, what)),
            None => self.diags.extend(self.type_problem(ty)),
        }
    }

    /// A constant integer (`what`: its role): a literal, negated only when it is a plain
    /// decimal number. A based literal is unsigned, so its negation wraps at its width (`-2'd1`
    /// is 3), a value the reader does not compute.
    fn constant(&self, e: &Expr, what: &str) -> Result<i64, Diagnostic> {
        let value = match &e.kind {
            ExprKind::Number(l) => literal_value(l),
            ExprKind::Unary(UnOp::Minus, inner) => match &inner.kind {
                ExprKind::Number(l) if l.plain => literal_value(l).map(|v| -v),
                _ => None,
            },
            _ => None,
        };
        value.ok_or_else(|| {
            let msg = format!(
                "the {what} {} (only integer literals are accepted, with a minus sign only \
                 before a decimal number)",
                self.quote(e.span)
            );
            unsupported(e.span, msg)
        })
    }

    /// A range bound: a constant that fits 32 bits (`i32::MIN` is reserved).
    fn bound(&self, e: &Expr) -> Result<i32, Diagnostic> {
        let v = self.constant(e, "range bound")?;
        i32::try_from(v)
            .ok()
            .filter(|&v| v != i32::MIN)
            .ok_or_else(|| {
                Diagnostic::error(WIDTH, e.span, format!("range bound {v} is too large"))
            })
    }

    /// Takes the bits of the range of `name` from the budget before its nets are made; false,
    /// with a diagnostic, when they exceed what is left.
    fn charge(&mut self, name: &str, range: Option<(i32, i32)>, span: Span) -> bool {
        let Some((msb, lsb)) = range else {
            return true;
        };
        match self.bits_left.checked_sub(range_width(msb, lsb)) {
            Some(left) => {
                self.bits_left = left;
                true
            }
            None => {
                let msg = format!(
                    "range [{msb}:{lsb}] of '{name}' exceeds the {} bits that the ranges of this \
                     text may declare together (one bit per byte of text, at least \
                     {MAX_RANGE_WIDTH})",
                    self.budget
                );
                self.error(WIDTH, span, msg);
                false
            }
        }
    }

    /// A port direction; `ref` is reported and read as inout.
    fn direction(&mut self, dir: Direction, span: Span) -> PortDir {
        match dir {
            Direction::Input => PortDir::Input,
            Direction::Output => PortDir::Output,
            Direction::Inout => PortDir::Inout,
            Direction::Ref => {
                self.diags.push(unsupported(span, "a ref port"));
                PortDir::Inout
            }
        }
    }

    /// Reports a port whose net type is not written while the `` `default_nettype`` is not
    /// `wire`: the port would take that type (IEEE 1800-2017 clause 23.2.2), or need one under
    /// `none`.
    fn default_net_type(&mut self, name: &str, span: Span) {
        let msg = match self.b.nettype.as_deref() {
            None | Some("wire") => return,
            Some("none") => {
                format!("port '{name}' has no net type, and `default_nettype none requires one")
            }
            Some(t) => {
                format!("port '{name}' would have net type '{t}'; only wire nets are supported")
            }
        };
        self.error(DECLARATION, span, msg);
    }

    // A rejected declaration still declares its name when its range is readable, so that the
    // uses of the name do not add errors of their own.

    /// An ANSI port; `named`: a net type is written for it.
    fn ansi_port(&mut self, p: &AnsiPort, named: bool) {
        let dir = self.direction(p.dir, p.span);
        let problem = match (p.net, &p.interface) {
            (_, Some(_)) => Some("an interface port".to_string()),
            (None, _) => Some(format!("the variable port '{}'", p.name.name)),
            (Some(NetKind::Wire), _) => None,
            (Some(k), _) => Some(format!("a '{}' port", net_kind_name(k))),
        };
        let problem = problem
            .or_else(|| (!p.unpacked.is_empty()).then(|| "an unpacked port dimension".into()))
            .or_else(|| p.default.is_some().then(|| "a port default".into()));
        self.report(problem, p.span, &p.ty);
        if p.net == Some(NetKind::Wire) && p.interface.is_none() && !named {
            self.default_net_type(&p.name.name, p.name.span);
        }
        match self.range(&p.ty) {
            Ok(range) => {
                if !self.charge(&p.name.name, range, p.name.span) {
                    return;
                }
                // An ANSI port declaration is complete (IEEE 1800-2017 clause 23.2.2.2).
                if let Err(e) = self.b.port(&p.name.name, dir, range, true, p.name.span) {
                    self.error(DECLARATION, p.name.span, e);
                }
            }
            Err(d) => self.diags.push(d),
        }
    }

    /// Ports of a non-ANSI header in header order, with the directions and ranges of their
    /// declarations in the body.
    fn non_ansi_ports(&mut self, m: &Module, ports: &[NonAnsiPort]) {
        let mut header = Vec::new();
        for p in ports {
            match (&p.external, p.expr.as_ref().and_then(Expr::as_ident)) {
                (None, Some(id)) => header.push(id),
                _ => self.diags.push(unsupported(
                    p.span,
                    format!("the port expression {}", self.quote(p.span)),
                )),
            }
        }
        // Names with a `wire` declaration, which gives a port its net type.
        let wired: HashSet<&str> = m
            .items
            .iter()
            .filter_map(|it| match it {
                ModuleItem::Data(d) if d.net == Some(NetKind::Wire) => Some(&d.decls),
                _ => None,
            })
            .flatten()
            .map(|d| d.name.name.as_str())
            .collect();
        // Direction, range and whether a net or variable type is given (which completes the
        // declaration) of each declared port; `None` when the range is unreadable.
        let mut decls: HashMap<&str, Option<(PortDir, Option<(i32, i32)>, bool)>> = HashMap::new();
        for item in &m.items {
            let ModuleItem::PortDecl(d) = item else {
                continue;
            };
            let dir = self.direction(d.dir, d.span);
            let problem = match d.net {
                _ if d.is_var => Some("a variable port".to_string()),
                None | Some(NetKind::Wire) => None,
                Some(k) => Some(format!("a '{}' port", net_kind_name(k))),
            };
            self.report(problem, d.span, &d.ty);
            let range = self.range(&d.ty).map_err(|e| self.diags.push(e)).ok();
            let typed = d.net.is_some() || d.is_var;
            for (name, dims, init) in &d.names {
                let extra = dims
                    .first()
                    .map(|_| name.span)
                    .or(init.as_ref().map(|i| i.span));
                if let Some(span) = extra {
                    self.diags
                        .push(unsupported(span, "an unpacked dimension or initializer"));
                }
                if !header.iter().any(|h| h.name == name.name) {
                    let msg = format!("'{}' is not in the port list", name.name);
                    self.error(DECLARATION, name.span, msg);
                } else if decls
                    .insert(name.name.as_str(), range.map(|r| (dir, r, typed)))
                    .is_some()
                {
                    let msg = format!("port '{}' is declared twice", name.name);
                    self.error(DECLARATION, name.span, msg);
                } else if d.net.is_none() && !d.is_var && !wired.contains(name.name.as_str()) {
                    self.default_net_type(&name.name, name.span);
                }
            }
        }
        for h in header {
            match decls.get(h.name.as_str()) {
                None => {
                    let msg = format!("port '{}' has no direction declaration", h.name);
                    self.error(DECLARATION, h.span, msg);
                }
                Some(None) => {}
                Some(&Some((dir, range, typed))) => {
                    if !self.charge(&h.name, range, h.span) {
                        continue;
                    }
                    if let Err(e) = self.b.port(&h.name, dir, range, typed, h.span) {
                        self.error(DECLARATION, h.span, e);
                    }
                }
            }
        }
    }

    fn wires(&mut self, d: &DataDecl, non_ansi: bool) {
        let problem = match d.net {
            Some(NetKind::Wire) => None,
            None => Some("a variable declaration".to_string()),
            Some(k) => Some(format!("a '{}' net declaration", net_kind_name(k))),
        };
        self.report(problem, d.span, &d.ty);
        let range = match self.range(&d.ty) {
            Ok(r) => r,
            Err(e) => {
                self.diags.push(e);
                return;
            }
        };
        for decl in &d.decls {
            if let Some(dim) = decl.dims.first() {
                let span = match dim {
                    Dim::Range(e, _) | Dim::Size(e) => e.span,
                    Dim::Unsupported(s) => *s,
                };
                self.diags.push(unsupported(span, "an array of nets"));
            } else if let Some(init) = &decl.init {
                let what = "a net declaration assignment (use a continuous assignment)";
                self.diags.push(unsupported(init.span, what));
            }
            // A wire that repeats a port (non-ANSI style) makes no nets.
            let port = matches!(
                self.b.names.get(&decl.name.name),
                Some(Decl::Net {
                    kind: NetDecl::Port { .. },
                    ..
                })
            );
            if !port && !self.charge(&decl.name.name, range, decl.name.span) {
                continue;
            }
            if let Err(e) = self
                .b
                .wire(&decl.name.name, range, decl.name.span, non_ansi)
            {
                self.error(DECLARATION, decl.name.span, e);
            }
        }
    }

    fn instantiation(&mut self, inst: &Instantiation) {
        if !inst.params.is_empty() {
            self.diags
                .push(unsupported(inst.span, "a parameter value assignment"));
            return;
        }
        // Verilog takes the name for the module being read, not for a library cell.
        if inst.module.name == self.b.g.name() {
            let what = format!(
                "an instance of the module '{}' itself (hierarchy)",
                inst.module.name
            );
            self.diags.push(unsupported(inst.module.span, what));
            return;
        }
        let cell = match self.b.cell(&inst.module.name) {
            Ok(c) => c,
            Err(e) => {
                self.error(CELL, inst.module.span, e);
                return;
            }
        };
        for i in &inst.instances {
            if !i.dims.is_empty() {
                self.diags.push(unsupported(i.span, "an instance array"));
                continue;
            }
            let id = match self.b.instance(&i.name.name, cell, i.name.span) {
                Ok(id) => id,
                Err(e) => {
                    self.error(DECLARATION, i.name.span, e);
                    continue;
                }
            };
            let mut connected = HashSet::new();
            for c in &i.conns {
                let (pin, e) = match c {
                    PortConn::Named(pin, Some(e)) => (pin, e),
                    PortConn::Named(pin, None) => {
                        let what = format!("the implicit connection '.{}'", pin.name);
                        self.diags.push(unsupported(pin.span, what));
                        continue;
                    }
                    PortConn::Ordered(e) => {
                        let span = e.as_ref().map_or(i.span, |e| e.span);
                        self.diags
                            .push(unsupported(span, "a positional connection"));
                        continue;
                    }
                    PortConn::Wildcard(span) => {
                        self.diags.push(unsupported(*span, "a '.*' connection"));
                        continue;
                    }
                };
                let Some(k) = self.b.pin_index(cell, &pin.name) else {
                    let msg = format!("cell '{}' has no pin '{}'", inst.module.name, pin.name);
                    self.error(CELL, pin.span, msg);
                    continue;
                };
                if !connected.insert(k) {
                    let msg = format!("pin '{}' of '{}' is connected twice", pin.name, i.name.name);
                    self.error(CELL, pin.span, msg);
                    continue;
                }
                // `.P()` leaves the pin unconnected.
                let Some(e) = e else {
                    continue;
                };
                match self.bits(e, true) {
                    Ok(bits) => match bits[..] {
                        [c] => {
                            if let Err(err) = self.b.g.connect(id, PinRef::Index(k), c) {
                                self.error(CELL, pin.span, err.to_string());
                            }
                        }
                        _ => {
                            let msg = format!(
                                "pin '{}' takes 1 bit, {} has {}",
                                pin.name,
                                self.quote(e.span),
                                bit_count(bits.len())
                            );
                            self.error(WIDTH, e.span, msg);
                        }
                    },
                    Err(d) => self.diags.push(d),
                }
            }
        }
    }

    fn assign(&mut self, a: &ContAssign) {
        let lhs = self.bits(&a.lhs, true);
        let rhs = self.bits(&a.rhs, false);
        match (lhs, rhs) {
            (Ok(lhs), Ok(rhs)) => {
                if let Err(e) = assign_bits(&mut self.b.g, &lhs, &rhs) {
                    self.error(WIDTH, a.span, e);
                }
            }
            (lhs, rhs) => self.diags.extend(lhs.err().into_iter().chain(rhs.err())),
        }
    }

    /// Bits of a connection or assignment side, in declaration order.
    fn bits(&mut self, e: &Expr, implicit: bool) -> Result<Vec<Conn>, Diagnostic> {
        let what = match &e.kind {
            ExprKind::Ident(p) if p.scope.is_none() => {
                return self
                    .b
                    .whole(&p.name.name, implicit, e.span)
                    .map_err(|m| Diagnostic::error(DECLARATION, e.span, m));
            }
            ExprKind::Index(base, index) => match base.as_ident() {
                Some(id) => {
                    let k = self.constant(index, "bit index")?;
                    let declared = matches!(self.b.names.get(&id.name), Some(Decl::Net { .. }));
                    let code = if declared { WIDTH } else { DECLARATION };
                    return self
                        .b
                        .bit(&id.name, k)
                        .map(|c| vec![c])
                        .map_err(|m| Diagnostic::error(code, e.span, m));
                }
                None => "a select of a select or member",
            },
            ExprKind::Number(l) if l.size == Some(1) && !l.plain && !l.is_truncated() => {
                match l.bits().ok().and_then(|b| b.first().copied()) {
                    Some(Bit4::Zero) => return Ok(vec![Conn::Const(false)]),
                    Some(Bit4::One) => return Ok(vec![Conn::Const(true)]),
                    _ => "the x/z literal",
                }
            }
            ExprKind::Number(_) => "the constant (only 1'b0 and 1'b1 are accepted)",
            ExprKind::UnbasedUnsized(_) => "the unbased unsized literal",
            ExprKind::Ident(_) => "the package-scoped name",
            ExprKind::Slice { .. } => "the part-select",
            ExprKind::Member(..) => "the hierarchical or member reference",
            ExprKind::Concat(_) => "the concatenation",
            ExprKind::Repl(..) => "the replication",
            ExprKind::Real(_) | ExprKind::Str(_) => "the literal",
            ExprKind::Call { .. } | ExprKind::SysCall { .. } => "the function call",
            ExprKind::Pattern { .. } => "the assignment pattern",
            _ => "the expression",
        };
        Err(unsupported(
            e.span,
            format!("{what} {}", self.quote(e.span)),
        ))
    }
}
