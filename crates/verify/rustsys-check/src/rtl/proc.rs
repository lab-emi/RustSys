// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Procedural statements with path conditions (SYNTH-REQ-0210 to -0219).
//!
//! Every write `x = v` executed under condition `g` makes `x` become `g ? v : x`. `break`,
//! `continue` and `return` accumulate kill conditions in frames; a write is guarded by its path
//! condition and by the negation of every active kill condition. Loop variables declared in a
//! `for` header are unrolling counters: they are constant and updated without guards.

use std::collections::{HashMap, HashSet};

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{
    self, AssignOp, BinOp, BlockDecl, CaseKind, Expr, ExprKind, Stmt, StmtKind,
};
use rusteda_formats::verilog::number::Bit4;

use super::eval::{Loc, Src};
use super::ty::{Ty, index_at};
use super::{Builder, Name, R, Val};
use crate::circ::B;

const MAX_ITER: usize = 65_536;

/// Kill conditions of a loop body or subroutine.
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub brk: B,
    pub cnt: B,
    pub ret: B,
}

/// State of the procedure being interpreted.
#[derive(Debug, Default, Clone)]
pub(crate) struct PState {
    /// Current value of written bits: (is signal, id, flat bit).
    pub cur: HashMap<(bool, usize, usize), B>,
    /// Pending nonblocking values.
    pub nba: HashMap<(bool, usize, usize), B>,
    /// Condition under which a signal bit has been written (blocking or nonblocking).
    pub wrote: HashMap<(usize, usize), B>,
    pub blocking: HashSet<usize>,
    pub nonblocking: HashSet<usize>,
    pub frames: Vec<Frame>,
    /// Unrolling counters: (is module signal, variable id). A module-visible
    /// counter retains its guarded final value after the loop.
    pub counters: HashSet<(bool, usize)>,
}

impl<'a> Builder<'a> {
    pub(crate) fn ps(&mut self) -> &mut PState {
        self.ps.as_mut().expect("in a procedure")
    }

    /// Combined kill condition of all frames.
    fn dead(&mut self) -> B {
        let flags: Vec<B> = self
            .ps()
            .frames
            .iter()
            .flat_map(|f| [f.brk, f.cnt, f.ret])
            .collect();
        let mut d = B::F;
        for f in flags {
            d = self.c.or(d, f);
        }
        d
    }

    pub(crate) fn guard(&mut self, g: B) -> B {
        let d = self.dead();
        let nd = self.c.not(d);
        self.c.and(g, nd)
    }

    /// Writes bits to a location under a guard.
    pub(crate) fn write(&mut self, loc: &Loc, v: &[B], g: B, nba: bool, sp: Span) -> R<()> {
        for (j, alts) in loc.iter().enumerate() {
            let bit = v[j];
            for (src, c) in alts {
                let Src::Var(is_sig, id, flat) = *src else {
                    return self.fail(sp, "not assignable");
                };
                if self.ps().counters.contains(&(is_sig, id)) && g != B::T {
                    return self.fail(sp, "assignment to a loop variable inside its loop is not supported by the checker");
                }
                let cond = self.c.and(g, *c);
                if cond == B::F {
                    continue;
                }
                let key = (is_sig, id, flat);
                let old = if nba {
                    match self.ps().nba.get(&key).copied() {
                        Some(x) => x,
                        None => self.start_value(is_sig, id, flat),
                    }
                } else {
                    self.read_src(Src::Var(is_sig, id, flat))
                };
                let new = self.c.mux(cond, bit, old);
                if nba {
                    self.ps().nba.insert(key, new);
                } else {
                    self.ps().cur.insert(key, new);
                }
                if is_sig {
                    let w = self.ps().wrote.get(&(id, flat)).copied().unwrap_or(B::F);
                    let w2 = self.c.or(w, cond);
                    let ps = self.ps();
                    ps.wrote.insert((id, flat), w2);
                    if nba {
                        ps.nonblocking.insert(id);
                    } else {
                        ps.blocking.insert(id);
                    }
                }
            }
        }
        Ok(())
    }

    fn assign_stmt(
        &mut self,
        sc: usize,
        lhs: &Expr,
        op: AssignOp,
        rhs: &Expr,
        nba: bool,
        g: B,
        sp: Span,
    ) -> R<()> {
        let (loc, t) = self.target(sc, lhs)?;
        let compound;
        let rhs = match op {
            AssignOp::Eq => rhs,
            AssignOp::Compound(b) => {
                compound = Expr::new(
                    ExprKind::Binary(b, Box::new(lhs.clone()), Box::new(rhs.clone())),
                    sp,
                );
                &compound
            }
        };
        let v = self.assign_val(sc, rhs, &t)?;
        let bits: Vec<B> = match v {
            Val::P(b) => b,
            Val::U(es) => es.into_iter().flatten().collect(),
        };
        let ge = self.guard(g);
        self.write(&loc, &bits, ge, nba, sp)
    }

    /// Declares block-level automatic variables (SYNTH-REQ-0218).
    pub(crate) fn block_decls(&mut self, sc: usize, decls: &[BlockDecl], g: B) -> R<()> {
        for d in decls {
            match d {
                BlockDecl::Data(dd) => {
                    let t = self.resolve(sc, &dd.ty, Some(sc))?;
                    for dc in &dd.decls {
                        let ty = self.with_unpacked(sc, t.clone(), &dc.dims)?;
                        self.locals.push(ty);
                        let l = self.locals.len() - 1;
                        self.put(sc, &dc.name.name, Name::Local(l), dc.name.span)?;
                        if let Some(init) = &dc.init {
                            let target = Expr::new(
                                ExprKind::Ident(ast::Path {
                                    scope: None,
                                    name: dc.name.clone(),
                                }),
                                dc.name.span,
                            );
                            self.assign_stmt(
                                sc,
                                &target,
                                AssignOp::Eq,
                                init,
                                false,
                                g,
                                dc.name.span,
                            )?;
                        }
                    }
                }
                BlockDecl::Param(ps) => {
                    for p in ps {
                        self.param(sc, p, None)?;
                    }
                }
                BlockDecl::Typedef(t) => {
                    let ty = self.resolve(sc, &t.ty, Some(sc))?;
                    let ty = self.with_unpacked(sc, ty, &t.dims)?;
                    self.put(sc, &t.name.name, Name::Type(ty), t.name.span)?;
                }
                BlockDecl::Import(is) => {
                    for i in is {
                        self.scopes[sc].imports.push((
                            i.package.name.clone(),
                            i.name.as_ref().map(|n| n.name.clone()),
                        ));
                    }
                }
                BlockDecl::Port(p) => return self.fail(p.span, "port declaration in a block"),
            }
        }
        Ok(())
    }

    /// Executes a statement under path condition `g`.
    pub(crate) fn exec(&mut self, sc: usize, s: &Stmt, g: B) -> R<()> {
        if g == B::F {
            return Ok(());
        }
        self.depth += 1;
        if self.depth > 4000 {
            self.depth -= 1;
            return self.fail(s.span, "statements nested too deeply");
        }
        let r = self.exec_inner(sc, s, g);
        self.depth -= 1;
        r
    }

    fn exec_inner(&mut self, sc: usize, s: &Stmt, g: B) -> R<()> {
        match &s.kind {
            StmtKind::Null => Ok(()),
            StmtKind::Block { name, decls, stmts } => {
                let path = match name {
                    Some(n) => self.full(sc, &n.name),
                    None => self.scopes[sc].path.clone(),
                };
                let inner = self.scope(sc, path);
                self.block_decls(inner, decls, g)?;
                for st in stmts {
                    self.exec(inner, st, g)?;
                }
                Ok(())
            }
            StmtKind::Assign {
                lhs,
                op,
                rhs,
                nonblocking,
            } => self.assign_stmt(sc, lhs, *op, rhs, *nonblocking, g, s.span),
            StmtKind::IncDec { target, inc } => {
                let one = Expr::new(
                    ExprKind::Number(rusteda_formats::verilog::number::Literal::decimal("1")),
                    s.span,
                );
                let op = AssignOp::Compound(if *inc { BinOp::Add } else { BinOp::Sub });
                self.assign_stmt(sc, target, op, &one, false, g, s.span)
            }
            StmtKind::If {
                cond, then, els, ..
            } => {
                let c = self.truthy_pub(sc, cond)?;
                let gt = self.c.and(g, c);
                let nc = self.c.not(c);
                let ge = self.c.and(g, nc);
                self.exec(sc, then, gt)?;
                if let Some(e) = els {
                    self.exec(sc, e, ge)?;
                }
                Ok(())
            }
            StmtKind::Case {
                qual,
                attrs,
                kind,
                inside,
                expr,
                items,
            } => {
                // Marked complete: `full_case`, `unique` or `priority` (CHECK-REQ-0012).
                let complete = attrs.full
                    || matches!(
                        qual,
                        Some(ast::Qualifier::Unique) | Some(ast::Qualifier::Priority)
                    );
                // Marked parallel: `parallel_case`, `unique` or `unique0` (CHECK-REQ-0013).
                let parallel = attrs.parallel
                    || matches!(
                        qual,
                        Some(ast::Qualifier::Unique) | Some(ast::Qualifier::Unique0)
                    );
                self.case(sc, *kind, *inside, expr, items, (complete, parallel), g)
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                let inner = self.scope(sc, self.scopes[sc].path.clone());
                let mut counters = Vec::new();
                let mut outer_vars = Vec::new();
                let mut outside_counter = false;
                for fi in init {
                    if let Some(t) = &fi.ty {
                        let ty = self.resolve(inner, t, None)?;
                        let Some(id) = fi.lhs.as_ident() else {
                            return self.fail(fi.lhs.span, "loop variable must be a name");
                        };
                        self.locals.push(ty.clone());
                        let l = self.locals.len() - 1;
                        self.put(inner, &id.name, Name::Local(l), id.span)?;
                        counters.push((false, l));
                        let v = self.assign_val(inner, &fi.rhs, &ty)?;
                        self.set_local(l, &v);
                    } else {
                        outside_counter = true;
                        // During this invocation the counter starts at its initializer.
                        // Preserve its prior value and apply the path guard once, at exit.
                        let (loc, t) = self.target(inner, &fi.lhs)?;
                        let v = self.assign_val(inner, &fi.rhs, &t)?;
                        let Val::P(bits) = v else {
                            return self.fail(fi.lhs.span, "array loop variable");
                        };
                        // Keep the enclosing value separately. Within this invocation a
                        // counter starts at the initializer even when the loop's path is
                        // symbolic; guarding that initializer would make the bound symbolic.
                        let plain = loc.iter().all(|alts| {
                            alts.len() == 1 && matches!(alts[0], (Src::Var(_, _, _), B::T))
                        });
                        if plain {
                            let old = self.read_loc(&loc);
                            outer_vars.push((loc.clone(), old));
                            for (j, alts) in loc.iter().enumerate() {
                                if let Src::Var(is_sig, id, bit) = alts[0].0 {
                                    let key = (is_sig, id);
                                    if !self.ps().counters.contains(&key)
                                        && !counters.contains(&key)
                                    {
                                        counters.push(key);
                                    }
                                    self.ps().cur.insert((is_sig, id, bit), bits[j]);
                                }
                            }
                        } else {
                            // A computed/dynamic target is not an unrolling counter.
                            let ge = self.guard(g);
                            self.write(&loc, &bits, ge, false, fi.lhs.span)?;
                        }
                    }
                }
                for l in &counters {
                    self.ps().counters.insert(*l);
                }
                let r = self.run_loop(
                    inner,
                    cond.as_ref(),
                    step,
                    body,
                    g,
                    false,
                    s.span,
                    outside_counter,
                );
                for l in &counters {
                    self.ps().counters.remove(l);
                }
                if r.is_ok() {
                    let ge = self.guard(g);
                    for (loc, old) in outer_vars {
                        let final_value = self.read_loc(&loc);
                        for (j, alts) in loc.iter().enumerate() {
                            if let Src::Var(is_sig, id, bit) = alts[0].0 {
                                self.ps().cur.insert((is_sig, id, bit), old[j]);
                            }
                        }
                        self.write(&loc, &final_value, ge, false, s.span)?;
                    }
                }
                r
            }
            StmtKind::While { cond, body } => {
                self.run_loop(sc, Some(cond), &[], body, g, false, s.span, true)
            }
            StmtKind::DoWhile { body, cond } => {
                self.run_loop(sc, Some(cond), &[], body, g, true, s.span, true)
            }
            StmtKind::Repeat { count, body } => {
                let n = self.cint(sc, count)?;
                if n as usize > MAX_ITER {
                    return self.fail(s.span, "iteration limit");
                }
                self.ps().frames.push(Frame {
                    brk: B::F,
                    cnt: B::F,
                    ret: B::F,
                });
                for _ in 0..n.max(0) {
                    self.exec(sc, body, g)?;
                    if let Some(f) = self.ps().frames.last_mut() {
                        f.cnt = B::F;
                    }
                }
                self.ps().frames.pop();
                Ok(())
            }
            StmtKind::Forever(_) => self.fail(s.span, "forever loop"),
            StmtKind::Foreach { array, vars, body } => {
                let t = self.type_of(sc, array)?;
                let inner = self.scope(sc, self.scopes[sc].path.clone());
                let mut dims = Vec::new();
                let mut cur = t;
                for v in vars {
                    let Some((l, r, et)) = cur.index_type() else {
                        return self.fail(s.span, "too many foreach variables");
                    };
                    dims.push((l, r));
                    cur = et;
                    let _ = v;
                }
                let mut ls = Vec::new();
                for v in vars {
                    match v {
                        Some(id) => {
                            self.locals.push(Ty::bits(32, true));
                            let l = self.locals.len() - 1;
                            self.put(inner, &id.name, Name::Local(l), id.span)?;
                            self.ps().counters.insert((false, l));
                            ls.push(Some(l));
                        }
                        None => ls.push(None),
                    }
                }
                self.ps().frames.push(Frame {
                    brk: B::F,
                    cnt: B::F,
                    ret: B::F,
                });
                let r = self.foreach_level(inner, &dims, &ls, 0, body, g);
                self.ps().frames.pop();
                r
            }
            StmtKind::Break | StmtKind::Continue => {
                let ge = self.guard(g);
                let is_break = matches!(s.kind, StmtKind::Break);
                let f = match self.ps().frames.last().cloned() {
                    Some(f) => f,
                    None => return self.fail(s.span, "break or continue outside a loop"),
                };
                let nf = if is_break {
                    Frame {
                        brk: self.c.or(f.brk, ge),
                        ..f
                    }
                } else {
                    Frame {
                        cnt: self.c.or(f.cnt, ge),
                        ..f
                    }
                };
                *self.ps().frames.last_mut().expect("frame") = nf;
                Ok(())
            }
            StmtKind::Return(v) => {
                let Some(fi) = self.ret_frames.last().copied() else {
                    return self.fail(s.span, "return outside a subroutine");
                };
                if let Some(v) = v {
                    let Some((l, rsc)) = self.ret_vars.last().cloned().flatten() else {
                        return self.fail(s.span, "return value in a void subroutine");
                    };
                    let t = self.locals[l].clone();
                    let val = self.assign_val(sc, v, &t)?;
                    let bits: Vec<B> = match val {
                        Val::P(b) => b,
                        Val::U(es) => es.into_iter().flatten().collect(),
                    };
                    let loc: Loc = (0..bits.len())
                        .map(|i| vec![(Src::Var(false, l, i), B::T)])
                        .collect();
                    let _ = rsc;
                    let ge = self.guard(g);
                    self.write(&loc, &bits, ge, false, s.span)?;
                }
                let ge = self.guard(g);
                let f = self.ps().frames[fi].clone();
                self.ps().frames[fi].ret = self.c.or(f.ret, ge);
                Ok(())
            }
            StmtKind::Timed {
                ctrl: ast::TimingCtrl::Delay,
                body,
            } => self.exec(sc, body, g),
            StmtKind::Timed { .. } => self.fail(s.span, "event control inside a procedure"),
            StmtKind::Call(e) => match &e.kind {
                ExprKind::SysCall { name, .. } => {
                    if name.starts_with("$readmem") {
                        return self.fail(e.span, "$readmem cannot be synthesized");
                    }
                    Ok(())
                }
                ExprKind::Call { func, args } => {
                    self.call_guarded(sc, func, args, e.span, g)?;
                    Ok(())
                }
                ExprKind::Ident(p) => {
                    self.call_guarded(sc, p, &[], e.span, g)?;
                    Ok(())
                }
                _ => self.fail(e.span, "unsupported call"),
            },
            // An assertion does not change what the procedure computes; every other statement
            // that the reader keeps without its operands (`wait`, `disable`, `->`) changes when
            // or whether statements run, so the model rejects it with its own code instead of
            // proving a netlist against a procedure that is not the RTL (CHECK-REQ-0014).
            StmtKind::Ignored("assertion") => Ok(()),
            StmtKind::Ignored(what) => Err(format!(
                "error[C0106]: {}: {} is outside the checker's model: the procedure would be \
                 compared without the control flow it adds",
                self.loc(s.span),
                match *what {
                    "wait" => "a `wait` statement",
                    "disable" => "a `disable` statement",
                    "event trigger" => "an event trigger (`->`)",
                    other => other,
                }
            )),
            StmtKind::Unsupported(w) => self.fail(s.span, format!("{w} not supported")),
        }
    }

    pub(crate) fn truthy_pub(&mut self, sc: usize, e: &Expr) -> R<B> {
        let s = self.size(sc, e)?;
        let v = self.ev(sc, e, &s, s.w, s.s)?;
        let mut r = B::F;
        for b in v {
            r = self.c.or(r, b);
        }
        Ok(r)
    }

    pub(crate) fn set_local(&mut self, l: usize, v: &Val) {
        let bits: Vec<B> = match v {
            Val::P(b) => b.clone(),
            Val::U(es) => es.iter().flatten().copied().collect(),
        };
        for (i, b) in bits.into_iter().enumerate() {
            self.ps().cur.insert((false, l, i), b);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_loop(
        &mut self,
        sc: usize,
        cond: Option<&Expr>,
        step: &[Stmt],
        body: &Stmt,
        g: B,
        do_first: bool,
        sp: Span,
        no_break_allowed_check: bool,
    ) -> R<()> {
        self.ps().frames.push(Frame {
            brk: B::F,
            cnt: B::F,
            ret: B::F,
        });
        let mut n = 0usize;
        let r = (|| -> R<()> {
            loop {
                if !(do_first && n == 0) {
                    if let Some(c) = cond {
                        let cv = self.truthy_pub(sc, c)?;
                        match cv.konst() {
                            Some(false) => break,
                            Some(true) => {}
                            None => {
                                return self.fail(
                                    c.span,
                                    "loop condition is not constant during unrolling",
                                );
                            }
                        }
                    }
                }
                n += 1;
                if n > MAX_ITER {
                    return self.fail(sp, "iteration limit");
                }
                self.exec(sc, body, g)?;
                if let Some(f) = self.ps().frames.last_mut() {
                    f.cnt = B::F;
                }
                let brk = self.ps().frames.last().map_or(B::F, |f| f.brk);
                if brk == B::T {
                    break;
                }
                for st in step {
                    self.exec_step(sc, st, g)?;
                }
            }
            Ok(())
        })();
        let f = self.ps().frames.pop().expect("frame");
        r?;
        if no_break_allowed_check && f.brk != B::F {
            return self.fail(sp, "break in a loop whose variable is declared outside the loop is not supported by the checker");
        }
        if no_break_allowed_check && f.ret != B::F {
            return self.fail(sp, "return in a loop whose variable is declared outside the loop is not supported by the checker");
        }
        Ok(())
    }

    /// Loop step: counters are updated without guards; other targets use the path guard.
    fn exec_step(&mut self, sc: usize, st: &Stmt, g: B) -> R<()> {
        let (lhs, rhs_expr): (&Expr, Expr) = match &st.kind {
            StmtKind::IncDec { target, inc } => {
                let one = Expr::new(
                    ExprKind::Number(rusteda_formats::verilog::number::Literal::decimal("1")),
                    st.span,
                );
                let op = if *inc { BinOp::Add } else { BinOp::Sub };
                (
                    target,
                    Expr::new(
                        ExprKind::Binary(op, Box::new(target.clone()), Box::new(one)),
                        st.span,
                    ),
                )
            }
            StmtKind::Assign {
                lhs,
                op: AssignOp::Eq,
                rhs,
                ..
            } => (lhs, rhs.clone()),
            StmtKind::Assign {
                lhs,
                op: AssignOp::Compound(b),
                rhs,
                ..
            } => (
                lhs,
                Expr::new(
                    ExprKind::Binary(*b, Box::new(lhs.clone()), Box::new(rhs.clone())),
                    st.span,
                ),
            ),
            _ => return self.exec(sc, st, g),
        };
        let (loc, t) = self.target(sc, lhs)?;
        let v = self.assign_val(sc, &rhs_expr, &t)?;
        let Val::P(bits) = v else {
            return self.fail(st.span, "array step");
        };
        let is_counter = loc.iter().all(|alts| alts.iter().all(|(s, _)| matches!(s, Src::Var(is_sig, id, _) if self.ps.as_ref().is_some_and(|p| p.counters.contains(&(*is_sig, *id))))));
        if is_counter {
            for (j, alts) in loc.iter().enumerate() {
                for (s, _) in alts {
                    if let Src::Var(is_sig, id, bit) = s {
                        self.ps().cur.insert((*is_sig, *id, *bit), bits[j]);
                    }
                }
            }
            Ok(())
        } else {
            // A loop variable declared outside the loop: guarded like other writes, but the
            // path guard excludes kill conditions only (they are rejected for such loops).
            self.write(&loc, &bits, g, false, st.span)
        }
    }

    fn foreach_level(
        &mut self,
        sc: usize,
        dims: &[(i64, i64)],
        ls: &[Option<usize>],
        k: usize,
        body: &Stmt,
        g: B,
    ) -> R<()> {
        if k == dims.len() {
            self.exec(sc, body, g)?;
            if let Some(f) = self.ps().frames.last_mut() {
                f.cnt = B::F;
            }
            return Ok(());
        }
        let (l, r) = dims[k];
        let n = (l - r).unsigned_abs() + 1;
        for p in 0..n {
            if let Some(loc) = ls[k] {
                let v = index_at(l, r, p);
                for i in 0..32 {
                    self.ps()
                        .cur
                        .insert((false, loc, i), B::from_bool((v >> i) & 1 == 1));
                }
            }
            self.foreach_level(sc, dims, ls, k + 1, body, g)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn case(
        &mut self,
        sc: usize,
        kind: CaseKind,
        inside: bool,
        expr: &Expr,
        items: &[ast::CaseItem],
        (complete, parallel): (bool, bool),
        g: B,
    ) -> R<()> {
        // Case expression and items are context-determined at the widest width (SYNTH-REQ-0220).
        let es = self.size(sc, expr)?;
        let mut cw = es.w;
        let mut cs = es.s;
        for it in items {
            for l in &it.labels {
                let parts: Vec<&Expr> = match &l.kind {
                    ExprKind::Range(lo, hi) => vec![lo, hi],
                    _ => vec![l],
                };
                for p in parts {
                    let s = self.size(sc, p)?;
                    cw = cw.max(s.w);
                    cs = cs && s.s;
                }
            }
        }
        let selx = self.ev(sc, expr, &es, cw, cs)?;
        let mut taken = B::F;
        let mut default = None;
        // Under the complete-case assumption a case marked complete without a default item
        // runs its last item whenever no earlier item matches (CHECK-REQ-0012).
        let last_default = self.assume_full_case
            && complete
            && !items.is_empty()
            && items.iter().all(|it| !it.labels.is_empty());
        let mut alternatives = Vec::new();
        for (k, it) in items.iter().enumerate() {
            if it.labels.is_empty() || (last_default && k + 1 == items.len()) {
                default = Some(&it.body);
                continue;
            }
            let m = self.item_match(sc, kind, inside, it, &selx, (cw, cs))?;
            if self.assume_parallel_case && parallel {
                alternatives.push((m, &it.body));
                continue;
            }
            let nt = self.c.not(taken);
            let cond = self.c.and(nt, m);
            let gi = self.c.and(g, cond);
            self.exec(sc, &it.body, gi)?;
            taken = self.c.or(taken, m);
        }
        if self.assume_parallel_case && parallel {
            return self.case_parallel(sc, &alternatives, default, g);
        }
        if let Some(d) = default {
            let nt = self.c.not(taken);
            let gd = self.c.and(g, nt);
            self.exec(sc, d, gd)?;
        }
        Ok(())
    }

    /// Whether any label of a case item matches the selector (SYNTH-REQ-0201 wildcard rules).
    fn item_match(
        &mut self,
        sc: usize,
        kind: CaseKind,
        inside: bool,
        it: &ast::CaseItem,
        selx: &[B],
        (cw, cs): (u32, bool),
    ) -> R<B> {
        let mut m = B::F;
        for l in &it.labels {
            let hit = if let (true, ExprKind::Range(lo, hi)) = (inside, &l.kind) {
                let sl = self.size(sc, lo)?;
                let sh = self.size(sc, hi)?;
                let lv = self.ev(sc, lo, &sl, cw, cs)?;
                let hv = self.ev(sc, hi, &sh, cw, cs)?;
                let lt = self.lt_pub(selx, &lv, cs);
                let gt = self.lt_pub(&hv, selx, cs);
                let a = self.c.not(lt);
                let b = self.c.not(gt);
                self.c.and(a, b)
            } else {
                let ls = self.size(sc, l)?;
                let lv = self.ev(sc, l, &ls, cw, cs)?;
                let wild: Vec<bool> = if inside || kind != CaseKind::Case {
                    self.label_bits(sc, l, &ls, cw, cs)?
                        .into_iter()
                        .map(|bit| match bit {
                            Bit4::Z => inside || kind != CaseKind::Case,
                            Bit4::X => inside || kind == CaseKind::Casex,
                            _ => false,
                        })
                        .collect()
                } else {
                    vec![false; cw as usize]
                };
                let mut eq = B::T;
                for i in 0..cw as usize {
                    if wild[i] {
                        continue;
                    }
                    let d = self.c.xor(selx[i], lv[i]);
                    let nd = self.c.not(d);
                    eq = self.c.and(eq, nd);
                }
                eq
            };
            m = self.c.or(m, hit);
        }
        Ok(m)
    }

    /// A case marked parallel under the parallel-case assumption (CHECK-REQ-0013): each item runs
    /// from the saved procedure state under its own match condition, the default item under the
    /// condition that none matches; every written bit, pending nonblocking value, write condition
    /// and kill condition becomes the OR of the matching items' results, or the default's result
    /// when none matches.
    fn case_parallel(
        &mut self,
        sc: usize,
        alternatives: &[(B, &Stmt)],
        default: Option<&Stmt>,
        g: B,
    ) -> R<()> {
        let saved = self.ps().clone();
        let mut results: Vec<PState> = Vec::new();
        let mut any = B::F;
        for (m, body) in alternatives {
            *self.ps() = saved.clone();
            let gi = self.c.and(g, *m);
            self.exec(sc, body, gi)?;
            results.push(std::mem::take(self.ps()));
            any = self.c.or(any, *m);
        }
        let none = self.c.not(any);
        *self.ps() = saved.clone();
        if let Some(d) = default {
            let gd = self.c.and(g, none);
            self.exec(sc, d, gd)?;
        }
        let dres = std::mem::replace(self.ps(), saved.clone());
        let ms: Vec<B> = alternatives.iter().map(|(m, _)| *m).collect();
        let merge = |this: &mut Self, vals: &[B], dv: B| -> B {
            let mut out = this.c.and(none, dv);
            for (m, v) in ms.iter().zip(vals) {
                let t = this.c.and(*m, *v);
                out = this.c.or(out, t);
            }
            out
        };
        let mut out = dres.clone();
        // Current values; a bit an item leaves alone keeps the saved value.
        let mut keys: Vec<(bool, usize, usize)> = dres.cur.keys().copied().collect();
        keys.extend(results.iter().flat_map(|r| r.cur.keys().copied()));
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            let base = self.read_src(Src::Var(key.0, key.1, key.2));
            let vals: Vec<B> = results
                .iter()
                .map(|r| r.cur.get(&key).copied().unwrap_or(base))
                .collect();
            let dv = dres.cur.get(&key).copied().unwrap_or(base);
            let v = merge(self, &vals, dv);
            out.cur.insert(key, v);
        }
        // Pending nonblocking values.
        let mut keys: Vec<(bool, usize, usize)> = dres.nba.keys().copied().collect();
        keys.extend(results.iter().flat_map(|r| r.nba.keys().copied()));
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            let base = match saved.nba.get(&key) {
                Some(x) => *x,
                None => self.start_value(key.0, key.1, key.2),
            };
            let vals: Vec<B> = results
                .iter()
                .map(|r| r.nba.get(&key).copied().unwrap_or(base))
                .collect();
            let dv = dres.nba.get(&key).copied().unwrap_or(base);
            let v = merge(self, &vals, dv);
            out.nba.insert(key, v);
        }
        // Write conditions.
        let mut keys: Vec<(usize, usize)> = dres.wrote.keys().copied().collect();
        keys.extend(results.iter().flat_map(|r| r.wrote.keys().copied()));
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            let base = saved.wrote.get(&key).copied().unwrap_or(B::F);
            let vals: Vec<B> = results
                .iter()
                .map(|r| r.wrote.get(&key).copied().unwrap_or(base))
                .collect();
            let dv = dres.wrote.get(&key).copied().unwrap_or(base);
            let v = merge(self, &vals, dv);
            out.wrote.insert(key, v);
        }
        // Kill conditions of `break`, `continue` and `return`.
        for f in 0..saved.frames.len() {
            for field in 0..3 {
                let get = |p: &PState| {
                    let fr = &p.frames[f];
                    [fr.brk, fr.cnt, fr.ret][field]
                };
                let vals: Vec<B> = results.iter().map(get).collect();
                let v = merge(self, &vals, get(&dres));
                let fr = &mut out.frames[f];
                match field {
                    0 => fr.brk = v,
                    1 => fr.cnt = v,
                    _ => fr.ret = v,
                }
            }
        }
        for r in &results {
            out.blocking.extend(r.blocking.iter().copied());
            out.nonblocking.extend(r.nonblocking.iter().copied());
        }
        *self.ps() = out;
        Ok(())
    }

    pub(crate) fn lt_pub(&mut self, a: &[B], b: &[B], signed: bool) -> B {
        // a < b from the most significant bit.
        let n = a.len();
        let mut lt = B::F;
        let mut eq = B::T;
        for i in (0..n).rev() {
            let less = if signed && i == n - 1 {
                let nb = self.c.not(b[i]);
                self.c.and(a[i], nb)
            } else {
                let na = self.c.not(a[i]);
                self.c.and(na, b[i])
            };
            let t = self.c.and(eq, less);
            lt = self.c.or(lt, t);
            let d = self.c.xor(a[i], b[i]);
            let nd = self.c.not(d);
            eq = self.c.and(eq, nd);
        }
        lt
    }
}

impl Builder<'_> {
    /// Value of a bit at the start of the procedure (base of nonblocking updates).
    fn start_value(&mut self, is_sig: bool, id: usize, flat: usize) -> B {
        if is_sig {
            let v = self.sigs[id].vars[flat];
            match self.forced.get(&v) {
                Some(k) => B::from_bool(*k),
                None => B::V(v),
            }
        } else {
            self.read_src(Src::Var(false, id, flat))
        }
    }
}
