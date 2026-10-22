// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Modules, packages, parameters and declarations (SYNTH-REQ-0103 to 0141); instances and
//! generate constructs are in `hier`, continuous assignments and gates in `drive`.

use std::rc::Rc;

use rustc_hash::FxHashSet;
use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{
    self, Direction, Expr, ExprKind, Item, ModuleItem, ParamKind, PortConn, PortList,
};

use super::stmt::ident_expr;
use super::types::Ty;
use super::{EResult, ElabOptions, Elaborator, FuncDef, ScopeId, SigId, Symbol, TVal, TaskDef};
use crate::aig::Lit;

/// A parameter value passed to an instance.
#[derive(Debug, Clone)]
pub(crate) enum ParamValueE {
    Value(TVal),
    Type(Ty),
}

/// Parameter override before or after evaluation.
pub(crate) enum Ovr<'e> {
    Val(ParamValueE),
    Expr(&'e Expr),
}

/// A port of an elaborated module.
#[derive(Debug, Clone)]
pub(crate) struct PortInfo<'a> {
    pub name: String,
    pub sig: SigId,
    pub dir: Direction,
    pub default: Option<&'a Expr>,
    pub span: Span,
}

/// Maximum iterations of a generate loop.
pub(super) const MAX_GENERATE_ITERATIONS: i64 = 65_536;

/// Items of a scope with `generate` regions flattened.
pub(super) fn flatten_items<'x>(items: &'x [ModuleItem], out: &mut Vec<&'x ModuleItem>) {
    for it in items {
        match it {
            ModuleItem::GenRegion(inner, _) => flatten_items(inner, out),
            other => out.push(other),
        }
    }
}

/// Collects names of instantiated modules (including inside generate constructs).
fn instantiated(items: &[ModuleItem], out: &mut FxHashSet<String>) {
    for it in items {
        match it {
            ModuleItem::Instance(i) => {
                out.insert(i.module.name.clone());
            }
            ModuleItem::GenRegion(v, _) => instantiated(v, out),
            ModuleItem::GenFor(g) => instantiated(&g.body.items, out),
            ModuleItem::GenIf(g) => {
                instantiated(&g.then.items, out);
                if let Some(e) = &g.els {
                    instantiated(&e.items, out);
                }
            }
            ModuleItem::GenCase(g) => {
                for (_, b) in &g.items {
                    instantiated(&b.items, out);
                }
            }
            ModuleItem::GenBlock(b) => instantiated(&b.items, out),
            _ => {}
        }
    }
}

impl<'a> Elaborator<'a> {
    /// Elaborates packages, compilation-unit declarations and the top module.
    pub(crate) fn run(
        &mut self,
        units: &'a [ast::SourceUnit],
        opts: &'a ElabOptions,
    ) -> EResult<()> {
        for u in units {
            for it in &u.items {
                match it {
                    Item::Package(p) => self.elab_package(p)?,
                    Item::Decl(d) => {
                        let scope = self.unit_scope;
                        self.decl_item(scope, d, false)?;
                        self.behavior_item(scope, d)?;
                    }
                    Item::Unsupported { what, span } => {
                        return Err(self.err(
                            "S0103",
                            *span,
                            format!("'{what}' is not supported for synthesis"),
                        ));
                    }
                    Item::Module(m) if m.kind != ast::UnitKind::Module => {
                        // Interfaces and programs are rejected only if used.
                    }
                    Item::Module(_) => {}
                }
            }
        }
        let top = self.choose_top(opts.top.as_deref())?;
        let m: &'a ast::Module = self.modules[&top];
        if m.kind != ast::UnitKind::Module {
            return Err(self.err("S0103", m.name.span, "the top unit must be a module"));
        }
        self.top_name = top;
        let scope = self.new_scope(self.unit_scope, String::new());
        let ovr: Vec<(String, Ovr<'a>)> = opts
            .params
            .iter()
            .map(|(n, e)| (n.clone(), Ovr::Expr(e)))
            .collect();
        let ports = self.elab_module(scope, m, ovr)?;
        for p in ports {
            let (packed, w) = {
                let sig = &self.signals[p.sig as usize];
                (sig.ty.is_packed(), sig.elem_width)
            };
            if !packed {
                return Err(self.err(
                    "S0602",
                    p.span,
                    format!(
                        "top-level port '{}' has an unpacked type (SYNTH-REQ-0602)",
                        p.name
                    ),
                ));
            }
            match p.dir {
                Direction::Input => {
                    let proc = self.new_proc("primary input", p.span);
                    for b in 0..w {
                        let l = self.aig.input();
                        self.top_input_nodes.insert(l.node());
                        self.drive(p.sig, b, l, proc, p.span)?;
                    }
                    self.top_ports.push((p.name.clone(), p.sig, true));
                }
                Direction::Output => self.top_ports.push((p.name.clone(), p.sig, false)),
                _ => {
                    return Err(self.err(
                        "S0602",
                        p.span,
                        format!("port '{}': inout and ref ports are not supported", p.name),
                    ));
                }
            }
        }
        self.finish_var_inits()
    }

    fn choose_top(&self, top: Option<&str>) -> EResult<String> {
        if let Some(t) = top {
            if self.modules.contains_key(t) {
                return Ok(t.to_string());
            }
            return Err(self.err(
                "S0104",
                Span::default(),
                format!("top module '{t}' not found"),
            ));
        }
        let mut used = FxHashSet::default();
        for m in self.modules.values() {
            instantiated(&m.items, &mut used);
        }
        let mut cands: Vec<&String> = self
            .modules
            .iter()
            .filter(|(n, m)| !used.contains(*n) && m.kind == ast::UnitKind::Module)
            .map(|(n, _)| n)
            .collect();
        cands.sort();
        match cands.len() {
            1 => Ok(cands[0].clone()),
            0 => Err(self.err(
                "S0104",
                Span::default(),
                "no top module candidate found; use --top",
            )),
            _ => Err(self.err(
                "S0104",
                Span::default(),
                format!(
                    "several top module candidates: {}; use --top",
                    cands
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    fn elab_package(&mut self, p: &'a ast::Package) -> EResult<()> {
        let scope = self.new_scope(self.unit_scope, String::new());
        self.scopes[scope].package = Some(p.name.name.clone());
        self.package_scopes.insert(p.name.name.clone(), scope);
        for it in &p.items {
            if let ModuleItem::Data(d) = it {
                return Err(self.err("S0107", d.span, "package variables are not supported"));
            }
            self.decl_item(scope, it, false)?;
        }
        Ok(())
    }

    /// Elaborates a module body in `scope` and returns its ports in header order.
    pub(crate) fn elab_module(
        &mut self,
        scope: ScopeId,
        m: &'a ast::Module,
        mut ovr: Vec<(String, Ovr<'a>)>,
    ) -> EResult<Vec<PortInfo<'a>>> {
        self.scopes[scope].nettype_none = m.default_nettype.as_deref() == Some("none");
        for i in &m.imports {
            self.scopes[scope].imports.push((
                i.package.name.clone(),
                i.name.as_ref().map(|n| n.name.clone()),
            ));
        }
        let has_param_ports = !m.params.is_empty();
        for p in &m.params {
            let o = if p.local {
                None
            } else {
                self.take_override(scope, p, &mut ovr)?
            };
            self.local_param(scope, p, o)?;
        }
        let mut ports: Vec<PortInfo<'a>> = Vec::new();
        if let PortList::Ansi(ps) = &m.ports {
            for p in ps {
                if p.interface.is_some() {
                    return Err(self.err("S0103", p.span, "interface ports are not supported"));
                }
                let t = self.resolve_type(scope, &p.ty, true)?;
                let t = self.apply_unpacked(scope, t, &p.unpacked)?;
                let name = self.hier_name(scope, &p.name.name);
                let sig = self.new_signal(name, t, p.net.is_some(), p.span)?;
                self.declare(scope, &p.name.name, Symbol::Signal(sig), p.name.span)?;
                ports.push(PortInfo {
                    name: p.name.name.clone(),
                    sig,
                    dir: p.dir,
                    default: p.default.as_ref(),
                    span: p.span,
                });
            }
        }
        let mut flat = Vec::new();
        flatten_items(&m.items, &mut flat);
        // Non-ANSI port declarations; a data declaration of the same name gives the type.
        let mut pending: Vec<(String, Direction, Span)> = Vec::new();
        let mut data_names = FxHashSet::default();
        for it in &flat {
            match it {
                ModuleItem::PortDecl(pd) => {
                    for (n, _, _) in &pd.names {
                        pending.push((n.name.clone(), pd.dir, pd.span));
                    }
                }
                ModuleItem::Data(d) => {
                    for dc in &d.decls {
                        data_names.insert(dc.name.name.clone());
                    }
                }
                _ => {}
            }
        }
        let saved = std::mem::replace(&mut self.port_data_names, data_names);
        for it in &flat {
            let r = if let ModuleItem::Param(ps) = it {
                let mut r = Ok(());
                for p in ps {
                    let o = if has_param_ports || p.local {
                        None
                    } else {
                        self.take_override(scope, p, &mut ovr)?
                    };
                    r = self.local_param(scope, p, o);
                    if r.is_err() {
                        break;
                    }
                }
                r
            } else {
                self.decl_item(scope, it, true)
            };
            if let Err(d) = r {
                self.port_data_names = saved;
                return Err(d);
            }
        }
        self.port_data_names = saved;
        if let Some((n, _)) = ovr.first() {
            return Err(self.err(
                "S0105",
                m.name.span,
                format!("module '{}' has no parameter '{n}'", m.name.name),
            ));
        }
        if let PortList::NonAnsi(list) = &m.ports {
            for p in list {
                let Some(e) = &p.expr else { continue };
                let id = e.as_ident().ok_or_else(|| {
                    self.err(
                        "S0103",
                        p.span,
                        "port expressions other than names are not supported",
                    )
                })?;
                let sym = self.lookup(scope, &id.name);
                let Some(Symbol::Signal(sig)) = sym else {
                    return Err(self.err(
                        "S0106",
                        id.span,
                        format!("port '{}' is not declared", id.name),
                    ));
                };
                let dir = pending
                    .iter()
                    .find(|(n, _, _)| *n == id.name)
                    .map(|x| x.1)
                    .ok_or_else(|| {
                        self.err(
                            "S0106",
                            id.span,
                            format!("port '{}' has no direction declaration", id.name),
                        )
                    })?;
                let ext = p
                    .external
                    .as_ref()
                    .map_or(id.name.clone(), |x| x.name.clone());
                ports.push(PortInfo {
                    name: ext,
                    sig,
                    dir,
                    default: None,
                    span: p.span,
                });
            }
        }
        self.items_behavior(scope, &flat)?;
        Ok(ports)
    }

    fn take_override(
        &mut self,
        scope: ScopeId,
        p: &ast::ParamDecl,
        ovr: &mut Vec<(String, Ovr<'a>)>,
    ) -> EResult<Option<ParamValueE>> {
        let name = match &p.kind {
            ParamKind::Value { name, .. } | ParamKind::Type { name, .. } => name,
        };
        let Some(k) = ovr.iter().position(|(n, _)| *n == name.name) else {
            return Ok(None);
        };
        let (_, o) = ovr.remove(k);
        Ok(Some(match o {
            Ovr::Val(v) => v,
            Ovr::Expr(e) => match &p.kind {
                ParamKind::Type { .. } => match (&e.kind, self.cast_type_name(scope, e)) {
                    (ExprKind::Type(t), _) => {
                        ParamValueE::Type(self.resolve_type(scope, t, false)?)
                    }
                    (_, Some(t)) => ParamValueE::Type(t),
                    _ => {
                        return Err(self.err(
                            "S0232",
                            e.span,
                            "type parameter override must be a type",
                        ));
                    }
                },
                ParamKind::Value { .. } => ParamValueE::Value(self.eval_const(scope, e, None)?),
            },
        }))
    }

    /// Declaration pass for one item (parameters are handled by the caller at module level).
    pub(crate) fn decl_item(
        &mut self,
        scope: ScopeId,
        it: &'a ModuleItem,
        module_level: bool,
    ) -> EResult<()> {
        match it {
            ModuleItem::Param(ps) => {
                for p in ps {
                    self.local_param(scope, p, None)?;
                }
            }
            ModuleItem::Typedef(t) => {
                let ty = self.resolve_type(scope, &t.ty, true)?;
                let ty = self.apply_unpacked(scope, ty, &t.dims)?;
                self.declare(scope, &t.name.name, Symbol::Type(ty), t.name.span)?;
            }
            ModuleItem::Import(is) => {
                for i in is {
                    if !self.package_scopes.contains_key(&i.package.name) {
                        return Err(self.err(
                            "S0103",
                            i.package.span,
                            format!("unknown package '{}'", i.package.name),
                        ));
                    }
                    self.scopes[scope].imports.push((
                        i.package.name.clone(),
                        i.name.as_ref().map(|n| n.name.clone()),
                    ));
                }
            }
            ModuleItem::Function(f) => {
                let def = Rc::new(FuncDef { decl: f, scope });
                self.declare(scope, &f.name.name, Symbol::Func(def), f.name.span)?;
            }
            ModuleItem::Task(t) => {
                let def = Rc::new(TaskDef { decl: t, scope });
                self.declare(scope, &t.name.name, Symbol::Task(def), t.name.span)?;
            }
            ModuleItem::Genvar(vs) => {
                for v in vs {
                    self.declare(scope, &v.name, Symbol::Genvar, v.span)?;
                }
            }
            ModuleItem::Data(d) => {
                if d.is_const {
                    return Err(self.err("S0107", d.span, "const variables are not supported"));
                }
                let base = self.resolve_type(scope, &d.ty, true)?;
                for dc in &d.decls {
                    let t = self.apply_unpacked(scope, base.clone(), &dc.dims)?;
                    let name = self.hier_name(scope, &dc.name.name);
                    let is_net = d.net.is_some();
                    if matches!(
                        d.net,
                        Some(
                            ast::NetKind::Wand
                                | ast::NetKind::Wor
                                | ast::NetKind::Triand
                                | ast::NetKind::Trior
                                | ast::NetKind::Tri0
                                | ast::NetKind::Tri1
                                | ast::NetKind::Trireg
                                | ast::NetKind::Interconnect
                        )
                    ) {
                        return Err(self.err(
                            "S0111",
                            d.span,
                            "this net type is not supported (SYNTH-REQ-0111)",
                        ));
                    }
                    let sig = self.new_signal(name, t, is_net, dc.name.span)?;
                    self.declare(scope, &dc.name.name, Symbol::Signal(sig), dc.name.span)?;
                    if matches!(d.net, Some(ast::NetKind::Supply0 | ast::NetKind::Supply1)) {
                        let v = matches!(d.net, Some(ast::NetKind::Supply1));
                        let proc = self.new_proc("supply net", d.span);
                        let total = self.signals[sig as usize].ph.len() as u32;
                        for b in 0..total {
                            self.drive(sig, b, Lit::from_bool(v), proc, d.span)?;
                        }
                    }
                }
            }
            ModuleItem::PortDecl(pd) => {
                // Declared here unless a data declaration of the same name follows.
                let base = self.resolve_type(scope, &pd.ty, true)?;
                for (n, dims, init) in &pd.names {
                    if init.is_some() {
                        return Err(self.err(
                            "S0107",
                            n.span,
                            "port declaration initializers are not supported",
                        ));
                    }
                    if module_level && self.port_data_names.contains(&n.name) {
                        continue;
                    }
                    let t = self.apply_unpacked(scope, base.clone(), dims)?;
                    let name = self.hier_name(scope, &n.name);
                    let sig = self.new_signal(name, t, !pd.is_var, n.span)?;
                    self.declare(scope, &n.name, Symbol::Signal(sig), n.span)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Behavior pass over items of a scope.
    pub(crate) fn items_behavior(
        &mut self,
        scope: ScopeId,
        items: &[&'a ModuleItem],
    ) -> EResult<()> {
        self.declare_implicit_nets(scope, items)?;
        for it in items {
            if let Err(d) = self.behavior_item(scope, it) {
                self.diags.push(d);
            }
        }
        Ok(())
    }

    /// Implicit 1-bit nets for undeclared names used as continuous-assignment targets or port
    /// connections (IEEE 1800-2017 6.10).
    fn declare_implicit_nets(&mut self, scope: ScopeId, items: &[&'a ModuleItem]) -> EResult<()> {
        let mut names: Vec<&ast::Ident> = Vec::new();
        fn from_expr<'x>(e: &'x Expr, out: &mut Vec<&'x ast::Ident>) {
            match &e.kind {
                ExprKind::Ident(p) if p.scope.is_none() => out.push(&p.name),
                ExprKind::Concat(v) => v.iter().for_each(|x| from_expr(x, out)),
                _ => {}
            }
        }
        for it in items {
            match it {
                ModuleItem::Assign(v) => v.iter().for_each(|a| from_expr(&a.lhs, &mut names)),
                ModuleItem::Instance(i) => {
                    for inst in &i.instances {
                        for c in &inst.conns {
                            match c {
                                PortConn::Ordered(Some(e)) | PortConn::Named(_, Some(Some(e))) => {
                                    from_expr(e, &mut names)
                                }
                                _ => {}
                            }
                        }
                    }
                }
                ModuleItem::Gate(gs) => {
                    for g in gs {
                        g.terms.iter().for_each(|t| from_expr(t, &mut names));
                    }
                }
                _ => {}
            }
        }
        for n in names {
            if self.lookup(scope, &n.name).is_none() {
                if self.scopes[scope].nettype_none {
                    return Err(self.err(
                        "S0106",
                        n.span,
                        format!("'{}' is not declared (`default_nettype none)", n.name),
                    ));
                }
                let name = self.hier_name(scope, &n.name);
                let sig = self.new_signal(name, Ty::Bit { signed: false }, true, n.span)?;
                self.declare(scope, &n.name, Symbol::Signal(sig), n.span)?;
            }
        }
        Ok(())
    }

    fn behavior_item(&mut self, scope: ScopeId, it: &'a ModuleItem) -> EResult<()> {
        match it {
            ModuleItem::Assign(v) => {
                for a in v {
                    let proc = self.new_proc("continuous assignment", a.span);
                    self.cont_assign(scope, &a.lhs, &a.rhs, proc, a.span)?;
                }
            }
            ModuleItem::Data(d) => {
                for dc in &d.decls {
                    let Some(init) = &dc.init else { continue };
                    let target = ident_expr(&dc.name);
                    if d.net.is_some() {
                        let proc = self.new_proc("net declaration assignment", dc.name.span);
                        self.cont_assign(scope, &target, init, proc, dc.name.span)?;
                    } else {
                        let Some(Symbol::Signal(sig)) = self.lookup(scope, &dc.name.name) else {
                            continue;
                        };
                        let ty = self.signals[sig as usize].ty.clone();
                        let v = self.eval_assign(scope, init, &ty)?;
                        let elems = match v {
                            TVal::Packed(b, _) => vec![b],
                            TVal::Unpacked(es, _) => es,
                        };
                        self.var_inits.push((sig, elems, dc.name.span));
                    }
                }
            }
            ModuleItem::Always(a) => self.elab_always(scope, a)?,
            ModuleItem::Initial(s) => self.note_once("initial procedure", s.span),
            ModuleItem::Final(s) => self.note_once("final procedure", s.span),
            ModuleItem::Instance(i) => self.elab_instance(scope, i)?,
            ModuleItem::Gate(gs) => {
                for g in gs {
                    self.elab_gate(scope, g)?;
                }
            }
            ModuleItem::GenFor(g) => self.elab_gen_for(scope, g)?,
            ModuleItem::GenIf(g) => {
                self.scopes[scope].gen_count += 1;
                let n = self.scopes[scope].gen_count;
                self.elab_gen_if(scope, g, n)?;
            }
            ModuleItem::GenCase(g) => {
                self.scopes[scope].gen_count += 1;
                let n = self.scopes[scope].gen_count;
                let mut chosen = None;
                let mut default = None;
                'outer: for (labels, blk) in &g.items {
                    if labels.is_empty() {
                        default = Some(blk);
                        continue;
                    }
                    for l in labels {
                        let eq = Expr::new(
                            ExprKind::Binary(
                                ast::BinOp::Eq,
                                Box::new(g.expr.clone()),
                                Box::new(l.clone()),
                            ),
                            l.span,
                        );
                        if self.eval_const_int(scope, &eq)? != 0 {
                            chosen = Some(blk);
                            break 'outer;
                        }
                    }
                }
                if let Some(b) = chosen.or(default) {
                    self.elab_gen_block(scope, b, n, None)?;
                }
            }
            ModuleItem::GenBlock(b) => {
                self.scopes[scope].gen_count += 1;
                let n = self.scopes[scope].gen_count;
                self.elab_gen_block(scope, b, n, None)?;
            }
            ModuleItem::ElabTask { name, args, span } => {
                let msg = match args.first().map(|a| &a.kind) {
                    Some(ExprKind::Str(s)) => String::from_utf8_lossy(s).into_owned(),
                    _ => String::new(),
                };
                match name.as_str() {
                    "$error" | "$fatal" => {
                        return Err(self.err("S0124", *span, format!("{name}: {msg}")));
                    }
                    "$warning" | "$info" => self.warn("S0124", *span, format!("{name}: {msg}")),
                    _ => {
                        return Err(self.err(
                            "S0151",
                            *span,
                            format!("{name} is not supported at module level"),
                        ));
                    }
                }
            }
            ModuleItem::Defparam(sp) => {
                return Err(self.err("S0122", *sp, "defparam is not supported (SYNTH-REQ-0122)"));
            }
            ModuleItem::Ignored { what, span } => self.note_once(what, *span),
            ModuleItem::Unsupported { what, span } => {
                return Err(self.err(
                    "S0141",
                    *span,
                    format!("{what} is not supported for synthesis"),
                ));
            }
            ModuleItem::GenRegion(v, _) => {
                let mut flat = Vec::new();
                flatten_items(v, &mut flat);
                for i in flat {
                    self.behavior_item(scope, i)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Applies declaration initializers of variables without other drivers (SYNTH-REQ-0112).
    fn finish_var_inits(&mut self) -> EResult<()> {
        let inits = std::mem::take(&mut self.var_inits);
        for (sig, elems, span) in inits {
            let any = self.signals[sig as usize]
                .drivers
                .iter()
                .any(Option::is_some);
            if any {
                let name = self.signals[sig as usize].name.clone();
                self.warn("S0112", span, format!("initializer of '{name}' is ignored because the variable is driven elsewhere"));
                continue;
            }
            let proc = self.new_proc("declaration initializer", span);
            let ew = self.signals[sig as usize].elem_width;
            for (k, e) in elems.iter().enumerate() {
                for (j, l) in e.iter().enumerate() {
                    self.drive(sig, k as u32 * ew + j as u32, *l, proc, span)?;
                }
            }
        }
        Ok(())
    }
}
