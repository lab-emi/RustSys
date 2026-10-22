// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Function and task invocation in the checker's procedure interpreter.

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{self, BlockDecl, Direction, Expr, Stmt};

use super::eval::Src;
use super::proc::Frame;
use super::proc::PState;
use super::{Builder, Name, R, Val};
use crate::circ::B;

impl<'a> Builder<'a> {
    /// Subroutine call as a statement under a guard (SYNTH-REQ-0219).
    pub(crate) fn call_guarded(
        &mut self,
        sc: usize,
        func: &ast::Path,
        args: &[ast::Arg],
        sp: Span,
        g: B,
    ) -> R<Option<Val>> {
        self.call_guard.push(g);
        let r = self.call(sc, func, args, sp);
        self.call_guard.pop();
        r
    }

    /// Inlines a subroutine call.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn invoke(
        &mut self,
        caller: usize,
        def: usize,
        name: &ast::Ident,
        ret: Option<&ast::DataType>,
        ports: &[ast::TfPort],
        decls: &[BlockDecl],
        body: &[Stmt],
        args: &[ast::Arg],
        sp: Span,
    ) -> R<Option<Val>> {
        if self.ret_frames.len() > 256 {
            return self.fail(sp, "calls nested too deeply");
        }
        let temp = self.ps.is_none();
        if temp {
            self.ps = Some(PState::default());
        }
        let g = self.call_guard.last().copied().unwrap_or(B::T);
        let r = self.invoke_inner(caller, def, name, ret, ports, decls, body, args, sp, g);
        if temp {
            self.ps = None;
        }
        r
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn invoke_inner(
        &mut self,
        caller: usize,
        def: usize,
        name: &ast::Ident,
        ret: Option<&ast::DataType>,
        ports: &[ast::TfPort],
        decls: &[BlockDecl],
        body: &[Stmt],
        args: &[ast::Arg],
        sp: Span,
        g: B,
    ) -> R<Option<Val>> {
        let fsc = self.scope(def, self.scopes[def].path.clone());
        let mut formals: Vec<(
            Direction,
            ast::DataType,
            ast::Ident,
            Vec<ast::Dim>,
            Option<Expr>,
        )> = ports
            .iter()
            .map(|p| {
                (
                    p.dir,
                    p.ty.clone(),
                    p.name.clone(),
                    p.dims.clone(),
                    p.default.clone(),
                )
            })
            .collect();
        for d in decls {
            if let BlockDecl::Port(pd) = d {
                for (n, dims, _) in &pd.names {
                    formals.push((pd.dir, pd.ty.clone(), n.clone(), dims.clone(), None));
                }
            }
        }
        let mut actual: Vec<Option<&Expr>> = vec![None; formals.len()];
        let mut next = 0;
        for a in args {
            match a {
                ast::Arg::Positional(e) => {
                    if next >= formals.len() {
                        return self.fail(sp, "too many arguments");
                    }
                    actual[next] = e.as_ref();
                    next += 1;
                }
                ast::Arg::Named(n, e) => {
                    let Some(k) = formals.iter().position(|f| f.2.name == n.name) else {
                        return self.fail(n.span, "no such argument");
                    };
                    actual[k] = e.as_ref();
                }
            }
        }
        let mut outs = Vec::new();
        for (k, (dir, dt, fname, dims, default)) in formals.iter().enumerate() {
            let t = self.resolve(fsc, dt, None)?;
            let t = self.with_unpacked(fsc, t, dims)?;
            self.locals.push(t.clone());
            let l = self.locals.len() - 1;
            self.put(fsc, &fname.name, Name::Local(l), fname.span)?;
            if *dir != Direction::Output {
                let v = match (actual[k], default) {
                    (Some(e), _) => self.assign_val(caller, e, &t)?,
                    (None, Some(d)) => self.assign_val(fsc, d, &t)?,
                    (None, None) => {
                        return self.fail(sp, format!("missing argument {}", fname.name));
                    }
                };
                self.set_local(l, &v);
            }
            if *dir != Direction::Input {
                if let Some(e) = actual[k] {
                    outs.push((l, e.clone()));
                }
            }
        }
        let ret_local = match ret {
            Some(rt) => {
                let t = self.resolve(fsc, rt, None)?;
                self.locals.push(t);
                let l = self.locals.len() - 1;
                self.put(fsc, &name.name, Name::Local(l), name.span)?;
                Some((l, fsc))
            }
            None => None,
        };
        let others: Vec<BlockDecl> = decls
            .iter()
            .filter(|d| !matches!(d, BlockDecl::Port(_)))
            .cloned()
            .collect();
        self.block_decls(fsc, &others, g)?;
        self.ps().frames.push(Frame {
            brk: B::F,
            cnt: B::F,
            ret: B::F,
        });
        let fi = self.ps().frames.len() - 1;
        self.ret_frames.push(fi);
        self.ret_vars.push(ret_local);
        let mut res = Ok(());
        for st in body {
            res = self.exec(fsc, st, g);
            if res.is_err() {
                break;
            }
        }
        self.ret_vars.pop();
        self.ret_frames.pop();
        self.ps().frames.pop();
        res?;
        let result = ret_local.map(|(l, _)| self.local_val(l));
        for (l, e) in outs {
            let v = self.local_val(l);
            let (loc, t) = self.target(caller, &e)?;
            let bits: Vec<B> = match v {
                Val::P(b) => {
                    let lt = self.locals[l].clone();
                    self.ext(&b, t.width() as usize, lt.signed)
                }
                Val::U(es) => es.into_iter().flatten().collect(),
            };
            let ge = self.guard(g);
            self.write(&loc, &bits, ge, false, e.span)?;
        }
        Ok(result)
    }

    pub(crate) fn local_val(&mut self, l: usize) -> Val {
        let t = self.locals[l].clone();
        let w = t.width() as usize;
        let n = t.count() as usize;
        let bits: Vec<B> = (0..w * n)
            .map(|i| self.read_src(Src::Var(false, l, i)))
            .collect();
        if t.is_array() {
            Val::U(bits.chunks(w).map(<[B]>::to_vec).collect())
        } else {
            Val::P(bits)
        }
    }
}
