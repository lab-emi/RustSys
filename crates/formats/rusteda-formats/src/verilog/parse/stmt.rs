// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Procedural statements.

use super::expr::is_type_keyword;
use super::{PResult, Parser};
use crate::verilog::ast::{
    AssignOp, BinOp, BlockDecl, CaseAttrs, CaseItem, CaseKind, Edge, EventExpr, Expr, ExprKind,
    ForInit, Qualifier, Stmt, StmtKind, TimingCtrl,
};
use crate::verilog::span::Span;
use crate::verilog::token::Tok;

fn compound_op(t: &Tok) -> Option<BinOp> {
    let Tok::Op(o) = t else { return None };
    Some(match *o {
        "+=" => BinOp::Add,
        "-=" => BinOp::Sub,
        "*=" => BinOp::Mul,
        "/=" => BinOp::Div,
        "%=" => BinOp::Mod,
        "&=" => BinOp::BitAnd,
        "|=" => BinOp::BitOr,
        "^=" => BinOp::BitXor,
        "<<=" => BinOp::Shl,
        ">>=" => BinOp::Shr,
        "<<<=" => BinOp::AShl,
        ">>>=" => BinOp::AShr,
        _ => return None,
    })
}

impl Parser<'_> {
    /// True if the cursor starts a block item declaration.
    pub(crate) fn starts_block_decl(&self) -> bool {
        match self.peek() {
            Tok::Kw(
                "var" | "const" | "static" | "automatic" | "typedef" | "localparam" | "parameter"
                | "import",
            ) => true,
            t if is_type_keyword(t) => !self.is_op_at(1, "'"),
            Tok::Ident(_) | Tok::EscIdent(_) => self.named_type_ahead(),
            _ => false,
        }
    }

    /// One block item declaration.
    pub(crate) fn block_decl(&mut self) -> PResult<Option<BlockDecl>> {
        Ok(Some(match self.peek() {
            Tok::Kw("typedef") => match self.typedef()? {
                Some(t) => BlockDecl::Typedef(t),
                None => return Ok(None),
            },
            Tok::Kw("localparam" | "parameter") => BlockDecl::Param(self.param_decl_stmt()?),
            Tok::Kw("import") => BlockDecl::Import(self.import_stmt()?),
            _ => BlockDecl::Data(self.var_decl()?),
        }))
    }

    fn mk(kind: StmtKind, span: Span) -> Stmt {
        Stmt {
            kind,
            label: None,
            span,
        }
    }

    /// A statement.
    pub(crate) fn stmt(&mut self) -> PResult<Stmt> {
        self.enter()?;
        let r = self.stmt_inner();
        self.leave();
        r
    }

    fn stmt_inner(&mut self) -> PResult<Stmt> {
        let start = self.span();
        // Statement label `name :`.
        if self.is_ident() && self.is_op_at(1, ":") && !self.is_op_at(1, "::") {
            let label = self.ident()?;
            self.bump();
            let mut s = self.stmt()?;
            if let StmtKind::Block { name: n @ None, .. } = &mut s.kind {
                *n = Some(label.clone());
            }
            s.label = Some(label);
            return Ok(s);
        }
        let kind = match self.peek().clone() {
            Tok::CaseAttrs(a) => {
                self.bump();
                let mut s = self.stmt()?;
                if let StmtKind::Case { attrs, .. } = &mut s.kind {
                    *attrs = a;
                }
                return Ok(s);
            }
            Tok::Op(";") => {
                self.bump();
                StmtKind::Null
            }
            Tok::Kw("begin") => return self.block(),
            Tok::Kw("fork") => {
                self.bump();
                let mut depth = 1;
                while depth > 0 {
                    match self.peek() {
                        Tok::Eof => return Err(self.unexpected("'join'")),
                        Tok::Kw("fork") => depth += 1,
                        Tok::Kw("join" | "join_any" | "join_none") => depth -= 1,
                        _ => {}
                    }
                    self.bump();
                }
                self.end_label();
                StmtKind::Unsupported("fork".into())
            }
            Tok::Kw("unique" | "unique0" | "priority") => {
                let q = match self.bump().tok {
                    Tok::Kw("unique") => Qualifier::Unique,
                    Tok::Kw("unique0") => Qualifier::Unique0,
                    _ => Qualifier::Priority,
                };
                if self.is_kw("if") {
                    return self.if_stmt(Some(q), start);
                }
                return self.case_stmt(Some(q), start);
            }
            Tok::Kw("if") => return self.if_stmt(None, start),
            Tok::Kw("case" | "casez" | "casex") => return self.case_stmt(None, start),
            Tok::Kw("for") => return self.for_stmt(),
            Tok::Kw("while") => {
                self.bump();
                self.expect_op("(")?;
                let cond = self.expr()?;
                self.expect_op(")")?;
                let body = Box::new(self.stmt()?);
                StmtKind::While { cond, body }
            }
            Tok::Kw("do") => {
                self.bump();
                let body = Box::new(self.stmt()?);
                self.expect_kw("while")?;
                self.expect_op("(")?;
                let cond = self.expr()?;
                self.expect_op(")")?;
                self.expect_op(";")?;
                StmtKind::DoWhile { body, cond }
            }
            Tok::Kw("repeat") => {
                self.bump();
                self.expect_op("(")?;
                let count = self.expr()?;
                self.expect_op(")")?;
                let body = Box::new(self.stmt()?);
                StmtKind::Repeat { count, body }
            }
            Tok::Kw("forever") => {
                self.bump();
                StmtKind::Forever(Box::new(self.stmt()?))
            }
            Tok::Kw("foreach") => {
                self.bump();
                self.expect_op("(")?;
                let array_path = self.path()?;
                let mut array = Parser::path_expr(array_path);
                while self.is_op(".") && self.is_ident_at(1) {
                    self.bump();
                    let m = self.ident()?;
                    let sp = array.span.to(m.span);
                    array = Expr::new(ExprKind::Member(Box::new(array), m), sp);
                }
                self.expect_op("[")?;
                let mut vars = Vec::new();
                loop {
                    if self.is_op(",") || self.is_op("]") {
                        vars.push(None);
                    } else {
                        vars.push(Some(self.ident()?));
                    }
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op("]")?;
                self.expect_op(")")?;
                let body = Box::new(self.stmt()?);
                StmtKind::Foreach { array, vars, body }
            }
            Tok::Kw("break") => {
                self.bump();
                self.expect_op(";")?;
                StmtKind::Break
            }
            Tok::Kw("continue") => {
                self.bump();
                self.expect_op(";")?;
                StmtKind::Continue
            }
            Tok::Kw("return") => {
                self.bump();
                let v = if self.is_op(";") {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect_op(";")?;
                StmtKind::Return(v)
            }
            Tok::Op("@") => {
                let ctrl = self.event_control()?;
                let body = Box::new(self.stmt()?);
                StmtKind::Timed { ctrl, body }
            }
            Tok::Op("#") => {
                self.skip_delay()?;
                let body = Box::new(self.stmt()?);
                StmtKind::Timed {
                    ctrl: TimingCtrl::Delay,
                    body,
                }
            }
            Tok::Kw("assert" | "assume" | "cover" | "restrict" | "expect") => {
                self.skip_assertion()?;
                StmtKind::Ignored("assertion")
            }
            Tok::Kw("wait" | "wait_order") => {
                self.bump();
                if self.is_op("(") {
                    self.skip_balanced()?;
                }
                let _ = self.stmt()?;
                StmtKind::Ignored("wait")
            }
            Tok::Kw("disable") => {
                self.skip_to_semicolon()?;
                StmtKind::Ignored("disable")
            }
            Tok::Op("->" | "->>") => {
                self.skip_to_semicolon()?;
                StmtKind::Ignored("event trigger")
            }
            Tok::Kw(k @ ("force" | "release" | "assign" | "deassign")) => {
                self.skip_to_semicolon()?;
                StmtKind::Unsupported(format!("procedural '{k}'"))
            }
            Tok::Kw("randcase" | "randsequence") => {
                return Err(self.error(start, "randomization statements are not supported"));
            }
            Tok::SysIdent(_) => {
                let e = self.expr()?;
                self.expect_op(";")?;
                StmtKind::Call(e)
            }
            Tok::Kw("void") if self.is_op_at(1, "'") => {
                self.bump();
                self.bump();
                self.expect_op("(")?;
                let e = self.expr()?;
                self.expect_op(")")?;
                self.expect_op(";")?;
                StmtKind::Call(e)
            }
            Tok::Op("++" | "--") => {
                let inc = self.is_op("++");
                self.bump();
                let target = self.lvalue()?;
                self.expect_op(";")?;
                StmtKind::IncDec { target, inc }
            }
            _ => {
                let lhs = self.lvalue()?;
                if matches!(lhs.kind, ExprKind::Call { .. }) {
                    self.expect_op(";")?;
                    StmtKind::Call(lhs)
                } else if self.is_op(";") && matches!(lhs.kind, ExprKind::Ident(_)) {
                    // Task call without arguments.
                    self.bump();
                    StmtKind::Call(lhs)
                } else if self.is_op("++") || self.is_op("--") {
                    let inc = self.is_op("++");
                    self.bump();
                    self.expect_op(";")?;
                    StmtKind::IncDec { target: lhs, inc }
                } else {
                    let (op, nonblocking) = if self.eat_op("=") {
                        (AssignOp::Eq, false)
                    } else if self.eat_op("<=") {
                        (AssignOp::Eq, true)
                    } else if let Some(b) = compound_op(self.peek()) {
                        self.bump();
                        (AssignOp::Compound(b), false)
                    } else {
                        return Err(self.unexpected("an assignment operator"));
                    };
                    // Intra-assignment timing control is ignored.
                    if self.is_op("#") {
                        self.skip_delay()?;
                    } else if self.is_op("@") {
                        let _ = self.event_control()?;
                    }
                    let rhs = self.expr()?;
                    self.expect_op(";")?;
                    StmtKind::Assign {
                        lhs,
                        op,
                        rhs,
                        nonblocking,
                    }
                }
            }
        };
        Ok(Self::mk(kind, start.to(self.prev_span())))
    }

    pub(crate) fn path_expr(p: crate::verilog::ast::Path) -> Expr {
        let span = p
            .scope
            .as_ref()
            .map_or(p.name.span, |s| s.span.to(p.name.span));
        Expr::new(ExprKind::Ident(p), span)
    }

    /// `begin [: name] decls stmts end [: name]`
    pub(crate) fn block(&mut self) -> PResult<Stmt> {
        let start = self.expect_kw("begin")?;
        let name = if self.eat_op(":") {
            Some(self.ident()?)
        } else {
            None
        };
        let mut decls = Vec::new();
        let mut stmts = Vec::new();
        while !self.is_kw("end") {
            if self.at_eof() {
                return Err(self.unexpected("'end'"));
            }
            if self.starts_block_decl() {
                if let Some(d) = self.block_decl()? {
                    decls.push(d);
                }
                continue;
            }
            stmts.push(self.stmt()?);
        }
        self.bump();
        self.end_label();
        Ok(Self::mk(
            StmtKind::Block { name, decls, stmts },
            start.to(self.prev_span()),
        ))
    }

    fn if_stmt(&mut self, qual: Option<Qualifier>, start: Span) -> PResult<Stmt> {
        self.expect_kw("if")?;
        self.expect_op("(")?;
        let cond = self.expr()?;
        self.expect_op(")")?;
        let then = Box::new(self.stmt()?);
        let els = if self.eat_kw("else") {
            Some(Box::new(self.stmt()?))
        } else {
            None
        };
        Ok(Self::mk(
            StmtKind::If {
                qual,
                cond,
                then,
                els,
            },
            start.to(self.prev_span()),
        ))
    }

    fn case_stmt(&mut self, qual: Option<Qualifier>, start: Span) -> PResult<Stmt> {
        let kind = match self.bump().tok {
            Tok::Kw("case") => CaseKind::Case,
            Tok::Kw("casez") => CaseKind::Casez,
            Tok::Kw("casex") => CaseKind::Casex,
            _ => return Err(self.error(start, "expected 'case'")),
        };
        self.expect_op("(")?;
        let expr = self.expr()?;
        self.expect_op(")")?;
        let inside = self.eat_kw("inside");
        if self.is_kw("matches") {
            return Err(self.error(self.span(), "pattern-matching case is not supported"));
        }
        let mut items = Vec::new();
        while !self.is_kw("endcase") {
            if self.at_eof() {
                return Err(self.unexpected("'endcase'"));
            }
            let mut labels = Vec::new();
            if self.eat_kw("default") {
                self.eat_op(":");
            } else {
                loop {
                    labels.push(if inside {
                        self.range_or_expr()?
                    } else {
                        self.expr()?
                    });
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op(":")?;
            }
            let body = self.stmt()?;
            items.push(CaseItem { labels, body });
        }
        self.bump();
        Ok(Self::mk(
            StmtKind::Case {
                qual,
                attrs: CaseAttrs::default(),
                kind,
                inside,
                expr,
                items,
            },
            start.to(self.prev_span()),
        ))
    }

    fn for_stmt(&mut self) -> PResult<Stmt> {
        let start = self.expect_kw("for")?;
        self.expect_op("(")?;
        let mut init = Vec::new();
        if !self.is_op(";") {
            let mut ty = None;
            loop {
                let _ = self.eat_kw("var") || self.eat_kw("automatic") || self.eat_kw("static");
                if self.starts_explicit_type()
                    || self.named_type_ahead()
                    || self.is_kw("signed")
                    || self.is_kw("unsigned")
                {
                    ty = Some(self.data_type()?);
                }
                let lhs = self.lvalue()?;
                self.expect_op("=")?;
                let rhs = self.expr()?;
                init.push(ForInit {
                    ty: ty.clone(),
                    lhs,
                    rhs,
                });
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op(";")?;
        let cond = if self.is_op(";") {
            None
        } else {
            Some(self.expr()?)
        };
        self.expect_op(";")?;
        let mut step = Vec::new();
        if !self.is_op(")") {
            loop {
                step.push(self.step_stmt()?);
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        let body = Box::new(self.stmt()?);
        Ok(Self::mk(
            StmtKind::For {
                init,
                cond,
                step,
                body,
            },
            start.to(self.prev_span()),
        ))
    }

    /// `i++`, `++i`, `i += 1`, `i = i + 1` without a trailing `;`.
    pub(crate) fn step_stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        if self.is_op("++") || self.is_op("--") {
            let inc = self.is_op("++");
            self.bump();
            let target = self.lvalue()?;
            return Ok(Self::mk(
                StmtKind::IncDec { target, inc },
                start.to(self.prev_span()),
            ));
        }
        let lhs = self.lvalue()?;
        let kind = if self.is_op("++") || self.is_op("--") {
            let inc = self.is_op("++");
            self.bump();
            StmtKind::IncDec { target: lhs, inc }
        } else if self.eat_op("=") {
            let rhs = self.expr()?;
            StmtKind::Assign {
                lhs,
                op: AssignOp::Eq,
                rhs,
                nonblocking: false,
            }
        } else if let Some(b) = compound_op(self.peek()) {
            self.bump();
            let rhs = self.expr()?;
            StmtKind::Assign {
                lhs,
                op: AssignOp::Compound(b),
                rhs,
                nonblocking: false,
            }
        } else {
            return Err(self.unexpected("a loop step"));
        };
        Ok(Self::mk(kind, start.to(self.prev_span())))
    }

    /// `@*`, `@(*)`, `@(ev or ev, ev)`, `@name`.
    pub(crate) fn event_control(&mut self) -> PResult<TimingCtrl> {
        self.expect_op("@")?;
        if self.eat_op("*") {
            return Ok(TimingCtrl::Star);
        }
        if self.is_ident() {
            let p = self.path()?;
            return Ok(TimingCtrl::Events(vec![EventExpr {
                edge: Edge::Any,
                expr: Parser::path_expr(p),
                iff: None,
            }]));
        }
        self.expect_op("(")?;
        if self.eat_op("*") {
            self.expect_op(")")?;
            return Ok(TimingCtrl::Star);
        }
        let mut evs = Vec::new();
        loop {
            let edge = if self.eat_kw("posedge") {
                Edge::Pos
            } else if self.eat_kw("negedge") {
                Edge::Neg
            } else if self.eat_kw("edge") {
                Edge::Both
            } else {
                Edge::Any
            };
            let expr = self.expr()?;
            let iff = if self.eat_kw("iff") {
                Some(self.expr()?)
            } else {
                None
            };
            evs.push(EventExpr { edge, expr, iff });
            if !(self.eat_kw("or") || self.eat_op(",")) {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(TimingCtrl::Events(evs))
    }

    /// Skips an immediate, deferred or concurrent assertion including its action blocks.
    pub(crate) fn skip_assertion(&mut self) -> PResult<()> {
        self.bump();
        self.eat_kw("property");
        self.eat_kw("sequence");
        self.eat_kw("final");
        if self.is_op("#") {
            self.bump();
            self.bump();
        }
        if self.is_op("(") {
            self.skip_balanced()?;
        }
        if self.eat_op(";") {
            return Ok(());
        }
        if !self.is_kw("else") {
            let _ = self.stmt()?;
        }
        if self.eat_kw("else") {
            let _ = self.stmt()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::verilog::ast::{AssignOp, StmtKind};
    use crate::verilog::test_util::parse_stmt;

    #[test]
    fn statement_forms() {
        let s = parse_stmt("begin : b logic [3:0] t; int i; t = a; q <= t; c += 2; i++; end");
        let StmtKind::Block { name, decls, stmts } = &s.kind else {
            panic!()
        };
        assert_eq!(name.as_ref().unwrap().name, "b");
        assert_eq!(decls.len(), 2);
        assert_eq!(stmts.len(), 4);
        assert!(matches!(
            stmts[1].kind,
            StmtKind::Assign {
                nonblocking: true,
                ..
            }
        ));
        assert!(matches!(
            stmts[2].kind,
            StmtKind::Assign {
                op: AssignOp::Compound(_),
                ..
            }
        ));
        assert!(matches!(stmts[3].kind, StmtKind::IncDec { inc: true, .. }));
        let s = parse_stmt("unique case (s) A, B: x = 1; default: x = 0; endcase");
        assert!(matches!(s.kind, StmtKind::Case { ref items, .. } if items.len() == 2));
        let s = parse_stmt("case (s) inside [0:3]: x = 1; 5: x = 2; endcase");
        assert!(matches!(s.kind, StmtKind::Case { inside: true, .. }));
        // VERILOG-REQ-0102: case attributes are recorded; other attributes are not.
        use crate::verilog::ast::{CaseAttrs, Qualifier};
        let s = parse_stmt("(* full_case, parallel_case *) case (s) 0: x = 1; 1: x = 2; endcase");
        assert!(matches!(
            s.kind,
            StmtKind::Case {
                attrs: CaseAttrs {
                    full: true,
                    parallel: true
                },
                ..
            }
        ));
        let s = parse_stmt("(* parallel_case *) (* keep = 1 *) unique casez (s) 0: x = 1; endcase");
        assert!(matches!(
            s.kind,
            StmtKind::Case {
                attrs: CaseAttrs {
                    full: false,
                    parallel: true
                },
                qual: Some(Qualifier::Unique),
                ..
            }
        ));
        let s = parse_stmt("(* keep *) case (s) 0: x = 1; endcase");
        assert!(matches!(
            s.kind,
            StmtKind::Case {
                attrs: CaseAttrs {
                    full: false,
                    parallel: false
                },
                ..
            }
        ));
        let s = parse_stmt("begin (* full_case *) x = 1; case (s) 0: x = 2; endcase end");
        let StmtKind::Block { stmts, .. } = &s.kind else {
            panic!()
        };
        assert!(matches!(
            stmts[1].kind,
            StmtKind::Case {
                attrs: CaseAttrs { full: false, .. },
                ..
            }
        ));
        let s = parse_stmt("for (int i = 0, j = 1; i < 4; i++, j += 2) x[i] = j;");
        assert!(
            matches!(s.kind, StmtKind::For { ref init, ref step, .. } if init.len() == 2 && step.len() == 2)
        );
        let s = parse_stmt("@(posedge clk or negedge rst_n) if (!rst_n) q <= '0; else q <= d;");
        assert!(matches!(s.kind, StmtKind::Timed { .. }));
        let s = parse_stmt("assert (a == b) else $error(\"x\");");
        assert!(matches!(s.kind, StmtKind::Ignored(_)));
        let s = parse_stmt("{a, b} = c;");
        assert!(matches!(s.kind, StmtKind::Assign { .. }));
        let s = parse_stmt("my_task(a);");
        assert!(matches!(s.kind, StmtKind::Call(_)));
        let s = parse_stmt("foreach (m[i, j]) m[i][j] = 0;");
        assert!(matches!(s.kind, StmtKind::Foreach { ref vars, .. } if vars.len() == 2));
        let s = parse_stmt("begin my_t x; x = y; end");
        assert!(matches!(s.kind, StmtKind::Block { ref decls, .. } if decls.len() == 1));
    }
}
