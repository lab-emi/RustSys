// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Scans for what the lexer and the parser skip: attribute instances, macros that could hide
//! constructs or expand without bound, and unsupported directives in the raw text; delays,
//! drive strengths, `` `default_nettype`` inside a module, parenthesized and min:typ:max
//! expressions, and the written net types of ANSI ports in the tokens.

use std::collections::HashMap;
use std::ops::Range;

use super::super::diag::Diagnostic;
use super::super::span::{FileId, Span};
use super::super::token::{Tok, Token};
use super::read::UNSUPPORTED;

/// Attribute names that only record where a construct came from.
const NEUTRAL_ATTRIBUTES: [&str; 1] = ["src"];

/// The bytes that the macro uses of a text may add, at least: the length of the text if more.
const MIN_EXPANSION: usize = 1 << 20;

/// End of a string literal starting at `i` (the quote): after the closing quote, or at the end
/// of the line for an unterminated string.
fn skip_string(b: &[u8], mut i: usize) -> usize {
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            b'\n' => return i,
            _ => i += 1,
        }
    }
    b.len()
}

/// The ends of the content and of the attribute instance starting at `i` (at `(*`): the lexer
/// ends an instance at the first `*)` outside a string, and so does this scan. An unterminated
/// instance runs to the end of the text.
fn attribute_end(b: &[u8], mut i: usize) -> (usize, usize) {
    i += 2;
    while i < b.len() {
        match b[i] {
            b'"' => i = skip_string(b, i),
            b'*' if b.get(i + 1) == Some(&b')') => return (i, i + 2),
            _ => i += 1,
        }
    }
    (b.len(), b.len())
}

/// The attribute names of the text of an attribute instance (between `(*` and `*)`): names
/// (simple or escaped identifiers) separated by commas, each without a value or with a string
/// value. Anything else is refused, with what was found: the lexer skips the text unread, so
/// the reader cannot tell what another form means.
fn attribute_names(s: &[u8]) -> Result<Vec<String>, &'static str> {
    const MACRO: &str = "a macro inside an attribute instance";
    let blanks = |i: &mut usize| {
        while s.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
    };
    let mut names = Vec::new();
    let mut i = 0;
    loop {
        blanks(&mut i);
        let start = i;
        match s.get(i) {
            Some(b'\\') => {
                i += 1;
                while s.get(i).is_some_and(|c| !c.is_ascii_whitespace()) {
                    i += 1;
                }
                if i == start + 1 {
                    return Err("an attribute name that is not an identifier");
                }
                // An escaped identifier names the same as its text without the backslash.
                names.push(String::from_utf8_lossy(&s[start + 1..i]).into_owned());
            }
            Some(&c) if c.is_ascii_alphabetic() || c == b'_' => {
                while s
                    .get(i)
                    .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$')
                {
                    i += 1;
                }
                names.push(String::from_utf8_lossy(&s[start..i]).into_owned());
            }
            Some(b'`') => return Err(MACRO),
            None => return Err("a missing attribute name"),
            Some(_) => return Err("an attribute name that is not an identifier"),
        }
        blanks(&mut i);
        if s.get(i) == Some(&b'=') {
            i += 1;
            blanks(&mut i);
            match s.get(i) {
                Some(b'"') => i = skip_string(s, i),
                Some(b'`') => return Err(MACRO),
                _ => return Err("an attribute value other than a string literal"),
            }
            blanks(&mut i);
        }
        match s.get(i) {
            None => return Ok(names),
            Some(b',') => i += 1,
            Some(b'`') => return Err(MACRO),
            Some(_) => return Err("a missing comma between attributes"),
        }
    }
}

/// A macro definition.
struct Define<'t> {
    name: &'t str,
    /// The end of the name.
    name_end: usize,
    /// An argument list follows the name directly.
    args: bool,
    /// The rest of the definition, as far as the lexer reads it.
    body: Range<usize>,
}

/// The macro definition after `` `define`` (`i`: after the directive name). It runs to the end
/// of the line, as the lexer reads it: a backslash before the line break continues it, and a
/// block comment can span lines.
fn define(text: &str, mut i: usize) -> Option<Define<'_>> {
    let b = text.as_bytes();
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\r' | 0x0c)) {
        i += 1;
    }
    let start = i;
    while b
        .get(i)
        .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$')
    {
        i += 1;
    }
    if i == start {
        return None;
    }
    let mut end = i;
    while end < b.len() {
        match b[end] {
            b'\n' => break,
            b'\\' if b.get(end + 1) == Some(&b'\n') => end += 2,
            b'\\' if b.get(end + 1) == Some(&b'\r') && b.get(end + 2) == Some(&b'\n') => end += 3,
            b'/' if b.get(end + 1) == Some(&b'/') => {
                // A line comment; a backslash before the line break still continues.
                while end < b.len()
                    && b[end] != b'\n'
                    && !(b[end] == b'\\' && b.get(end + 1) == Some(&b'\n'))
                {
                    end += 1;
                }
            }
            b'/' if b.get(end + 1) == Some(&b'*') => {
                end = text[end + 2..]
                    .find("*/")
                    .map_or(b.len(), |k| end + 2 + k + 2);
            }
            b'"' => end = skip_string(b, end),
            _ => end += 1,
        }
    }
    Some(Define {
        name: &text[start..i],
        name_end: i,
        args: b.get(i) == Some(&b'('),
        body: i..end,
    })
}

/// Finds in the raw text the attribute instances, the macros and the directives that a
/// structural netlist cannot honor: the lexer skips attributes, `` `include`` must be refused
/// before the preprocessor opens a file, and the preprocessor limits the nesting of macro
/// expansion but not its size. So a macro may have no arguments (substituting one can assemble
/// an attribute instance that no scan sees) and no macro in its body (macros that use a macro
/// twice grow exponentially), and the uses of macros may add at most the length of the text,
/// or [`MIN_EXPANSION`] bytes. Comments, strings and escaped identifiers are skipped;
/// conditional branches and macro bodies are scanned too.
pub(super) fn scan_text(text: &str, file: FileId) -> Vec<Diagnostic> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    // Macros with the length of their definition, and the bytes their uses add.
    let mut macros: HashMap<&str, usize> = HashMap::new();
    let budget = text.len().max(MIN_EXPANSION);
    let mut expanded = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(b.len(), |k| i + 2 + k + 2);
            }
            b'"' => i = skip_string(b, i),
            b'\\' => {
                i += 1;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
            }
            b'(' if b.get(i + 1) == Some(&b'*') => {
                let rest = &text[i + 2..];
                if rest.trim_start_matches([' ', '\t']).starts_with(')') {
                    i += 2;
                    continue;
                }
                let (inner, end) = attribute_end(b, i);
                let span = Span::new(file, i, end);
                match attribute_names(&b[i + 2..inner]) {
                    Ok(names) => {
                        let unsupported_name = names
                            .iter()
                            .find(|n| !NEUTRAL_ATTRIBUTES.contains(&n.as_str()));
                        if let Some(n) = unsupported_name {
                            out.push(Diagnostic::error(
                                UNSUPPORTED,
                                span,
                                format!(
                                    "attribute '{n}' is not supported in a structural netlist: \
                                     its meaning cannot be represented (only 'src' attributes \
                                     are accepted)"
                                ),
                            ));
                        }
                    }
                    Err(what) => out.push(Diagnostic::error(
                        UNSUPPORTED,
                        span,
                        format!(
                            "{what} is not supported in a structural netlist (only 'src' \
                             attributes, without a value or with a string value, are accepted)"
                        ),
                    )),
                }
                i = end;
            }
            b'`' => {
                let s = i + 1;
                let mut e = s;
                while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'_' || b[e] == b'$')
                {
                    e += 1;
                }
                let name = &text[s..e];
                let why = match name {
                    "include" => Some("a structural netlist is read as one text"),
                    "unconnected_drive" => Some("it gives unconnected input pins a value"),
                    "pragma" => Some("its effect depends on the program reading it"),
                    _ => None,
                };
                if let Some(why) = why {
                    out.push(Diagnostic::error(
                        UNSUPPORTED,
                        Span::new(file, i, e),
                        format!("`{name} is not supported in a structural netlist: {why}"),
                    ));
                } else if name == "define" {
                    if let Some(d) = define(text, e) {
                        let span = Span::new(file, i, d.name_end);
                        let problem = if d.args {
                            Some(
                                "with arguments is not supported in a structural netlist: \
                                 substituting an argument can assemble an attribute instance \
                                 that the reader does not see",
                            )
                        } else if b[d.body.clone()].contains(&b'`') {
                            Some(
                                "uses a macro or directive, which is not supported in a \
                                 structural netlist: macros that use other macros can expand \
                                 exponentially",
                            )
                        } else {
                            None
                        };
                        if let Some(p) = problem {
                            let msg = format!("the macro '{}' {p}", d.name);
                            out.push(Diagnostic::error(UNSUPPORTED, span, msg));
                        }
                        macros.insert(d.name, d.body.len());
                    }
                } else if let Some(&len) = macros.get(name) {
                    let before = expanded;
                    expanded = expanded.saturating_add(len);
                    if before <= budget && expanded > budget {
                        out.push(Diagnostic::error(
                            UNSUPPORTED,
                            Span::new(file, i, e),
                            format!(
                                "macro uses that add more than {budget} bytes are not supported \
                                 in a structural netlist (the uses of macros may add the length \
                                 of the text, or {MIN_EXPANSION} bytes)"
                            ),
                        ));
                    }
                }
                i = e;
            }
            _ => i += 1,
        }
    }
    out
}

fn net_keyword(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Kw(
            "wire"
                | "tri"
                | "uwire"
                | "wand"
                | "wor"
                | "triand"
                | "trior"
                | "tri0"
                | "tri1"
                | "trireg"
                | "supply0"
                | "supply1"
                | "interconnect"
                | "vectored"
                | "scalared"
        )
    )
}

/// Whether a parenthesis after `prev` encloses an operand: it follows an operator or an
/// opening bracket. After a name, a parenthesis opens a port list, the connections of an
/// instance, the connection of a pin or the arguments of a call; after a keyword, `#`, `##` or
/// `@` it belongs to that construct; after `'` it is a cast, after a closing bracket part of
/// another construct.
fn opens_operand(prev: Option<&Tok>) -> bool {
    match prev {
        Some(Tok::Op(op)) => !matches!(
            *op,
            "#" | "##" | "@" | "'" | ")" | "]" | "}" | "." | ";" | "::" | "$"
        ),
        Some(Tok::TickBrace) => true,
        _ => false,
    }
}

/// A bracket that is open at the current token.
struct Open {
    /// Index of the opening token.
    at: usize,
    paren: bool,
    /// A parenthesis around an operand ([`opens_operand`]).
    operand: bool,
    /// A parenthesis after a keyword, `#`, `##` or `@`: its construct is reported on its own
    /// (a delay, a drive strength, a procedure), so nothing inside it is.
    quiet: bool,
    /// The `?` of conditional operators directly inside, not yet matched by their `:`.
    conditions: usize,
    /// A `:` directly inside the parenthesis that no `?` explains: min:typ:max.
    min_typ_max: bool,
}

/// Finds the parentheses that the parser reads away: around an operand, which it drops, and
/// around a min:typ:max expression, of which it keeps the typical value only. The parentheses
/// of a structural netlist all follow a name: the port list, the connections of an instance,
/// the connection of a pin. One pass with a stack of the open brackets: a construct is
/// reported when it closes, an operand parenthesis only when no other one encloses it.
#[derive(Default)]
struct Parens {
    open: Vec<Open>,
    /// Open parentheses that are quiet, and that enclose operands.
    quiet: usize,
    operands: usize,
    found: Vec<(usize, &'static str)>,
}

impl Parens {
    fn token(&mut self, k: usize, tok: &Tok, prev: Option<&Tok>) {
        match tok {
            Tok::Op("(") => {
                let quiet = matches!(prev, Some(Tok::Kw(_) | Tok::Op("#" | "##" | "@")));
                self.push(k, true, opens_operand(prev), quiet);
            }
            Tok::Op("[" | "{") | Tok::TickBrace => self.push(k, false, false, false),
            Tok::Op(")" | "]" | "}") => self.close(),
            Tok::Op("?") => {
                if let Some(o) = self.open.last_mut() {
                    o.conditions += 1;
                }
            }
            Tok::Op(":") => {
                if let Some(o) = self.open.last_mut() {
                    if o.conditions > 0 {
                        o.conditions -= 1;
                    } else if o.paren {
                        o.min_typ_max = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn push(&mut self, at: usize, paren: bool, operand: bool, quiet: bool) {
        self.quiet += usize::from(quiet);
        self.operands += usize::from(operand);
        self.open.push(Open {
            at,
            paren,
            operand,
            quiet,
            conditions: 0,
            min_typ_max: false,
        });
    }

    /// Closes the innermost bracket; an unbalanced closing bracket is left to the parser.
    fn close(&mut self) {
        let Some(o) = self.open.pop() else {
            return;
        };
        self.quiet -= usize::from(o.quiet);
        self.operands -= usize::from(o.operand);
        if o.quiet || self.quiet > 0 {
            return;
        }
        if o.min_typ_max {
            self.found.push((o.at, "a min:typ:max expression"));
        } else if o.operand && self.operands == 0 {
            self.found.push((o.at, "a parenthesized expression"));
        }
    }
}

/// Finds what the parser skips: delays and drive strengths (every `#`, and a parenthesis
/// directly after `assign` or a net type), `` `default_nettype`` inside a module, whose value
/// the module header has captured already, and the parentheses that it reads away
/// ([`Parens`]).
pub(super) fn scan_tokens(toks: &[Token]) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    let mut parens = Parens::default();
    let mut depth = 0usize;
    for (k, t) in toks.iter().enumerate() {
        let prev = k.checked_sub(1).map(|j| &toks[j].tok);
        parens.token(k, &t.tok, prev);
        let what = match (&t.tok, prev) {
            (Tok::Kw("module" | "macromodule"), _) => {
                depth += 1;
                continue;
            }
            (Tok::Kw("endmodule"), _) => {
                depth = depth.saturating_sub(1);
                continue;
            }
            (Tok::DefaultNettype(_), _) if depth > 0 => "`default_nettype inside a module",
            (Tok::Op("#"), Some(Tok::Kw("assign"))) => "a delay on a continuous assignment",
            (Tok::Op("#"), Some(p)) if net_keyword(p) => "a delay on a net declaration",
            (Tok::Op("#"), Some(Tok::Ident(_) | Tok::EscIdent(_)))
                if k >= 2 && matches!(toks[k - 2].tok, Tok::Kw("module" | "macromodule")) =>
            {
                "a parameter port list"
            }
            (Tok::Op("#"), Some(Tok::Ident(_) | Tok::EscIdent(_))) => {
                "a parameter value assignment or delay on an instance"
            }
            (Tok::Op("#"), _) => "a delay",
            (Tok::Op("("), Some(Tok::Kw("assign"))) => {
                "a drive strength on a continuous assignment"
            }
            (Tok::Op("("), Some(p)) if net_keyword(p) => "a drive strength on a net declaration",
            _ => continue,
        };
        found.push((k, what));
    }
    found.append(&mut parens.found);
    found.sort_by_key(|&(k, _)| k);
    found
        .into_iter()
        .map(|(k, what)| {
            Diagnostic::error(
                UNSUPPORTED,
                toks[k].span,
                format!("{what} is not supported in a structural netlist"),
            )
        })
        .collect()
}

/// Net types that a port declaration can name.
fn net_type_keyword(t: &Tok) -> bool {
    net_keyword(t) && !matches!(t, Tok::Kw("vectored" | "scalared"))
}

/// Whether the tokens of one ANSI port declaration name a net type; a bare name (with
/// unpacked dimensions or a default at most) repeats the declaration before it, `prev`.
fn names_net_type(decl: &[&Tok], prev: Option<bool>) -> bool {
    let bare = matches!(decl.first(), Some(Tok::Ident(_) | Tok::EscIdent(_)))
        && matches!(decl.get(1), None | Some(Tok::Op("[" | "=")));
    if bare {
        prev.unwrap_or(false)
    } else {
        decl.iter().any(|t| net_type_keyword(t))
    }
}

/// For each port of the first module's ANSI port list, whether a net type (`wire` and the
/// like) is written for it, in its declaration or in the one it repeats. The syntax tree gives
/// every net port a net type, so the tokens tell the ports that take the `` `default_nettype``.
pub(super) fn ansi_net_types(toks: &[Token]) -> Vec<bool> {
    let mut out = Vec::new();
    let unit = |t: &Token| {
        matches!(
            t.tok,
            Tok::Kw("module" | "macromodule" | "interface" | "program")
        )
    };
    let Some(start) = toks.iter().position(unit) else {
        return out;
    };
    let header = toks[start..]
        .iter()
        .position(|t| matches!(t.tok, Tok::Op("(" | ";")))
        .map(|k| start + k);
    let Some(open) = header.filter(|&k| toks[k].tok == Tok::Op("(")) else {
        return out;
    };
    let mut decl: Vec<&Tok> = Vec::new();
    let mut depth = 0usize;
    for t in &toks[open + 1..] {
        match &t.tok {
            Tok::Op("(" | "[" | "{") | Tok::TickBrace => depth += 1,
            Tok::Op(")" | "]" | "}") | Tok::Eof if depth == 0 => break,
            Tok::Op(")" | "]" | "}") => depth -= 1,
            Tok::Op(",") if depth == 0 => {
                let named = names_net_type(&decl, out.last().copied());
                out.push(named);
                decl.clear();
                continue;
            }
            _ => {}
        }
        decl.push(&t.tok);
    }
    let named = names_net_type(&decl, out.last().copied());
    out.push(named);
    out
}
