// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Design units, hierarchy, generate constructs and procedures of the checker's RTL model.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{
    self, Direction, Expr, ExprKind, GateKind, Item, Module, ModuleItem, ParamKind, PortConn,
    PortList, StmtKind, UnitKind,
};

use super::eval::Src;
use super::ty::Ty;
use super::{Builder, Name, R, RtlModel, RtlOptions, Val};
use crate::circ::B;

/// A parameter override.
#[derive(Debug, Clone)]
pub(crate) enum PVal {
    V(Val, Ty),
    T(Ty),
}

pub(crate) struct Port<'x> {
    pub(crate) name: String,
    pub(crate) sig: usize,
    pub(crate) dir: Direction,
    pub(crate) default: Option<&'x Expr>,
}

pub(crate) fn items_flat<'x>(v: &'x [ModuleItem], out: &mut Vec<&'x ModuleItem>) {
    for it in v {
        if let ModuleItem::GenRegion(inner, _) = it {
            items_flat(inner, out);
        } else {
            out.push(it);
        }
    }
}

fn used_modules(v: &[ModuleItem], out: &mut HashSet<String>) {
    for it in v {
        match it {
            ModuleItem::Instance(i) => {
                out.insert(i.module.name.clone());
            }
            ModuleItem::GenRegion(x, _) => used_modules(x, out),
            ModuleItem::GenFor(g) => used_modules(&g.body.items, out),
            ModuleItem::GenIf(g) => {
                used_modules(&g.then.items, out);
                if let Some(e) = &g.els {
                    used_modules(&e.items, out);
                }
            }
            ModuleItem::GenCase(g) => g
                .items
                .iter()
                .for_each(|(_, b)| used_modules(&b.items, out)),
            ModuleItem::GenBlock(b) => used_modules(&b.items, out),
            _ => {}
        }
    }
}

/// The top module: the given one, or the one module that no other module instantiates.
pub(crate) fn top_name(units: &[ast::SourceUnit], top: Option<&String>) -> R<String> {
    if let Some(t) = top {
        return Ok(t.clone());
    }
    // A later definition of a name replaces an earlier one, as in `Builder::modules`.
    let mut modules: HashMap<&String, &Module> = HashMap::new();
    for it in units.iter().flat_map(|u| &u.items) {
        if let Item::Module(m) = it {
            modules.insert(&m.name.name, m);
        }
    }
    let mut used = HashSet::new();
    for m in modules.values() {
        used_modules(&m.items, &mut used);
    }
    let mut c: Vec<&String> = modules
        .iter()
        .filter(|(n, m)| !used.contains(**n) && m.kind == UnitKind::Module)
        .map(|(n, _)| *n)
        .collect();
    c.sort();
    if c.len() != 1 {
        return Err("cannot determine the top module; give --top".into());
    }
    Ok(c[0].clone())
}

impl<'a> Builder<'a> {
    pub(crate) fn run(&mut self, units: &'a [ast::SourceUnit], opts: &RtlOptions) -> R<RtlModel> {
        for u in units {
            for it in &u.items {
                if let Item::Module(m) = it {
                    self.modules.insert(m.name.name.clone(), m);
                }
            }
        }
        for u in units {
            for it in &u.items {
                match it {
                    Item::Package(p) => {
                        let sc = self.scope(0, String::new());
                        self.packages.insert(p.name.name.clone(), sc);
                        for i in &p.items {
                            self.decl(sc, i, &HashSet::new())?;
                        }
                    }
                    Item::Decl(d) => {
                        self.decl(0, d, &HashSet::new())?;
                        self.behave(0, d)?;
                    }
                    Item::Unsupported { what, span } => {
                        return self.fail(*span, format!("{what} not supported"));
                    }
                    Item::Module(_) => {}
                }
            }
        }
        let top = top_name(units, opts.top.as_ref())?;
        let Some(m) = self.modules.get(&top).copied() else {
            return Err(format!("top module {top} not found"));
        };
        let sc = self.scope(0, String::new());
        let mut ov = HashMap::new();
        for (n, e) in &opts.params {
            ov.insert(n.clone(), Err(e.clone()));
        }
        let ports = self.module(sc, m, ov)?;
        let mut model = RtlModel {
            top: top.clone(),
            inputs: vec![],
            outputs: vec![],
            port_order: vec![],
            states: vec![],
            observations: vec![],
            warnings: vec![],
        };
        for p in &ports {
            let s = &self.sigs[p.sig];
            if s.ty.is_array() {
                return self.fail(s.span, "top-level port of unpacked type");
            }
            let scalar = matches!(s.ty.packed, super::ty::Packed::Bit);
            match p.dir {
                Direction::Input => {
                    if s.driver.iter().any(Option::is_some) {
                        return self.fail(
                            s.span,
                            format!("input port {} is driven inside the module", p.name),
                        );
                    }
                    model.port_order.push((true, model.inputs.len()));
                    model.inputs.push((p.name.clone(), s.vars.clone(), scalar));
                }
                Direction::Output => {
                    model.port_order.push((false, model.outputs.len()));
                    model.outputs.push((
                        p.name.clone(),
                        s.vars.iter().map(|v| B::V(*v)).collect(),
                        scalar,
                    ));
                }
                _ => return self.fail(s.span, "inout ports are not supported"),
            }
        }
        // Declaration initializers (SYNTH-REQ-0112).
        let inits = std::mem::take(&mut self.inits);
        for (s, v) in inits {
            if self.sigs[s].driver.iter().any(Option::is_some) {
                continue;
            }
            let pr = self.proc_id();
            let bits: Vec<B> = match v {
                Val::P(b) => b,
                Val::U(es) => es.into_iter().flatten().collect(),
            };
            let sp = self.sigs[s].span;
            for (i, b) in bits.into_iter().enumerate() {
                self.set(s, i, b, pr, sp)?;
            }
        }
        // Undriven bits read as 0 (SYNTH-REQ-0213); primary inputs stay free.
        let inputs: HashSet<u32> = model
            .inputs
            .iter()
            .flat_map(|(_, v, _)| v.iter().copied())
            .collect();
        for s in 0..self.sigs.len() {
            for i in 0..self.sigs[s].vars.len() {
                let v = self.sigs[s].vars[i];
                if self.sigs[s].driver[i].is_none() && !inputs.contains(&v) {
                    self.c.define(v, B::F);
                }
            }
        }
        if opts.observations.len() > 64
            || opts.observations.iter().any(|p| {
                p.name.is_empty() || p.name.len() > 65536 || p.width == 0 || p.width > 4096
            })
            || opts.observations.iter().map(|p| p.width).sum::<usize>() > 262_144
        {
            return Err("observation name/count/bit budget reached".into());
        }
        let wanted: HashSet<&str> = opts.observations.iter().map(|p| p.name.as_str()).collect();
        let mut found = HashMap::new();
        if !wanted.is_empty() {
            for sig in &self.sigs {
                if wanted.contains(sig.name.as_str())
                    && found.insert(sig.name.as_str(), sig).is_some()
                {
                    return Err(format!("ambiguous RTL observation {}", sig.name));
                }
            }
        }
        for probe in &opts.observations {
            let sig = found
                .get(probe.name.as_str())
                .ok_or_else(|| format!("RTL observation {} not found", probe.name))?;
            let end = probe
                .offset
                .checked_add(probe.width)
                .ok_or("observation offset overflow")?;
            if sig.ty.is_array() || end > sig.vars.len() {
                return Err(format!(
                    "RTL observation {} is not a valid packed slice",
                    probe.name
                ));
            }
            model.observations.push((
                probe.clone(),
                sig.vars[probe.offset..end]
                    .iter()
                    .copied()
                    .map(B::V)
                    .collect(),
            ));
        }
        model.states = std::mem::take(&mut self.states);
        model.warnings = std::mem::take(&mut self.warnings);
        Ok(model)
    }

    /// Declares a parameter.
    pub(crate) fn param(
        &mut self,
        sc: usize,
        p: &ast::ParamDecl,
        ov: Option<Result<PVal, Expr>>,
    ) -> R<()> {
        match &p.kind {
            ParamKind::Value {
                ty,
                name,
                unpacked,
                default,
            } => {
                let typed = !(ty.is_implicit() && ty.packed.is_empty() && ty.signing.is_none());
                let dt = if typed {
                    let t = self.resolve(sc, ty, Some(sc))?;
                    Some(self.with_unpacked(sc, t, unpacked)?)
                } else {
                    None
                };
                let (v, t) = match ov {
                    Some(Ok(PVal::V(v, vt))) => match &dt {
                        Some(t) => {
                            let v = match v {
                                Val::P(b) => Val::P(self.ext(&b, t.width() as usize, vt.signed)),
                                u => u,
                            };
                            (v, t.clone())
                        }
                        None => (v, vt),
                    },
                    Some(Ok(PVal::T(_))) => {
                        return self.fail(name.span, "type given for a value parameter");
                    }
                    Some(Err(e)) => {
                        let v = self.const_val(sc, &e, dt.as_ref())?;
                        let t = match &dt {
                            Some(t) => t.clone(),
                            None => self.type_of(sc, &e)?,
                        };
                        (v, t)
                    }
                    None => {
                        let Some(d) = default else {
                            return self.fail(name.span, "parameter without value");
                        };
                        let v = self.const_val(sc, d, dt.as_ref())?;
                        let t = match &dt {
                            Some(t) => t.clone(),
                            None => {
                                let s = self.size(sc, d)?;
                                Ty::bits(u64::from(s.w), s.s)
                            }
                        };
                        (v, t)
                    }
                };
                self.put(sc, &name.name, Name::Value(Rc::new((v, t))), name.span)
            }
            ParamKind::Type { name, default } => {
                let t = match ov {
                    Some(Ok(PVal::T(t))) => t,
                    Some(Err(e)) => match &e.kind {
                        ExprKind::Type(dt) => self.resolve(sc, dt, None)?,
                        ExprKind::Ident(p) => match self.find_path(sc, p)? {
                            Name::Type(t) => t,
                            _ => return self.fail(e.span, "not a type"),
                        },
                        _ => return self.fail(e.span, "not a type"),
                    },
                    Some(Ok(PVal::V(..))) => {
                        return self.fail(name.span, "value given for a type parameter");
                    }
                    None => {
                        let Some(d) = default else {
                            return self.fail(name.span, "type parameter without default");
                        };
                        self.resolve(sc, d, None)?
                    }
                };
                self.put(sc, &name.name, Name::Type(t), name.span)
            }
        }
    }

    pub(crate) fn const_val(&mut self, sc: usize, e: &Expr, t: Option<&Ty>) -> R<Val> {
        let v = match t {
            Some(t) => self.assign_val(sc, e, t)?,
            None => {
                // An unpacked value (such as an unpacked parameter passed on) keeps its shape.
                let et = self.type_of(sc, e)?;
                if et.is_array() {
                    self.assign_val(sc, e, &et)?
                } else {
                    let s = self.size(sc, e)?;
                    Val::P(self.ev(sc, e, &s, s.w, s.s)?)
                }
            }
        };
        let all_const = match &v {
            Val::P(b) => b.iter().all(|x| x.konst().is_some()),
            Val::U(es) => es.iter().flatten().all(|x| x.konst().is_some()),
        };
        if !all_const {
            return self.fail(e.span, "not a constant");
        }
        Ok(v)
    }

    pub(crate) fn module(
        &mut self,
        sc: usize,
        m: &'a Module,
        mut ov: HashMap<String, Result<PVal, Expr>>,
    ) -> R<Vec<Port<'a>>> {
        if m.kind != UnitKind::Module {
            return self.fail(m.name.span, "only modules are supported");
        }
        self.scopes[sc].nettype_none = m.default_nettype.as_deref() == Some("none");
        for i in &m.imports {
            self.scopes[sc].imports.push((
                i.package.name.clone(),
                i.name.as_ref().map(|n| n.name.clone()),
            ));
        }
        let header_params = !m.params.is_empty();
        for p in &m.params {
            let name = match &p.kind {
                ParamKind::Value { name, .. } | ParamKind::Type { name, .. } => name.name.clone(),
            };
            let o = if p.local { None } else { ov.remove(&name) };
            self.param(sc, p, o)?;
        }
        let mut ports: Vec<Port<'a>> = Vec::new();
        if let PortList::Ansi(ps) = &m.ports {
            for p in ps {
                if p.interface.is_some() {
                    return self.fail(p.span, "interface ports are not supported");
                }
                let t = self.resolve(sc, &p.ty, Some(sc))?;
                let t = self.with_unpacked(sc, t, &p.unpacked)?;
                let s = self.new_sig(self.full(sc, &p.name.name), t, p.span)?;
                self.put(sc, &p.name.name, Name::Sig(s), p.name.span)?;
                ports.push(Port {
                    name: p.name.name.clone(),
                    sig: s,
                    dir: p.dir,
                    default: p.default.as_ref(),
                });
            }
        }
        let mut flat = Vec::new();
        items_flat(&m.items, &mut flat);
        let mut dirs: HashMap<String, Direction> = HashMap::new();
        let mut data_names = HashSet::new();
        for it in &flat {
            match it {
                ModuleItem::PortDecl(pd) => {
                    for (n, _, _) in &pd.names {
                        dirs.insert(n.name.clone(), pd.dir);
                    }
                }
                ModuleItem::Data(d) => {
                    for x in &d.decls {
                        data_names.insert(x.name.name.clone());
                    }
                }
                _ => {}
            }
        }
        for it in &flat {
            if let ModuleItem::Param(ps) = it {
                for p in ps {
                    let name = match &p.kind {
                        ParamKind::Value { name, .. } | ParamKind::Type { name, .. } => {
                            name.name.clone()
                        }
                    };
                    let o = if header_params || p.local {
                        None
                    } else {
                        ov.remove(&name)
                    };
                    self.param(sc, p, o)?;
                }
            } else {
                self.decl(sc, it, &data_names)?;
            }
        }
        if let Some(n) = ov.keys().next() {
            return self.fail(m.name.span, format!("no parameter {n}"));
        }
        if let PortList::NonAnsi(list) = &m.ports {
            for p in list {
                let Some(e) = &p.expr else { continue };
                let Some(id) = e.as_ident() else {
                    return self.fail(p.span, "complex port expression");
                };
                let Some(Name::Sig(s)) = self.find(sc, &id.name) else {
                    return self.fail(id.span, "undeclared port");
                };
                let Some(dir) = dirs.get(&id.name).copied() else {
                    return self.fail(id.span, "port without direction");
                };
                let name = p
                    .external
                    .as_ref()
                    .map_or(id.name.clone(), |x| x.name.clone());
                ports.push(Port {
                    name,
                    sig: s,
                    dir,
                    default: None,
                });
            }
        }
        self.implicit_nets(sc, &flat)?;
        for it in &flat {
            self.behave(sc, it)?;
        }
        Ok(ports)
    }

    pub(crate) fn decl(
        &mut self,
        sc: usize,
        it: &'a ModuleItem,
        data_names: &HashSet<String>,
    ) -> R<()> {
        match it {
            ModuleItem::Param(ps) => {
                for p in ps {
                    self.param(sc, p, None)?;
                }
            }
            ModuleItem::Typedef(t) => {
                let ty = self.resolve(sc, &t.ty, Some(sc))?;
                let ty = self.with_unpacked(sc, ty, &t.dims)?;
                self.put(sc, &t.name.name, Name::Type(ty), t.name.span)?;
            }
            ModuleItem::Import(is) => {
                for i in is {
                    self.scopes[sc].imports.push((
                        i.package.name.clone(),
                        i.name.as_ref().map(|n| n.name.clone()),
                    ));
                }
            }
            ModuleItem::Function(f) => {
                self.put(sc, &f.name.name, Name::Func(f, sc), f.name.span)?
            }
            ModuleItem::Task(t) => self.put(sc, &t.name.name, Name::Task(t, sc), t.name.span)?,
            ModuleItem::Genvar(v) => {
                for g in v {
                    self.put(sc, &g.name, Name::Genvar, g.span)?;
                }
            }
            ModuleItem::Data(d) => {
                let t = self.resolve(sc, &d.ty, Some(sc))?;
                for x in &d.decls {
                    let ty = self.with_unpacked(sc, t.clone(), &x.dims)?;
                    let s = self.new_sig(self.full(sc, &x.name.name), ty, x.name.span)?;
                    self.put(sc, &x.name.name, Name::Sig(s), x.name.span)?;
                    if let Some(k @ (ast::NetKind::Supply0 | ast::NetKind::Supply1)) = d.net {
                        let pr = self.proc_id();
                        let n = self.sigs[s].vars.len();
                        for i in 0..n {
                            self.set(
                                s,
                                i,
                                B::from_bool(k == ast::NetKind::Supply1),
                                pr,
                                x.name.span,
                            )?;
                        }
                    }
                }
            }
            ModuleItem::PortDecl(pd) => {
                let t = self.resolve(sc, &pd.ty, Some(sc))?;
                for (n, dims, _) in &pd.names {
                    if data_names.contains(&n.name) {
                        continue;
                    }
                    let ty = self.with_unpacked(sc, t.clone(), dims)?;
                    let s = self.new_sig(self.full(sc, &n.name), ty, n.span)?;
                    self.put(sc, &n.name, Name::Sig(s), n.span)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn implicit_nets(&mut self, sc: usize, items: &[&'a ModuleItem]) -> R<()> {
        let mut names: Vec<(String, Span)> = Vec::new();
        let add = |e: &Expr, names: &mut Vec<(String, Span)>| {
            let mut stack = vec![e];
            while let Some(x) = stack.pop() {
                match &x.kind {
                    ExprKind::Ident(p) if p.scope.is_none() => {
                        names.push((p.name.name.clone(), p.name.span))
                    }
                    ExprKind::Concat(v) => stack.extend(v.iter()),
                    _ => {}
                }
            }
        };
        for it in items {
            match it {
                ModuleItem::Assign(v) => v.iter().for_each(|a| add(&a.lhs, &mut names)),
                ModuleItem::Instance(i) => {
                    for inst in &i.instances {
                        for c in &inst.conns {
                            if let PortConn::Ordered(Some(e)) | PortConn::Named(_, Some(Some(e))) =
                                c
                            {
                                add(e, &mut names);
                            }
                        }
                    }
                }
                ModuleItem::Gate(gs) => gs
                    .iter()
                    .for_each(|g| g.terms.iter().for_each(|t| add(t, &mut names))),
                _ => {}
            }
        }
        for (n, sp) in names {
            if self.find(sc, &n).is_none() {
                if self.scopes[sc].nettype_none {
                    return self.fail(sp, format!("{n} undeclared"));
                }
                let s = self.new_sig(self.full(sc, &n), Ty::bits(1, false), sp)?;
                self.put(sc, &n, Name::Sig(s), sp)?;
            }
        }
        Ok(())
    }

    /// Drives a location with bits from a continuous context.
    pub(crate) fn drive(
        &mut self,
        loc: &super::eval::Loc,
        bits: &[B],
        pr: usize,
        sp: Span,
    ) -> R<()> {
        for (j, alts) in loc.iter().enumerate() {
            match alts.as_slice() {
                [] => {}
                [(Src::Var(true, s, bit), B::T)] => self.set(*s, *bit, bits[j], pr, sp)?,
                _ => {
                    return self.fail(
                        sp,
                        "continuous assignment targets need constant selects of signals",
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn behave(&mut self, sc: usize, it: &'a ModuleItem) -> R<()> {
        match it {
            ModuleItem::Assign(v) => {
                for a in v {
                    let pr = self.proc_id();
                    let (loc, t) = self.target(sc, &a.lhs)?;
                    let val = self.assign_val(sc, &a.rhs, &t)?;
                    let bits: Vec<B> = match val {
                        Val::P(b) => b,
                        Val::U(es) => es.into_iter().flatten().collect(),
                    };
                    self.drive(&loc, &bits, pr, a.span)?;
                }
            }
            ModuleItem::Data(d) => {
                for x in &d.decls {
                    let Some(init) = &x.init else { continue };
                    let Some(Name::Sig(s)) = self.find(sc, &x.name.name) else {
                        continue;
                    };
                    let t = self.sigs[s].ty.clone();
                    let val = self.assign_val(sc, init, &t)?;
                    if d.net.is_some() {
                        let pr = self.proc_id();
                        let bits: Vec<B> = match val {
                            Val::P(b) => b,
                            Val::U(es) => es.into_iter().flatten().collect(),
                        };
                        for (i, b) in bits.into_iter().enumerate() {
                            self.set(s, i, b, pr, x.name.span)?;
                        }
                    } else {
                        self.inits.push((s, val));
                    }
                }
            }
            ModuleItem::Always(a) => self.always(sc, a)?,
            ModuleItem::Instance(i) => self.instance(sc, i)?,
            ModuleItem::Gate(gs) => {
                for g in gs {
                    let n = g.terms.len();
                    let (outs, ins) = if matches!(g.kind, GateKind::Not | GateKind::Buf) {
                        (&g.terms[..n - 1], &g.terms[n - 1..])
                    } else {
                        (&g.terms[..1], &g.terms[1..])
                    };
                    let mut acc: Option<B> = None;
                    for i in ins {
                        let s = self.size(sc, i)?;
                        let v = self.ev(sc, i, &s, s.w, s.s)?[0];
                        acc = Some(match (acc, g.kind) {
                            (None, _) => v,
                            (Some(a), GateKind::And | GateKind::Nand) => self.c.and(a, v),
                            (Some(a), GateKind::Or | GateKind::Nor) => self.c.or(a, v),
                            (Some(a), _) => self.c.xor(a, v),
                        });
                    }
                    let mut r = acc.unwrap_or(B::F);
                    if matches!(
                        g.kind,
                        GateKind::Nand | GateKind::Nor | GateKind::Xnor | GateKind::Not
                    ) {
                        r = self.c.not(r);
                    }
                    let pr = self.proc_id();
                    for o in outs {
                        let (loc, _) = self.target(sc, o)?;
                        let bits = vec![r; loc.len()];
                        let loc1: super::eval::Loc = loc.into_iter().take(1).collect();
                        self.drive(&loc1, &bits, pr, g.span)?;
                    }
                }
            }
            ModuleItem::GenFor(g) => {
                self.scopes[sc].gen_n += 1;
                let n = self.scopes[sc].gen_n;
                let mut v = self.cint(sc, &g.init)?;
                let mut count = 0;
                loop {
                    let t = self.scope(sc, self.scopes[sc].path.clone());
                    let bits: Vec<B> = (0..32).map(|i| B::from_bool((v >> i) & 1 == 1)).collect();
                    self.put(
                        t,
                        &g.genvar.name,
                        Name::Value(Rc::new((Val::P(bits.clone()), Ty::bits(32, true)))),
                        g.genvar.span,
                    )?;
                    if self.cint(t, &g.cond)? == 0 {
                        break;
                    }
                    count += 1;
                    if count > 65_536 {
                        return self.fail(g.span, "generate loop limit");
                    }
                    let base = g
                        .body
                        .label
                        .as_ref()
                        .map_or(format!("genblk{n}"), |l| l.name.clone());
                    let path = self.full(sc, &format!("{base}[{v}]"));
                    let inner = self.scope(sc, path);
                    self.put(
                        inner,
                        &g.genvar.name,
                        Name::Value(Rc::new((Val::P(bits), Ty::bits(32, true)))),
                        g.genvar.span,
                    )?;
                    self.gen_items(inner, &g.body.items)?;
                    v = match &g.step.kind {
                        StmtKind::IncDec { inc, .. } => v + if *inc { 1 } else { -1 },
                        StmtKind::Assign { op, rhs, .. } => {
                            let r = self.cint(t, rhs)?;
                            match op {
                                ast::AssignOp::Eq => r,
                                ast::AssignOp::Compound(ast::BinOp::Add) => v + r,
                                ast::AssignOp::Compound(ast::BinOp::Sub) => v - r,
                                ast::AssignOp::Compound(ast::BinOp::Mul) => v * r,
                                ast::AssignOp::Compound(ast::BinOp::Shl) => v << r,
                                ast::AssignOp::Compound(ast::BinOp::Shr) => v >> r,
                                ast::AssignOp::Compound(ast::BinOp::Div) if r != 0 => v / r,
                                _ => return self.fail(g.step.span, "generate step"),
                            }
                        }
                        _ => return self.fail(g.step.span, "generate step"),
                    };
                }
            }
            ModuleItem::GenIf(g) => {
                self.scopes[sc].gen_n += 1;
                let n = self.scopes[sc].gen_n;
                self.gen_if(sc, g, n)?;
            }
            ModuleItem::GenCase(g) => {
                self.scopes[sc].gen_n += 1;
                let n = self.scopes[sc].gen_n;
                let mut chosen = None;
                let mut default = None;
                'o: for (labels, blk) in &g.items {
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
                        if self.cint(sc, &eq)? != 0 {
                            chosen = Some(blk);
                            break 'o;
                        }
                    }
                }
                if let Some(b) = chosen.or(default) {
                    self.gen_block(sc, b, n)?;
                }
            }
            ModuleItem::GenBlock(b) => {
                self.scopes[sc].gen_n += 1;
                let n = self.scopes[sc].gen_n;
                self.gen_block(sc, b, n)?;
            }
            ModuleItem::ElabTask { name, span, .. } if name == "$error" || name == "$fatal" => {
                return self.fail(*span, format!("{name} reached"));
            }
            ModuleItem::Defparam(sp) => return self.fail(*sp, "defparam"),
            ModuleItem::Unsupported { what, span } => {
                return self.fail(*span, format!("{what} not supported"));
            }
            _ => {}
        }
        Ok(())
    }
}
