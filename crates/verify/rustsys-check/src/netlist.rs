// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Netlist model (CHECK-REQ-0020 to CHECK-REQ-0023, CHECK-REQ-0031).

mod cells;
mod connections;
pub mod frozen;
mod source;

use std::collections::{HashMap, HashSet};

use rusteda_formats::liberty::{Direction as LDir, Library};
use rusteda_formats::verilog::ast::{
    self, Expr, ExprKind, Item, ModuleItem, NetKind, PortConn, PortList, TypeKind,
};
use rusteda_formats::verilog::number::{Bit4, Literal};
use rusteda_formats::verilog::{SourceMap, Span};

use crate::circ::{B, Circuit, Def};
use crate::liberty::Unread;

pub(crate) use source::read;

/// The cells of the libraries by name, as the netlist model takes them (CHECK-REQ-0023): a name
/// that two definitions describe differently maps to the difference.
pub(crate) fn cell_table(
    libs: &[Library],
) -> HashMap<&str, Result<&rusteda_formats::liberty::Cell, String>> {
    cells::table(libs)
}

/// A sequential cell instance.
#[derive(Debug, Clone)]
pub struct SeqInst {
    pub inst: String,
    pub cell: String,
    pub latch: bool,
    /// Internal state variable.
    pub iq: u32,
    pub next: B,
    /// Clock (rising edge) or enable (active high).
    pub clk: B,
    /// Conditions forcing the internal state to 0 and to 1.
    pub clear: B,
    pub preset: B,
}

/// The netlist model.
#[derive(Debug, Clone, Default)]
pub struct NetModel {
    pub name: String,
    /// Ports in order: name, is input, width (`None` = scalar).
    pub ports: Vec<(String, bool, Option<u32>)>,
    /// Output port bits.
    pub outputs: HashMap<String, Vec<B>>,
    /// Scalar and vector nets: name -> bit variables.
    pub nets: HashMap<String, Vec<u32>>,
    /// Declared `[left:right]` range of each vector net.
    pub ranges: HashMap<String, (i64, i64)>,
    pub seq: Vec<SeqInst>,
    /// Supply ports, which take no part in the port correspondence (CHECK-REQ-0022).
    pub supply_ports: Vec<String>,
    pub errors: Vec<String>,
}

/// Codes of the constructs outside the checker's model (CHECK-REQ-0023), and of a library
/// description that the checker cannot use (CHECK-REQ-0082).
pub(crate) const THREE_STATE: &str = "C0101";
pub(crate) const DUAL_EDGE: &str = "C0102";
const UNKNOWN_BIT: &str = "C0103";
pub(crate) const UNMODELED_CELL: &str = "C0104";
const NET_KIND: &str = "C0105";
const LIBERTY: &str = "C0003";

/// Widest netlist net the model allocates.
const MAX_NET_BITS: u64 = 1 << 20;

/// The coded message of a construct that the checker rejects instead of evaluating it partially
/// (CHECK-REQ-0023).
fn unmodeled(code: &str, sm: &SourceMap, at: Span, msg: impl std::fmt::Display) -> String {
    format!("error[{code}]: {}: {msg}", sm.describe(at))
}

/// A structural error with its netlist location (CHECK-REQ-0020).
fn located(sm: &SourceMap, at: Span, msg: impl std::fmt::Display) -> String {
    format!("{}: {msg}", sm.describe(at))
}

/// The text of a literal in `e`, a bit index included, that has an `x` or `z` bit.
fn unknown_literal<'s>(sm: &'s SourceMap, e: &Expr) -> Option<&'s str> {
    let unknown = match &e.kind {
        ExprKind::Number(l) => l.bits().is_ok_and(|b| b.iter().any(|x| x.is_unknown())),
        ExprKind::UnbasedUnsized(c) => matches!(c, 'x' | 'z'),
        ExprKind::Index(b, i) => {
            return unknown_literal(sm, b).or_else(|| unknown_literal(sm, i));
        }
        _ => false,
    };
    unknown.then(|| sm.snippet(e.span))
}

/// The value of an integer literal of a declared range or a bit index as Verilog reads it, a
/// signed literal with its top bit set being negative; `None` outside 32 bits.
fn int_value(l: &Literal) -> Result<Option<i64>, String> {
    let mut bits = l.bits()?;
    let negative = l.is_signed() && bits.last() == Some(&Bit4::One);
    let fill = if negative { Bit4::One } else { Bit4::Zero };
    if bits.len() < 32 {
        bits.resize(32, fill);
    }
    if bits[31..].iter().any(|b| *b != fill) {
        return Ok(None);
    }
    let low =
        bits[..31].iter().enumerate().fold(
            0i64,
            |a, (k, b)| if *b == Bit4::One { a | 1 << k } else { a },
        );
    Ok(Some(if negative { low - (1 << 31) } else { low }))
}

fn kind_name(k: NetKind) -> &'static str {
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

/// Rejects a net kind that resolves several drivers, pulls an undriven net or stores charge: the
/// model gives a net one driver, as `wire`, `tri`, `uwire` and variables have (CHECK-REQ-0023).
fn one_driver(sm: &SourceMap, kind: Option<NetKind>, what: &str, at: Span) -> Result<(), String> {
    match kind {
        Some(
            k @ (NetKind::Wand
            | NetKind::Wor
            | NetKind::Triand
            | NetKind::Trior
            | NetKind::Tri0
            | NetKind::Tri1
            | NetKind::Trireg
            | NetKind::Interconnect),
        ) => Err(unmodeled(
            NET_KIND,
            sm,
            at,
            format!(
                "{what} is declared {}; the checker models nets with one driver and supply \
                 nets, not the resolution of {} nets",
                kind_name(k),
                kind_name(k)
            ),
        )),
        _ => Ok(()),
    }
}

/// The value of a supply net: `supply1` is 1 and `supply0` is 0 whatever drives it.
fn supply_value(kind: Option<NetKind>) -> Option<bool> {
    match kind {
        Some(NetKind::Supply0) => Some(false),
        Some(NetKind::Supply1) => Some(true),
        _ => None,
    }
}

/// Rejects a data type other than the bit types of netlists, whose value the model would read
/// as something else (CHECK-REQ-0020).
fn bit_type(sm: &SourceMap, ty: &ast::DataType, what: &str, at: Span) -> Result<(), String> {
    match ty.kind {
        TypeKind::Implicit | TypeKind::Logic | TypeKind::Reg | TypeKind::Bit => Ok(()),
        _ => Err(located(
            sm,
            at,
            format!(
                "{what} has a data type other than logic, reg or bit, which netlists do not use"
            ),
        )),
    }
}

/// The declared `[left:right]` range of `what`, a port or net, if any.
fn range_of(sm: &SourceMap, dims: &[ast::Dim], what: &str) -> Result<Option<(i64, i64)>, String> {
    let bound = |e: &Expr| -> Result<i64, String> {
        if let Some(t) = unknown_literal(sm, e) {
            return Err(unmodeled(
                UNKNOWN_BIT,
                sm,
                e.span,
                format!("{what}: range bound {t} has an x or z bit"),
            ));
        }
        let ExprKind::Number(l) = &e.kind else {
            return Err(located(
                sm,
                e.span,
                format!("{what}: netlist ranges must be integer literals"),
            ));
        };
        int_value(l)?.ok_or_else(|| {
            located(
                sm,
                e.span,
                format!(
                    "{what}: range bound {} is outside 32 bits",
                    sm.snippet(e.span)
                ),
            )
        })
    };
    let r = match dims {
        [] => return Ok(None),
        [ast::Dim::Range(l, r)] => (bound(l)?, bound(r)?),
        _ => return Err(format!("{what}: unsupported netlist dimension")),
    };
    if (r.0 - r.1).unsigned_abs() >= MAX_NET_BITS {
        return Err(format!("{what} is wider than {MAX_NET_BITS} bits"));
    }
    Ok(Some(r))
}

/// A port with its declaration.
struct Port<'m> {
    name: &'m str,
    dir: ast::Direction,
    dims: &'m [ast::Dim],
    net: Option<NetKind>,
    ty: &'m ast::DataType,
    at: Span,
}

/// Ports in header order: declared in the header (ANSI) or by port declarations in the body
/// (non-ANSI) (CHECK-REQ-0020).
fn ports<'m>(sm: &SourceMap, m: &'m ast::Module) -> Result<Vec<Port<'m>>, String> {
    // Never evaluated partially (CHECK-REQ-0020).
    let plain = |name: &ast::Ident, unpacked: &[ast::Dim], init: bool| {
        let what = if !unpacked.is_empty() {
            "has unpacked dimensions"
        } else if init {
            "is declared with a value"
        } else {
            return Ok(());
        };
        Err(located(
            sm,
            name.span,
            format!("port {} {what}, which netlists do not use", name.name),
        ))
    };
    match &m.ports {
        PortList::Ansi(ps) => {
            let mut out = Vec::new();
            for p in ps {
                plain(&p.name, &p.unpacked, p.default.is_some())?;
                if p.interface.is_some() {
                    return Err(located(
                        sm,
                        p.name.span,
                        format!("port {} is an interface port", p.name.name),
                    ));
                }
                out.push(Port {
                    name: &p.name.name,
                    dir: p.dir,
                    dims: &p.ty.packed,
                    net: p.net,
                    ty: &p.ty,
                    at: p.name.span,
                });
            }
            Ok(out)
        }
        PortList::NonAnsi(ps) => {
            let mut decls: HashMap<&str, Port<'m>> = HashMap::new();
            for it in &m.items {
                if let ModuleItem::PortDecl(d) = it {
                    for (n, unpacked, init) in &d.names {
                        plain(n, unpacked, init.is_some())?;
                        let p = Port {
                            name: &n.name,
                            dir: d.dir,
                            dims: &d.ty.packed,
                            net: d.net,
                            ty: &d.ty,
                            at: n.span,
                        };
                        if decls.insert(&n.name, p).is_some() {
                            return Err(located(
                                sm,
                                n.span,
                                format!("port {} is declared twice", n.name),
                            ));
                        }
                    }
                }
            }
            let mut out = Vec::new();
            for p in ps {
                let name = p
                    .expr
                    .as_ref()
                    .and_then(|e| e.as_ident())
                    .filter(|_| p.external.is_none())
                    .ok_or_else(|| located(sm, p.span, "netlist ports must be plain names"))?;
                let d = decls
                    .remove(name.name.as_str())
                    .ok_or_else(|| format!("port {} has no direction declaration", name.name))?;
                out.push(d);
            }
            Ok(out)
        }
        PortList::None => Ok(Vec::new()),
    }
}

/// Builds the netlist into the circuit. `input_vars` gives the variables of the RTL input bits;
/// `None` checks the netlist alone when there is no RTL model (CHECK-REQ-0002): its inputs get
/// variables of their own, and nothing that depends on the RTL ports is reported. `sm` locates
/// rejected constructs and `unread` holds the library constructs that the library model does not
/// keep (CHECK-REQ-0023).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn build(
    c: &mut Circuit,
    sm: &SourceMap,
    units: &[ast::SourceUnit],
    libs: &[Library],
    top: &str,
    input_vars: Option<&HashMap<String, Vec<u32>>>,
    supply_ports: &[String],
    unread: &[Unread],
) -> Result<NetModel, String> {
    let mut modules = Vec::new();
    for u in units {
        for it in &u.items {
            match it {
                Item::Module(m) => modules.push(m),
                // A `bind` adds instances to a module, and a primitive is what a simulator
                // would instantiate under its name.
                Item::Unsupported { what, span } => {
                    return Err(located(
                        sm,
                        *span,
                        format!(
                            "a {what} construct in the netlist; the checker reads one flat module \
                             of library cells"
                        ),
                    ));
                }
                Item::Package(_) | Item::Decl(_) => {}
            }
        }
    }
    let m = modules
        .iter()
        .find(|m| m.name.name == top)
        .or_else(|| (modules.len() == 1).then(|| &modules[0]))
        .ok_or("netlist module not found")?;
    let mut module_names: HashSet<&str> = HashSet::new();
    for x in &modules {
        if !module_names.insert(x.name.name.as_str()) && x.name.name == m.name.name {
            return Err(located(
                sm,
                x.name.span,
                format!("module {} is defined twice in the netlist", x.name.name),
            ));
        }
    }
    let cells = cells::table(libs);
    // The first construct of each cell that the library model does not keep.
    let mut unread_of: HashMap<&str, &Unread> = HashMap::new();
    for u in unread {
        unread_of.entry(u.cell.as_str()).or_insert(u);
    }
    // Power and ground pin names of the libraries, and the given names, name supply ports
    // (CHECK-REQ-0022).
    let pg_names: HashSet<&str> = libs
        .iter()
        .flat_map(|l| &l.cells)
        .flat_map(|cl| cl.pg_pins.iter().map(String::as_str))
        .chain(supply_ports.iter().map(String::as_str))
        .collect();
    let mut nm = NetModel {
        name: m.name.name.clone(),
        ..NetModel::default()
    };
    // A port declared without a net kind takes the default net type (CHECK-REQ-0023).
    if let Some(k) = m
        .default_nettype
        .as_deref()
        .filter(|k| !matches!(*k, "none" | "tri" | "uwire"))
    {
        return Err(unmodeled(
            NET_KIND,
            sm,
            m.name.span,
            format!(
                "`default_nettype {k} makes the nets declared without a net kind {k} nets; the \
                 checker models nets with one driver and supply nets"
            ),
        ));
    }
    // Ranges of vector nets for bit addressing.
    let mut ranges: HashMap<String, (i64, i64)> = HashMap::new();
    let mut declare = |nm: &mut NetModel,
                       c: &mut Circuit,
                       name: &str,
                       range: Option<(i64, i64)>,
                       vars: Option<&Vec<u32>>|
     -> Result<(), String> {
        if nm.nets.contains_key(name) {
            return Err(format!("net {name} declared twice"));
        }
        let w = range.map_or(1, |(l, r)| (l - r).unsigned_abs() + 1) as usize;
        let v: Vec<u32> = match vars {
            Some(v) => {
                if v.len() != w {
                    return Err(format!(
                        "port {name} has width {w} but the RTL port has width {}",
                        v.len()
                    ));
                }
                v.clone()
            }
            None => (0..w).map(|_| c.free()).collect(),
        };
        if let Some(r) = range {
            ranges.insert(name.to_string(), r);
        }
        nm.nets.insert(name.to_string(), v);
        Ok(())
    };
    let ports = ports(sm, m)?;
    let mut supply: HashSet<String> = HashSet::new();
    // Without the RTL ports, an input named as a power or ground pin may be a supply port or a
    // logic input: it is read as a logic input and its declarations as a net are not judged.
    let mut either: HashSet<&str> = HashSet::new();
    let mut port_ranges: HashMap<&str, Option<(i64, i64)>> = HashMap::new();
    for p in &ports {
        let what = format!("port {}", p.name);
        one_driver(sm, p.net, &what, p.at)?;
        bit_type(sm, p.ty, &what, p.at)?;
        let r = range_of(sm, p.dims, &what)?;
        port_ranges.insert(p.name, r);
        let input = p.dir == ast::Direction::Input;
        if pg_names.contains(p.name) && r.is_none() {
            let is_supply = match input_vars {
                Some(v) => {
                    !v.contains_key(p.name)
                        && matches!(p.dir, ast::Direction::Input | ast::Direction::Inout)
                }
                None => {
                    if input {
                        either.insert(p.name);
                    }
                    p.dir == ast::Direction::Inout
                }
            };
            if is_supply {
                supply.insert(p.name.to_string());
                nm.supply_ports.push(p.name.to_string());
                continue;
            }
        }
        if let Some(k) = p.net.filter(|k| supply_value(Some(*k)).is_some()) {
            return Err(supply_port(sm, &what, k, p.at));
        }
        if !input && p.dir != ast::Direction::Output {
            return Err(format!("port {} has an unsupported direction", p.name));
        }
        let vars = match input_vars.filter(|_| input) {
            Some(v) => Some(
                v.get(p.name)
                    .ok_or_else(|| format!("netlist input {} is not an RTL input", p.name))?,
            ),
            None => None,
        };
        declare(&mut nm, c, p.name, r, vars)?;
        nm.ports.push((
            p.name.to_string(),
            input,
            r.map(|(l, rr)| ((l - rr).unsigned_abs() + 1) as u32),
        ));
    }
    let inputs: HashSet<u32> = nm
        .ports
        .iter()
        .filter(|p| p.1)
        .flat_map(|p| nm.nets[&p.0].clone())
        .collect();
    // Bits of supply nets, which keep their value whatever drives them, with the net named.
    let mut supply_nets: HashMap<u32, String> = HashMap::new();
    for it in &m.items {
        if let ModuleItem::Data(d) = it {
            for x in &d.decls {
                let what = format!("net {}", x.name.name);
                one_driver(sm, d.net, &what, x.name.span)?;
                bit_type(sm, &d.ty, &what, x.name.span)?;
                let r = range_of(sm, &d.ty.packed, &what)?;
                // Never evaluated partially (CHECK-REQ-0020).
                if !x.dims.is_empty() {
                    return Err(located(
                        sm,
                        x.name.span,
                        format!(
                            "net {} has unpacked dimensions, which netlists do not use",
                            x.name.name
                        ),
                    ));
                }
                if x.init.is_some() {
                    return Err(located(
                        sm,
                        x.name.span,
                        format!(
                            "net {} is declared with an assignment; netlists use assign",
                            x.name.name
                        ),
                    ));
                }
                // A supply port may be declared again, also as a supply net.
                if supply.contains(&x.name.name) || either.contains(x.name.name.as_str()) {
                    continue;
                }
                // A non-ANSI port may be declared again as a net of the same range.
                let kept = supply_value(d.net);
                match port_ranges.get(x.name.name.as_str()) {
                    Some(_) if kept.is_some() => {
                        let k = d.net.expect("supply kind");
                        return Err(supply_port(
                            sm,
                            &format!("port {}", x.name.name),
                            k,
                            x.name.span,
                        ));
                    }
                    Some(pr) if *pr == r => continue,
                    Some(_) => {
                        return Err(format!(
                            "net {} redeclares a port with another range",
                            x.name.name
                        ));
                    }
                    None => declare(&mut nm, c, &x.name.name, r, None)?,
                }
                if let (Some(v), Some(k)) = (kept, d.net) {
                    for &b in &nm.nets[&x.name.name] {
                        c.define(b, B::from_bool(v));
                        supply_nets.insert(b, format!("{} net {}", kind_name(k), x.name.name));
                    }
                }
            }
        }
    }
    // `what` names the reader of the bit for a rejected literal (CHECK-REQ-0023).
    let bit = |nm: &NetModel, e: &Expr, what: &dyn Fn() -> String| -> Result<B, String> {
        if let Some(t) = unknown_literal(sm, e) {
            return Err(unmodeled(
                UNKNOWN_BIT,
                sm,
                e.span,
                format!(
                    "{}: literal {t} has an x or z bit; the checker does not model unknown or \
                     high-impedance values",
                    what()
                ),
            ));
        }
        if let Some(p) = e.as_ident().filter(|p| supply.contains(&p.name)) {
            return Err(format!(
                "supply port {} is read as a logic value (CHECK-REQ-0022)",
                p.name
            ));
        }
        match &e.kind {
            // A name in a package scope is not a net of the module.
            ExprKind::Ident(p) if p.scope.is_none() => {
                let v = nm.nets.get(&p.name.name).ok_or_else(|| {
                    located(
                        sm,
                        e.span,
                        format!("{}: undeclared net {}", what(), p.name.name),
                    )
                })?;
                if v.len() != 1 {
                    return Err(located(
                        sm,
                        e.span,
                        format!("{}: vector net {} used as a bit", what(), p.name.name),
                    ));
                }
                Ok(B::V(v[0]))
            }
            ExprKind::Index(b, i) => {
                let name = &b
                    .as_ident()
                    .ok_or_else(|| located(sm, e.span, "unsupported net reference"))?
                    .name;
                let v = nm.nets.get(name).ok_or_else(|| {
                    located(sm, e.span, format!("{}: undeclared net {name}", what()))
                })?;
                let (l, r) = ranges.get(name).copied().ok_or_else(|| {
                    located(sm, e.span, format!("{}: {name} is not a vector", what()))
                })?;
                let ExprKind::Number(n) = &i.kind else {
                    return Err(located(
                        sm,
                        i.span,
                        format!("{}: net index must be a literal", what()),
                    ));
                };
                // An index outside the range reads no bit of the net.
                let (lo, hi) = (l.min(r), l.max(r));
                let Some(k) = int_value(n)?.filter(|k| (lo..=hi).contains(k)) else {
                    return Err(located(
                        sm,
                        e.span,
                        format!(
                            "{}: index {} is outside the range [{l}:{r}] of {name}",
                            what(),
                            sm.snippet(i.span)
                        ),
                    ));
                };
                let off = if l >= r { k - r } else { r - k };
                Ok(B::V(v[off as usize]))
            }
            ExprKind::Number(n) => {
                let b = n.bits()?;
                if b.len() != 1 {
                    return Err(located(
                        sm,
                        e.span,
                        format!("{}: only 1-bit constants are allowed in netlists", what()),
                    ));
                }
                Ok(B::from_bool(b[0] == Bit4::One))
            }
            _ => Err(located(
                sm,
                e.span,
                format!("{}: unsupported netlist expression", what()),
            )),
        }
    };
    let define = |c: &mut Circuit, target: B, value: B, what: &str, at: Span| {
        let B::V(v) = target else {
            return Err(located(
                sm,
                at,
                format!("{what}: a constant cannot be driven"),
            ));
        };
        if inputs.contains(&v) {
            return Err(located(sm, at, format!("{what} drives a primary input")));
        }
        if let Some(net) = supply_nets.get(&v) {
            return Err(unmodeled(
                NET_KIND,
                sm,
                at,
                format!(
                    "{what} drives {net}, which keeps its value whatever drives it; the checker \
                     does not model drive strengths"
                ),
            ));
        }
        if !c.define(v, value) {
            return Err(located(
                sm,
                at,
                format!("{what}: net has more than one driver"),
            ));
        }
        Ok(())
    };
    for it in &m.items {
        match it {
            ModuleItem::Data(_) | ModuleItem::PortDecl(_) => {}
            ModuleItem::Assign(v) => {
                for a in v {
                    let what = || format!("assign to {}", sm.snippet(a.lhs.span));
                    let l = connections::bits(sm, &nm, &ranges, &supply, &a.lhs, &bit, &what)?;
                    let r = connections::bits(sm, &nm, &ranges, &supply, &a.rhs, &bit, &what)?;
                    if l.len() != r.len() {
                        return Err(located(
                            sm,
                            a.span,
                            format!(
                                "{}: connection widths differ ({} and {}); netlist assignments must have explicit matching widths",
                                what(),
                                l.len(),
                                r.len()
                            ),
                        ));
                    }
                    for (target, value) in l.into_iter().zip(r) {
                        define(c, target, value, &what(), a.span)?;
                    }
                }
            }
            ModuleItem::Instance(inst) => {
                let name = inst.module.name.as_str();
                // A module of the netlist would be what a simulator instantiates.
                if module_names.contains(name) {
                    return Err(located(
                        sm,
                        inst.span,
                        format!(
                            "{name} is a module of the netlist; the checker reads one flat \
                             module of library cells"
                        ),
                    ));
                }
                let cell = match cells.get(name) {
                    Some(Ok(cell)) => *cell,
                    Some(Err(d)) => {
                        return Err(unmodeled(
                            LIBERTY,
                            sm,
                            inst.span,
                            format!(
                                "cell {name}: {d}; the checker does not choose between \
                                 definitions"
                            ),
                        ));
                    }
                    None => return Err(located(sm, inst.span, format!("unknown cell {name}"))),
                };
                if !inst.params.is_empty() {
                    return Err(located(
                        sm,
                        inst.span,
                        format!(
                            "cell {} is instantiated with parameters, which library cells do \
                             not have",
                            cell.name
                        ),
                    ));
                }
                for i in &inst.instances {
                    let at = format!("instance {} of cell {}", i.name.name, cell.name);
                    if !i.dims.is_empty() {
                        return Err(located(
                            sm,
                            i.span,
                            format!("{at} is an array of instances, which netlists do not use"),
                        ));
                    }
                    cells::modeled(sm, i, cell, unread_of.get(name).copied())?;
                    // Connected pins with their bit and the place of the connection.
                    let mut conn: HashMap<&str, (B, Span)> = HashMap::new();
                    let mut seen: HashSet<&str> = HashSet::new();
                    for pc in &i.conns {
                        let (pin, e) = match pc {
                            PortConn::Named(pin, Some(e)) => (pin, e.as_ref()),
                            PortConn::Named(pin, None) => {
                                return Err(located(
                                    sm,
                                    pin.span,
                                    format!(
                                        "{at}: implicit connection .{}; netlists connect pins \
                                         as .pin(net)",
                                        pin.name
                                    ),
                                ));
                            }
                            PortConn::Ordered(_) | PortConn::Wildcard(_) => {
                                return Err(located(
                                    sm,
                                    i.span,
                                    format!("{at}: only named connections are supported"),
                                ));
                            }
                        };
                        if !seen.insert(&pin.name) {
                            return Err(located(
                                sm,
                                pin.span,
                                format!("{at}: pin {} is connected twice", pin.name),
                            ));
                        }
                        // Power and ground connections are not evaluated: every cell is taken
                        // to be powered. An x or z literal is rejected there too
                        // (CHECK-REQ-0022, CHECK-REQ-0023).
                        if cell.pg_pins.contains(&pin.name) {
                            if let Some((e, t)) = e.and_then(|e| Some((e, unknown_literal(sm, e)?)))
                            {
                                return Err(unmodeled(
                                    UNKNOWN_BIT,
                                    sm,
                                    e.span,
                                    format!(
                                        "{at} power or ground pin {}: literal {t} has an x or z \
                                         bit; the checker does not model unknown or \
                                         high-impedance values",
                                        pin.name
                                    ),
                                ));
                            }
                            continue;
                        }
                        let Some(p) = cell.pin(&pin.name) else {
                            return Err(located(
                                sm,
                                pin.span,
                                format!("{at}: cell {} has no pin {}", cell.name, pin.name),
                            ));
                        };
                        // An empty connection leaves the pin unconnected.
                        let Some(e) = e else {
                            continue;
                        };
                        if let LDir::Inout | LDir::Internal = p.direction {
                            return Err(unmodeled(
                                UNMODELED_CELL,
                                sm,
                                pin.span,
                                format!(
                                    "{at}: pin {} is {}; the checker models input and output \
                                     pins only",
                                    pin.name,
                                    if p.direction == LDir::Inout {
                                        "bidirectional (inout)"
                                    } else {
                                        "internal"
                                    }
                                ),
                            ));
                        }
                        let what = || format!("{at} pin {}", pin.name);
                        conn.insert(&pin.name, (bit(&nm, e, &what)?, e.span));
                    }
                    if let Some(seq) = cells::evaluate(c, sm, i, cell, &conn, &define)? {
                        nm.seq.push(seq);
                    }
                }
            }
            // Assertions, specify blocks and time declarations do not drive nets.
            ModuleItem::Ignored { .. } => {}
            other => return Err(outside_subset(sm, other)),
        }
    }
    for (name, is_in, _) in &nm.ports {
        if !is_in {
            let bits: Vec<B> = nm.nets[name].iter().map(|v| B::V(*v)).collect();
            nm.outputs.insert(name.clone(), bits);
        }
    }
    nm.ranges = ranges;
    Ok(nm)
}

/// The message of a module item outside the netlist subset (CHECK-REQ-0020): a primitive that
/// can float, switch or pull a net is high impedance (CHECK-REQ-0023), anything else is named
/// with its location.
fn outside_subset(sm: &SourceMap, it: &ModuleItem) -> String {
    let first = |spans: &mut dyn Iterator<Item = Span>| spans.next().unwrap_or_default();
    let (at, what) = match it {
        ModuleItem::Unsupported { what, span } if what.starts_with("gate primitive") => {
            return unmodeled(
                THREE_STATE,
                sm,
                *span,
                format!("a {what}; the checker does not model high impedance, switches or pulls"),
            );
        }
        ModuleItem::Unsupported { what, span } => (*span, format!("{what} construct")),
        ModuleItem::Gate(g) => {
            let kind = g.first().map_or("", |x| gate_name(x.kind));
            let name = g
                .first()
                .and_then(|x| x.name.as_ref())
                .map_or(String::new(), |n| format!(" {}", n.name));
            (
                first(&mut g.iter().map(|x| x.span)),
                format!("gate primitive '{kind}'{name}"),
            )
        }
        ModuleItem::Param(v) => (first(&mut v.iter().map(|x| x.span)), "parameter".into()),
        ModuleItem::Typedef(t) => (t.span, "typedef".into()),
        ModuleItem::Import(v) => (
            first(&mut v.iter().map(|x| x.span)),
            "package import".into(),
        ),
        ModuleItem::Always(a) => (a.span, "always procedure".into()),
        ModuleItem::Initial(x) => (x.span, "initial procedure".into()),
        ModuleItem::Final(x) => (x.span, "final procedure".into()),
        ModuleItem::Genvar(v) => (
            first(&mut v.iter().map(|x| x.span)),
            "genvar declaration".into(),
        ),
        ModuleItem::GenFor(g) => (g.span, "generate loop".into()),
        ModuleItem::GenIf(g) => (g.span, "generate if".into()),
        ModuleItem::GenCase(g) => (g.span, "generate case".into()),
        ModuleItem::GenBlock(g) => (g.span, "generate block".into()),
        ModuleItem::GenRegion(_, x) => (*x, "generate region".into()),
        ModuleItem::Function(f) => (f.span, "function".into()),
        ModuleItem::Task(t) => (t.span, "task".into()),
        ModuleItem::ElabTask { name, span, .. } => (*span, format!("elaboration task {name}")),
        ModuleItem::Defparam(x) => (*x, "defparam".into()),
        ModuleItem::Modport(v) => (first(&mut v.iter().map(|x| x.span)), "modport".into()),
        ModuleItem::PortDecl(d) => (d.span, "port declaration".into()),
        ModuleItem::Data(d) => (d.span, "declaration".into()),
        ModuleItem::Assign(v) => (
            first(&mut v.iter().map(|x| x.span)),
            "continuous assignment".into(),
        ),
        ModuleItem::Instance(i) => (i.span, "instance".into()),
        ModuleItem::Ignored { what, span } => (*span, (*what).to_string()),
    };
    let article = if what.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    located(
        sm,
        at,
        format!(
            "{article} {what} in the netlist; netlists hold port and net declarations, \
             continuous assignments of single bits and instances of library cells"
        ),
    )
}

fn gate_name(k: ast::GateKind) -> &'static str {
    match k {
        ast::GateKind::And => "and",
        ast::GateKind::Nand => "nand",
        ast::GateKind::Or => "or",
        ast::GateKind::Nor => "nor",
        ast::GateKind::Xor => "xor",
        ast::GateKind::Xnor => "xnor",
        ast::GateKind::Not => "not",
        ast::GateKind::Buf => "buf",
    }
}

/// The message of a logic port declared as a supply net (CHECK-REQ-0023).
fn supply_port(sm: &SourceMap, what: &str, k: NetKind, at: Span) -> String {
    unmodeled(
        NET_KIND,
        sm,
        at,
        format!(
            "{what} is declared {}, which keeps its value whatever drives it; the checker does \
             not take a logic port for a supply net",
            kind_name(k)
        ),
    )
}

impl NetModel {
    /// RTL state names use least-significant-bit ordinals (SYNTH-REQ-0603), not
    /// the written bounds of a foreign netlist vector. Explicit scalar probes win.
    pub(crate) fn state_bit(&self, name: &str) -> Result<Option<u32>, String> {
        if self.nets.contains_key(name) {
            return self.named_bit(name);
        }
        let Some((base, index)) = name
            .strip_suffix(']')
            .and_then(|n| n.rsplit_once('['))
            .and_then(|(b, i)| Some((b, i.parse::<usize>().ok()?)))
        else {
            return Ok(None);
        };
        Ok(self.nets.get(base).and_then(|v| v.get(index)).copied())
    }

    /// The variable of the wire named `name` (CHECK-REQ-0031): a scalar net of that name or, for
    /// a name ending in an index `[k]`, bit k of the vector net named by the rest. `Err` for a
    /// vector net named exactly `name`.
    pub fn named_bit(&self, name: &str) -> Result<Option<u32>, String> {
        if let Some(v) = self.nets.get(name) {
            return match v.as_slice() {
                [x] => Ok(Some(*x)),
                _ => Err(format!("state net {name} is a vector")),
            };
        }
        let Some((base, index)) = name
            .strip_suffix(']')
            .and_then(|n| n.rsplit_once('['))
            .and_then(|(b, i)| Some((b, i.parse::<i64>().ok()?)))
        else {
            return Ok(None);
        };
        let (Some(v), Some(&(l, r))) = (self.nets.get(base), self.ranges.get(base)) else {
            return Ok(None);
        };
        let (lo, hi) = (l.min(r), l.max(r));
        if index < lo || index > hi {
            return Ok(None);
        }
        let off = if l >= r { index - r } else { r - index };
        Ok(Some(v[off as usize]))
    }
}

/// Follows a net back through aliases and inversions to a free variable; returns the variable
/// and the inversion parity, or `None` if another gate is found.
pub fn trace(c: &Circuit, mut v: u32) -> Option<(u32, bool)> {
    let mut parity = false;
    for _ in 0..1_000_000 {
        match c.defs[v as usize] {
            Def::Alias(B::V(x)) => v = x,
            Def::Not(x) => {
                parity = !parity;
                v = x;
            }
            Def::Free => return Some((v, parity)),
            _ => return None,
        }
    }
    None
}
