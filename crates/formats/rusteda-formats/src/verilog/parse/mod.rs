// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Recursive-descent parser (VERILOG-REQ-0100, VERILOG-REQ-0101). Expressions use operator
//! precedence parsing [@pratt1973top].

mod expr;
mod instances;
mod items;
mod stmt;
mod subroutines;
mod types;

use crate::verilog::ast::{Ident, SourceUnit};
use crate::verilog::diag::Diagnostic;
use crate::verilog::span::Span;
use crate::verilog::token::{Tok, Token};

/// Maximum syntactic nesting (VERILOG-REQ-0101).
pub const MAX_NESTING: usize = 1024;

pub(crate) type PResult<T> = Result<T, Diagnostic>;

pub(crate) struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
    depth: usize,
    default_nettype: Option<String>,
    /// Stop token for simple-attribute-like contexts (unused except for recovery).
    eof: Token,
}

/// Parses a preprocessed token stream (ending with `Eof`).
pub fn parse_tokens(toks: &[Token]) -> Result<SourceUnit, Diagnostic> {
    let eof_span = toks.last().map(|t| t.span).unwrap_or_default();
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
        default_nettype: None,
        eof: Token {
            tok: Tok::Eof,
            span: eof_span,
        },
    };
    p.source_unit()
}

/// Parses a token stream holding exactly one expression.
pub fn parse_expr_tokens(toks: &[Token]) -> Result<crate::verilog::ast::Expr, Diagnostic> {
    let eof_span = toks.last().map(|t| t.span).unwrap_or_default();
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
        default_nettype: None,
        eof: Token {
            tok: Tok::Eof,
            span: eof_span,
        },
    };
    let e = p.expr()?;
    if !p.at_eof() {
        return Err(p.unexpected("end of expression"));
    }
    Ok(e)
}

impl<'a> Parser<'a> {
    pub(crate) fn tok_at(&self, k: usize) -> &Token {
        self.toks.get(self.pos + k).unwrap_or(&self.eof)
    }

    pub(crate) fn peek(&self) -> &Tok {
        &self.tok_at(0).tok
    }

    pub(crate) fn peek_at(&self, k: usize) -> &Tok {
        &self.tok_at(k).tok
    }

    pub(crate) fn span(&self) -> Span {
        self.tok_at(0).span
    }

    pub(crate) fn prev_span(&self) -> Span {
        if self.pos == 0 {
            self.span()
        } else {
            self.toks
                .get(self.pos - 1)
                .map_or(self.eof.span, |t| t.span)
        }
    }

    pub(crate) fn bump(&mut self) -> Token {
        let t = self.tok_at(0).clone();
        if self.pos < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    pub(crate) fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    pub(crate) fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), Tok::Kw(x) if *x == k)
    }

    pub(crate) fn is_kw_at(&self, n: usize, k: &str) -> bool {
        matches!(self.peek_at(n), Tok::Kw(x) if *x == k)
    }

    pub(crate) fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(crate) fn expect_kw(&mut self, k: &str) -> PResult<Span> {
        if self.is_kw(k) {
            Ok(self.bump().span)
        } else {
            Err(self.unexpected(&format!("'{k}'")))
        }
    }

    pub(crate) fn is_op(&self, o: &str) -> bool {
        matches!(self.peek(), Tok::Op(x) if *x == o)
    }

    pub(crate) fn is_op_at(&self, n: usize, o: &str) -> bool {
        matches!(self.peek_at(n), Tok::Op(x) if *x == o)
    }

    pub(crate) fn eat_op(&mut self, o: &str) -> bool {
        if self.is_op(o) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(crate) fn expect_op(&mut self, o: &str) -> PResult<Span> {
        if self.is_op(o) {
            Ok(self.bump().span)
        } else {
            Err(self.unexpected(&format!("'{o}'")))
        }
    }

    pub(crate) fn is_ident(&self) -> bool {
        matches!(self.peek(), Tok::Ident(_) | Tok::EscIdent(_))
    }

    pub(crate) fn is_ident_at(&self, n: usize) -> bool {
        matches!(self.peek_at(n), Tok::Ident(_) | Tok::EscIdent(_))
    }

    pub(crate) fn ident(&mut self) -> PResult<Ident> {
        match self.peek().clone() {
            Tok::Ident(n) | Tok::EscIdent(n) => {
                let span = self.bump().span;
                Ok(Ident { name: n, span })
            }
            _ => Err(self.unexpected("an identifier")),
        }
    }

    pub(crate) fn describe(t: &Tok) -> String {
        match t {
            Tok::Ident(n) => format!("identifier '{n}'"),
            Tok::EscIdent(n) => format!("identifier '\\{n}'"),
            Tok::SysIdent(n) => format!("'{n}'"),
            Tok::Kw(k) => format!("keyword '{k}'"),
            Tok::Dec(d) => format!("number '{d}'"),
            Tok::Based { .. } | Tok::UnbasedUnsized(_) => "a based number".into(),
            Tok::Real(r) => format!("real number '{r}'"),
            Tok::Time(r) => format!("time literal '{r}'"),
            Tok::Str(_) => "a string".into(),
            Tok::Op(o) => format!("'{o}'"),
            Tok::TickBrace => "'''{'".into(),
            Tok::DefaultNettype(_) => "`default_nettype".into(),
            Tok::CaseAttrs(_) => "case attributes".into(),
            Tok::Eof => "end of file".into(),
        }
    }

    pub(crate) fn unexpected(&self, expected: &str) -> Diagnostic {
        Diagnostic::error(
            "V0100",
            self.span(),
            format!("expected {expected}, found {}", Self::describe(self.peek())),
        )
    }

    pub(crate) fn error(&self, span: Span, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::error("V0101", span, msg)
    }

    pub(crate) fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(self.error(self.span(), "construct nested too deeply"));
        }
        Ok(())
    }

    pub(crate) fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Skips a balanced bracketed group starting at the current `(`, `[` or `{`.
    pub(crate) fn skip_balanced(&mut self) -> PResult<()> {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                Tok::Eof => return Err(self.unexpected("a closing bracket")),
                Tok::Op("(" | "[" | "{") | Tok::TickBrace => depth += 1,
                Tok::Op(")" | "]" | "}") => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        self.bump();
                        return Ok(());
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Skips tokens up to and including the next `;` at bracket depth 0.
    pub(crate) fn skip_to_semicolon(&mut self) -> PResult<Span> {
        let start = self.span();
        let mut depth = 0usize;
        loop {
            match self.peek() {
                Tok::Eof => return Err(self.unexpected("';'")),
                Tok::Op("(" | "[" | "{") | Tok::TickBrace => depth += 1,
                Tok::Op(")" | "]" | "}") => depth = depth.saturating_sub(1),
                Tok::Op(";") if depth == 0 => {
                    let end = self.bump().span;
                    return Ok(start.to(end));
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Skips tokens up to and including keyword `end`.
    pub(crate) fn skip_to_kw(&mut self, end: &str) -> PResult<Span> {
        let start = self.span();
        while !self.is_kw(end) {
            if self.at_eof() {
                return Err(self.unexpected(&format!("'{end}'")));
            }
            self.bump();
        }
        let e = self.bump().span;
        self.end_label();
        Ok(start.to(e))
    }

    /// Consumes an optional `: label` after an end keyword.
    pub(crate) fn end_label(&mut self) {
        if self.is_op(":") && self.is_ident_at(1) {
            self.bump();
            self.bump();
        }
    }

    /// Index just after a balanced bracket group starting at offset `k`.
    pub(crate) fn skip_brackets_from(&self, mut k: usize) -> usize {
        let mut depth = 0usize;
        loop {
            match self.peek_at(k) {
                Tok::Eof => return k,
                Tok::Op("(" | "[" | "{") | Tok::TickBrace => depth += 1,
                Tok::Op(")" | "]" | "}") => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return k + 1;
                    }
                }
                _ => {}
            }
            k += 1;
        }
    }
}
