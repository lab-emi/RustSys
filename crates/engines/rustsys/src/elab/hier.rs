// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Generate constructs and module instances (SYNTH-REQ-0122, SYNTH-REQ-0123, SYNTH-REQ-0140).

use std::rc::Rc;

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{
    self, AssignOp, Direction, Expr, GenBlock, ModuleItem, ParamKind, ParamValue, PortConn,
    StmtKind,
};

use super::module::{MAX_GENERATE_ITERATIONS, Ovr, ParamValueE, PortInfo, flatten_items};
use super::select::{ElemSel, LvPiece, Pos};
use super::stmt::ident_expr;
use super::types::Ty;
use super::{EResult, Elaborator, ScopeId, Symbol, TVal, VarRef};
use crate::aig::Lit;
use crate::bits;

impl<'a> Elaborator<'a> {
    // ------------------------------------------------------------- generate

    pub(crate) fn elab_gen_block(
        &mut self,
        scope: ScopeId,
        b: &'a GenBlock,
        n: u32,
        suffix: Option<i64>,
    ) -> EResult<ScopeId> {
        let base = match &b.label {
            Some(l) => l.name.clone(),
            None => format!("genblk{n}"),
        };
        let seg = match suffix {
            Some(i) => format!("{base}[{i}]"),
            None => base,
        };
        let path = self.hier_name(scope, &seg);
        let inner = self.new_scope(scope, path);
        let mut flat = Vec::new();
        flatten_items(&b.items, &mut flat);
        for it in &flat {
            self.decl_item(inner, it, false)?;
        }
        self.items_behavior(inner, &flat)?;
        Ok(inner)
    }

    pub(crate) fn elab_gen_if(&mut self, scope: ScopeId, g: &'a ast::GenIf, n: u32) -> EResult<()> {
        let c = self.eval_const_int(scope, &g.cond)? != 0;
        let chosen = if c { Some(&g.then) } else { g.els.as_ref() };
        let Some(b) = chosen else { return Ok(()) };
        // `else if` chains are directly nested and share the construct number (27.5).
        if b.label.is_none() && !b.has_begin && b.items.len() == 1 {
            if let ModuleItem::GenIf(inner) = &b.items[0] {
                return self.elab_gen_if(scope, inner, n);
            }
        }
        self.elab_gen_block(scope, b, n, None)?;
        Ok(())
    }

    pub(crate) fn elab_gen_for(&mut self, scope: ScopeId, g: &'a ast::GenFor) -> EResult<()> {
        self.scopes[scope].gen_count += 1;
        let n = self.scopes[scope].gen_count;
        if !g.declares_genvar && !matches!(self.lookup(scope, &g.genvar.name), Some(Symbol::Genvar))
        {
            return Err(self.err(
                "S0123",
                g.genvar.span,
                format!("'{}' is not a genvar", g.genvar.name),
            ));
        }
        let mut value = self.eval_const_int(scope, &g.init)?;
        let mut iters = 0i64;
        loop {
            let tmp = self.new_scope(scope, self.scopes[scope].path.clone());
            let cv = TVal::Packed(bits::constant(value as u64, 32), Ty::int());
            self.declare(
                tmp,
                &g.genvar.name,
                Symbol::Const(Rc::new(cv.clone())),
                g.genvar.span,
            )?;
            if self.eval_const_int(tmp, &g.cond)? == 0 {
                break;
            }
            iters += 1;
            if iters > MAX_GENERATE_ITERATIONS {
                return Err(self.err("S0123", g.span, "generate loop exceeds the iteration limit"));
            }
            self.elab_gen_block_with(scope, &g.body, n, value, &g.genvar)?;
            // Next value from the step statement.
            value = match &g.step.kind {
                StmtKind::IncDec { inc, .. } => value + if *inc { 1 } else { -1 },
                StmtKind::Assign { op, rhs, .. } => {
                    let r = self.eval_const_int(tmp, rhs)?;
                    match op {
                        AssignOp::Eq => r,
                        AssignOp::Compound(ast::BinOp::Add) => value + r,
                        AssignOp::Compound(ast::BinOp::Sub) => value - r,
                        AssignOp::Compound(ast::BinOp::Mul) => value * r,
                        AssignOp::Compound(ast::BinOp::Shl) => value << r,
                        AssignOp::Compound(ast::BinOp::Shr) => value >> r,
                        AssignOp::Compound(ast::BinOp::Div) if r != 0 => value / r,
                        _ => {
                            return Err(self.err(
                                "S0123",
                                g.step.span,
                                "unsupported generate loop step",
                            ));
                        }
                    }
                }
                _ => return Err(self.err("S0123", g.step.span, "unsupported generate loop step")),
            };
        }
        Ok(())
    }

    pub(crate) fn elab_gen_block_with(
        &mut self,
        scope: ScopeId,
        b: &'a GenBlock,
        n: u32,
        value: i64,
        gv: &ast::Ident,
    ) -> EResult<ScopeId> {
        let base = match &b.label {
            Some(l) => l.name.clone(),
            None => format!("genblk{n}"),
        };
        let path = self.hier_name(scope, &format!("{base}[{value}]"));
        let inner = self.new_scope(scope, path);
        let cv = TVal::Packed(bits::constant(value as u64, 32), Ty::int());
        self.declare(inner, &gv.name, Symbol::Const(Rc::new(cv)), gv.span)?;
        let mut flat = Vec::new();
        flatten_items(&b.items, &mut flat);
        for it in &flat {
            self.decl_item(inner, it, false)?;
        }
        self.items_behavior(inner, &flat)?;
        Ok(inner)
    }

    // ------------------------------------------------------------- instances

    pub(crate) fn elab_instance(
        &mut self,
        scope: ScopeId,
        inst: &'a ast::Instantiation,
    ) -> EResult<()> {
        let m: &'a ast::Module = *self.modules.get(&inst.module.name).ok_or_else(|| {
            self.err(
                "S0130",
                inst.module.span,
                format!("unknown module '{}'", inst.module.name),
            )
        })?;
        if m.kind != ast::UnitKind::Module {
            return Err(self.err(
                "S0103",
                inst.module.span,
                "interfaces and programs are not supported",
            ));
        }
        // Parameter overrides, evaluated in the parent scope.
        // Legacy Verilog declares overridable parameters in the module body.
        // With a parameter-port list, body parameters are local instead; do
        // not add them to the ordered override positions in that case.
        let body_params = m
            .items
            .iter()
            .filter_map(|item| match item {
                ModuleItem::Param(params) if m.params.is_empty() => Some(params.iter()),
                _ => None,
            })
            .flatten();
        let param_names: Vec<(String, bool)> = m
            .params
            .iter()
            .chain(body_params)
            .filter(|p| !p.local)
            .map(|p| match &p.kind {
                ParamKind::Value { name, .. } => (name.name.clone(), false),
                ParamKind::Type { name, .. } => (name.name.clone(), true),
            })
            .collect();
        let mut ovr_vals: Vec<(String, ParamValueE)> = Vec::new();
        let mut ordered = 0usize;
        for pa in &inst.params {
            let (name, v) = match pa {
                ast::ParamAssign::Ordered(v) => {
                    let n = param_names
                        .get(ordered)
                        .ok_or_else(|| self.err("S0105", inst.span, "too many parameter values"))?;
                    ordered += 1;
                    (n.0.clone(), Some(v))
                }
                ast::ParamAssign::Named(n, v) => (n.name.clone(), v.as_ref()),
            };
            let Some(v) = v else { continue };
            let is_type = param_names.iter().find(|p| p.0 == name).map(|p| p.1);
            let val = match (v, is_type) {
                (ParamValue::Type(t), _) => ParamValueE::Type(self.resolve_type(scope, t, false)?),
                (ParamValue::Expr(e), Some(true)) => match self.cast_type_name(scope, e) {
                    Some(t) => ParamValueE::Type(t),
                    None => {
                        return Err(self.err(
                            "S0232",
                            e.span,
                            "type parameter override must be a type",
                        ));
                    }
                },
                (ParamValue::Expr(e), _) => ParamValueE::Value(self.eval_const(scope, e, None)?),
            };
            ovr_vals.push((name, val));
        }
        for i in &inst.instances {
            if !i.dims.is_empty() {
                return Err(self.err(
                    "S0122",
                    i.span,
                    "arrays of instances are not supported (SYNTH-REQ-0122)",
                ));
            }
            if self.inst_depth >= super::MAX_INSTANCE_DEPTH {
                return Err(self.err(
                    "S0131",
                    i.span,
                    "instances nested too deeply (recursive instantiation?)",
                ));
            }
            let path = self.hier_name(scope, &i.name.name);
            let child = self.new_scope(self.unit_scope, path);
            self.declare(scope, &i.name.name, Symbol::Scope(child), i.name.span)?;
            let ovr: Vec<(String, Ovr<'a>)> = ovr_vals
                .iter()
                .map(|(n, v)| (n.clone(), Ovr::Val(v.clone())))
                .collect();
            self.inst_depth += 1;
            let ports = self.elab_module(child, m, ovr);
            self.inst_depth -= 1;
            let ports = ports?;
            self.connect(scope, child, &ports, &i.conns, i.span)?;
        }
        Ok(())
    }

    pub(crate) fn connect(
        &mut self,
        parent: ScopeId,
        child: ScopeId,
        ports: &[PortInfo<'a>],
        conns: &'a [PortConn],
        span: Span,
    ) -> EResult<()> {
        let mut assigned: Vec<Option<Option<Expr>>> = vec![None; ports.len()];
        let mut wildcard = false;
        let mut pos = 0usize;
        for c in conns {
            match c {
                PortConn::Ordered(e) => {
                    if pos >= ports.len() {
                        return Err(self.err("S0132", span, "too many port connections"));
                    }
                    assigned[pos] = Some(e.clone());
                    pos += 1;
                }
                PortConn::Named(n, e) => {
                    let k = ports.iter().position(|p| p.name == n.name).ok_or_else(|| {
                        self.err("S0133", n.span, format!("module has no port '{}'", n.name))
                    })?;
                    assigned[k] = Some(match e {
                        Some(inner) => inner.clone(),
                        None => Some(ident_expr(n)),
                    });
                }
                PortConn::Wildcard(_) => wildcard = true,
            }
        }
        for (k, p) in ports.iter().enumerate() {
            let conn: Option<Expr> = match &assigned[k] {
                Some(e) => e.clone(),
                None if wildcard => {
                    let id = ast::Ident {
                        name: p.name.clone(),
                        span,
                    };
                    if self.lookup(parent, &p.name).is_none() {
                        return Err(self.err(
                            "S0133",
                            span,
                            format!(".* found no signal named '{}'", p.name),
                        ));
                    }
                    Some(ident_expr(&id))
                }
                None => None,
            };
            let sig_ty = self.signals[p.sig as usize].ty.clone();
            let proc = self.new_proc("port connection", span);
            match p.dir {
                Direction::Input => {
                    let v = match (&conn, p.default) {
                        (Some(e), _) => self.eval_assign(parent, e, &sig_ty)?,
                        (None, Some(d)) => self.eval_assign(child, d, &sig_ty)?,
                        (None, None) => {
                            self.warn(
                                "S0213",
                                span,
                                format!("input port '{}' is unconnected and reads as 0", p.name),
                            );
                            let w = self.signals[p.sig as usize].ph.len();
                            let (_, et) = sig_ty.unpacked_dims();
                            if sig_ty.is_packed() {
                                TVal::Packed(vec![Lit::FALSE; w], sig_ty.clone())
                            } else {
                                let ew = et.width() as usize;
                                TVal::Unpacked(
                                    vec![vec![Lit::FALSE; ew]; w / ew.max(1)],
                                    sig_ty.clone(),
                                )
                            }
                        }
                    };
                    let len = sig_ty.element_count();
                    let piece = LvPiece {
                        var: VarRef::Sig(p.sig),
                        elem: ElemSel::Static { base: 0, len },
                        lo: Pos::Const(Some(0)),
                        width: if sig_ty.is_packed() {
                            sig_ty.width()
                        } else {
                            0
                        },
                        ty: sig_ty.clone(),
                        span,
                    };
                    self.drive_pieces(&[piece], v, proc, span)?;
                }
                Direction::Output => {
                    let Some(e) = conn else { continue };
                    let pieces = self.lvalue(parent, &e)?;
                    let target = if pieces.len() == 1 {
                        pieces[0].ty.clone()
                    } else {
                        Ty::vec(pieces.iter().map(|p| p.width).sum(), false)
                    };
                    let v = self.read_whole(VarRef::Sig(p.sig));
                    let v = self.convert(v, &target, e.span)?;
                    self.drive_pieces(&pieces, v, proc, span)?;
                }
                _ => {
                    return Err(self.err("S0602", p.span, "inout and ref ports are not supported"));
                }
            }
        }
        Ok(())
    }
}
