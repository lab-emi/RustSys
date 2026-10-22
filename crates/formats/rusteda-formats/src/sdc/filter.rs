// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Object query `-filter` expression AST and parser (SDC-REQ-0006).

use std::fmt;

/// Comparison operator in a `-filter` atom (SDC-REQ-0006).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterOp {
    /// Equality (`==` or `=`).
    Eq,
    /// Inequality (`!=`).
    Ne,
    /// Glob or regular-expression match (`=~`).
    Match,
    /// Negated glob or regular-expression match (`!~`).
    NotMatch,
}

impl FilterOp {
    /// Canonical operator token.
    pub fn as_str(self) -> &'static str {
        match self {
            FilterOp::Eq => "==",
            FilterOp::Ne => "!=",
            FilterOp::Match => "=~",
            FilterOp::NotMatch => "!~",
        }
    }
}

impl fmt::Display for FilterOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `attr op value` predicate in a `-filter` expression (SDC-REQ-0006).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FilterAtom {
    pub attr: String,
    pub op: FilterOp,
    pub value: String,
}

/// Parsed `-filter` expression tree (`attr op value` joined by `&&` and `||`; SDC-REQ-0006).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Filter {
    Atom(FilterAtom),
    And(Box<Filter>, Box<Filter>),
    Or(Box<Filter>, Box<Filter>),
}

/// Alias for [`Filter`].
pub type FilterExpr = Filter;

impl Filter {
    /// Parses a `-filter` expression string (SDC-REQ-0006).
    pub fn parse(input: &str) -> Result<Filter, String> {
        let tokens = tokenize(input)?;
        if tokens.is_empty() {
            return Err("empty -filter expression".to_string());
        }
        let mut pos = 0;
        let expr = parse_or(&tokens, &mut pos)?;
        if pos < tokens.len() {
            return Err(format!(
                "unexpected token '{}' in -filter expression",
                tokens[pos]
            ));
        }
        Ok(expr)
    }

    /// Formats this filter expression into its canonical string form.
    pub fn to_canonical(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Filter::Atom(a) => {
                write!(f, "{} {} {}", a.attr, a.op, format_filter_val(&a.value))
            }
            Filter::And(lhs, rhs) => {
                if matches!(lhs.as_ref(), Filter::Or(_, _)) {
                    write!(f, "({lhs})")?;
                } else {
                    write!(f, "{lhs}")?;
                }
                write!(f, " && ")?;
                if matches!(rhs.as_ref(), Filter::Or(_, _)) {
                    write!(f, "({rhs})")
                } else {
                    write!(f, "{rhs}")
                }
            }
            Filter::Or(lhs, rhs) => {
                write!(f, "{lhs} || {rhs}")
            }
        }
    }
}

fn format_filter_val(v: &str) -> String {
    if v.is_empty()
        || v.chars()
            .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '&' | '|' | '=' | '!' | '"'))
    {
        let mut out = String::with_capacity(v.len() + 2);
        out.push('"');
        for c in v.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    } else {
        v.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    LParen,
    RParen,
    And,
    Or,
    Op(FilterOp),
    Word(String),
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::LParen => f.write_str("("),
            Tok::RParen => f.write_str(")"),
            Tok::And => f.write_str("&&"),
            Tok::Or => f.write_str("||"),
            Tok::Op(op) => f.write_str(op.as_str()),
            Tok::Word(w) => f.write_str(w),
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Tok>, String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'&' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                    out.push(Tok::And);
                    i += 2;
                } else {
                    return Err("single '&' is not supported in -filter; use '&&'".to_string());
                }
            }
            b'|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    out.push(Tok::Or);
                    i += 2;
                } else {
                    return Err("single '|' is not supported in -filter; use '||'".to_string());
                }
            }
            b'=' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Op(FilterOp::Eq));
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'~' {
                    out.push(Tok::Op(FilterOp::Match));
                    i += 2;
                } else {
                    out.push(Tok::Op(FilterOp::Eq));
                    i += 1;
                }
            }
            b'!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Op(FilterOp::Ne));
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'~' {
                    out.push(Tok::Op(FilterOp::NotMatch));
                    i += 2;
                } else {
                    return Err(
                        "unary '!' is not supported in -filter; use 'attr == value'".to_string()
                    );
                }
            }
            b'"' | b'\'' => {
                let quote = b;
                i += 1;
                let mut s = String::new();
                let mut closed = false;
                while i < bytes.len() {
                    let c = bytes[i];
                    if c == b'\\' && i + 1 < bytes.len() {
                        i += 1;
                        s.push(bytes[i] as char);
                        i += 1;
                    } else if c == quote {
                        i += 1;
                        closed = true;
                        break;
                    } else {
                        let ch = input[i..].chars().next().unwrap();
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
                if !closed {
                    return Err("unclosed quote in -filter expression".to_string());
                }
                out.push(Tok::Word(s));
            }
            _ => {
                let start = i;
                while i < bytes.len() {
                    let c = bytes[i];
                    if c.is_ascii_whitespace()
                        || matches!(c, b'(' | b')' | b'&' | b'|' | b'=' | b'!' | b'"' | b'\'')
                    {
                        break;
                    }
                    i += 1;
                }
                let word = &input[start..i];
                out.push(Tok::Word(word.to_string()));
            }
        }
    }
    Ok(out)
}

fn parse_or(tokens: &[Tok], pos: &mut usize) -> Result<Filter, String> {
    let mut lhs = parse_and(tokens, pos)?;
    while *pos < tokens.len() && tokens[*pos] == Tok::Or {
        *pos += 1;
        let rhs = parse_and(tokens, pos)?;
        lhs = Filter::Or(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn parse_and(tokens: &[Tok], pos: &mut usize) -> Result<Filter, String> {
    let mut lhs = parse_primary(tokens, pos)?;
    while *pos < tokens.len() && tokens[*pos] == Tok::And {
        *pos += 1;
        let rhs = parse_primary(tokens, pos)?;
        lhs = Filter::And(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn parse_primary(tokens: &[Tok], pos: &mut usize) -> Result<Filter, String> {
    if *pos >= tokens.len() {
        return Err("unexpected end of -filter expression".to_string());
    }
    if tokens[*pos] == Tok::LParen {
        *pos += 1;
        let inner = parse_or(tokens, pos)?;
        if *pos >= tokens.len() || tokens[*pos] != Tok::RParen {
            return Err("unclosed '(' in -filter expression".to_string());
        }
        *pos += 1;
        return Ok(inner);
    }
    let attr = match &tokens[*pos] {
        Tok::Word(w) => {
            let stripped = w.strip_prefix('@').unwrap_or(w);
            if stripped.is_empty()
                || !stripped
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(format!("invalid attribute name '{w}' in -filter"));
            }
            stripped.to_string()
        }
        other => {
            return Err(format!(
                "expected attribute name in -filter, found '{other}'"
            ));
        }
    };
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(format!(
            "missing comparison operator after attribute '{attr}' in -filter"
        ));
    }
    let op = match tokens[*pos] {
        Tok::Op(op) => op,
        ref other => {
            return Err(format!(
                "expected comparison operator after '{attr}' in -filter, found '{other}'"
            ));
        }
    };
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(format!(
            "missing comparison value after '{attr} {op}' in -filter"
        ));
    }
    let value = match &tokens[*pos] {
        Tok::Word(w) => w.clone(),
        other => {
            return Err(format!(
                "expected value after '{attr} {op}' in -filter, found '{other}'"
            ));
        }
    };
    *pos += 1;
    Ok(Filter::Atom(FilterAtom { attr, op, value }))
}
