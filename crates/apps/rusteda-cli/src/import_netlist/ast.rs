// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Extended structural elaborator (PG/supply dropping, hierarchy flattening, and diagnostics).

use std::collections::{HashMap, HashSet};

use rusteda_core::names::check_hierarchical;
use rusteda_core::netlist::{
    CellId, Conn, GateNetlist, MAX_RANGE_WIDTH, NetId, PinDir, PinRef, PortDir,
};
use rusteda_formats::verilog::ast::{
    AnsiPort, ContAssign, DataDecl, DataType, Dim, Direction, Expr, ExprKind, Instantiation, Item,
    Module, ModuleItem, NetKind, NonAnsiPort, PortConn, PortList, SliceKind, TypeKind, UnOp,
    UnitKind,
};
use rusteda_formats::verilog::diag::Diagnostic as VDiag;
use rusteda_formats::verilog::number::Bit4;
use rusteda_formats::verilog::span::{SourceMap, Span};

use super::{
    C_CELL, C_DECL, C_PG_DROP, C_UNSUP, C_WIDTH, CellCatalog, collect_expr_idents, literal_i64,
    qualify_hier, range_width,
};

fn pin_dir_label(d: PinDir) -> &'static str {
    match d {
        PinDir::Input => "input",
        PinDir::Output => "output",
        PinDir::Inout => "inout",
        PinDir::Other => "other",
    }
}

#[derive(Clone)]
enum ScopeEntry {
    Net {
        range: Option<(i32, i32)>,
        bits: Vec<NetId>,
        is_port: bool,
        typed: bool,
    },
    Supply(bool),
    DroppedPgNet,
    Instance,
}

#[derive(Clone)]
struct ResolvedPort {
    name: String,
    dir: PortDir,
    range: Option<(i32, i32)>,
    typed: bool,
    is_supply: Option<bool>,
    span: Span,
}

struct ExtElaborator<'a> {
    sources: &'a SourceMap,
    catalog: &'a CellCatalog,
    modules: HashMap<&'a str, &'a Module>,
    g: GateNetlist,
    cells: HashMap<String, CellId>,
    diags: Vec<VDiag>,
    bits_left: u64,
    budget: u64,
    pg_conns_dropped: usize,
    supply_ports_dropped: usize,
    first_pg_span: Option<Span>,
}

pub(super) fn elaborate_extended<'a>(
    sources: &'a SourceMap,
    items: &'a [Item],
    catalog: &'a CellCatalog,
    budget: u64,
) -> Result<(GateNetlist, Vec<VDiag>), Vec<VDiag>> {
    let mut diags = Vec::new();
    let mut modules: HashMap<&'a str, &'a Module> = HashMap::new();
    let mut mod_order: Vec<&'a Module> = Vec::new();

    for it in items {
        match it {
            Item::Module(m) => {
                let m = m.as_ref();
                if modules.insert(m.name.name.as_str(), m).is_some() {
                    diags.push(VDiag::error(
                        C_DECL,
                        m.name.span,
                        format!("module '{}' is defined twice", m.name.name),
                    ));
                } else {
                    mod_order.push(m);
                }
            }
            Item::Package(p) => {
                diags.push(VDiag::error(
                    C_UNSUP,
                    p.span,
                    "a package is not supported in a structural netlist",
                ));
            }
            Item::Decl(_) | Item::Unsupported { .. } => diags.push(VDiag::error(
                C_UNSUP,
                Span::default(),
                "items outside a module are not supported in a structural netlist",
            )),
        }
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    if mod_order.is_empty() {
        return Err(vec![VDiag::error(
            C_DECL,
            Span::default(),
            "no module found",
        )]);
    }

    let mut inst_mods: HashSet<&str> = HashSet::new();
    for m in &mod_order {
        for it in &m.items {
            if let ModuleItem::Instance(inst) = it
                && modules.contains_key(inst.module.name.as_str())
            {
                inst_mods.insert(inst.module.name.as_str());
            }
        }
    }
    let roots: Vec<&&Module> = mod_order
        .iter()
        .filter(|m| !inst_mods.contains(m.name.name.as_str()))
        .collect();
    let top = match roots[..] {
        [r] => *r,
        [] => {
            return Err(vec![VDiag::error(
                C_UNSUP,
                mod_order[0].name.span,
                "recursive module instantiation cycle in structural netlist",
            )]);
        }
        _ => {
            return Err(vec![VDiag::error(
                C_UNSUP,
                roots[1].name.span,
                format!(
                    "multiple uninstantiated top modules ('{}' and '{}')",
                    roots[0].name.name, roots[1].name.name
                ),
            )]);
        }
    };

    let g = GateNetlist::new(&top.name.name)
        .map_err(|e| vec![VDiag::error(C_DECL, top.name.span, e.to_string())])?;
    let mut elab = ExtElaborator {
        sources,
        catalog,
        modules,
        g,
        cells: HashMap::new(),
        diags: Vec::new(),
        bits_left: budget,
        budget,
        pg_conns_dropped: 0,
        supply_ports_dropped: 0,
        first_pg_span: None,
    };
    let mut stack = vec![top.name.name.as_str()];
    elab.elaborate_module(top, None, &mut stack);
    if elab.diags.iter().any(VDiag::is_error) {
        return Err(elab.diags);
    }
    if elab.pg_conns_dropped > 0 || elab.supply_ports_dropped > 0 {
        let span = elab.first_pg_span.unwrap_or(top.name.span);
        elab.diags.push(VDiag::warning(
            C_PG_DROP,
            span,
            format!(
                "dropped {} pg_pin connection(s) and {} supply port(s)",
                elab.pg_conns_dropped, elab.supply_ports_dropped
            ),
        ));
    }
    Ok((elab.g, elab.diags))
}

impl<'a> ExtElaborator<'a> {
    fn err(&mut self, code: &'static str, span: Span, msg: impl Into<String>) {
        self.diags.push(VDiag::error(code, span, msg));
    }

    fn err_r<T>(
        &mut self,
        code: &'static str,
        span: Span,
        msg: impl Into<String>,
    ) -> Result<T, ()> {
        self.err(code, span, msg);
        Err(())
    }

    fn map_dir(&mut self, d: Direction, sp: Span) -> PortDir {
        match d {
            Direction::Input => PortDir::Input,
            Direction::Output => PortDir::Output,
            Direction::Inout => PortDir::Inout,
            Direction::Ref => {
                self.err(C_UNSUP, sp, "ref ports are not supported");
                PortDir::Inout
            }
        }
    }

    fn const_int(&self, e: &Expr, what: &str) -> Result<i64, VDiag> {
        let val = match &e.kind {
            ExprKind::Number(l) => literal_i64(l),
            ExprKind::Unary(UnOp::Minus, inner) => match &inner.kind {
                ExprKind::Number(l) if l.plain => literal_i64(l).map(|v| -v),
                _ => None,
            },
            _ => None,
        };
        val.ok_or_else(|| VDiag::error(C_UNSUP, e.span, format!("unsupported {what} expression")))
    }

    fn bound(&self, e: &Expr) -> Result<i32, VDiag> {
        let v = self.const_int(e, "range bound")?;
        i32::try_from(v)
            .ok()
            .filter(|&v| v != i32::MIN)
            .ok_or_else(|| VDiag::error(C_WIDTH, e.span, format!("range bound {v} is too large")))
    }

    fn parse_range(&self, ty: &DataType) -> Result<Option<(i32, i32)>, VDiag> {
        if !matches!(ty.kind, TypeKind::Implicit | TypeKind::Logic) {
            return Err(VDiag::error(
                C_UNSUP,
                ty.span,
                "unsupported data type in structural netlist",
            ));
        }
        match &ty.packed[..] {
            [] => Ok(None),
            [Dim::Range(msb, lsb)] => {
                let (m, l) = (self.bound(msb)?, self.bound(lsb)?);
                if range_width(m, l) > u64::from(MAX_RANGE_WIDTH) {
                    return Err(VDiag::error(
                        C_WIDTH,
                        msb.span.to(lsb.span),
                        format!("range [{m}:{l}] is wider than {MAX_RANGE_WIDTH} bits"),
                    ));
                }
                Ok(Some((m, l)))
            }
            _ => Err(VDiag::error(
                C_UNSUP,
                ty.span,
                "multi-dimensional or size dimension is not supported",
            )),
        }
    }

    fn charge(&mut self, name: &str, range: Option<(i32, i32)>, span: Span) -> bool {
        let Some((msb, lsb)) = range else { return true };
        if let Some(left) = self.bits_left.checked_sub(range_width(msb, lsb)) {
            self.bits_left = left;
            true
        } else {
            self.err(
                C_WIDTH,
                span,
                format!(
                    "range [{msb}:{lsb}] of '{name}' exceeds {} bits budget",
                    self.budget
                ),
            );
            false
        }
    }

    fn resolve_ports(&mut self, m: &Module) -> (Vec<ResolvedPort>, bool) {
        match &m.ports {
            PortList::None => (Vec::new(), false),
            PortList::Ansi(ports) => (
                ports
                    .iter()
                    .filter_map(|p| self.resolve_ansi_port(p))
                    .collect(),
                false,
            ),
            PortList::NonAnsi(ports) => (self.resolve_non_ansi_ports(m, ports), true),
        }
    }

    fn resolve_ansi_port(&mut self, p: &AnsiPort) -> Option<ResolvedPort> {
        let dir = self.map_dir(p.dir, p.span);
        let is_supply = match p.net {
            Some(NetKind::Wire) => None,
            Some(NetKind::Supply0) => Some(false),
            Some(NetKind::Supply1) => Some(true),
            None => {
                self.err(
                    C_UNSUP,
                    p.span,
                    format!("variable port '{}' is not supported", p.name.name),
                );
                None
            }
            Some(_) => {
                self.err(
                    C_UNSUP,
                    p.span,
                    "unsupported net port kind in structural netlist",
                );
                None
            }
        };
        let range = self
            .parse_range(&p.ty)
            .map_err(|d| self.diags.push(d))
            .ok()?;
        Some(ResolvedPort {
            name: p.name.name.clone(),
            dir,
            range,
            typed: true,
            is_supply,
            span: p.name.span,
        })
    }

    fn resolve_non_ansi_ports(&mut self, m: &Module, ports: &[NonAnsiPort]) -> Vec<ResolvedPort> {
        let mut header = Vec::new();
        for p in ports {
            if p.external.is_none()
                && let Some(id) = p.expr.as_ref().and_then(Expr::as_ident)
            {
                header.push(id);
            } else {
                self.err(
                    C_UNSUP,
                    p.span,
                    "unsupported port expression in module header",
                );
            }
        }
        let mut decls: HashMap<&str, ResolvedPort> = HashMap::new();
        for item in &m.items {
            let ModuleItem::PortDecl(d) = item else {
                continue;
            };
            let dir = self.map_dir(d.dir, d.span);
            let is_supply = match d.net {
                None | Some(NetKind::Wire) if !d.is_var => None,
                Some(NetKind::Supply0) => Some(false),
                Some(NetKind::Supply1) => Some(true),
                _ => {
                    self.err(
                        C_UNSUP,
                        d.span,
                        "unsupported port net/var kind in structural netlist",
                    );
                    None
                }
            };
            let Ok(range) = self
                .parse_range(&d.ty)
                .map_err(|diag| self.diags.push(diag))
            else {
                continue;
            };
            let typed = d.net.is_some() || d.is_var;
            for (ident, dims, init) in &d.names {
                if !dims.is_empty() || init.is_some() {
                    self.err(
                        C_UNSUP,
                        ident.span,
                        "unpacked dimension or initializer on port is not supported",
                    );
                }
                if !header.iter().any(|h| h.name == ident.name) {
                    self.err(
                        C_DECL,
                        ident.span,
                        format!("'{}' is not in the port list", ident.name),
                    );
                } else if decls
                    .insert(
                        ident.name.as_str(),
                        ResolvedPort {
                            name: ident.name.clone(),
                            dir,
                            range,
                            typed,
                            is_supply,
                            span: ident.span,
                        },
                    )
                    .is_some()
                {
                    self.err(
                        C_DECL,
                        ident.span,
                        format!("port '{}' is declared twice", ident.name),
                    );
                }
            }
        }
        let mut out = Vec::with_capacity(header.len());
        for h in header {
            if let Some(rp) = decls.remove(h.name.as_str()) {
                out.push(rp);
            } else {
                self.err(
                    C_DECL,
                    h.span,
                    format!("port '{}' has no direction declaration", h.name),
                );
            }
        }
        out
    }

    fn scan_pg_only_nets(&self, m: &'a Module) -> HashSet<&'a str> {
        let (mut pg_used, mut sig_used) = (HashSet::new(), HashSet::new());
        for item in &m.items {
            match item {
                ModuleItem::Instance(inst) => {
                    let cinfo = self.catalog.cells.get(&inst.module.name);
                    for i in &inst.instances {
                        for c in &i.conns {
                            if let PortConn::Named(pin, Some(Some(e))) = c {
                                if cinfo.is_some_and(|ci| ci.pg_pins.contains(&pin.name)) {
                                    collect_expr_idents(e, &mut pg_used);
                                } else {
                                    collect_expr_idents(e, &mut sig_used);
                                }
                            }
                        }
                    }
                }
                ModuleItem::Assign(assigns) => {
                    for a in assigns {
                        collect_expr_idents(&a.lhs, &mut sig_used);
                        collect_expr_idents(&a.rhs, &mut sig_used);
                    }
                }
                _ => {}
            }
        }
        pg_used.retain(|n| !sig_used.contains(n));
        pg_used
    }

    fn add_hierarchical_nets(
        &mut self,
        hname: &str,
        range: Option<(i32, i32)>,
        span: Span,
    ) -> Option<Vec<NetId>> {
        if let Err(e) = check_hierarchical(hname) {
            self.err(C_DECL, span, e.to_string());
            return None;
        }
        let indices: Vec<Option<i32>> = match range {
            None => vec![None],
            Some((msb, lsb)) if msb >= lsb => (lsb..=msb).rev().map(Some).collect(),
            Some((msb, lsb)) => (msb..=lsb).map(Some).collect(),
        };
        let mut bits = Vec::with_capacity(indices.len());
        for idx in indices {
            match self.g.add_net(Some(hname), idx) {
                Ok(id) => bits.push(id),
                Err(e) => {
                    self.err(C_DECL, span, e.to_string());
                    return None;
                }
            }
        }
        Some(bits)
    }

    fn elaborate_module(
        &mut self,
        m: &'a Module,
        prefix: Option<&str>,
        stack: &mut Vec<&'a str>,
    ) -> HashMap<String, (PortDir, Vec<Conn>)> {
        if m.kind != UnitKind::Module {
            self.err(
                C_UNSUP,
                m.span,
                "interfaces and programs are not supported in a structural netlist",
            );
        }
        if let Some(p) = m.params.first() {
            self.err(
                C_UNSUP,
                p.span,
                "parameters are not supported in a structural netlist",
            );
        }
        let (ports, non_ansi) = self.resolve_ports(m);
        let pg_only = self.scan_pg_only_nets(m);
        let mut scope: HashMap<String, ScopeEntry> = HashMap::new();
        let mut port_conns: HashMap<String, (PortDir, Vec<Conn>)> = HashMap::new();

        for rp in ports {
            if let Some(val) = rp.is_supply {
                self.supply_ports_dropped += 1;
                self.first_pg_span.get_or_insert(rp.span);
                scope.insert(rp.name, ScopeEntry::Supply(val));
                continue;
            }
            if pg_only.contains(rp.name.as_str()) {
                self.supply_ports_dropped += 1;
                self.first_pg_span.get_or_insert(rp.span);
                scope.insert(rp.name, ScopeEntry::DroppedPgNet);
                continue;
            }
            if !self.charge(&rp.name, rp.range, rp.span) {
                continue;
            }
            let hname = qualify_hier(prefix, &rp.name);
            let bits = if prefix.is_none() {
                match self.g.add_port(&hname, rp.dir, rp.range) {
                    Ok(k) => self.g.ports()[k].bits.clone(),
                    Err(e) => {
                        self.err(C_DECL, rp.span, e.to_string());
                        continue;
                    }
                }
            } else {
                let Some(b) = self.add_hierarchical_nets(&hname, rp.range, rp.span) else {
                    continue;
                };
                b
            };
            port_conns.insert(
                rp.name.clone(),
                (rp.dir, bits.iter().map(|&b| Conn::Net(b)).collect()),
            );
            scope.insert(
                rp.name,
                ScopeEntry::Net {
                    range: rp.range,
                    bits,
                    is_port: true,
                    typed: rp.typed,
                },
            );
        }

        for item in &m.items {
            match item {
                ModuleItem::PortDecl(_) if non_ansi => {}
                ModuleItem::PortDecl(d) => {
                    self.err(
                        C_UNSUP,
                        d.span,
                        "port declaration in a module with an ANSI port list",
                    );
                }
                ModuleItem::Data(d) => {
                    self.elaborate_data_decl(d, non_ansi, prefix, &pg_only, &mut scope);
                }
                ModuleItem::Instance(inst) => {
                    self.elaborate_instantiation(inst, prefix, &mut scope, stack);
                }
                ModuleItem::Assign(assigns) => {
                    for a in assigns {
                        self.elaborate_assign(a, prefix, &m.default_nettype, &mut scope);
                    }
                }
                other => {
                    let span = match other {
                        ModuleItem::Always(a) => a.span,
                        ModuleItem::Initial(s) | ModuleItem::Final(s) => s.span,
                        ModuleItem::Param(v) => v.first().map_or(m.span, |p| p.span),
                        ModuleItem::Gate(v) => v.first().map_or(m.span, |g| g.span),
                        _ => m.span,
                    };
                    self.err(C_UNSUP, span, "unsupported construct in structural netlist");
                }
            }
        }
        port_conns
    }

    fn elaborate_data_decl(
        &mut self,
        d: &DataDecl,
        non_ansi: bool,
        prefix: Option<&str>,
        pg_only: &HashSet<&str>,
        scope: &mut HashMap<String, ScopeEntry>,
    ) {
        let supply_val = match d.net {
            Some(NetKind::Wire) => None,
            Some(NetKind::Supply0) => Some(false),
            Some(NetKind::Supply1) => Some(true),
            _ => {
                self.err(
                    C_UNSUP,
                    d.span,
                    "only wire, supply0, and supply1 declarations are supported",
                );
                return;
            }
        };
        let Ok(range) = self
            .parse_range(&d.ty)
            .map_err(|diag| self.diags.push(diag))
        else {
            return;
        };

        for decl in &d.decls {
            if !decl.dims.is_empty() || decl.init.is_some() {
                self.err(
                    C_UNSUP,
                    decl.name.span,
                    "net arrays and net declaration assignments are not supported",
                );
                continue;
            }
            if let Some(val) = supply_val {
                scope.insert(decl.name.name.clone(), ScopeEntry::Supply(val));
                continue;
            }
            if pg_only.contains(decl.name.name.as_str()) {
                scope.insert(decl.name.name.clone(), ScopeEntry::DroppedPgNet);
                continue;
            }
            if non_ansi
                && let Some(ScopeEntry::Net {
                    range: pr,
                    is_port: true,
                    typed,
                    ..
                }) = scope.get_mut(&decl.name.name)
            {
                if *typed {
                    self.err(
                        C_DECL,
                        decl.name.span,
                        format!("port '{}' is already completely declared", decl.name.name),
                    );
                } else if *pr != range {
                    self.err(
                        C_DECL,
                        decl.name.span,
                        format!(
                            "wire '{}' does not match the range of port '{}'",
                            decl.name.name, decl.name.name
                        ),
                    );
                } else {
                    *typed = true;
                }
                continue;
            }
            if scope.contains_key(&decl.name.name) {
                self.err(
                    C_DECL,
                    decl.name.span,
                    format!("'{}' is already declared", decl.name.name),
                );
                continue;
            }
            if !self.charge(&decl.name.name, range, decl.name.span) {
                continue;
            }
            let hname = qualify_hier(prefix, &decl.name.name);
            if let Some(bits) = self.add_hierarchical_nets(&hname, range, decl.name.span) {
                scope.insert(
                    decl.name.name.clone(),
                    ScopeEntry::Net {
                        range,
                        bits,
                        is_port: false,
                        typed: true,
                    },
                );
            }
        }
    }

    fn elaborate_instantiation(
        &mut self,
        inst: &'a Instantiation,
        prefix: Option<&str>,
        scope: &mut HashMap<String, ScopeEntry>,
        stack: &mut Vec<&'a str>,
    ) {
        if !inst.params.is_empty() {
            self.err(
                C_UNSUP,
                inst.span,
                "parameter value assignment is not supported in a structural netlist",
            );
            return;
        }
        if let Some(&submod) = self.modules.get(inst.module.name.as_str()) {
            if stack.contains(&inst.module.name.as_str()) {
                self.err(
                    C_UNSUP,
                    inst.module.span,
                    format!(
                        "recursive instantiation of module '{}' (hierarchy)",
                        inst.module.name
                    ),
                );
                return;
            }
            for i in &inst.instances {
                if !i.dims.is_empty() {
                    self.err(C_UNSUP, i.span, "instance arrays are not supported");
                    continue;
                }
                if scope.contains_key(&i.name.name) {
                    self.err(
                        C_DECL,
                        i.name.span,
                        format!("'{}' is already declared", i.name.name),
                    );
                    continue;
                }
                scope.insert(i.name.name.clone(), ScopeEntry::Instance);
                let child_prefix = qualify_hier(prefix, &i.name.name);
                stack.push(inst.module.name.as_str());
                let sub_ports = self.elaborate_module(submod, Some(&child_prefix), stack);
                stack.pop();

                let mut connected = HashSet::new();
                for c in &i.conns {
                    let PortConn::Named(pin, opt_e) = c else {
                        self.err(C_UNSUP, i.span, "only named port connections are supported");
                        continue;
                    };
                    let Some(opt_e) = opt_e else {
                        self.err(
                            C_UNSUP,
                            pin.span,
                            format!("implicit connection '.{}' is not supported", pin.name),
                        );
                        continue;
                    };
                    let Some((pdir, inner_bits)) = sub_ports.get(&pin.name) else {
                        self.err(
                            C_CELL,
                            pin.span,
                            format!("module '{}' has no port '{}'", inst.module.name, pin.name),
                        );
                        continue;
                    };
                    if !connected.insert(pin.name.as_str()) {
                        self.err(
                            C_CELL,
                            pin.span,
                            format!(
                                "port '{}' of '{}' is connected twice",
                                pin.name, i.name.name
                            ),
                        );
                        continue;
                    }
                    let Some(e) = opt_e else { continue };
                    if let Ok(outer_bits) = self.eval_expr(e, true, prefix, &None, scope) {
                        match pdir {
                            PortDir::Input => self.connect_bus(inner_bits, &outer_bits, e.span),
                            PortDir::Output | PortDir::Inout => {
                                self.connect_bus(&outer_bits, inner_bits, e.span);
                            }
                        }
                    }
                }
            }
            return;
        }

        let Some(cinfo) = self.catalog.cells.get(&inst.module.name) else {
            self.err(
                C_CELL,
                inst.module.span,
                format!("unknown cell '{}'", inst.module.name),
            );
            return;
        };
        if !cinfo.conflicts.is_empty() {
            for c in &cinfo.conflicts {
                self.err(
                    C_CELL,
                    inst.module.span,
                    format!(
                        "cell '{}' pin '{}' has conflicting directions across corners: '{}' in corner '{}' vs '{}' in corner '{}'",
                        inst.module.name,
                        c.pin,
                        pin_dir_label(c.dir_a),
                        c.corner_a,
                        pin_dir_label(c.dir_b),
                        c.corner_b
                    ),
                );
            }
            return;
        }
        let cell_id = if let Some(&id) = self.cells.get(&inst.module.name) {
            id
        } else {
            match self.g.add_cell_type(&inst.module.name, &cinfo.pins) {
                Ok(id) => {
                    self.cells.insert(inst.module.name.clone(), id);
                    id
                }
                Err(e) => {
                    self.err(C_CELL, inst.module.span, e.to_string());
                    return;
                }
            }
        };

        for i in &inst.instances {
            if !i.dims.is_empty() {
                self.err(C_UNSUP, i.span, "instance arrays are not supported");
                continue;
            }
            if scope.contains_key(&i.name.name) {
                self.err(
                    C_DECL,
                    i.name.span,
                    format!("'{}' is already declared", i.name.name),
                );
                continue;
            }
            let hname = qualify_hier(prefix, &i.name.name);
            let inst_id = match self.g.add_instance(&hname, cell_id) {
                Ok(id) => id,
                Err(e) => {
                    self.err(C_DECL, i.name.span, e.to_string());
                    continue;
                }
            };
            scope.insert(i.name.name.clone(), ScopeEntry::Instance);

            let mut connected = HashSet::new();
            for c in &i.conns {
                let (pin, opt_e) = match c {
                    PortConn::Named(pin, Some(opt_e)) => (pin, opt_e),
                    PortConn::Named(pin, None) => {
                        self.err(
                            C_UNSUP,
                            pin.span,
                            format!("implicit connection '.{}' is not supported", pin.name),
                        );
                        continue;
                    }
                    PortConn::Ordered(e) => {
                        self.err(
                            C_UNSUP,
                            e.as_ref().map_or(i.span, |x| x.span),
                            "positional connections are not supported",
                        );
                        continue;
                    }
                    PortConn::Wildcard(sp) => {
                        self.err(C_UNSUP, *sp, "'.*' connections are not supported");
                        continue;
                    }
                };
                if cinfo.pg_pins.contains(&pin.name) {
                    self.pg_conns_dropped += 1;
                    self.first_pg_span.get_or_insert(pin.span);
                    continue;
                }
                let Some(&pin_idx) = cinfo.pin_index.get(&pin.name) else {
                    self.err(
                        C_CELL,
                        pin.span,
                        format!("cell '{}' has no pin '{}'", inst.module.name, pin.name),
                    );
                    continue;
                };
                if !connected.insert(pin_idx) {
                    self.err(
                        C_CELL,
                        pin.span,
                        format!("pin '{}' of '{}' is connected twice", pin.name, i.name.name),
                    );
                    continue;
                }
                let Some(e) = opt_e else { continue };
                if let Ok(bits) = self.eval_expr(e, true, prefix, &None, scope) {
                    match bits[..] {
                        [conn] => {
                            if let Err(err) = self.g.connect(inst_id, PinRef::Index(pin_idx), conn)
                            {
                                self.err(C_CELL, pin.span, err.to_string());
                            }
                        }
                        _ => self.err(
                            C_WIDTH,
                            e.span,
                            format!("pin '{}' takes 1 bit, got {} bits", pin.name, bits.len()),
                        ),
                    }
                }
            }
        }
    }

    fn elaborate_assign(
        &mut self,
        a: &ContAssign,
        prefix: Option<&str>,
        default_nettype: &Option<String>,
        scope: &mut HashMap<String, ScopeEntry>,
    ) {
        let lhs = self.eval_expr(&a.lhs, true, prefix, default_nettype, scope);
        let rhs = self.eval_expr(&a.rhs, false, prefix, default_nettype, scope);
        if let (Ok(l), Ok(r)) = (lhs, rhs) {
            self.connect_bus(&l, &r, a.span);
        }
    }

    fn connect_bus(&mut self, lhs: &[Conn], rhs: &[Conn], span: Span) {
        if lhs.len() != rhs.len() {
            self.err(
                C_WIDTH,
                span,
                format!("cannot assign {} bit(s) to {} bit(s)", rhs.len(), lhs.len()),
            );
            return;
        }
        for (&l, &r) in lhs.iter().zip(rhs.iter()) {
            let Conn::Net(lhs_net) = l else {
                self.err(
                    C_UNSUP,
                    span,
                    "a constant cannot be on the left-hand side of an assignment",
                );
                return;
            };
            let _ = self.g.add_assign(lhs_net, r);
        }
    }

    fn eval_expr(
        &mut self,
        e: &Expr,
        allow_implicit: bool,
        prefix: Option<&str>,
        default_nettype: &Option<String>,
        scope: &mut HashMap<String, ScopeEntry>,
    ) -> Result<Vec<Conn>, ()> {
        match &e.kind {
            ExprKind::Ident(p) if p.scope.is_none() => {
                let name = &p.name.name;
                match scope.get(name) {
                    Some(ScopeEntry::Net { bits, .. }) => {
                        return Ok(bits.iter().map(|&b| Conn::Net(b)).collect());
                    }
                    Some(ScopeEntry::Supply(val)) => return Ok(vec![Conn::Const(*val)]),
                    Some(ScopeEntry::DroppedPgNet) => return Ok(vec![Conn::Const(false)]),
                    Some(ScopeEntry::Instance) => {
                        return self.err_r(
                            C_DECL,
                            e.span,
                            format!("'{name}' is an instance, not a net"),
                        );
                    }
                    None if !allow_implicit => {
                        return self.err_r(C_DECL, e.span, format!("'{name}' is not declared"));
                    }
                    None => {}
                }
                if let Some(nt) = default_nettype.as_deref()
                    && nt != "wire"
                {
                    return self.err_r(
                        C_DECL,
                        e.span,
                        format!("implicit net '{name}' is not allowed under `default_nettype {nt}"),
                    );
                }
                let hname = qualify_hier(prefix, name);
                let bits = self.add_hierarchical_nets(&hname, None, e.span).ok_or(())?;
                let conn = Conn::Net(bits[0]);
                scope.insert(
                    name.clone(),
                    ScopeEntry::Net {
                        range: None,
                        bits,
                        is_port: false,
                        typed: true,
                    },
                );
                Ok(vec![conn])
            }
            ExprKind::Index(base, idx_expr) => {
                let Some(id) = base.as_ident() else {
                    return self.err_r(
                        C_UNSUP,
                        e.span,
                        "only bit-selects on identifiers are supported",
                    );
                };
                let k = self
                    .const_int(idx_expr, "bit index")
                    .map_err(|d| self.diags.push(d))?;
                self.bit_of(&id.name, k, e.span, scope).map(|c| vec![c])
            }
            ExprKind::Slice {
                base,
                left,
                right,
                kind: SliceKind::Const,
            } => {
                let Some(id) = base.as_ident() else {
                    return self.err_r(
                        C_UNSUP,
                        e.span,
                        "only part-selects on identifiers are supported",
                    );
                };
                let msb = self
                    .const_int(left, "part-select bound")
                    .map_err(|d| self.diags.push(d))?;
                let lsb = self
                    .const_int(right, "part-select bound")
                    .map_err(|d| self.diags.push(d))?;
                let indices: Vec<i64> = if msb >= lsb {
                    (lsb..=msb).rev().collect()
                } else {
                    (msb..=lsb).collect()
                };
                indices
                    .into_iter()
                    .map(|k| self.bit_of(&id.name, k, e.span, scope))
                    .collect()
            }
            ExprKind::Concat(items) => {
                let mut out = Vec::new();
                for item in items {
                    out.extend(self.eval_expr(
                        item,
                        allow_implicit,
                        prefix,
                        default_nettype,
                        scope,
                    )?);
                }
                if out.is_empty() {
                    return self.err_r(C_UNSUP, e.span, "empty concatenation");
                }
                Ok(out)
            }
            ExprKind::Repl(count_expr, items) => {
                let Ok(v) = self.const_int(count_expr, "replication count") else {
                    return self.err_r(C_WIDTH, count_expr.span, "invalid replication count");
                };
                if !(1..=i64::from(MAX_RANGE_WIDTH)).contains(&v) {
                    return self.err_r(C_WIDTH, count_expr.span, "invalid replication count");
                }
                let mut body = Vec::new();
                for item in items {
                    body.extend(self.eval_expr(
                        item,
                        allow_implicit,
                        prefix,
                        default_nettype,
                        scope,
                    )?);
                }
                let mut out = Vec::with_capacity(body.len() * (v as usize));
                for _ in 0..v {
                    out.extend_from_slice(&body);
                }
                Ok(out)
            }
            ExprKind::Number(l) if !l.plain && !l.is_truncated() => {
                let Some(w) = l.size else {
                    return self.err_r(C_UNSUP, e.span, "unsized constant in structural netlist");
                };
                if w == 0 || w > MAX_RANGE_WIDTH {
                    return self.err_r(C_WIDTH, e.span, format!("invalid constant width {w}"));
                }
                let Ok(bits) = l.bits() else {
                    return self.err_r(C_UNSUP, e.span, "invalid integer literal");
                };
                let mut out = Vec::with_capacity(bits.len());
                for b in bits.into_iter().rev() {
                    match b {
                        Bit4::Zero => out.push(Conn::Const(false)),
                        Bit4::One => out.push(Conn::Const(true)),
                        Bit4::X | Bit4::Z => {
                            return self.err_r(C_UNSUP, e.span, "x/z literals are not supported");
                        }
                    }
                }
                Ok(out)
            }
            ExprKind::UnbasedUnsized('0') => Ok(vec![Conn::Const(false)]),
            ExprKind::UnbasedUnsized('1') => Ok(vec![Conn::Const(true)]),
            _ => {
                let snippet = self.sources.snippet(e.span).trim();
                self.err_r(
                    C_UNSUP,
                    e.span,
                    format!("unsupported expression '{snippet}' in a structural netlist"),
                )
            }
        }
    }

    fn bit_of(
        &mut self,
        name: &str,
        k: i64,
        span: Span,
        scope: &HashMap<String, ScopeEntry>,
    ) -> Result<Conn, ()> {
        match scope.get(name) {
            Some(ScopeEntry::Net {
                range: Some((msb, lsb)),
                bits,
                ..
            }) => {
                let (m, l) = (i64::from(*msb), i64::from(*lsb));
                let pos = if m >= l { m - k } else { k - m };
                if (m.min(l)..=m.max(l)).contains(&k) {
                    Ok(Conn::Net(bits[pos as usize]))
                } else {
                    self.err_r(
                        C_WIDTH,
                        span,
                        format!("bit {k} is outside the range [{m}:{l}] of '{name}'"),
                    )
                }
            }
            Some(ScopeEntry::Net { range: None, .. }) => self.err_r(
                C_WIDTH,
                span,
                format!("'{name}' is a scalar and has no bit {k}"),
            ),
            Some(ScopeEntry::Instance) => {
                self.err_r(C_DECL, span, format!("'{name}' is an instance, not a net"))
            }
            _ => self.err_r(C_DECL, span, format!("'{name}' is not declared")),
        }
    }
}
