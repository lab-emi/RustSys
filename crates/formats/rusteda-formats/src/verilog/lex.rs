// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Lexer over one text buffer (VERILOG-REQ-0001, VERILOG-REQ-0002). The preprocessor drives a
//! stack of cursors and uses the character-level helpers for directive processing.

use std::sync::Arc;

use crate::verilog::span::{FileId, Span};
use crate::verilog::token::{OPERATORS, Tok, Token, keyword};

/// Result of lexing one raw token.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    Token(Token),
    /// A backtick followed by a name: directive or macro use.
    Directive(String, Span),
    Eof,
}

/// Lexer error.
#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub span: Span,
    pub msg: String,
}

/// Cursor over a text buffer.
#[derive(Debug, Clone)]
pub struct Cursor {
    text: Arc<str>,
    pub pos: usize,
    pub file: FileId,
    /// All tokens of a macro expansion carry the span of the macro use.
    pub span_override: Option<Span>,
    /// Case attributes of the attribute instances skipped since the last token.
    case_attrs: crate::verilog::ast::CaseAttrs,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

impl Cursor {
    pub fn new(text: Arc<str>, file: FileId, span_override: Option<Span>) -> Cursor {
        Cursor {
            text,
            pos: 0,
            file,
            span_override,
            case_attrs: crate::verilog::ast::CaseAttrs::default(),
        }
    }

    fn bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    pub fn peek(&self) -> Option<u8> {
        self.bytes().get(self.pos).copied()
    }

    pub fn peek_at(&self, k: usize) -> Option<u8> {
        self.bytes().get(self.pos + k).copied()
    }

    pub fn at_eof(&self) -> bool {
        self.pos >= self.bytes().len()
    }

    pub fn span(&self, start: usize, end: usize) -> Span {
        self.span_override
            .unwrap_or_else(|| Span::new(self.file, start, end))
    }

    fn err(&self, start: usize, msg: impl Into<String>) -> LexError {
        LexError {
            span: self.span(start, self.pos.max(start + 1)),
            msg: msg.into(),
        }
    }

    /// Skips spaces and tabs (not newlines).
    pub fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | 0x0c)) {
            self.pos += 1;
        }
    }

    /// Skips white space, newlines, comments and attribute instances.
    pub fn skip_trivia(&mut self) -> Result<(), LexError> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n' | 0x0c) => self.pos += 1,
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        match self.peek() {
                            None => return Err(self.err(start, "unterminated block comment")),
                            Some(b'*') if self.peek_at(1) == Some(b'/') => {
                                self.pos += 2;
                                break;
                            }
                            _ => self.pos += 1,
                        }
                    }
                }
                Some(b'(') if self.peek_at(1) == Some(b'*') && !self.is_star_paren() => {
                    // Attribute instance (* ... *). The case attributes are kept for the next
                    // token (VERILOG-REQ-0102); every other attribute is ignored.
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        match self.peek() {
                            None => return Err(self.err(start, "unterminated attribute instance")),
                            Some(b'*') if self.peek_at(1) == Some(b')') => {
                                self.pos += 2;
                                break;
                            }
                            Some(b'"') => {
                                self.string()?;
                            }
                            Some(b) if is_ident_start(b) => {
                                let name = self.read_ident().unwrap_or_default();
                                match name.as_str() {
                                    "full_case" => self.case_attrs.full = true,
                                    "parallel_case" => self.case_attrs.parallel = true,
                                    _ => {}
                                }
                            }
                            _ => self.pos += 1,
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// True at `(*)` (possibly with blanks), which is not an attribute.
    fn is_star_paren(&self) -> bool {
        let mut k = 2;
        while matches!(self.peek_at(k), Some(b' ' | b'\t')) {
            k += 1;
        }
        self.peek_at(k) == Some(b')')
    }

    /// Reads a simple identifier at the cursor.
    pub fn read_ident(&mut self) -> Option<String> {
        let start = self.pos;
        if !self.peek().is_some_and(is_ident_start) {
            return None;
        }
        while self.peek().is_some_and(is_ident_char) {
            self.pos += 1;
        }
        Some(self.text[start..self.pos].to_string())
    }

    /// Lexes the next raw token.
    pub fn next_raw(&mut self) -> Result<Raw, LexError> {
        self.skip_trivia()?;
        let start = self.pos;
        let attrs = std::mem::take(&mut self.case_attrs);
        if attrs != crate::verilog::ast::CaseAttrs::default() {
            let at = self.pos;
            let next = self.read_ident();
            self.pos = at;
            if matches!(
                next.as_deref(),
                Some("case" | "casez" | "casex" | "unique" | "unique0" | "priority")
            ) {
                return Ok(Raw::Token(Token {
                    tok: Tok::CaseAttrs(attrs),
                    span: self.span(start, start),
                }));
            }
        }
        let Some(c) = self.peek() else {
            return Ok(Raw::Eof);
        };
        let tok = match c {
            b'`' => {
                self.pos += 1;
                // Directive names may be escaped-free identifiers, including `__FILE__`.
                match self.read_ident() {
                    Some(name) => return Ok(Raw::Directive(name, self.span(start, self.pos))),
                    None => {
                        return Err(self.err(start, "expected a directive or macro name after '`'"));
                    }
                }
            }
            c if is_ident_start(c) => {
                let name = self.read_ident().unwrap_or_default();
                match keyword(&name) {
                    Some(k) => Tok::Kw(k),
                    None => Tok::Ident(name),
                }
            }
            b'\\' => {
                self.pos += 1;
                let s = self.pos;
                while self.peek().is_some_and(|b| !b.is_ascii_whitespace()) {
                    self.pos += 1;
                }
                if s == self.pos {
                    return Err(self.err(start, "empty escaped identifier"));
                }
                Tok::EscIdent(self.text[s..self.pos].to_string())
            }
            b'$' if self.peek_at(1).is_some_and(is_ident_char) => {
                self.pos += 1;
                while self.peek().is_some_and(is_ident_char) {
                    self.pos += 1;
                }
                Tok::SysIdent(self.text[start..self.pos].to_string())
            }
            b'0'..=b'9' => self.number()?,
            b'\'' => self.tick()?,
            b'"' => Tok::Str(self.string()?),
            _ => {
                let rest = &self.bytes()[self.pos..];
                // A comment is whitespace, including immediately after a case
                // label or ternary colon. Do not consume its first slash as
                // part of the distribution-weight operator `:/`.
                let colon_comment = rest.starts_with(b"://") || rest.starts_with(b":/*");
                match OPERATORS.iter().find(|op| {
                    rest.starts_with(op.as_bytes()) && !(colon_comment && op.as_bytes() == b":/")
                }) {
                    Some(op) => {
                        self.pos += op.len();
                        Tok::Op(op)
                    }
                    None => {
                        self.pos += 1;
                        return Err(
                            self.err(start, format!("unexpected character '{}'", c as char))
                        );
                    }
                }
            }
        };
        Ok(Raw::Token(Token {
            tok,
            span: self.span(start, self.pos),
        }))
    }

    fn number(&mut self) -> Result<Tok, LexError> {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9' | b'_')) {
            self.pos += 1;
        }
        let mut is_real = false;
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) {
            is_real = true;
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9' | b'_')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                is_real = true;
                while matches!(self.peek(), Some(b'0'..=b'9' | b'_')) {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        // Time literal suffix.
        for unit in ["ms", "us", "ns", "ps", "fs", "s"] {
            let rest = &self.bytes()[self.pos..];
            if rest.starts_with(unit.as_bytes())
                && !rest.get(unit.len()).is_some_and(|b| is_ident_char(*b))
            {
                self.pos += unit.len();
                return Ok(Tok::Time(self.text[start..self.pos].to_string()));
            }
        }
        let text: String = self.text[start..self.pos]
            .chars()
            .filter(|c| *c != '_')
            .collect();
        Ok(if is_real {
            Tok::Real(text)
        } else {
            Tok::Dec(text)
        })
    }

    fn tick(&mut self) -> Result<Tok, LexError> {
        let start = self.pos;
        self.pos += 1;
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                Ok(Tok::TickBrace)
            }
            Some(b'(') => Ok(Tok::Op("'")),
            Some(c @ (b'0' | b'1' | b'x' | b'X' | b'z' | b'Z'))
                if !self.peek_at(1).is_some_and(is_ident_char) =>
            {
                self.pos += 1;
                Ok(Tok::UnbasedUnsized((c as char).to_ascii_lowercase()))
            }
            _ => {
                let mut signed = false;
                if matches!(self.peek(), Some(b's' | b'S')) {
                    signed = true;
                    self.pos += 1;
                }
                let base = match self.peek() {
                    Some(b @ (b'b' | b'B' | b'o' | b'O' | b'd' | b'D' | b'h' | b'H')) => {
                        (b as char).to_ascii_lowercase()
                    }
                    _ => return Err(self.err(start, "invalid based literal or stray apostrophe")),
                };
                self.pos += 1;
                self.skip_inline_ws();
                let ds = self.pos;
                while self.peek().is_some_and(|b| {
                    b.is_ascii_hexdigit() || matches!(b, b'x' | b'X' | b'z' | b'Z' | b'?' | b'_')
                }) {
                    self.pos += 1;
                }
                let digits: String = self.text[ds..self.pos]
                    .chars()
                    .filter(|c| *c != '_')
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                if digits.is_empty() {
                    return Err(self.err(start, "based literal without digits"));
                }
                Ok(Tok::Based {
                    signed,
                    base,
                    digits,
                })
            }
        }
    }

    /// Reads a string literal at the cursor (opening quote) and returns its bytes.
    pub fn string(&mut self) -> Result<Vec<u8>, LexError> {
        let start = self.pos;
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err(start, "unterminated string literal"));
            };
            self.pos += 1;
            match c {
                b'"' => break,
                b'\n' => return Err(self.err(start, "newline in string literal")),
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err(self.err(start, "unterminated string literal"));
                    };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'v' => out.push(0x0b),
                        b'f' => out.push(0x0c),
                        b'a' => out.push(0x07),
                        b'\\' => out.push(b'\\'),
                        b'"' => out.push(b'"'),
                        b'\n' => {}
                        b'\r' if self.peek() == Some(b'\n') => self.pos += 1,
                        b'x' => {
                            let mut v = 0u32;
                            let mut n = 0;
                            while n < 2 && self.peek().is_some_and(|b| b.is_ascii_hexdigit()) {
                                v = v * 16
                                    + (self.peek().unwrap_or(b'0') as char)
                                        .to_digit(16)
                                        .unwrap_or(0);
                                self.pos += 1;
                                n += 1;
                            }
                            out.push(v as u8);
                        }
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            let mut n = 1;
                            while n < 3 && matches!(self.peek(), Some(b'0'..=b'7')) {
                                v = v * 8 + u32::from(self.peek().unwrap_or(b'0') - b'0');
                                self.pos += 1;
                                n += 1;
                            }
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        Ok(out)
    }

    /// Reads the body of a `define: text to the end of the line, joining lines ended by a
    /// backslash and dropping `//` comments. Block comments are kept out of the body.
    pub fn read_define_body(&mut self) -> Result<String, LexError> {
        let mut out = String::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => break,
                Some(b'\\') if matches!(self.peek_at(1), Some(b'\n')) => {
                    self.pos += 2;
                    out.push('\n');
                }
                Some(b'\\') if self.peek_at(1) == Some(b'\r') && self.peek_at(2) == Some(b'\n') => {
                    self.pos += 3;
                    out.push('\n');
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    // Line comment: skip to end of line, but a trailing backslash still continues.
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        if self.peek() == Some(b'\\') && self.peek_at(1) == Some(b'\n') {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        match self.peek() {
                            None => return Err(self.err(start, "unterminated block comment")),
                            Some(b'*') if self.peek_at(1) == Some(b'/') => {
                                self.pos += 2;
                                break;
                            }
                            _ => self.pos += 1,
                        }
                    }
                    out.push(' ');
                }
                Some(b'"') => {
                    let s = self.pos;
                    // Copy the raw string text including escapes.
                    self.pos += 1;
                    loop {
                        match self.peek() {
                            None | Some(b'\n') => {
                                return Err(self.err(s, "unterminated string in macro body"));
                            }
                            Some(b'\\') => self.pos += 2,
                            Some(b'"') => {
                                self.pos += 1;
                                break;
                            }
                            _ => self.pos += 1,
                        }
                    }
                    out.push_str(&self.text[s..self.pos.min(self.text.len())]);
                }
                Some(_) => {
                    let s = self.pos;
                    let ch = self.text[s..].chars().next().unwrap_or(' ');
                    self.pos += ch.len_utf8();
                    out.push(ch);
                }
            }
        }
        Ok(out.trim().to_string())
    }

    /// Reads the rest of the current line (for directives such as `timescale).
    pub fn read_line(&mut self) -> String {
        let s = self.pos;
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.pos += 1;
        }
        self.text[s..self.pos].to_string()
    }

    /// Reads macro actual arguments; the cursor must be at `(`. Returns raw argument texts.
    pub fn read_macro_args(&mut self) -> Result<Vec<String>, LexError> {
        let start = self.pos;
        self.pos += 1;
        let mut args = Vec::new();
        let mut cur = String::new();
        let mut depth = 0usize;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err(start, "unterminated macro argument list"));
            };
            match c {
                b'"' => {
                    let s = self.pos;
                    self.string()?;
                    cur.push_str(&self.text[s..self.pos]);
                }
                b'/' if self.peek_at(1) == Some(b'/') || self.peek_at(1) == Some(b'*') => {
                    self.skip_trivia()?;
                    cur.push(' ');
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    cur.push(c as char);
                    self.pos += 1;
                }
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        if c == b')' {
                            self.pos += 1;
                            args.push(cur.trim().to_string());
                            return Ok(args);
                        }
                        return Err(self.err(self.pos, "unbalanced bracket in macro arguments"));
                    }
                    depth -= 1;
                    cur.push(c as char);
                    self.pos += 1;
                }
                b',' if depth == 0 => {
                    args.push(cur.trim().to_string());
                    cur.clear();
                    self.pos += 1;
                }
                _ => {
                    let ch = self.text[self.pos..].chars().next().unwrap_or(' ');
                    self.pos += ch.len_utf8();
                    cur.push(ch);
                }
            }
        }
    }

    /// In a skipped conditional region: advances to the next conditional directive
    /// (`ifdef`, `ifndef`, `elsif`, `else`, `endif`) and returns its name and span.
    pub fn skip_to_conditional(&mut self) -> Result<Option<(String, Span)>, LexError> {
        loop {
            match self.peek() {
                None => return Ok(None),
                Some(b'/') if matches!(self.peek_at(1), Some(b'/' | b'*')) => self.skip_trivia()?,
                Some(b'"') => {
                    // Strings in skipped text may be malformed; skip to the closing quote or line end.
                    self.pos += 1;
                    while !matches!(self.peek(), None | Some(b'"' | b'\n')) {
                        if self.peek() == Some(b'\\') {
                            self.pos += 1;
                        }
                        self.pos += 1;
                    }
                    if self.peek() == Some(b'"') {
                        self.pos += 1;
                    }
                }
                Some(b'`') => {
                    let start = self.pos;
                    self.pos += 1;
                    if let Some(name) = self.read_ident()
                        && matches!(
                            name.as_str(),
                            "ifdef" | "ifndef" | "elsif" | "else" | "endif"
                        )
                    {
                        return Ok(Some((name, self.span(start, self.pos))));
                    }
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Current byte offset in the underlying text.
    pub fn offset(&self) -> usize {
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_all(s: &str) -> Vec<Tok> {
        let mut c = Cursor::new(Arc::from(s), FileId(0), None);
        let mut out = Vec::new();
        loop {
            match c.next_raw().unwrap() {
                Raw::Token(t) => out.push(t.tok),
                Raw::Directive(n, _) => out.push(Tok::Ident(format!("`{n}"))),
                Raw::Eof => break,
            }
        }
        out
    }

    // VERILOG-REQ-0001
    #[test]
    fn literals_identifiers_and_operators() {
        let t =
            lex_all("8'hFF 4'sb1x_0z 'd 12 'h 3f '0 '1 'x 'Z 1_000 3.5 2e3 10ns \\a+b  $clog2 a$b");
        assert_eq!(t[0], Tok::Dec("8".into()));
        assert_eq!(
            t[1],
            Tok::Based {
                signed: false,
                base: 'h',
                digits: "ff".into()
            }
        );
        assert_eq!(t[2], Tok::Dec("4".into()));
        assert_eq!(
            t[3],
            Tok::Based {
                signed: true,
                base: 'b',
                digits: "1x0z".into()
            }
        );
        assert_eq!(
            t[4],
            Tok::Based {
                signed: false,
                base: 'd',
                digits: "12".into()
            }
        );
        assert_eq!(
            t[5],
            Tok::Based {
                signed: false,
                base: 'h',
                digits: "3f".into()
            }
        );
        assert_eq!(
            &t[6..10],
            &[
                Tok::UnbasedUnsized('0'),
                Tok::UnbasedUnsized('1'),
                Tok::UnbasedUnsized('x'),
                Tok::UnbasedUnsized('z')
            ]
        );
        assert_eq!(t[10], Tok::Dec("1000".into()));
        assert_eq!(t[11], Tok::Real("3.5".into()));
        assert_eq!(t[12], Tok::Real("2e3".into()));
        assert_eq!(t[13], Tok::Time("10ns".into()));
        assert_eq!(t[14], Tok::EscIdent("a+b".into()));
        assert_eq!(t[15], Tok::SysIdent("$clog2".into()));
        assert_eq!(t[16], Tok::Ident("a$b".into()));
        let ops = lex_all("a<<<=b !==c ==? d+:e ::f '{ g'(h) @(*) (* keep *) i");
        assert!(ops.contains(&Tok::Op("<<<=")));
        assert!(ops.contains(&Tok::Op("!==")));
        assert!(ops.contains(&Tok::Op("==?")));
        assert!(ops.contains(&Tok::Op("+:")));
        assert!(ops.contains(&Tok::Op("::")));
        assert!(ops.contains(&Tok::TickBrace));
        assert!(ops.contains(&Tok::Op("'")));
        // @(*) is kept, the attribute is dropped.
        assert!(
            ops.windows(3)
                .any(|w| w == [Tok::Op("("), Tok::Op("*"), Tok::Op(")")])
        );
        assert!(!ops.contains(&Tok::Ident("keep".into())));
        assert_eq!(
            lex_all("module x"),
            vec![Tok::Kw("module"), Tok::Ident("x".into())]
        );
    }

    #[test]
    fn strings_and_comments() {
        let t = lex_all("\"a\\n\\\"b\\101\" // c\n /* d */ e");
        assert_eq!(t[0], Tok::Str(b"a\n\"bA".to_vec()));
        assert_eq!(t[1], Tok::Ident("e".into()));
        let mut c = Cursor::new(Arc::from("\"abc"), FileId(0), None);
        assert!(c.next_raw().is_err());
        let mut c = Cursor::new(Arc::from("/* x"), FileId(0), None);
        assert!(c.next_raw().is_err());
    }

    // VERILOG-REQ-0001, VERILOG-REQ-0002
    #[test]
    fn colon_does_not_consume_an_adjacent_comment() {
        assert_eq!(
            lex_all("1:// line\n x :/* block */ y :/2 :=3 ::tail"),
            vec![
                Tok::Dec("1".into()),
                Tok::Op(":"),
                Tok::Ident("x".into()),
                Tok::Op(":"),
                Tok::Ident("y".into()),
                Tok::Op(":/"),
                Tok::Dec("2".into()),
                Tok::Op(":="),
                Tok::Dec("3".into()),
                Tok::Op("::"),
                Tok::Ident("tail".into()),
            ]
        );
    }

    #[test]
    fn define_body_and_args() {
        let mut c = Cursor::new(Arc::from(" a + \\\n b // c\nnext"), FileId(0), None);
        assert_eq!(c.read_define_body().unwrap(), "a + \n b");
        let mut c = Cursor::new(Arc::from("(a, f(b, c), \"x,y\", {d,e})"), FileId(0), None);
        assert_eq!(
            c.read_macro_args().unwrap(),
            vec!["a", "f(b, c)", "\"x,y\"", "{d,e}"]
        );
    }
}
