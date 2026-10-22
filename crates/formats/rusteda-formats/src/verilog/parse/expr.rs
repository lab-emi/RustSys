// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Expressions (IEEE 1800-2017 clause 11 operator precedence).

use super::{PResult, Parser};
use crate::verilog::ast::{
    Arg, BinOp, CastTarget, Expr, ExprKind, Path, PatternItem, PatternKey, SliceKind, UnOp,
};
use crate::verilog::number::Literal;
use crate::verilog::token::Tok;

/// Binding power and operator for a binary token.
fn binary_op(t: &Tok) -> Option<(u8, BinOp)> {
    let Tok::Op(o) = t else {
        return None;
    };
    Some(match *o {
        "->" => (1, BinOp::Implies),
        "<->" => (1, BinOp::Equiv),
        "||" => (3, BinOp::LogOr),
        "&&" => (4, BinOp::LogAnd),
        "|" => (5, BinOp::BitOr),
        "^" => (6, BinOp::BitXor),
        "~^" | "^~" => (6, BinOp::BitXnor),
        "&" => (7, BinOp::BitAnd),
        "==" => (8, BinOp::Eq),
        "!=" => (8, BinOp::Ne),
        "===" => (8, BinOp::CaseEq),
        "!==" => (8, BinOp::CaseNe),
        "==?" => (8, BinOp::WildEq),
        "!=?" => (8, BinOp::WildNe),
        "<" => (9, BinOp::Lt),
        "<=" => (9, BinOp::Le),
        ">" => (9, BinOp::Gt),
        ">=" => (9, BinOp::Ge),
        "<<" => (10, BinOp::Shl),
        ">>" => (10, BinOp::Shr),
        "<<<" => (10, BinOp::AShl),
        ">>>" => (10, BinOp::AShr),
        "+" => (11, BinOp::Add),
        "-" => (11, BinOp::Sub),
        "*" => (12, BinOp::Mul),
        "/" => (12, BinOp::Div),
        "%" => (12, BinOp::Mod),
        "**" => (13, BinOp::Pow),
        _ => return None,
    })
}

const COND_BP: u8 = 2;
const INSIDE_BP: u8 = 9;
const UNARY_BP: u8 = 14;

/// True if the token starts a built-in data type usable in casts and type arguments.
pub(crate) fn is_type_keyword(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Kw(
            "logic"
                | "reg"
                | "bit"
                | "byte"
                | "shortint"
                | "int"
                | "longint"
                | "integer"
                | "time"
                | "real"
                | "shortreal"
                | "realtime"
                | "string"
                | "struct"
                | "union"
                | "enum"
                | "chandle"
                | "event"
                | "void"
        )
    )
}

impl Parser<'_> {
    /// Full expression.
    pub(crate) fn expr(&mut self) -> PResult<Expr> {
        self.expr_bp(0)
    }

    fn expr_bp(&mut self, min_bp: u8) -> PResult<Expr> {
        self.enter()?;
        let mut lhs = self.unary()?;
        loop {
            if self.is_op("?") && min_bp <= COND_BP {
                self.bump();
                let then = self.expr_bp(0)?;
                self.expect_op(":")?;
                let els = self.expr_bp(COND_BP)?;
                let span = lhs.span.to(els.span);
                lhs = Expr::new(
                    ExprKind::Cond(Box::new(lhs), Box::new(then), Box::new(els)),
                    span,
                );
                continue;
            }
            if self.is_kw("inside") && min_bp <= INSIDE_BP {
                self.bump();
                let items = self.inside_list()?;
                let span = lhs.span.to(self.prev_span());
                lhs = Expr::new(ExprKind::Inside(Box::new(lhs), items), span);
                continue;
            }
            let Some((bp, op)) = binary_op(self.peek()) else {
                break;
            };
            if bp < min_bp {
                break;
            }
            self.bump();
            // Left associative except implication operators.
            let next = if matches!(op, BinOp::Implies | BinOp::Equiv) {
                bp
            } else {
                bp + 1
            };
            let rhs = self.expr_bp(next)?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::new(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), span);
        }
        self.leave();
        Ok(lhs)
    }

    /// `{ item, [lo:hi], ... }` after `inside`.
    pub(crate) fn inside_list(&mut self) -> PResult<Vec<Expr>> {
        self.expect_op("{")?;
        let mut items = Vec::new();
        loop {
            items.push(self.range_or_expr()?);
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op("}")?;
        Ok(items)
    }

    /// `[a:b]` range or plain expression (inside lists, case-inside labels).
    pub(crate) fn range_or_expr(&mut self) -> PResult<Expr> {
        if self.is_op("[") {
            let start = self.bump().span;
            let a = self.expr()?;
            self.expect_op(":")?;
            let b = self.expr()?;
            let end = self.expect_op("]")?;
            return Ok(Expr::new(
                ExprKind::Range(Box::new(a), Box::new(b)),
                start.to(end),
            ));
        }
        self.expr()
    }

    fn unary(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Op("+") => Some(UnOp::Plus),
            Tok::Op("-") => Some(UnOp::Minus),
            Tok::Op("!") => Some(UnOp::LogNot),
            Tok::Op("~") => Some(UnOp::BitNot),
            Tok::Op("&") => Some(UnOp::RedAnd),
            Tok::Op("~&") => Some(UnOp::RedNand),
            Tok::Op("|") => Some(UnOp::RedOr),
            Tok::Op("~|") => Some(UnOp::RedNor),
            Tok::Op("^") => Some(UnOp::RedXor),
            Tok::Op("~^" | "^~") => Some(UnOp::RedXnor),
            _ => None,
        };
        if let Some(op) = op {
            let start = self.bump().span;
            self.enter()?;
            let e = self.expr_bp(UNARY_BP)?;
            self.leave();
            let span = start.to(e.span);
            return Ok(Expr::new(ExprKind::Unary(op, Box::new(e)), span));
        }
        if self.is_op("++") || self.is_op("--") {
            return Err(self.error(
                self.span(),
                "increment and decrement operators are not supported inside expressions",
            ));
        }
        let p = self.primary()?;
        self.postfix(p)
    }

    /// Parses a path `name` or `pkg::name` at the cursor.
    pub(crate) fn path(&mut self) -> PResult<Path> {
        let first = self.ident()?;
        if self.is_op("::") && self.is_ident_at(1) {
            self.bump();
            let name = self.ident()?;
            return Ok(Path {
                scope: Some(first),
                name,
            });
        }
        Ok(Path {
            scope: None,
            name: first,
        })
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.span();
        match self.peek().clone() {
            Tok::Dec(d) => {
                self.bump();
                if let Tok::Based {
                    signed,
                    base,
                    digits,
                } = self.peek().clone()
                {
                    let end = self.bump().span;
                    let size: u32 = d
                        .parse()
                        .map_err(|_| self.error(start, "literal size too large"))?;
                    let lit = Literal {
                        size: Some(size),
                        plain: false,
                        signed,
                        base,
                        digits,
                    };
                    return Ok(Expr::new(ExprKind::Number(lit), start.to(end)));
                }
                Ok(Expr::new(ExprKind::Number(Literal::decimal(&d)), start))
            }
            Tok::Based {
                signed,
                base,
                digits,
            } => {
                self.bump();
                Ok(Expr::new(
                    ExprKind::Number(Literal {
                        size: None,
                        plain: false,
                        signed,
                        base,
                        digits,
                    }),
                    start,
                ))
            }
            Tok::UnbasedUnsized(c) => {
                self.bump();
                Ok(Expr::new(ExprKind::UnbasedUnsized(c), start))
            }
            Tok::Real(r) | Tok::Time(r) => {
                self.bump();
                Ok(Expr::new(ExprKind::Real(r), start))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Expr::new(ExprKind::Str(s), start))
            }
            Tok::Ident(_) | Tok::EscIdent(_) => {
                let path = self.path()?;
                if self.is_op("(") {
                    let args = self.call_args()?;
                    let span = start.to(self.prev_span());
                    return Ok(Expr::new(ExprKind::Call { func: path, args }, span));
                }
                let span = start.to(self.prev_span());
                Ok(Expr::new(ExprKind::Ident(path), span))
            }
            Tok::SysIdent(name) => {
                self.bump();
                let mut args = Vec::new();
                if self.eat_op("(") {
                    if !self.is_op(")") {
                        loop {
                            if self.is_op(",") {
                                // Empty argument.
                                args.push(Expr::new(
                                    ExprKind::Unsupported("empty argument".into()),
                                    self.span(),
                                ));
                            } else if is_type_keyword(self.peek())
                                || self.is_kw("signed")
                                || self.is_kw("unsigned")
                            {
                                let t = self.data_type()?;
                                let sp = t.span;
                                args.push(Expr::new(ExprKind::Type(Box::new(t)), sp));
                            } else {
                                args.push(self.expr()?);
                            }
                            if !self.eat_op(",") {
                                break;
                            }
                        }
                    }
                    self.expect_op(")")?;
                }
                Ok(Expr::new(
                    ExprKind::SysCall { name, args },
                    start.to(self.prev_span()),
                ))
            }
            Tok::Op("(") => {
                self.bump();
                self.enter()?;
                let e = self.expr()?;
                if self.is_op(":") {
                    // min:typ:max; keep the typical value.
                    self.bump();
                    let typ = self.expr()?;
                    self.expect_op(":")?;
                    let _max = self.expr()?;
                    self.expect_op(")")?;
                    self.leave();
                    return Ok(typ);
                }
                self.expect_op(")")?;
                self.leave();
                let mut e = e;
                e.span = start.to(self.prev_span());
                Ok(e)
            }
            Tok::Op("{") => self.concat(),
            Tok::TickBrace => self.pattern(None),
            Tok::Kw("signed" | "unsigned" | "const") if self.is_op_at(1, "'") => {
                let k = self.bump();
                self.bump();
                let target = match k.tok {
                    Tok::Kw("signed") => CastTarget::Signed,
                    Tok::Kw("unsigned") => CastTarget::Unsigned,
                    _ => CastTarget::Const,
                };
                self.cast_operand(target, start)
            }
            t if is_type_keyword(&t) => {
                let ty = self.data_type()?;
                if self.eat_op("'") {
                    return self.cast_operand(CastTarget::Type(ty), start);
                }
                if self.is_tick_brace() {
                    return self.pattern(Some(ty));
                }
                let sp = ty.span;
                Ok(Expr::new(ExprKind::Type(Box::new(ty)), sp))
            }
            Tok::Kw("type") => {
                let ty = self.data_type()?;
                let sp = ty.span;
                if self.eat_op("'") {
                    return self.cast_operand(CastTarget::Type(ty), start);
                }
                Ok(Expr::new(ExprKind::Type(Box::new(ty)), sp))
            }
            Tok::Kw(k @ ("null" | "this" | "super" | "new")) => {
                self.bump();
                Ok(Expr::new(ExprKind::Unsupported(k.to_string()), start))
            }
            Tok::Op("$") => {
                self.bump();
                Ok(Expr::new(ExprKind::Unsupported("$".into()), start))
            }
            _ => Err(self.unexpected("an expression")),
        }
    }

    fn is_tick_brace(&self) -> bool {
        matches!(self.peek(), Tok::TickBrace)
    }

    /// `( expr )` after `target'`.
    fn cast_operand(
        &mut self,
        target: CastTarget,
        start: crate::verilog::span::Span,
    ) -> PResult<Expr> {
        self.expect_op("(")?;
        let e = self.expr()?;
        let end = self.expect_op(")")?;
        Ok(Expr::new(
            ExprKind::Cast {
                target,
                expr: Box::new(e),
            },
            start.to(end),
        ))
    }

    fn postfix(&mut self, mut e: Expr) -> PResult<Expr> {
        loop {
            if self.is_op("[") {
                self.bump();
                let a = self.expr()?;
                let kind = if self.eat_op(":") {
                    Some(SliceKind::Const)
                } else if self.eat_op("+:") {
                    Some(SliceKind::Up)
                } else if self.eat_op("-:") {
                    Some(SliceKind::Down)
                } else {
                    None
                };
                let (kind_expr, end) = match kind {
                    Some(k) => {
                        let b = self.expr()?;
                        let end = self.expect_op("]")?;
                        (Some((k, b)), end)
                    }
                    None => (None, self.expect_op("]")?),
                };
                let span = e.span.to(end);
                e = match kind_expr {
                    Some((k, b)) => Expr::new(
                        ExprKind::Slice {
                            base: Box::new(e),
                            kind: k,
                            left: Box::new(a),
                            right: Box::new(b),
                        },
                        span,
                    ),
                    None => Expr::new(ExprKind::Index(Box::new(e), Box::new(a)), span),
                };
            } else if self.is_op(".") && self.is_ident_at(1) {
                self.bump();
                let m = self.ident()?;
                let span = e.span.to(m.span);
                e = Expr::new(ExprKind::Member(Box::new(e), m), span);
            } else if self.is_op("'") && self.is_op_at(1, "(") {
                self.bump();
                let start = e.span;
                e = self.cast_operand(CastTarget::Expr(Box::new(e)), start)?;
            } else if self.is_tick_brace() && matches!(e.kind, ExprKind::Ident(_)) {
                let ty = match e.kind {
                    ExprKind::Ident(p) => crate::verilog::ast::DataType {
                        kind: crate::verilog::ast::TypeKind::Named(p),
                        signing: None,
                        packed: Vec::new(),
                        span: e.span,
                    },
                    _ => unreachable!("checked above"),
                };
                e = self.pattern(Some(ty))?;
            } else {
                return Ok(e);
            }
        }
    }

    /// Subroutine call arguments `( ... )`.
    pub(crate) fn call_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect_op("(")?;
        let mut args = Vec::new();
        if self.eat_op(")") {
            return Ok(args);
        }
        loop {
            if self.is_op(".") && self.is_ident_at(1) {
                self.bump();
                let n = self.ident()?;
                self.expect_op("(")?;
                let v = if self.is_op(")") {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect_op(")")?;
                args.push(Arg::Named(n, v));
            } else if self.is_op(",") || self.is_op(")") {
                args.push(Arg::Positional(None));
            } else {
                args.push(Arg::Positional(Some(self.expr()?)));
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(args)
    }

    /// Concatenation, replication or streaming at `{`.
    fn concat(&mut self) -> PResult<Expr> {
        let start = self.expect_op("{")?;
        if self.is_op("<<") || self.is_op(">>") {
            // Streaming concatenation: skip.
            let mut depth = 1usize;
            while depth > 0 {
                match self.peek() {
                    Tok::Eof => return Err(self.unexpected("'}'")),
                    Tok::Op("{") | Tok::TickBrace => depth += 1,
                    Tok::Op("}") => depth -= 1,
                    _ => {}
                }
                self.bump();
            }
            return Ok(Expr::new(
                ExprKind::Unsupported("streaming concatenation".into()),
                start.to(self.prev_span()),
            ));
        }
        self.enter()?;
        let first = self.expr()?;
        if self.is_op("{") {
            // Replication {n{...}}.
            self.bump();
            let mut items = vec![self.expr()?];
            while self.eat_op(",") {
                items.push(self.expr()?);
            }
            self.expect_op("}")?;
            let end = self.expect_op("}")?;
            self.leave();
            return Ok(Expr::new(
                ExprKind::Repl(Box::new(first), items),
                start.to(end),
            ));
        }
        let mut items = vec![first];
        while self.eat_op(",") {
            items.push(self.expr()?);
        }
        let end = self.expect_op("}")?;
        self.leave();
        Ok(Expr::new(ExprKind::Concat(items), start.to(end)))
    }

    /// Assignment pattern at `'{`.
    fn pattern(&mut self, ty: Option<crate::verilog::ast::DataType>) -> PResult<Expr> {
        let start = ty.as_ref().map_or(self.span(), |t| t.span);
        if !self.is_tick_brace() {
            return Err(self.unexpected("'{"));
        }
        self.bump();
        self.enter()?;
        let mut items = Vec::new();
        let mut repl = None;
        if !self.is_op("}") {
            loop {
                let item = if self.is_kw("default") && self.is_op_at(1, ":") {
                    self.bump();
                    self.bump();
                    PatternItem::Keyed(PatternKey::Default, self.expr()?)
                } else if (is_type_keyword(self.peek())) && self.is_op_at(1, ":") {
                    let t = self.data_type()?;
                    self.expect_op(":")?;
                    PatternItem::Keyed(PatternKey::Type(t), self.expr()?)
                } else {
                    let k = self.expr()?;
                    if self.eat_op(":") {
                        PatternItem::Keyed(PatternKey::Expr(k), self.expr()?)
                    } else if self.is_op("{") && items.is_empty() && repl.is_none() {
                        // '{n{a, b}}
                        self.bump();
                        let mut inner = vec![PatternItem::Positional(self.expr()?)];
                        while self.eat_op(",") {
                            inner.push(PatternItem::Positional(self.expr()?));
                        }
                        self.expect_op("}")?;
                        repl = Some(Box::new(k));
                        items = inner;
                        break;
                    } else {
                        PatternItem::Positional(k)
                    }
                };
                items.push(item);
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        let end = self.expect_op("}")?;
        self.leave();
        Ok(Expr::new(
            ExprKind::Pattern {
                ty: ty.map(Box::new),
                repl,
                items,
            },
            start.to(end),
        ))
    }

    /// Assignment target: identifier with selects and members, or a concatenation of targets.
    pub(crate) fn lvalue(&mut self) -> PResult<Expr> {
        if self.is_op("{") {
            return self.concat();
        }
        if self.is_tick_brace() {
            return self.pattern(None);
        }
        let start = self.span();
        let path = self.path()?;
        if self.is_op("(") {
            // Subroutine call used as a statement.
            let args = self.call_args()?;
            let span = start.to(self.prev_span());
            return Ok(Expr::new(ExprKind::Call { func: path, args }, span));
        }
        let span = start.to(self.prev_span());
        let base = Expr::new(ExprKind::Ident(path), span);
        self.postfix(base)
    }
}

#[cfg(test)]
mod tests {
    use crate::verilog::ast::{BinOp, ExprKind, UnOp};
    use crate::verilog::test_util::parse_expr;

    #[test]
    fn precedence_and_associativity() {
        let e = parse_expr("a + b * c - d");
        // (a + (b * c)) - d
        let ExprKind::Binary(BinOp::Sub, l, _) = &e.kind else {
            panic!("{e:?}")
        };
        let ExprKind::Binary(BinOp::Add, _, r) = &l.kind else {
            panic!()
        };
        assert!(matches!(r.kind, ExprKind::Binary(BinOp::Mul, _, _)));
        let e = parse_expr("a ? b : c ? d : e");
        let ExprKind::Cond(_, _, els) = &e.kind else {
            panic!()
        };
        assert!(matches!(els.kind, ExprKind::Cond(..)));
        let e = parse_expr("a == b && c | d & e");
        let ExprKind::Binary(BinOp::LogAnd, l, r) = &e.kind else {
            panic!()
        };
        assert!(matches!(l.kind, ExprKind::Binary(BinOp::Eq, _, _)));
        let ExprKind::Binary(BinOp::BitOr, _, rr) = &r.kind else {
            panic!()
        };
        assert!(matches!(rr.kind, ExprKind::Binary(BinOp::BitAnd, _, _)));
        let e = parse_expr("-a ** 2");
        let ExprKind::Binary(BinOp::Pow, l, _) = &e.kind else {
            panic!()
        };
        assert!(matches!(l.kind, ExprKind::Unary(UnOp::Minus, _)));
        let e = parse_expr("a ** b ** c");
        let ExprKind::Binary(BinOp::Pow, l, _) = &e.kind else {
            panic!()
        };
        assert!(matches!(l.kind, ExprKind::Binary(BinOp::Pow, _, _)));
        let e = parse_expr("a << 1 < b");
        assert!(matches!(e.kind, ExprKind::Binary(BinOp::Lt, _, _)));
        let e = parse_expr("x inside {1, [2:3]} || y");
        let ExprKind::Binary(BinOp::LogOr, l, _) = &e.kind else {
            panic!()
        };
        assert!(matches!(l.kind, ExprKind::Inside(..)));
    }

    #[test]
    fn primaries_and_postfix() {
        assert!(matches!(parse_expr("8'hff").kind, ExprKind::Number(_)));
        assert!(
            matches!(parse_expr("{a, b[3:0], c[i+:2]}").kind, ExprKind::Concat(ref v) if v.len() == 3)
        );
        assert!(matches!(parse_expr("{4{a}}").kind, ExprKind::Repl(..)));
        assert!(matches!(parse_expr("s.f[2].g").kind, ExprKind::Member(..)));
        assert!(matches!(parse_expr("pkg::C").kind, ExprKind::Ident(ref p) if p.scope.is_some()));
        assert!(matches!(
            parse_expr("f(a, .b(c))").kind,
            ExprKind::Call { .. }
        ));
        assert!(matches!(
            parse_expr("$clog2(N)").kind,
            ExprKind::SysCall { .. }
        ));
        assert!(
            matches!(parse_expr("$bits(logic [3:0])").kind, ExprKind::SysCall { ref args, .. } if matches!(args[0].kind, ExprKind::Type(_)))
        );
        assert!(matches!(parse_expr("W'(x)").kind, ExprKind::Cast { .. }));
        assert!(matches!(parse_expr("8'(x)").kind, ExprKind::Cast { .. }));
        assert!(matches!(parse_expr("int'(x)").kind, ExprKind::Cast { .. }));
        assert!(matches!(
            parse_expr("signed'(x)").kind,
            ExprKind::Cast { .. }
        ));
        assert!(matches!(
            parse_expr("'{default: '0}").kind,
            ExprKind::Pattern { .. }
        ));
        assert!(matches!(
            parse_expr("my_t'{a: 1, b: 2}").kind,
            ExprKind::Pattern { ty: Some(_), .. }
        ));
        assert!(matches!(
            parse_expr("'{4{1'b0}}").kind,
            ExprKind::Pattern { repl: Some(_), .. }
        ));
        assert!(matches!(
            parse_expr("{<<{x}}").kind,
            ExprKind::Unsupported(_)
        ));
    }
}
