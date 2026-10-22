// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Control-flow built-ins, collection helpers, and Tcl script lexer (`SDC-REQ-0007` to `SDC-REQ-0015`).

use std::path::Path;

use super::super::constr::xxh3_64;
use super::super::expr::eval_condition;
use super::super::obj::Obj;
use super::super::{SourceRecord, glob_match, glob_match_nocase};
use super::{EvalError, Interpreter, TclVal, unescape_char};
use rusteda_core::diag::{Diag, Severity};

impl<'a> Interpreter<'a> {
    pub(super) fn eval_if(
        &mut self,
        args: &[TclVal],
        loc: (&str, u32, u32),
        is_top_level: bool,
    ) -> Result<TclVal, EvalError> {
        if args.len() < 2 {
            return Err(EvalError::new(
                "D0004",
                "'if' requires condition and body",
                loc,
            ));
        }
        let mut i = 0;
        loop {
            if i >= args.len() {
                return Err(EvalError::new("D0002", "malformed 'if' command", loc));
            }
            let cond_str = args[i].as_scalar_str().ok_or_else(|| {
                EvalError::new(
                    "D0006",
                    "'if' condition over an object collection needs the netlist",
                    loc,
                )
            })?;
            i += 1;
            if i < args.len() && args[i].as_scalar_str().is_some_and(|s| s == "then") {
                i += 1;
            }
            if i >= args.len() {
                return Err(EvalError::new(
                    "D0004",
                    "missing body after condition in 'if'",
                    loc,
                ));
            }
            let body = args[i]
                .as_scalar_str()
                .ok_or_else(|| EvalError::new("D0002", "'if' body must be a script string", loc))?;
            i += 1;
            if eval_condition(self, &cond_str, loc)? {
                return self.eval_script(&body, loc.0, loc.1, loc.2, is_top_level);
            }
            if i >= args.len() {
                return Ok(TclVal::Str(String::new()));
            }
            let kw = args[i].as_scalar_str().unwrap_or_default();
            if kw == "elseif" {
                i += 1;
                continue;
            } else if kw == "else" {
                i += 1;
                if i != args.len() - 1 {
                    return Err(EvalError::new(
                        "D0004",
                        "expected exactly 1 body after 'else' in 'if'",
                        loc,
                    ));
                }
                let else_body = args[i].as_scalar_str().ok_or_else(|| {
                    EvalError::new("D0002", "'else' body must be a script string", loc)
                })?;
                return self.eval_script(&else_body, loc.0, loc.1, loc.2, is_top_level);
            } else if i == args.len() - 1 {
                let else_body = args[i].as_scalar_str().ok_or_else(|| {
                    EvalError::new("D0002", "'else' body must be a script string", loc)
                })?;
                return self.eval_script(&else_body, loc.0, loc.1, loc.2, is_top_level);
            } else {
                return Err(EvalError::new(
                    "D0002",
                    format!("unexpected clause '{kw}' in 'if'"),
                    loc,
                ));
            }
        }
    }

    pub(super) fn eval_foreach(
        &mut self,
        args: &[TclVal],
        loc: (&str, u32, u32),
        is_top_level: bool,
    ) -> Result<TclVal, EvalError> {
        if args.len() != 3 {
            return Err(EvalError::new(
                "D0004",
                format!("'foreach' expects 3 arguments, got {}", args.len()),
                loc,
            ));
        }
        let var_name = args[0].as_scalar_str().ok_or_else(|| {
            EvalError::new("D0005", "'foreach' expects a loop variable name", loc)
        })?;
        if args[1].contains_obj() || matches!(args[1], TclVal::SearchIndex { .. }) {
            return Err(EvalError::new(
                "D0006",
                "'foreach' over an object collection needs the netlist",
                loc,
            ));
        }
        let items = args[1].to_string_list().ok_or_else(|| {
            EvalError::new(
                "D0006",
                "'foreach' over an object collection needs the netlist",
                loc,
            )
        })?;
        let body = args[2].as_scalar_str().ok_or_else(|| {
            EvalError::new("D0002", "'foreach' body must be a script string", loc)
        })?;
        let mut last = TclVal::Str(String::new());
        for item in items {
            self.step(loc)?;
            self.set_var(&var_name, TclVal::Str(item), loc)?;
            last = self.eval_script(&body, loc.0, loc.1, loc.2, is_top_level)?;
        }
        Ok(last)
    }

    pub(super) fn eval_info(
        &mut self,
        args: &[TclVal],
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        if args.is_empty() {
            return Err(EvalError::new("D0004", "'info' requires a subcommand", loc));
        }
        let sub = args[0].as_scalar_str().unwrap_or_default();
        match sub.as_str() {
            "exists" => {
                if args.len() != 2 {
                    return Err(EvalError::new(
                        "D0004",
                        format!("'info exists' expects 1 argument, got {}", args.len() - 1),
                        loc,
                    ));
                }
                let var = args[1].as_scalar_str().ok_or_else(|| {
                    EvalError::new("D0005", "'info exists' expects a variable name", loc)
                })?;
                let exists = self.var_exists(&var);
                Ok(TclVal::Str(if exists { "1" } else { "0" }.to_string()))
            }
            other => Err(EvalError::new(
                "D0006",
                format!("'info {other}' is outside the deterministic SDC subset"),
                loc,
            )),
        }
    }

    pub(super) fn eval_puts(
        &mut self,
        args: &[TclVal],
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        let mut idx = 0;
        if idx < args.len() && args[idx].as_scalar_str().is_some_and(|s| s == "-nonewline") {
            idx += 1;
        }
        let rem = &args[idx..];
        let msg = match rem.len() {
            1 => rem[0].as_scalar_str().unwrap_or_default(),
            2 => rem[1].as_scalar_str().unwrap_or_default(),
            _ => {
                return Err(EvalError::new(
                    "D0004",
                    format!("'puts' expects 1 or 2 arguments, got {}", args.len()),
                    loc,
                ));
            }
        };
        self.diags.push(
            Diag::new(Severity::Note, "D0040", format!("puts: {msg}")).at(loc.0, loc.1, loc.2),
        );
        Ok(TclVal::Str(String::new()))
    }

    pub(super) fn eval_string_cmd(
        &mut self,
        args: &[TclVal],
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        if args.is_empty() {
            return Err(EvalError::new(
                "D0004",
                "'string' requires a subcommand",
                loc,
            ));
        }
        let sub = args[0].as_scalar_str().unwrap_or_default();
        let rest = &args[1..];
        match sub.as_str() {
            "match" => {
                let mut idx = 0;
                let mut nocase = false;
                if idx < rest.len() && rest[idx].as_scalar_str().is_some_and(|s| s == "-nocase") {
                    nocase = true;
                    idx += 1;
                }
                if rest.len() - idx != 2 {
                    return Err(EvalError::new(
                        "D0004",
                        "'string match' expects ?-nocase? pattern string",
                        loc,
                    ));
                }
                let pat = rest[idx].as_scalar_str().ok_or_else(|| {
                    EvalError::new(
                        "D0006",
                        "'string match' over an object collection needs the netlist",
                        loc,
                    )
                })?;
                let text = rest[idx + 1].as_scalar_str().ok_or_else(|| {
                    EvalError::new(
                        "D0006",
                        "'string match' over an object collection needs the netlist",
                        loc,
                    )
                })?;
                let matched = if nocase {
                    glob_match_nocase(&pat, &text)
                } else {
                    glob_match(&pat, &text)
                };
                Ok(TclVal::Str(if matched { "1" } else { "0" }.to_string()))
            }
            "equal" | "compare" => {
                let mut idx = 0;
                let mut nocase = false;
                let mut length: Option<usize> = None;
                while idx < rest.len() {
                    if let Some(tok) = rest[idx].as_option_token() {
                        if tok == "-nocase" {
                            nocase = true;
                            idx += 1;
                        } else if tok == "-length" {
                            if idx + 1 >= rest.len() {
                                return Err(EvalError::new(
                                    "D0004",
                                    "'-length' requires a value in 'string'",
                                    loc,
                                ));
                            }
                            let n_str = rest[idx + 1].as_scalar_str().unwrap_or_default();
                            let n = n_str.parse::<usize>().map_err(|_| {
                                EvalError::new("D0005", "invalid '-length' integer", loc)
                            })?;
                            length = Some(n);
                            idx += 2;
                        } else {
                            return Err(EvalError::new(
                                "D0003",
                                format!("unknown option '{tok}' in 'string {sub}'"),
                                loc,
                            ));
                        }
                    } else {
                        break;
                    }
                }
                if rest.len() - idx != 2 {
                    return Err(EvalError::new(
                        "D0004",
                        format!("'string {sub}' expects 2 string arguments"),
                        loc,
                    ));
                }
                let mut s1 = rest[idx].as_scalar_str().ok_or_else(|| {
                    EvalError::new(
                        "D0006",
                        "'string' comparison over an object collection needs the netlist",
                        loc,
                    )
                })?;
                let mut s2 = rest[idx + 1].as_scalar_str().ok_or_else(|| {
                    EvalError::new(
                        "D0006",
                        "'string' comparison over an object collection needs the netlist",
                        loc,
                    )
                })?;
                if let Some(n) = length {
                    s1 = s1.chars().take(n).collect();
                    s2 = s2.chars().take(n).collect();
                }
                if nocase {
                    s1 = s1.to_ascii_lowercase();
                    s2 = s2.to_ascii_lowercase();
                }
                if sub == "equal" {
                    Ok(TclVal::Str(if s1 == s2 { "1" } else { "0" }.to_string()))
                } else {
                    let cmp = if s1 < s2 {
                        "-1"
                    } else if s1 > s2 {
                        "1"
                    } else {
                        "0"
                    };
                    Ok(TclVal::Str(cmp.to_string()))
                }
            }
            "length" => {
                if rest.len() != 1 {
                    return Err(EvalError::new(
                        "D0004",
                        "'string length' expects 1 argument",
                        loc,
                    ));
                }
                let s = rest[0].as_scalar_str().ok_or_else(|| {
                    EvalError::new(
                        "D0006",
                        "'string length' over an object collection needs the netlist",
                        loc,
                    )
                })?;
                Ok(TclVal::Str(s.chars().count().to_string()))
            }
            other => Err(EvalError::new(
                "D0006",
                format!("'string {other}' is outside the deterministic SDC subset"),
                loc,
            )),
        }
    }

    pub(super) fn eval_lsearch(
        &mut self,
        args: &[TclVal],
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        let mut idx = 0;
        let mut inline = false;
        let mut not = false;
        let mut all = false;
        let mut exact = false;
        while idx < args.len() {
            if let Some(tok) = args[idx].as_option_token() {
                match tok {
                    "-inline" => inline = true,
                    "-not" => not = true,
                    "-all" => all = true,
                    "-exact" => exact = true,
                    "-glob" => {}
                    _ => {
                        return Err(EvalError::new(
                            "D0003",
                            format!("unsupported option '{tok}' in 'lsearch'"),
                            loc,
                        ));
                    }
                }
                idx += 1;
            } else {
                break;
            }
        }
        if args.len() - idx != 2 {
            return Err(EvalError::new(
                "D0004",
                "'lsearch' expects list and pattern arguments",
                loc,
            ));
        }
        let haystack_val = &args[idx];
        let needle_val = &args[idx + 1];
        if haystack_val.contains_obj() || needle_val.contains_obj() {
            let haystack = haystack_val
                .to_obj()
                .ok_or_else(|| EvalError::new("D0005", "invalid collection in 'lsearch'", loc))?;
            let needle = match needle_val.to_obj() {
                Some(n) => n,
                None => return Ok(TclVal::Obj(haystack)),
            };
            if inline && not {
                return Ok(TclVal::Obj(Obj::diff(haystack, needle)));
            }
            return Ok(TclVal::SearchIndex { haystack, needle });
        }
        let list = haystack_val.to_string_list().unwrap_or_default();
        let pat = needle_val.as_scalar_str().unwrap_or_default();
        let matches_item = |item: &str| -> bool {
            let m = if exact {
                item == pat
            } else {
                glob_match(&pat, item)
            };
            if not { !m } else { m }
        };
        if all {
            let mut res = Vec::new();
            for (i, item) in list.iter().enumerate() {
                if matches_item(item) {
                    if inline {
                        res.push(TclVal::Str(item.clone()));
                    } else {
                        res.push(TclVal::Str(i.to_string()));
                    }
                }
            }
            Ok(TclVal::List(res))
        } else {
            for (i, item) in list.iter().enumerate() {
                if matches_item(item) {
                    return Ok(TclVal::Str(if inline {
                        item.clone()
                    } else {
                        i.to_string()
                    }));
                }
            }
            Ok(TclVal::Str(if inline {
                String::new()
            } else {
                "-1".to_string()
            }))
        }
    }

    pub(super) fn eval_lreplace(
        &mut self,
        args: &[TclVal],
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        if args.len() < 3 {
            return Err(EvalError::new(
                "D0004",
                format!(
                    "'lreplace' expects at least 3 arguments, got {}",
                    args.len()
                ),
                loc,
            ));
        }
        if let TclVal::SearchIndex { needle, .. } = &args[1] {
            let haystack = args[0]
                .to_obj()
                .ok_or_else(|| EvalError::new("D0005", "'lreplace' expects a collection", loc))?;
            let mut diff = Obj::diff(haystack, needle.clone());
            for extra in &args[3..] {
                if let Some(s) = extra.as_scalar_str()
                    && s.is_empty()
                {
                    continue;
                }
                if let Some(added) = extra.to_obj() {
                    diff = Obj::union(vec![diff, added]);
                }
            }
            return Ok(TclVal::Obj(diff));
        }
        if args[0].contains_obj() {
            return Err(EvalError::new(
                "D0006",
                "'lreplace' with numeric indices over an object collection needs the netlist",
                loc,
            ));
        }
        let mut items = args[0].to_string_list().unwrap_or_default();
        let first_s = args[1].as_scalar_str().unwrap_or_default();
        let last_s = args[2].as_scalar_str().unwrap_or_default();
        let first = parse_list_index(&first_s, items.len(), loc)?.unwrap_or(0);
        let last = parse_list_index(&last_s, items.len(), loc)?.unwrap_or(0);
        let mut repl = Vec::new();
        for a in &args[3..] {
            if let Some(s) = a.as_scalar_str()
                && !s.is_empty()
            {
                repl.push(s);
            }
        }
        if first <= last && last < items.len() {
            items.splice(first..=last, repl);
        }
        Ok(TclVal::List(items.into_iter().map(TclVal::Str).collect()))
    }

    pub(super) fn eval_source(
        &mut self,
        args: &[TclVal],
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        if args.len() != 1 {
            return Err(EvalError::new(
                "D0004",
                format!("'source' expects 1 argument, got {}", args.len()),
                loc,
            ));
        }
        let rel = args[0]
            .as_scalar_str()
            .ok_or_else(|| EvalError::new("D0005", "'source' expects a file path string", loc))?;
        let rel_path = Path::new(&rel);
        if rel.is_empty()
            || rel_path.is_absolute()
            || rel.starts_with('/')
            || rel.starts_with('\\')
            || rel_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(EvalError::new(
                "D0006",
                format!("'source' path '{rel}' must be a relative path without '..'"),
                loc,
            ));
        }
        self.depth += 1;
        if self.depth > self.limits.max_depth {
            self.depth -= 1;
            return Err(EvalError::new(
                "D0008",
                format!(
                    "source recursion depth exceeded (limit: {})",
                    self.limits.max_depth
                ),
                loc,
            ));
        }
        let resolved_pair = if let Some(text) = self.limits.virtual_sources.get(&rel) {
            Some((rel.clone(), text.clone()))
        } else {
            let parent = Path::new(loc.0).parent().unwrap_or_else(|| Path::new(""));
            let full_path = if parent.as_os_str().is_empty() {
                rel_path.to_path_buf()
            } else {
                parent.join(rel_path)
            };
            let full_str = full_path.to_string_lossy().replace('\\', "/");
            if let Some(text) = self.limits.virtual_sources.get(&full_str) {
                Some((full_str, text.clone()))
            } else {
                std::fs::read_to_string(&full_path)
                    .ok()
                    .map(|text| (full_str, text))
            }
        };
        let Some((src_name, src_text)) = resolved_pair else {
            self.depth -= 1;
            return Err(EvalError::new(
                "D0006",
                format!("cannot read sourced file '{rel}'"),
                loc,
            ));
        };
        let hash = xxh3_64(src_text.as_bytes());
        let new_idx = self.sources.len() as u16;
        self.sources.push(SourceRecord {
            path: src_name.clone(),
            flags: SourceRecord::FLAG_SOURCED,
            xxh3: hash,
        });
        let prev_idx = self.current_source_idx;
        self.current_source_idx = new_idx;
        let res = self.eval_script(&src_text, &src_name, 1, 1, true);
        self.current_source_idx = prev_idx;
        self.depth -= 1;
        res
    }
}

pub(super) fn normalize_var_name(raw: &str) -> &str {
    raw.strip_prefix("::").unwrap_or(raw)
}

pub(super) fn parse_env_var(norm: &str) -> Option<&str> {
    norm.strip_prefix("env(").and_then(|s| s.strip_suffix(')'))
}

pub(super) fn parse_list_index(
    s: &str,
    len: usize,
    loc: (&str, u32, u32),
) -> Result<Option<usize>, EvalError> {
    let t = s.trim();
    if t == "end" {
        return Ok(len.checked_sub(1));
    }
    if let Some(off_s) = t.strip_prefix("end-") {
        let off = off_s
            .parse::<usize>()
            .map_err(|_| EvalError::new("D0005", format!("invalid list index '{s}'"), loc))?;
        return Ok(len.checked_sub(1 + off));
    }
    let idx = t
        .parse::<isize>()
        .map_err(|_| EvalError::new("D0005", format!("invalid list index '{s}'"), loc))?;
    if idx < 0 || (idx as usize) >= len {
        Ok(None)
    } else {
        Ok(Some(idx as usize))
    }
}

#[derive(Debug, Clone)]
pub(super) struct ParsedCommand {
    pub(super) words: Vec<WordAst>,
    pub(super) line: u32,
    pub(super) col: u32,
}

#[derive(Debug, Clone)]
pub(super) struct WordAst {
    pub(super) expand: bool,
    pub(super) kind: WordKind,
    pub(super) line: u32,
    pub(super) col: u32,
}

#[derive(Debug, Clone)]
pub(super) enum WordKind {
    Braced(String),
    Segments(Vec<Segment>),
}

#[derive(Debug, Clone)]
pub(super) enum Segment {
    Lit(String),
    Var(String),
    Cmd(String, u32, u32),
}

pub(super) fn parse_script_commands(
    script: &str,
    file: &str,
    base_line: u32,
    base_col: u32,
) -> Result<Vec<ParsedCommand>, EvalError> {
    let mut lx = ScriptLexer::new(script, file, base_line, base_col);
    lx.parse_all()
}

struct ScriptLexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file: &'a str,
    pub(super) line: u32,
    pub(super) col: u32,
}

impl<'a> ScriptLexer<'a> {
    fn new(src: &'a str, file: &'a str, line: u32, col: u32) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            file,
            line,
            col,
        }
    }

    fn advance_one(&mut self) {
        if self.pos < self.bytes.len() {
            if self.bytes[self.pos] == b'\n' {
                self.line += 1;
                self.col = 1;
                self.pos += 1;
            } else {
                let ch = self.src[self.pos..].chars().next().unwrap();
                self.pos += ch.len_utf8();
                self.col += 1;
            }
        }
    }

    fn check_line_continuation(&mut self) -> bool {
        if self.pos + 1 < self.bytes.len() && self.bytes[self.pos] == b'\\' {
            if self.bytes[self.pos + 1] == b'\n' {
                self.pos += 2;
                self.line += 1;
                self.col = 1;
                while self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b' ' | b'\t') {
                    self.pos += 1;
                    self.col += 1;
                }
                return true;
            } else if self.pos + 2 < self.bytes.len()
                && self.bytes[self.pos + 1] == b'\r'
                && self.bytes[self.pos + 2] == b'\n'
            {
                self.pos += 3;
                self.line += 1;
                self.col = 1;
                while self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b' ' | b'\t') {
                    self.pos += 1;
                    self.col += 1;
                }
                return true;
            }
        }
        false
    }

    fn skip_horiz_ws(&mut self) {
        while self.pos < self.bytes.len() {
            if matches!(self.bytes[self.pos], b' ' | b'\t' | b'\r') {
                self.advance_one();
            } else if self.check_line_continuation() {
                continue;
            } else {
                break;
            }
        }
    }

    fn parse_all(&mut self) -> Result<Vec<ParsedCommand>, EvalError> {
        let mut cmds = Vec::new();
        while self.pos < self.bytes.len() {
            self.skip_horiz_ws();
            if self.pos >= self.bytes.len() {
                break;
            }
            let b = self.bytes[self.pos];
            if b == b'\n' || b == b';' {
                self.advance_one();
                continue;
            }
            if b == b'#' {
                while self.pos < self.bytes.len() {
                    if self.check_line_continuation() {
                        continue;
                    }
                    if self.bytes[self.pos] == b'\n' {
                        self.advance_one();
                        break;
                    }
                    self.advance_one();
                }
                continue;
            }
            let cmd_line = self.line;
            let cmd_col = self.col;
            let mut words = Vec::new();
            while self.pos < self.bytes.len() {
                self.skip_horiz_ws();
                if self.pos >= self.bytes.len() {
                    break;
                }
                let c = self.bytes[self.pos];
                if c == b'\n' || c == b';' {
                    self.advance_one();
                    break;
                }
                words.push(self.parse_word()?);
            }
            if !words.is_empty() {
                cmds.push(ParsedCommand {
                    words,
                    line: cmd_line,
                    col: cmd_col,
                });
            }
        }
        Ok(cmds)
    }

    fn parse_word(&mut self) -> Result<WordAst, EvalError> {
        let w_line = self.line;
        let w_col = self.col;
        let mut expand = false;
        if self.src[self.pos..].starts_with("{*}")
            && self.pos + 3 < self.bytes.len()
            && !self.bytes[self.pos + 3].is_ascii_whitespace()
            && self.bytes[self.pos + 3] != b';'
        {
            expand = true;
            self.pos += 3;
            self.col += 3;
        }
        let kind = match self.bytes[self.pos] {
            b'{' => WordKind::Braced(self.read_braced_word(w_line, w_col)?),
            b'"' => WordKind::Segments(self.read_quoted_word(w_line, w_col)?),
            b'}' | b']' => {
                return Err(EvalError::new(
                    "D0001",
                    format!("unexpected '{}'", self.bytes[self.pos] as char),
                    (self.file, w_line, w_col),
                ));
            }
            _ => WordKind::Segments(self.read_bare_word()?),
        };
        Ok(WordAst {
            expand,
            kind,
            line: w_line,
            col: w_col,
        })
    }

    fn read_braced_word(&mut self, w_line: u32, w_col: u32) -> Result<String, EvalError> {
        self.advance_one(); // '{'
        let mut out = String::new();
        let mut depth = 1usize;
        while self.pos < self.bytes.len() {
            if self.check_line_continuation() {
                out.push(' ');
                continue;
            }
            let b = self.bytes[self.pos];
            if b == b'\\' && self.pos + 1 < self.bytes.len() {
                let next = self.bytes[self.pos + 1];
                if matches!(next, b'{' | b'}' | b'\\') {
                    out.push('\\');
                    out.push(next as char);
                    self.pos += 2;
                    self.col += 2;
                    continue;
                }
            }
            if b == b'{' {
                depth += 1;
                out.push('{');
                self.advance_one();
            } else if b == b'}' {
                depth -= 1;
                self.advance_one();
                if depth == 0 {
                    if self.pos < self.bytes.len()
                        && !self.bytes[self.pos].is_ascii_whitespace()
                        && self.bytes[self.pos] != b';'
                    {
                        return Err(EvalError::new(
                            "D0001",
                            "extra characters after close-brace",
                            (self.file, self.line, self.col),
                        ));
                    }
                    return Ok(out);
                }
                out.push('}');
            } else {
                let ch = self.src[self.pos..].chars().next().unwrap();
                out.push(ch);
                self.advance_one();
            }
        }
        Err(EvalError::new(
            "D0001",
            "unclosed '{'",
            (self.file, w_line, w_col),
        ))
    }

    fn read_quoted_word(&mut self, w_line: u32, w_col: u32) -> Result<Vec<Segment>, EvalError> {
        self.advance_one(); // '"'
        let mut segs = Vec::new();
        let mut lit = String::new();
        while self.pos < self.bytes.len() {
            if self.check_line_continuation() {
                lit.push(' ');
                continue;
            }
            let b = self.bytes[self.pos];
            match b {
                b'"' => {
                    self.advance_one();
                    if self.pos < self.bytes.len()
                        && !self.bytes[self.pos].is_ascii_whitespace()
                        && self.bytes[self.pos] != b';'
                    {
                        return Err(EvalError::new(
                            "D0001",
                            "extra characters after close-quote",
                            (self.file, self.line, self.col),
                        ));
                    }
                    if !lit.is_empty() {
                        segs.push(Segment::Lit(lit));
                    }
                    return Ok(segs);
                }
                b'\\' => {
                    self.advance_one();
                    if self.pos >= self.bytes.len() {
                        return Err(EvalError::new(
                            "D0001",
                            "trailing '\\' in quoted string",
                            (self.file, w_line, w_col),
                        ));
                    }
                    let esc = self.bytes[self.pos];
                    lit.push(unescape_char(esc));
                    self.advance_one();
                }
                b'$' => {
                    if !lit.is_empty() {
                        segs.push(Segment::Lit(std::mem::take(&mut lit)));
                    }
                    let var = self.read_var_seg()?;
                    segs.push(var);
                }
                b'[' => {
                    if !lit.is_empty() {
                        segs.push(Segment::Lit(std::mem::take(&mut lit)));
                    }
                    let cmd = self.read_bracket_seg()?;
                    segs.push(cmd);
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    lit.push(ch);
                    self.advance_one();
                }
            }
        }
        Err(EvalError::new(
            "D0001",
            "unclosed '\"'",
            (self.file, w_line, w_col),
        ))
    }

    fn read_bare_word(&mut self) -> Result<Vec<Segment>, EvalError> {
        let mut segs = Vec::new();
        let mut lit = String::new();
        while self.pos < self.bytes.len() {
            if self.check_line_continuation() {
                break;
            }
            let b = self.bytes[self.pos];
            if b.is_ascii_whitespace() || b == b';' {
                break;
            }
            match b {
                b'\\' => {
                    let esc_line = self.line;
                    let esc_col = self.col;
                    self.advance_one();
                    if self.pos >= self.bytes.len() {
                        return Err(EvalError::new(
                            "D0001",
                            "trailing '\\'",
                            (self.file, esc_line, esc_col),
                        ));
                    }
                    let esc = self.bytes[self.pos];
                    lit.push(unescape_char(esc));
                    self.advance_one();
                }
                b'$' => {
                    if !lit.is_empty() {
                        segs.push(Segment::Lit(std::mem::take(&mut lit)));
                    }
                    let var = self.read_var_seg()?;
                    segs.push(var);
                }
                b'[' => {
                    if !lit.is_empty() {
                        segs.push(Segment::Lit(std::mem::take(&mut lit)));
                    }
                    let cmd = self.read_bracket_seg()?;
                    segs.push(cmd);
                }
                b']' => {
                    return Err(EvalError::new(
                        "D0001",
                        "unexpected ']'",
                        (self.file, self.line, self.col),
                    ));
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    lit.push(ch);
                    self.advance_one();
                }
            }
        }
        if !lit.is_empty() {
            segs.push(Segment::Lit(lit));
        }
        Ok(segs)
    }

    fn read_var_seg(&mut self) -> Result<Segment, EvalError> {
        let v_line = self.line;
        let v_col = self.col;
        self.advance_one(); // '$'
        if self.pos >= self.bytes.len() {
            return Ok(Segment::Lit("$".to_string()));
        }
        if self.bytes[self.pos] == b'{' {
            self.advance_one();
            let start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos] != b'}' {
                self.advance_one();
            }
            if self.pos >= self.bytes.len() {
                return Err(EvalError::new(
                    "D0001",
                    "unclosed '${'",
                    (self.file, v_line, v_col),
                ));
            }
            let name = self.src[start..self.pos].to_string();
            self.advance_one(); // '}'
            return Ok(Segment::Var(name));
        }
        let start = self.pos;
        while self.pos < self.bytes.len()
            && (self.bytes[self.pos].is_ascii_alphanumeric()
                || matches!(self.bytes[self.pos], b'_' | b':'))
        {
            self.advance_one();
        }
        if self.pos == start {
            return Ok(Segment::Lit("$".to_string()));
        }
        let mut name = self.src[start..self.pos].to_string();
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'(' {
            self.advance_one();
            let k_start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos] != b')' {
                self.advance_one();
            }
            if self.pos >= self.bytes.len() {
                return Err(EvalError::new(
                    "D0001",
                    "unclosed '(' in variable reference",
                    (self.file, v_line, v_col),
                ));
            }
            name.push('(');
            name.push_str(&self.src[k_start..self.pos]);
            name.push(')');
            self.advance_one();
        }
        Ok(Segment::Var(name))
    }

    fn read_bracket_seg(&mut self) -> Result<Segment, EvalError> {
        let b_line = self.line;
        let b_col = self.col;
        self.advance_one(); // '['
        let sub_line = self.line;
        let sub_col = self.col;
        let start = self.pos;
        let mut depth = 1usize;
        let mut in_quotes = false;
        let mut brace_depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'\\' && self.pos + 1 < self.bytes.len() {
                self.advance_one();
                self.advance_one();
                continue;
            }
            if brace_depth > 0 {
                if b == b'{' {
                    brace_depth += 1;
                } else if b == b'}' {
                    brace_depth -= 1;
                }
                self.advance_one();
                continue;
            }
            if in_quotes {
                if b == b'"' {
                    in_quotes = false;
                }
                self.advance_one();
                continue;
            }
            match b {
                b'"' => in_quotes = true,
                b'{' => brace_depth = 1,
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        let script = self.src[start..self.pos].to_string();
                        self.advance_one();
                        return Ok(Segment::Cmd(script, sub_line, sub_col));
                    }
                }
                _ => {}
            }
            self.advance_one();
        }
        Err(EvalError::new(
            "D0001",
            "unclosed '['",
            (self.file, b_line, b_col),
        ))
    }
}
