// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Preprocessor (VERILOG-REQ-0200 to VERILOG-REQ-0205).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::verilog::diag::{Diagnostic, Severity};
use crate::verilog::lex::{Cursor, LexError, Raw};
use crate::verilog::span::{FileId, SourceMap, Span};
use crate::verilog::token::{Tok, Token};

/// Maximum macro expansion nesting (VERILOG-REQ-0205).
pub const MAX_EXPANSION_DEPTH: usize = 256;
/// Maximum include nesting (VERILOG-REQ-0202).
pub const MAX_INCLUDE_DEPTH: usize = 64;

/// A macro definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Macro {
    /// Formal arguments with optional defaults; `None` for object-like macros.
    pub params: Option<Vec<(String, Option<String>)>>,
    pub body: String,
}

/// Preprocessor options.
#[derive(Debug, Clone, Default)]
pub struct PpOptions {
    pub include_dirs: Vec<PathBuf>,
    pub defines: Vec<(String, String)>,
}

#[derive(Debug)]
struct Frame {
    cur: Cursor,
    /// Include frame (a real file) as opposed to a macro expansion.
    is_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CondState {
    /// Emitting this branch.
    Active,
    /// Skipping; a later `elsif`/`else` may become active.
    Waiting,
    /// Skipping; an earlier branch was taken.
    Done,
}

/// Preprocessor state that persists across files (VERILOG-REQ-0206).
#[derive(Debug, Default)]
pub struct Preprocessor {
    pub macros: HashMap<String, Macro>,
    pub options: PpOptions,
    pub diags: Vec<Diagnostic>,
}

fn pp_error(span: Span, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Severity::Error, "V0200", span, msg)
}

impl From<LexError> for Diagnostic {
    fn from(e: LexError) -> Diagnostic {
        Diagnostic::new(Severity::Error, "V0001", e.span, e.msg)
    }
}

/// Parses the header of a `define: name, optional formal list, body.
fn parse_define(cur: &mut Cursor) -> Result<(String, Macro), String> {
    cur.skip_inline_ws();
    let name = cur
        .read_ident()
        .ok_or("expected a macro name after `define")?;
    let mut params = None;
    if cur.peek() == Some(b'(') {
        // Formal arguments directly after the name.
        let args = cur.read_macro_args().map_err(|e| e.msg)?;
        let mut ps = Vec::new();
        for a in args {
            if a.is_empty() {
                return Err("empty formal argument".into());
            }
            match a.split_once('=') {
                Some((n, d)) => ps.push((n.trim().to_string(), Some(d.trim().to_string()))),
                None => ps.push((a.trim().to_string(), None)),
            }
        }
        params = Some(ps);
    }
    let body = cur.read_define_body().map_err(|e| e.msg)?;
    Ok((name, Macro { params, body }))
}

/// Substitutes actual arguments into a macro body, handling `` ``, `` `" `` and `` `\`" ``.
fn substitute(body: &str, formals: &[(String, Option<String>)], actuals: &[String]) -> String {
    let b = body.as_bytes();
    let mut out = String::with_capacity(body.len() + 16);
    let mut i = 0;
    let lookup = |name: &str| -> Option<&str> {
        formals
            .iter()
            .position(|(n, _)| n == name)
            .map(|k| actuals[k].as_str())
    };
    while i < b.len() {
        let c = b[i];
        if c == b'`' && b.get(i + 1) == Some(&b'`') {
            i += 2;
            continue;
        }
        if c == b'`' && b.get(i + 1) == Some(&b'"') {
            out.push('"');
            i += 2;
            continue;
        }
        if c == b'`'
            && b.get(i + 1) == Some(&b'\\')
            && b.get(i + 2) == Some(&b'`')
            && b.get(i + 3) == Some(&b'"')
        {
            out.push_str("\\\"");
            i += 4;
            continue;
        }
        if c == b'"' {
            // Ordinary string literal: copied without substitution.
            let s = i;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push_str(&body[s..i]);
            continue;
        }
        if c == b'\\' {
            // Escaped identifier: copied verbatim up to white space.
            let s = i;
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push_str(&body[s..i]);
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                i += 1;
            }
            let word = &body[s..i];
            // A word directly after a backtick is a macro or directive name, not a formal.
            let after_tick = s > 0 && b[s - 1] == b'`' && !(s > 1 && b[s - 2] == b'`');
            match lookup(word) {
                Some(a) if !after_tick => out.push_str(a),
                _ => out.push_str(word),
            }
            continue;
        }
        let ch = body[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

impl Preprocessor {
    pub fn new(options: PpOptions) -> Preprocessor {
        let mut pp = Preprocessor {
            options,
            ..Preprocessor::default()
        };
        let defs = pp.options.defines.clone();
        for (n, v) in defs {
            pp.macros.insert(
                n,
                Macro {
                    params: None,
                    body: v,
                },
            );
        }
        pp
    }

    fn find_include(&self, name: &str, from: Option<&Path>) -> Option<PathBuf> {
        let p = Path::new(name);
        if p.is_absolute() {
            return p.exists().then(|| p.to_path_buf());
        }
        if let Some(dir) = from.and_then(Path::parent) {
            let c = dir.join(p);
            if c.exists() {
                return Some(c);
            }
        }
        for d in &self.options.include_dirs {
            let c = d.join(p);
            if c.exists() {
                return Some(c);
            }
        }
        if p.exists() {
            return Some(p.to_path_buf());
        }
        None
    }

    /// Preprocesses one file and returns its tokens (ending with `Eof`). Errors are recorded in
    /// `self.diags`; the returned token stream is truncated at the first fatal error.
    pub fn run(&mut self, sm: &mut SourceMap, file: FileId) -> Vec<Token> {
        let text = sm.file(file).text.clone();
        let mut stack = vec![Frame {
            cur: Cursor::new(text, file, None),
            is_file: true,
        }];
        let mut conds: Vec<CondState> = Vec::new();
        let mut out = Vec::new();
        let mut last_span = Span::new(file, 0, 0);
        while let Some(frame) = stack.last_mut() {
            let active = conds.last().is_none_or(|c| *c == CondState::Active);
            if !active {
                // Skip text up to the next conditional directive.
                match frame.cur.skip_to_conditional() {
                    Err(e) => {
                        self.diags.push(e.into());
                        break;
                    }
                    Ok(None) => {
                        if frame.is_file && stack.len() == 1 {
                            self.diags.push(pp_error(last_span, "missing `endif"));
                            break;
                        }
                        stack.pop();
                        continue;
                    }
                    Ok(Some((name, span))) => {
                        if let Err(d) = self.conditional(&name, span, &mut stack, &mut conds) {
                            self.diags.push(d);
                            break;
                        }
                        continue;
                    }
                }
            }
            let raw = match frame.cur.next_raw() {
                Ok(r) => r,
                Err(e) => {
                    self.diags.push(e.into());
                    break;
                }
            };
            match raw {
                Raw::Eof => {
                    let f = stack.pop();
                    if stack.is_empty() {
                        if !conds.is_empty() {
                            self.diags.push(pp_error(last_span, "missing `endif"));
                        }
                        break;
                    }
                    let _ = f;
                }
                Raw::Token(t) => {
                    last_span = t.span;
                    out.push(t);
                }
                Raw::Directive(name, span) => {
                    last_span = span;
                    if let Err(d) =
                        self.directive(sm, &name, span, &mut stack, &mut conds, &mut out)
                    {
                        self.diags.push(d);
                        break;
                    }
                }
            }
        }
        out.push(Token {
            tok: Tok::Eof,
            span: last_span,
        });
        out
    }

    fn conditional(
        &mut self,
        name: &str,
        span: Span,
        stack: &mut [Frame],
        conds: &mut Vec<CondState>,
    ) -> Result<(), Diagnostic> {
        let Some(frame) = stack.last_mut() else {
            return Ok(());
        };
        match name {
            "ifdef" | "ifndef" => {
                frame.cur.skip_inline_ws();
                let m = frame
                    .cur
                    .read_ident()
                    .ok_or_else(|| pp_error(span, "expected a macro name"))?;
                let parent_active = conds.last().is_none_or(|c| *c == CondState::Active);
                let defined = self.macros.contains_key(&m);
                let take = if name == "ifdef" { defined } else { !defined };
                conds.push(if !parent_active {
                    CondState::Done
                } else if take {
                    CondState::Active
                } else {
                    CondState::Waiting
                });
            }
            "elsif" => {
                frame.cur.skip_inline_ws();
                let m = frame
                    .cur
                    .read_ident()
                    .ok_or_else(|| pp_error(span, "expected a macro name"))?;
                let top = conds
                    .last_mut()
                    .ok_or_else(|| pp_error(span, "`elsif without `ifdef"))?;
                *top = match *top {
                    CondState::Waiting if self.macros.contains_key(&m) => CondState::Active,
                    CondState::Waiting => CondState::Waiting,
                    _ => CondState::Done,
                };
            }
            "else" => {
                let top = conds
                    .last_mut()
                    .ok_or_else(|| pp_error(span, "`else without `ifdef"))?;
                *top = match *top {
                    CondState::Waiting => CondState::Active,
                    _ => CondState::Done,
                };
            }
            "endif" => {
                conds
                    .pop()
                    .ok_or_else(|| pp_error(span, "`endif without `ifdef"))?;
            }
            _ => {}
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn directive(
        &mut self,
        sm: &mut SourceMap,
        name: &str,
        span: Span,
        stack: &mut Vec<Frame>,
        conds: &mut Vec<CondState>,
        out: &mut Vec<Token>,
    ) -> Result<(), Diagnostic> {
        match name {
            "ifdef" | "ifndef" | "elsif" | "else" | "endif" => {
                return self.conditional(name, span, stack, conds);
            }
            _ => {}
        }
        let Some(frame) = stack.last_mut() else {
            return Ok(());
        };
        match name {
            "define" => {
                let (n, m) = parse_define(&mut frame.cur).map_err(|e| pp_error(span, e))?;
                self.macros.insert(n, m);
            }
            "undef" => {
                frame.cur.skip_inline_ws();
                let n = frame
                    .cur
                    .read_ident()
                    .ok_or_else(|| pp_error(span, "expected a macro name"))?;
                self.macros.remove(&n);
            }
            "undefineall" => self.macros.clear(),
            "include" => {
                frame.cur.skip_inline_ws();
                let fname = match frame.cur.peek() {
                    Some(b'"') => {
                        let s = frame.cur.string().map_err(Diagnostic::from)?;
                        String::from_utf8_lossy(&s).into_owned()
                    }
                    Some(b'<') => {
                        let l = frame.cur.read_line();
                        let l = l.trim();
                        let end = l.find('>').ok_or_else(|| pp_error(span, "expected '>'"))?;
                        l[1..end].to_string()
                    }
                    Some(b'`') => {
                        // `include `MACRO: expand the macro to a string.
                        frame.cur.pos += 1;
                        let m = frame.cur.read_ident().unwrap_or_default();
                        let body = self
                            .macros
                            .get(&m)
                            .map(|m| m.body.clone())
                            .ok_or_else(|| pp_error(span, format!("undefined macro `{m}")))?;
                        body.trim().trim_matches('"').to_string()
                    }
                    _ => return Err(pp_error(span, "expected a file name after `include")),
                };
                let from = if stack
                    .iter()
                    .rev()
                    .find(|f| f.is_file)
                    .is_some_and(|f| sm.get(f.cur.file).is_some())
                {
                    let f = stack
                        .iter()
                        .rev()
                        .find(|f| f.is_file)
                        .map(|f| f.cur.file)
                        .unwrap_or_default();
                    Some(sm.file(f).path.clone())
                } else {
                    None
                };
                let path = self
                    .find_include(&fname, from.as_deref())
                    .ok_or_else(|| pp_error(span, format!("include file '{fname}' not found")))?;
                if stack.iter().filter(|f| f.is_file).count() >= MAX_INCLUDE_DEPTH {
                    return Err(pp_error(span, "includes nested too deeply"));
                }
                let id = match sm.find(&path) {
                    Some(id) => id,
                    None => sm.load(&path).map_err(|e| {
                        pp_error(span, format!("cannot read '{}': {e}", path.display()))
                    })?,
                };
                let text = sm.file(id).text.clone();
                stack.push(Frame {
                    cur: Cursor::new(text, id, None),
                    is_file: true,
                });
            }
            "timescale" | "pragma" | "line" | "begin_keywords" | "end_keywords"
            | "unconnected_drive" => {
                let _ = frame.cur.read_line();
            }
            "celldefine" | "endcelldefine" | "resetall" | "nounconnected_drive" => {}
            "default_nettype" => {
                frame.cur.skip_inline_ws();
                let v = frame
                    .cur
                    .read_ident()
                    .ok_or_else(|| pp_error(span, "expected a net type"))?;
                out.push(Token {
                    tok: Tok::DefaultNettype(v),
                    span,
                });
            }
            "__FILE__" => {
                let f = stack
                    .iter()
                    .rev()
                    .find(|f| f.is_file)
                    .map(|f| f.cur.file)
                    .unwrap_or_default();
                let p = sm.file(f).path.display().to_string();
                out.push(Token {
                    tok: Tok::Str(p.into_bytes()),
                    span,
                });
            }
            "__LINE__" => {
                let fr = stack.iter().rev().find(|f| f.is_file);
                let line = fr
                    .map(|f| sm.file(f.cur.file).line_col(f.cur.offset() as u32).0)
                    .unwrap_or(0);
                out.push(Token {
                    tok: Tok::Dec(line.to_string()),
                    span,
                });
            }
            _ => {
                let m = self
                    .macros
                    .get(name)
                    .cloned()
                    .ok_or_else(|| pp_error(span, format!("undefined macro `{name}")))?;
                if stack.len() >= MAX_EXPANSION_DEPTH {
                    return Err(pp_error(span, "macro expansion nested too deeply"));
                }
                let frame = stack
                    .last_mut()
                    .ok_or_else(|| pp_error(span, "internal: empty stack"))?;
                let use_span = frame.cur.span_override.unwrap_or(span);
                let text = match &m.params {
                    None => m.body.clone(),
                    Some(formals) => {
                        // Allow white space between the name and the argument list.
                        let save = frame.cur.pos;
                        frame.cur.skip_trivia().map_err(Diagnostic::from)?;
                        if frame.cur.peek() != Some(b'(') {
                            frame.cur.pos = save;
                            return Err(pp_error(
                                span,
                                format!("macro `{name} requires arguments"),
                            ));
                        }
                        let mut actuals = frame.cur.read_macro_args().map_err(Diagnostic::from)?;
                        if formals.is_empty() && actuals.len() == 1 && actuals[0].is_empty() {
                            actuals.clear();
                        }
                        if actuals.len() > formals.len() {
                            return Err(pp_error(
                                span,
                                format!("too many arguments for macro `{name}"),
                            ));
                        }
                        while actuals.len() < formals.len() {
                            actuals.push(String::new());
                        }
                        for (k, (_, default)) in formals.iter().enumerate() {
                            if let (true, Some(d)) = (actuals[k].is_empty(), default) {
                                actuals[k] = d.clone();
                            }
                        }
                        substitute(&m.body, formals, &actuals)
                    }
                };
                let file = stack.last().map(|f| f.cur.file).unwrap_or_default();
                stack.push(Frame {
                    cur: Cursor::new(Arc::from(text), file, Some(use_span)),
                    is_file: false,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pp(src: &str, defines: &[(&str, &str)]) -> (Vec<Tok>, Vec<Diagnostic>) {
        let mut sm = SourceMap::new();
        let id = sm.add("t.sv", src);
        let opts = PpOptions {
            include_dirs: vec![],
            defines: defines
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        };
        let mut p = Preprocessor::new(opts);
        let toks = p.run(&mut sm, id);
        (
            toks.into_iter()
                .map(|t| t.tok)
                .filter(|t| *t != Tok::Eof)
                .collect(),
            p.diags,
        )
    }

    fn idents(t: &[Tok]) -> Vec<String> {
        t.iter()
            .map(|t| match t {
                Tok::Ident(s) => s.clone(),
                Tok::Dec(s) => s.clone(),
                Tok::Op(o) => o.to_string(),
                Tok::Kw(k) => k.to_string(),
                Tok::Str(s) => format!("\"{}\"", String::from_utf8_lossy(s)),
                other => format!("{other:?}"),
            })
            .collect()
    }

    // VERILOG-REQ-0200
    #[test]
    fn object_and_function_macros() {
        let (t, d) = pp(
            "`define W 8\n`define ADD(a, b=1) (a + b)\nx = `W; y = `ADD(p, q); z = `ADD(r);",
            &[],
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            idents(&t).join(" "),
            "x = 8 ; y = ( p + q ) ; z = ( r + 1 ) ;"
        );
        let (t, _) = pp(
            "`define CAT(a,b) a``b\n`define STR(a) `\"a`\"\n`CAT(foo,bar) `STR(hi)",
            &[],
        );
        assert_eq!(idents(&t), vec!["foobar", "\"hi\""]);
        let (t, _) = pp("`define M(x) \\\n  x + \\\n  1\n`M(a)", &[]);
        assert_eq!(idents(&t).join(" "), "a + 1");
        // Nested macro use in the body and in arguments.
        let (t, _) = pp("`define A 2\n`define B(x) x * `A\n`B(`A)", &[]);
        assert_eq!(idents(&t).join(" "), "2 * 2");
        // String literals in bodies are not substituted.
        let (t, _) = pp("`define S(x) \"x\"\n`S(1)", &[]);
        assert_eq!(idents(&t), vec!["\"x\""]);
    }

    // VERILOG-REQ-0201
    #[test]
    fn conditional_compilation() {
        let src =
            "`ifdef A a `elsif B b `else c `endif\n`ifndef A `ifdef B nb `else nn `endif `endif";
        let (t, d) = pp(src, &[]);
        assert!(d.is_empty());
        assert_eq!(idents(&t), vec!["c", "nn"]);
        let (t, _) = pp(src, &[("B", "")]);
        assert_eq!(idents(&t), vec!["b", "nb"]);
        let (t, _) = pp(src, &[("A", "")]);
        assert_eq!(idents(&t), vec!["a"]);
        let (_, d) = pp("`ifdef A x", &[]);
        assert!(!d.is_empty());
        let (_, d) = pp("`endif", &[]);
        assert!(!d.is_empty());
    }

    // VERILOG-REQ-0203, VERILOG-REQ-0204, VERILOG-REQ-0205
    #[test]
    fn misc_directives_and_limits() {
        let (t, d) = pp(
            "`timescale 1ns/1ps\n`default_nettype none\n`resetall\nq `__LINE__",
            &[],
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(t[0], Tok::DefaultNettype("none".into()));
        assert_eq!(idents(&t[1..]), vec!["q", "4"]);
        let (_, d) = pp("`UNDEFINED", &[]);
        assert!(!d.is_empty());
        let (_, d) = pp("`define R `R\n`R", &[]);
        assert!(!d.is_empty());
    }
}
