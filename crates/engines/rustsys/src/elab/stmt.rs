// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Symbolic execution of procedural statements (SYNTH-REQ-0130, SYNTH-REQ-0210 to -0216).
//!
//! The environment holds the value of every variable on the current path. Values on paths that
//! have left through `break`, `continue` or `return` are captured in snapshots that rejoin at the
//! end of the loop, loop body or subroutine, so the continuing path never needs to be guarded.

use std::rc::Rc;

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{
    self, AssignOp, BinOp, BlockDecl, Expr, ExprKind, ParamKind, Stmt, StmtKind,
};

use super::select::{ElemSel, LvPiece, Pos};
use super::types::Ty;
use super::{EResult, Elaborator, Env, ScopeId, Symbol, TVal, VarKey, VarRef};
use crate::aig::Lit;
use crate::bits::{self, Bv};

/// Maximum iterations of one loop activation (SYNTH-REQ-0216).
pub(crate) const MAX_LOOP_ITERATIONS: usize = 65_536;

/// Paths that left a loop or subroutine early.
#[derive(Debug, Default)]
pub(crate) struct Exits {
    pub brk: Option<Env>,
    pub cont: Option<Env>,
    pub ret: Option<Env>,
}

impl<'a> Elaborator<'a> {
    fn env(&mut self) -> &mut Env {
        self.env.as_mut().expect("procedural context")
    }

    /// Adds the current path to an exit accumulator and kills the current path.
    fn leave_path(&mut self, which: fn(&mut Exits) -> &mut Option<Env>, span: Span) -> EResult<()> {
        if self.exits.is_empty() {
            return Err(self.err(
                "S0220",
                span,
                "break, continue or return outside of a loop or subroutine",
            ));
        }
        let cur = self.env.take().expect("procedural context");
        let live = cur.live;
        let mut dead = cur.clone();
        dead.live = Lit::FALSE;
        let acc = which(self.exits.last_mut().expect("checked")).take();
        let merged = match acc {
            None => cur,
            Some(prev) => {
                let mut m = self.merge_env(live, cur, prev.clone());
                m.live = self.aig.or(live, prev.live);
                m
            }
        };
        *which(self.exits.last_mut().expect("checked")) = Some(merged);
        self.env = Some(dead);
        Ok(())
    }

    /// Rejoins an exit accumulator into the current path.
    pub(crate) fn rejoin(&mut self, acc: Option<Env>) {
        if let Some(a) = acc {
            let cur = self.env.take().expect("procedural context");
            let live = cur.live;
            let alive = self.aig.or(live, a.live);
            let mut m = self.merge_env(live, cur, a);
            m.live = alive;
            self.env = Some(m);
        }
    }

    // ------------------------------------------------------------ assignments

    /// Executes `lhs op rhs` as a blocking or nonblocking assignment.
    pub(crate) fn exec_assign(
        &mut self,
        scope: ScopeId,
        lhs: &Expr,
        op: AssignOp,
        rhs: &Expr,
        nba: bool,
        span: Span,
    ) -> EResult<()> {
        let pieces = self.lvalue(scope, lhs)?;
        let rhs_expr;
        let rhs = match op {
            AssignOp::Eq => rhs,
            AssignOp::Compound(b) => {
                rhs_expr = Expr::new(
                    ExprKind::Binary(b, Box::new(lhs.clone()), Box::new(rhs.clone())),
                    span,
                );
                &rhs_expr
            }
        };
        let target = if pieces.len() == 1 {
            pieces[0].ty.clone()
        } else {
            Ty::vec(pieces.iter().map(|p| p.width).sum(), false)
        };
        let val = self.eval_assign(scope, rhs, &target)?;
        self.write_pieces(&pieces, val, nba)
    }

    /// Writes a value to target pieces (most significant piece first).
    pub(crate) fn write_pieces(&mut self, pieces: &[LvPiece], val: TVal, nba: bool) -> EResult<()> {
        if pieces.len() == 1 {
            return self.write_piece(&pieces[0], val, nba);
        }
        let TVal::Packed(bits_v, _) = val else {
            return Err(self.err(
                "S0221",
                pieces[0].span,
                "unpacked value assigned to a concatenation",
            ));
        };
        let mut hi = bits_v.len();
        for p in pieces {
            let w = p.width as usize;
            let part = bits_v[hi - w..hi].to_vec();
            hi -= w;
            self.write_piece(p, TVal::Packed(part, p.ty.clone()), nba)?;
        }
        Ok(())
    }

    fn write_piece(&mut self, p: &LvPiece, val: TVal, nba: bool) -> EResult<()> {
        if let VarRef::Sig(s) = p.var {
            let e = self.env();
            if nba {
                e.nba_sigs.entry(s).or_insert(p.span);
            } else {
                e.blocking_sigs.entry(s).or_insert(p.span);
            }
        }
        // Element values to write, one per selected element (sub-arrays) or one packed value.
        let elem_vals: Vec<Bv> = match val {
            TVal::Packed(v, _) => vec![v],
            TVal::Unpacked(vs, _) => vs,
        };
        match &p.elem {
            ElemSel::Static { base, len } => {
                if elem_vals.len() == 1 && *len >= 1 && p.ty.is_packed() {
                    self.write_elem(
                        (p.var, *base),
                        &p.lo,
                        p.width,
                        &elem_vals[0],
                        Lit::TRUE,
                        nba,
                    );
                } else {
                    for (k, v) in elem_vals.iter().enumerate().take(*len as usize) {
                        let w = v.len() as u32;
                        self.write_elem(
                            (p.var, base + k as u32),
                            &Pos::Const(Some(0)),
                            w,
                            v,
                            Lit::TRUE,
                            nba,
                        );
                    }
                }
            }
            ElemSel::Dyn {
                base,
                count,
                stride,
                pos,
                valid,
            } => {
                for k in 0..*count {
                    let kb = bits::constant(u64::from(k), pos.len());
                    let hit = bits::eq(&mut self.aig, pos, &kb);
                    let cond = self.aig.and(hit, *valid);
                    if cond == Lit::FALSE {
                        continue;
                    }
                    for j in 0..*stride {
                        let flat = base + k * stride + j;
                        let v = if elem_vals.len() == 1 {
                            &elem_vals[0]
                        } else {
                            &elem_vals[j as usize]
                        };
                        let (lo, w) = if *stride == 1 {
                            (p.lo.clone(), p.width)
                        } else {
                            (Pos::Const(Some(0)), v.len() as u32)
                        };
                        self.write_elem((p.var, flat), &lo, w, v, cond, nba);
                    }
                }
            }
        }
        Ok(())
    }

    /// Replaces bits `[lo, lo + width)` of one element under condition `cond`.
    fn write_elem(&mut self, key: VarKey, lo: &Pos, width: u32, v: &[Lit], cond: Lit, nba: bool) {
        let ew = self.var_ty(key.0).unpacked_dims().1.width() as usize;
        if cond == Lit::TRUE && matches!(lo, Pos::Const(Some(0))) && width as usize == ew {
            let new = bits::resize(v, ew, false);
            if nba {
                self.env().nba.insert(key, (new, vec![Lit::TRUE; ew]));
            } else {
                self.env().vals.insert(key, new);
                if matches!(key.0, VarRef::Sig(_)) {
                    self.env().assigned.insert(key, vec![Lit::TRUE; ew]);
                }
            }
            return;
        }
        let old = if nba {
            let init = self.initial_value(key);
            self.env()
                .nba
                .get(&key)
                .map(|x| x.0.clone())
                .unwrap_or(init)
        } else {
            self.read_var(key)
        };
        let ew = old.len();
        let w = width as usize;
        // Window mask and shifted value.
        let (mask, shifted): (Bv, Bv) = match lo {
            Pos::Const(Some(o)) => {
                let o = *o as usize;
                let m = (0..ew)
                    .map(|i| Lit::from_bool(i >= o && i < o + w))
                    .collect();
                let s = (0..ew)
                    .map(|i| {
                        if i >= o && i < o + w {
                            v.get(i - o).copied().unwrap_or(Lit::FALSE)
                        } else {
                            Lit::FALSE
                        }
                    })
                    .collect();
                (m, s)
            }
            Pos::Const(None) => return,
            Pos::Dyn(off) => {
                // Shift within a vector padded by `w` bits below to allow negative offsets.
                let total = ew + 2 * w;
                let mut vv = bits::resize(v, w, false);
                vv.resize(total, Lit::FALSE);
                let mut ones = vec![Lit::TRUE; w];
                ones.resize(total, Lit::FALSE);
                let aw = off.len().max(64 - (total as u64).leading_zeros() as usize) + 2;
                let offw = bits::resize(off, aw, true);
                let amt = bits::add_const(&mut self.aig, &offw, w as i64);
                let neg = *amt.last().unwrap_or(&Lit::FALSE);
                let vs = bits::shift(&mut self.aig, &vv, &amt, true, Lit::FALSE);
                let ms = bits::shift(&mut self.aig, &ones, &amt, true, Lit::FALSE);
                let m: Bv = ms[w..w + ew]
                    .iter()
                    .map(|l| self.aig.and(*l, !neg))
                    .collect();
                (m, vs[w..w + ew].to_vec())
            }
        };
        let mask: Bv = mask.iter().map(|m| self.aig.and(*m, cond)).collect();
        let new: Bv = (0..ew)
            .map(|i| self.aig.mux(mask[i], shifted[i], old[i]))
            .collect();
        if nba {
            let prev_a = self
                .env()
                .nba
                .get(&key)
                .map(|x| x.1.clone())
                .unwrap_or_else(|| vec![Lit::FALSE; ew]);
            let a: Bv = prev_a
                .iter()
                .zip(&mask)
                .map(|(p, m)| self.aig.or(*p, *m))
                .collect();
            self.env().nba.insert(key, (new, a));
        } else {
            self.env().vals.insert(key, new);
            if matches!(key.0, VarRef::Sig(_)) {
                let prev_a = self
                    .env()
                    .assigned
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| vec![Lit::FALSE; ew]);
                let a: Bv = prev_a
                    .iter()
                    .zip(&mask)
                    .map(|(p, m)| self.aig.or(*p, *m))
                    .collect();
                self.env().assigned.insert(key, a);
            }
        }
    }

    // -------------------------------------------------------------- statements

    /// Executes a statement in the current environment.
    pub(crate) fn exec(&mut self, scope: ScopeId, s: &Stmt) -> EResult<()> {
        if self.env.as_ref().is_some_and(|e| e.live == Lit::FALSE) {
            return Ok(());
        }
        match &s.kind {
            StmtKind::Null => Ok(()),
            StmtKind::Block { name, decls, stmts } => {
                let path = match name {
                    Some(n) => self.hier_name(scope, &n.name),
                    None => self.scopes[scope].path.clone(),
                };
                let inner = self.new_scope(scope, path);
                self.block_decls(inner, decls)?;
                for st in stmts {
                    self.exec(inner, st)?;
                }
                Ok(())
            }
            StmtKind::Assign {
                lhs,
                op,
                rhs,
                nonblocking,
            } => self.exec_assign(scope, lhs, *op, rhs, *nonblocking, s.span),
            StmtKind::IncDec { target, inc } => {
                let one = Expr::new(
                    ExprKind::Number(rusteda_formats::verilog::number::Literal::decimal("1")),
                    s.span,
                );
                let op = if *inc { BinOp::Add } else { BinOp::Sub };
                self.exec_assign(scope, target, AssignOp::Compound(op), &one, false, s.span)
            }
            StmtKind::If {
                cond, then, els, ..
            } => {
                let c = self.eval_bool(scope, cond)?;
                match c.const_value() {
                    Some(true) => self.exec(scope, then),
                    Some(false) => match els {
                        Some(e) => self.exec(scope, e),
                        None => Ok(()),
                    },
                    None => {
                        // Each branch runs with its exact path condition, so that exits taken
                        // inside it are recorded under that condition.
                        let base = self.env.take().expect("procedural context");
                        let mut tb = base.clone();
                        tb.live = self.aig.and(base.live, c);
                        self.env = Some(tb);
                        self.exec(scope, then)?;
                        let te = self.env.take().expect("env");
                        let mut eb = base;
                        eb.live = self.aig.and(eb.live, !c);
                        self.env = Some(eb);
                        if let Some(e) = els {
                            self.exec(scope, e)?;
                        }
                        let ee = self.env.take().expect("env");
                        let m = self.merge_env(c, te, ee);
                        self.env = Some(m);
                        Ok(())
                    }
                }
            }
            StmtKind::Case {
                qual,
                attrs,
                kind,
                inside,
                expr,
                items,
            } => {
                let full = attrs.full
                    || matches!(
                        qual,
                        Some(ast::Qualifier::Unique | ast::Qualifier::Priority)
                    );
                let parallel = attrs.parallel
                    || matches!(qual, Some(ast::Qualifier::Unique | ast::Qualifier::Unique0));
                self.exec_case(scope, *kind, *inside, expr, items, (full, parallel))
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                let inner = self.new_scope(scope, self.scopes[scope].path.clone());
                for fi in init {
                    if let Some(t) = &fi.ty {
                        let ty = self.resolve_type(inner, t, false)?;
                        let name = fi.lhs.as_ident().ok_or_else(|| {
                            self.err("S0222", fi.lhs.span, "loop variable must be a name")
                        })?;
                        let l = self.new_local(&name.name, ty);
                        self.declare(inner, &name.name, Symbol::Local(l), name.span)?;
                    }
                    self.exec_assign(inner, &fi.lhs, AssignOp::Eq, &fi.rhs, false, fi.lhs.span)?;
                }
                self.exec_loop(inner, cond.as_ref(), step, body, false, s.span)
            }
            StmtKind::While { cond, body } => {
                self.exec_loop(scope, Some(cond), &[], body, false, s.span)
            }
            StmtKind::DoWhile { body, cond } => {
                self.exec_loop(scope, Some(cond), &[], body, true, s.span)
            }
            StmtKind::Repeat { count, body } => {
                let n = self.eval_const_int(scope, count)?;
                if n as usize > MAX_LOOP_ITERATIONS {
                    return Err(self.err("S0223", s.span, "loop exceeds the iteration limit"));
                }
                self.exits.push(Exits::default());
                for _ in 0..n.max(0) {
                    if self.env().live == Lit::FALSE {
                        break;
                    }
                    self.exec(scope, body)?;
                    let cont = self.exits.last_mut().expect("pushed").cont.take();
                    self.rejoin(cont);
                }
                let ex = self.exits.pop().expect("pushed");
                self.rejoin(ex.brk);
                self.propagate_return(ex.ret);
                Ok(())
            }
            StmtKind::Forever(_) => {
                Err(self.err("S0224", s.span, "forever loops cannot be synthesized"))
            }
            StmtKind::Foreach { array, vars, body } => {
                self.exec_foreach(scope, array, vars, body, s.span)
            }
            StmtKind::Break => self.leave_path(|x| &mut x.brk, s.span),
            StmtKind::Continue => self.leave_path(|x| &mut x.cont, s.span),
            StmtKind::Return(v) => {
                if let Some(v) = v {
                    let ret = self.ret_stack.last().copied().flatten().ok_or_else(|| {
                        self.err("S0225", s.span, "return with a value outside of a function")
                    })?;
                    let target = Expr::new(
                        ExprKind::Ident(ast::Path {
                            scope: None,
                            name: ast::Ident {
                                name: self.locals[ret as usize].name.clone(),
                                span: s.span,
                            },
                        }),
                        s.span,
                    );
                    let ret_scope = self.ret_scopes.last().copied().unwrap_or(scope);
                    let pieces = self.lvalue(ret_scope, &target)?;
                    let ty = pieces[0].ty.clone();
                    let val = self.eval_assign(scope, v, &ty)?;
                    self.write_pieces(&pieces, val, false)?;
                }
                // `return` leaves every enclosing loop of the subroutine.
                self.leave_return(s.span)
            }
            StmtKind::Timed { ctrl, body } => match ctrl {
                ast::TimingCtrl::Delay => self.exec(scope, body),
                _ => Err(self.err(
                    "S0226",
                    s.span,
                    "event controls are only allowed at the start of a procedure",
                )),
            },
            StmtKind::Call(e) => self.exec_call_stmt(scope, e),
            // Assertions do not change the logic (SYNTH-REQ-0121); `wait`, `disable` and event
            // triggers change when or whether statements run, so they are rejected rather than
            // dropped (SYNTH-REQ-0133, Rule 7.1). The reader keeps no operands of them.
            StmtKind::Ignored("assertion") => {
                self.note_once("assertion", s.span);
                Ok(())
            }
            StmtKind::Ignored(what) => Err(self.err(
                "S0235",
                s.span,
                match *what {
                    "wait" => "`wait` suspends the procedure until a condition holds; it is \
                               not supported for synthesis, and its statement is not \
                               synthesized either"
                        .to_string(),
                    "disable" => "`disable` ends a named block, task or fork early; it is not \
                                  supported for synthesis"
                        .to_string(),
                    "event trigger" => "an event trigger (`->`, `->>`) wakes processes waiting \
                                        on a named event; it is not supported for synthesis"
                        .to_string(),
                    other => format!("{other} is not supported for synthesis"),
                },
            )),
            StmtKind::Unsupported(what) => Err(self.err(
                "S0227",
                s.span,
                format!("{what} is not supported for synthesis"),
            )),
        }
    }

    fn propagate_return(&mut self, ret: Option<Env>) {
        if let Some(r) = ret {
            // A return inside a loop leaves the loop and continues to the enclosing exit frame.
            match self.exits.last_mut() {
                Some(outer) => {
                    let prev = outer.ret.take();
                    let merged = match prev {
                        None => r,
                        Some(p) => {
                            let live = r.live;
                            let pl = p.live;
                            let mut m = self.merge_env(live, r, p);
                            m.live = self.aig.or(live, pl);
                            m
                        }
                    };
                    self.exits.last_mut().expect("present").ret = Some(merged);
                }
                None => self.rejoin(Some(r)),
            }
        }
    }

    fn leave_return(&mut self, span: Span) -> EResult<()> {
        if self.exits.is_empty() {
            return Err(self.err("S0225", span, "return outside of a subroutine"));
        }
        self.leave_path(|x| &mut x.ret, span)
    }

    fn exec_loop(
        &mut self,
        scope: ScopeId,
        cond: Option<&Expr>,
        step: &[Stmt],
        body: &Stmt,
        do_first: bool,
        span: Span,
    ) -> EResult<()> {
        self.exits.push(Exits::default());
        let mut iter = 0usize;
        loop {
            if self.env().live == Lit::FALSE {
                break;
            }
            if !(do_first && iter == 0) {
                if let Some(c) = cond {
                    let cl = self.eval_bool(scope, c)?;
                    match cl.const_value() {
                        Some(false) => break,
                        Some(true) => {}
                        None => {
                            self.exits.pop();
                            return Err(self.err("S0228", c.span, "loop condition does not become constant during unrolling (SYNTH-REQ-0216)"));
                        }
                    }
                }
            }
            iter += 1;
            if iter > MAX_LOOP_ITERATIONS {
                self.exits.pop();
                return Err(self.err(
                    "S0223",
                    span,
                    "loop exceeds the iteration limit (SYNTH-REQ-0216)",
                ));
            }
            self.exec(scope, body)?;
            let cont = self.exits.last_mut().expect("pushed").cont.take();
            self.rejoin(cont);
            for st in step {
                self.exec(scope, st)?;
            }
        }
        let ex = self.exits.pop().expect("pushed");
        self.rejoin(ex.brk);
        self.propagate_return(ex.ret);
        Ok(())
    }

    fn exec_foreach(
        &mut self,
        scope: ScopeId,
        array: &Expr,
        vars: &[Option<ast::Ident>],
        body: &Stmt,
        span: Span,
    ) -> EResult<()> {
        let t = self.expr_ty(scope, array)?;
        // Collect dimension ranges, outermost first.
        let mut ranges = Vec::new();
        let mut cur = t;
        while ranges.len() < vars.len() {
            match cur.index_dim() {
                Some((r, et)) => {
                    ranges.push(r);
                    cur = et;
                }
                None => {
                    return Err(self.err(
                        "S0229",
                        span,
                        "foreach has more loop variables than the array has dimensions",
                    ));
                }
            }
        }
        let inner = self.new_scope(scope, self.scopes[scope].path.clone());
        let mut locals = Vec::new();
        for v in vars {
            locals.push(match v {
                Some(id) => {
                    let l = self.new_local(&id.name, Ty::int());
                    self.declare(inner, &id.name, Symbol::Local(l), id.span)?;
                    Some(l)
                }
                None => None,
            });
        }
        self.exits.push(Exits::default());
        let r = self.foreach_level(inner, &ranges, &locals, 0, body);
        let ex = self.exits.pop().expect("pushed");
        r?;
        self.rejoin(ex.brk);
        self.propagate_return(ex.ret);
        Ok(())
    }

    fn foreach_level(
        &mut self,
        scope: ScopeId,
        ranges: &[super::types::Range],
        locals: &[Option<u32>],
        level: usize,
        body: &Stmt,
    ) -> EResult<()> {
        if level == ranges.len() {
            self.exec(scope, body)?;
            let cont = self.exits.last_mut().expect("pushed").cont.take();
            self.rejoin(cont);
            return Ok(());
        }
        let r = ranges[level];
        for p in 0..r.len() {
            if self.env().live == Lit::FALSE {
                break;
            }
            if let Some(l) = locals[level] {
                let v = bits::constant(r.index_at_position(p) as u64, 32);
                self.env().vals.insert((VarRef::Local(l), 0), v);
            }
            self.foreach_level(scope, ranges, locals, level + 1, body)?;
        }
        Ok(())
    }

    /// Declarations at the start of a block.
    pub(crate) fn block_decls(&mut self, scope: ScopeId, decls: &[BlockDecl]) -> EResult<()> {
        for d in decls {
            match d {
                BlockDecl::Data(dd) => {
                    if dd.net.is_some() {
                        return Err(self.err(
                            "S0230",
                            dd.span,
                            "nets cannot be declared in procedural blocks",
                        ));
                    }
                    let base = self.resolve_type(scope, &dd.ty, true)?;
                    for dc in &dd.decls {
                        let ty = self.apply_unpacked(scope, base.clone(), &dc.dims)?;
                        let l = self.new_local(&dc.name.name, ty);
                        self.declare(scope, &dc.name.name, Symbol::Local(l), dc.name.span)?;
                        if let Some(init) = &dc.init {
                            let target = ident_expr(&dc.name);
                            self.exec_assign(
                                scope,
                                &target,
                                AssignOp::Eq,
                                init,
                                false,
                                dc.name.span,
                            )?;
                        }
                    }
                }
                BlockDecl::Param(ps) => {
                    for p in ps {
                        self.local_param(scope, p, None)?;
                    }
                }
                BlockDecl::Typedef(t) => {
                    let ty = self.resolve_type(scope, &t.ty, true)?;
                    let ty = self.apply_unpacked(scope, ty, &t.dims)?;
                    self.declare(scope, &t.name.name, Symbol::Type(ty), t.name.span)?;
                }
                BlockDecl::Import(is) => {
                    for i in is {
                        self.scopes[scope].imports.push((
                            i.package.name.clone(),
                            i.name.as_ref().map(|n| n.name.clone()),
                        ));
                    }
                }
                BlockDecl::Port(p) => {
                    return Err(self.err("S0231", p.span, "port declaration inside a block"));
                }
            }
        }
        Ok(())
    }

    /// Declares a parameter in `scope`; `override_val` replaces the default.
    pub(crate) fn local_param(
        &mut self,
        scope: ScopeId,
        p: &ast::ParamDecl,
        override_val: Option<super::module::ParamValueE>,
    ) -> EResult<()> {
        match &p.kind {
            ParamKind::Value {
                ty,
                name,
                unpacked,
                default,
            } => {
                let declared = if ty.is_implicit() && ty.packed.is_empty() && ty.signing.is_none() {
                    None
                } else {
                    let t = self.resolve_type(scope, ty, true)?;
                    Some(self.apply_unpacked(scope, t, unpacked)?)
                };
                let v = match override_val {
                    Some(super::module::ParamValueE::Value(v)) => match &declared {
                        Some(t) => self.convert(v, t, name.span)?,
                        None => v,
                    },
                    Some(super::module::ParamValueE::Type(_)) => {
                        return Err(self.err(
                            "S0232",
                            name.span,
                            format!("parameter '{}' expects a value, not a type", name.name),
                        ));
                    }
                    None => {
                        let d = default.as_ref().ok_or_else(|| {
                            self.err(
                                "S0233",
                                name.span,
                                format!("parameter '{}' has no value", name.name),
                            )
                        })?;
                        self.eval_const(scope, d, declared.as_ref())?
                    }
                };
                self.declare(scope, &name.name, Symbol::Const(Rc::new(v)), name.span)
            }
            ParamKind::Type { name, default } => {
                let t = match override_val {
                    Some(super::module::ParamValueE::Type(t)) => t,
                    Some(super::module::ParamValueE::Value(_)) => {
                        return Err(self.err(
                            "S0232",
                            name.span,
                            format!("parameter '{}' expects a type", name.name),
                        ));
                    }
                    None => {
                        let d = default.as_ref().ok_or_else(|| {
                            self.err(
                                "S0233",
                                name.span,
                                format!("type parameter '{}' has no default", name.name),
                            )
                        })?;
                        self.resolve_type(scope, d, false)?
                    }
                };
                self.declare(scope, &name.name, Symbol::Type(t), name.span)
            }
        }
    }

    /// Converts a value to a target type with assignment semantics.
    pub(crate) fn convert(&mut self, v: TVal, t: &Ty, span: Span) -> EResult<TVal> {
        match (v, t.is_packed()) {
            (TVal::Packed(b, vt), true) => {
                let w = t.width() as usize;
                Ok(TVal::Packed(bits::resize(&b, w, vt.is_signed()), t.clone()))
            }
            (TVal::Unpacked(es, _), false) if es.len() as u32 == t.element_count() => {
                Ok(TVal::Unpacked(es, t.clone()))
            }
            _ => Err(self.err("S0234", span, "value does not match the declared type")),
        }
    }
}

/// Builds an identifier expression.
pub(crate) fn ident_expr(id: &ast::Ident) -> Expr {
    Expr::new(
        ExprKind::Ident(ast::Path {
            scope: None,
            name: id.clone(),
        }),
        id.span,
    )
}
