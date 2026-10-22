// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Generate constructs and module instances of the checker's RTL model.

use std::collections::{HashMap, HashSet};

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{
    self, Direction, Expr, ExprKind, GenBlock, ModuleItem, ParamKind, ParamValue, PortConn,
};

use super::elab::PVal;
use super::elab::Port;
use super::elab::items_flat;
use super::ty::Ty;
use super::{Builder, Name, R, Val};
use crate::circ::B;

impl<'a> Builder<'a> {
    pub(crate) fn gen_if(&mut self, sc: usize, g: &'a ast::GenIf, n: u32) -> R<()> {
        let c = self.cint(sc, &g.cond)? != 0;
        let Some(b) = (if c { Some(&g.then) } else { g.els.as_ref() }) else {
            return Ok(());
        };
        if b.label.is_none() && !b.has_begin && b.items.len() == 1 {
            if let ModuleItem::GenIf(inner) = &b.items[0] {
                return self.gen_if(sc, inner, n);
            }
        }
        self.gen_block(sc, b, n)
    }

    pub(crate) fn gen_block(&mut self, sc: usize, b: &'a GenBlock, n: u32) -> R<()> {
        let base = b
            .label
            .as_ref()
            .map_or(format!("genblk{n}"), |l| l.name.clone());
        let path = self.full(sc, &base);
        let inner = self.scope(sc, path);
        self.gen_items(inner, &b.items)
    }

    pub(crate) fn gen_items(&mut self, sc: usize, items: &'a [ModuleItem]) -> R<()> {
        let mut flat = Vec::new();
        items_flat(items, &mut flat);
        for it in &flat {
            self.decl(sc, it, &HashSet::new())?;
        }
        self.implicit_nets(sc, &flat)?;
        for it in &flat {
            self.behave(sc, it)?;
        }
        Ok(())
    }

    pub(crate) fn instance(&mut self, sc: usize, inst: &'a ast::Instantiation) -> R<()> {
        let Some(m) = self.modules.get(&inst.module.name).copied() else {
            return self.fail(
                inst.module.span,
                format!("unknown module {}", inst.module.name),
            );
        };
        // Build the checker's declaration catalog independently of synthesis.
        // Legacy parameter declarations occur in the body; a parameter-port
        // list instead makes body declarations local to the module.
        let mut declarations = Vec::new();
        if m.params.is_empty() {
            for item in &m.items {
                if let ModuleItem::Param(params) = item {
                    for declaration in params {
                        if !declaration.local {
                            declarations.push(declaration);
                        }
                    }
                }
            }
        } else {
            for declaration in &m.params {
                if !declaration.local {
                    declarations.push(declaration);
                }
            }
        }
        let pnames: Vec<(String, bool)> = declarations
            .into_iter()
            .map(|p| match &p.kind {
                ParamKind::Value { name, .. } => (name.name.clone(), false),
                ParamKind::Type { name, .. } => (name.name.clone(), true),
            })
            .collect();
        let mut ov: HashMap<String, PVal> = HashMap::new();
        let mut k = 0;
        for pa in &inst.params {
            let (n, v) = match pa {
                ast::ParamAssign::Ordered(v) => {
                    let Some(p) = pnames.get(k) else {
                        return self.fail(inst.span, "too many parameters");
                    };
                    k += 1;
                    (p.0.clone(), Some(v))
                }
                ast::ParamAssign::Named(n, v) => (n.name.clone(), v.as_ref()),
            };
            let Some(v) = v else { continue };
            let is_type = pnames.iter().find(|p| p.0 == n).is_some_and(|p| p.1);
            let pv = match v {
                ParamValue::Type(t) => PVal::T(self.resolve(sc, t, None)?),
                ParamValue::Expr(e) if is_type => match &e.kind {
                    ExprKind::Ident(p) => match self.find_path(sc, p)? {
                        Name::Type(t) => PVal::T(t),
                        _ => return self.fail(e.span, "not a type"),
                    },
                    _ => return self.fail(e.span, "not a type"),
                },
                ParamValue::Expr(e) => {
                    let et = self.type_of(sc, e)?;
                    if et.is_array() {
                        let v = self.const_val(sc, e, Some(&et))?;
                        PVal::V(v, et)
                    } else {
                        let s = self.size(sc, e)?;
                        let v = self.const_val(sc, e, None)?;
                        PVal::V(v, Ty::bits(u64::from(s.w), s.s))
                    }
                }
            };
            ov.insert(n, pv);
        }
        for i in &inst.instances {
            if !i.dims.is_empty() {
                return self.fail(i.span, "instance arrays");
            }
            if self.depth > 200 {
                return self.fail(i.span, "instance nesting too deep");
            }
            let child = self.scope(0, self.full(sc, &i.name.name));
            self.put(sc, &i.name.name, Name::Instance, i.name.span)?;
            let o: HashMap<String, Result<PVal, Expr>> =
                ov.iter().map(|(k, v)| (k.clone(), Ok(v.clone()))).collect();
            self.depth += 1;
            let ports = self.module(child, m, o);
            self.depth -= 1;
            let ports = ports?;
            self.connect(sc, child, &ports, &i.conns, i.span)?;
        }
        Ok(())
    }

    pub(crate) fn connect(
        &mut self,
        parent: usize,
        child: usize,
        ports: &[Port<'a>],
        conns: &'a [PortConn],
        sp: Span,
    ) -> R<()> {
        let mut given: Vec<Option<Option<Expr>>> = vec![None; ports.len()];
        let mut wild = false;
        let mut k = 0;
        for c in conns {
            match c {
                PortConn::Ordered(e) => {
                    if k >= ports.len() {
                        return self.fail(sp, "too many connections");
                    }
                    given[k] = Some(e.clone());
                    k += 1;
                }
                PortConn::Named(n, e) => {
                    let Some(i) = ports.iter().position(|p| p.name == n.name) else {
                        return self.fail(n.span, "no such port");
                    };
                    given[i] = Some(match e {
                        Some(x) => x.clone(),
                        None => Some(Expr::new(
                            ExprKind::Ident(ast::Path {
                                scope: None,
                                name: n.clone(),
                            }),
                            n.span,
                        )),
                    });
                }
                PortConn::Wildcard(_) => wild = true,
            }
        }
        for (i, p) in ports.iter().enumerate() {
            let conn = match &given[i] {
                Some(e) => e.clone(),
                None if wild => Some(Expr::new(
                    ExprKind::Ident(ast::Path {
                        scope: None,
                        name: ast::Ident {
                            name: p.name.clone(),
                            span: sp,
                        },
                    }),
                    sp,
                )),
                None => None,
            };
            let t = self.sigs[p.sig].ty.clone();
            let pr = self.proc_id();
            match p.dir {
                Direction::Input => {
                    let val = match (&conn, p.default) {
                        (Some(e), _) => self.assign_val(parent, e, &t)?,
                        (None, Some(d)) => self.assign_val(child, d, &t)?,
                        (None, None) => {
                            self.warnings.push(format!("unconnected input {}", p.name));
                            Val::P(vec![B::F; (t.width() * t.count()) as usize])
                        }
                    };
                    let bits: Vec<B> = match val {
                        Val::P(b) => b,
                        Val::U(es) => es.into_iter().flatten().collect(),
                    };
                    for (j, b) in bits.into_iter().enumerate() {
                        self.set(p.sig, j, b, pr, sp)?;
                    }
                }
                Direction::Output => {
                    let Some(e) = conn else { continue };
                    let (loc, lt) = self.target(parent, &e)?;
                    let vars = self.sigs[p.sig].vars.clone();
                    let bits: Vec<B> = if t.is_array() {
                        vars.iter().map(|v| B::V(*v)).collect()
                    } else {
                        let raw: Vec<B> = vars.iter().map(|v| B::V(*v)).collect();
                        self.ext(&raw, (lt.width() * lt.count()) as usize, t.signed)
                    };
                    self.drive(&loc, &bits, pr, sp)?;
                }
                _ => return self.fail(sp, "inout port"),
            }
        }
        Ok(())
    }
}
