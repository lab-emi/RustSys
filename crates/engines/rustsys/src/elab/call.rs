// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Function and task calls (SYNTH-REQ-0219).

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{self, Arg, BlockDecl, Direction, Expr, ExprKind, Stmt};

use super::stmt::Exits;
use super::types::Ty;
use super::{EResult, Elaborator, Env, ScopeId, Symbol, TVal, VarRef};

impl<'a> Elaborator<'a> {
    // ---------------------------------------------------------------- calls

    /// Function call in an expression; returns the result value.
    pub(crate) fn call(
        &mut self,
        scope: ScopeId,
        func: &ast::Path,
        args: &[Arg],
        span: Span,
    ) -> EResult<Option<TVal>> {
        match self.lookup_path(scope, func)? {
            Symbol::Func(f) => {
                let d = f.decl;
                self.call_sub(
                    scope,
                    f.scope,
                    &d.name,
                    d.ret.as_ref(),
                    &d.ports,
                    &d.decls,
                    &d.body,
                    args,
                    span,
                )
            }
            Symbol::Task(t) => {
                let d = t.decl;
                self.call_sub(
                    scope, t.scope, &d.name, None, &d.ports, &d.decls, &d.body, args, span,
                )
            }
            _ => Err(self.err(
                "S0140",
                func.name.span,
                format!("'{}' is not a function or task", func.name.name),
            )),
        }
    }

    /// Subroutine call used as a statement.
    pub(crate) fn exec_call_stmt(&mut self, scope: ScopeId, e: &Expr) -> EResult<()> {
        match &e.kind {
            ExprKind::SysCall { name, .. } => {
                if matches!(
                    name.as_str(),
                    "$display"
                        | "$write"
                        | "$monitor"
                        | "$strobe"
                        | "$info"
                        | "$warning"
                        | "$error"
                        | "$fatal"
                        | "$finish"
                        | "$stop"
                        | "$dumpfile"
                        | "$dumpvars"
                        | "$readmemh"
                        | "$readmemb"
                ) {
                    if name.starts_with("$readmem") {
                        return Err(self.err(
                            "S0250",
                            e.span,
                            format!("{name} cannot be synthesized"),
                        ));
                    }
                    self.note_once("simulation system task", e.span);
                    Ok(())
                } else {
                    Err(self.err(
                        "S0151",
                        e.span,
                        format!("system task {name} is not supported"),
                    ))
                }
            }
            ExprKind::Call { func, args } => {
                self.call(scope, func, args, e.span)?;
                Ok(())
            }
            ExprKind::Ident(p) => {
                self.call(scope, p, &[], e.span)?;
                Ok(())
            }
            _ => Err(self.err("S0251", e.span, "unsupported call statement")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn call_sub(
        &mut self,
        caller: ScopeId,
        def_scope: ScopeId,
        name: &ast::Ident,
        ret: Option<&'a ast::DataType>,
        ports: &'a [ast::TfPort],
        decls: &'a [BlockDecl],
        body: &'a [Stmt],
        args: &[Arg],
        span: Span,
    ) -> EResult<Option<TVal>> {
        if self.call_depth >= super::MAX_CALL_DEPTH {
            return Err(self.err(
                "S0252",
                span,
                "subroutine calls nested too deeply (SYNTH-REQ-0132)",
            ));
        }
        self.call_depth += 1;
        let temp_env = self.env.is_none();
        if temp_env {
            self.env = Some(Env::new());
        }
        let r = self.call_inner(caller, def_scope, name, ret, ports, decls, body, args, span);
        if temp_env {
            self.env = None;
        }
        self.call_depth -= 1;
        r
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn call_inner(
        &mut self,
        caller: ScopeId,
        def_scope: ScopeId,
        name: &ast::Ident,
        ret: Option<&'a ast::DataType>,
        ports: &'a [ast::TfPort],
        decls: &'a [BlockDecl],
        body: &'a [Stmt],
        args: &[Arg],
        span: Span,
    ) -> EResult<Option<TVal>> {
        let fscope = self.new_scope(def_scope, self.scopes[def_scope].path.clone());
        // Formal arguments: header list, or old-style declarations in the body.
        let mut formals: Vec<(
            Direction,
            &'a ast::DataType,
            &'a ast::Ident,
            &'a [ast::Dim],
            Option<&'a Expr>,
        )> = Vec::new();
        for p in ports {
            formals.push((p.dir, &p.ty, &p.name, &p.dims, p.default.as_ref()));
        }
        for d in decls {
            if let BlockDecl::Port(pd) = d {
                for (n, dims, _) in &pd.names {
                    formals.push((pd.dir, &pd.ty, n, dims, None));
                }
            }
        }
        // Match actual arguments.
        let mut actuals: Vec<Option<&Expr>> = vec![None; formals.len()];
        let mut pos = 0usize;
        for a in args {
            match a {
                Arg::Positional(e) => {
                    if pos >= formals.len() {
                        return Err(self.err(
                            "S0253",
                            span,
                            format!("too many arguments for '{}'", name.name),
                        ));
                    }
                    actuals[pos] = e.as_ref();
                    pos += 1;
                }
                Arg::Named(n, e) => {
                    let k = formals
                        .iter()
                        .position(|f| f.2.name == n.name)
                        .ok_or_else(|| {
                            self.err(
                                "S0254",
                                n.span,
                                format!("'{}' has no argument '{}'", name.name, n.name),
                            )
                        })?;
                    actuals[k] = e.as_ref();
                }
            }
        }
        let mut outputs = Vec::new();
        for (k, (dir, ty, fname, dims, default)) in formals.iter().enumerate() {
            let t = self.resolve_type(fscope, ty, false)?;
            let t = self.apply_unpacked(fscope, t, dims)?;
            let l = self.new_local(&fname.name, t.clone());
            self.declare(fscope, &fname.name, Symbol::Local(l), fname.span)?;
            match dir {
                Direction::Input | Direction::Inout | Direction::Ref => {
                    let v = match (actuals[k], default) {
                        (Some(e), _) => self.eval_assign(caller, e, &t)?,
                        (None, Some(d)) => self.eval_assign(fscope, d, &t)?,
                        (None, None) => {
                            return Err(self.err(
                                "S0255",
                                span,
                                format!(
                                    "missing argument '{}' in call of '{}'",
                                    fname.name, name.name
                                ),
                            ));
                        }
                    };
                    self.set_local(l, v);
                    if *dir != Direction::Input {
                        if let Some(e) = actuals[k] {
                            outputs.push((l, e));
                        }
                    }
                }
                Direction::Output => {
                    if let Some(e) = actuals[k] {
                        outputs.push((l, e));
                    }
                }
            }
        }
        let ret_local = match ret {
            Some(rt) => {
                let t = self.resolve_type(fscope, rt, false)?;
                let l = self.new_local(&name.name, t);
                self.declare(fscope, &name.name, Symbol::Local(l), name.span)?;
                Some(l)
            }
            None => None,
        };
        for d in decls.iter().filter(|d| !matches!(d, BlockDecl::Port(_))) {
            self.block_decls(fscope, std::slice::from_ref(d))?;
        }
        self.exits.push(Exits::default());
        self.ret_stack.push(ret_local);
        self.ret_scopes.push(fscope);
        let mut res = Ok(());
        for st in body {
            res = self.exec(fscope, st);
            if res.is_err() {
                break;
            }
        }
        self.ret_scopes.pop();
        self.ret_stack.pop();
        let ex = self.exits.pop().expect("pushed");
        res?;
        self.rejoin(ex.ret);
        let result = ret_local.map(|l| self.read_whole(VarRef::Local(l)));
        for (l, e) in outputs {
            let v = self.read_whole(VarRef::Local(l));
            let pieces = self.lvalue(caller, e)?;
            let target = if pieces.len() == 1 {
                pieces[0].ty.clone()
            } else {
                Ty::vec(pieces.iter().map(|p| p.width).sum(), false)
            };
            let v = self.convert(v, &target, e.span)?;
            self.write_pieces(&pieces, v, false)?;
        }
        Ok(result)
    }

    pub(crate) fn set_local(&mut self, l: u32, v: TVal) {
        let env = self.env.as_mut().expect("procedural context");
        match v {
            TVal::Packed(b, _) => {
                env.vals.insert((VarRef::Local(l), 0), b);
            }
            TVal::Unpacked(es, _) => {
                for (k, b) in es.into_iter().enumerate() {
                    env.vals.insert((VarRef::Local(l), k as u32), b);
                }
            }
        }
    }
}
