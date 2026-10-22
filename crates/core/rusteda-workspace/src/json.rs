// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! An order-preserving JSON reformatter (WS-REQ-0091, WS-REQ-0092).
//!
//! `serde_json` is built without `preserve_order`, so a `serde_json::Value` sorts the keys of
//! objects. The envelope therefore keeps nested values as JSON text, splices them, and
//! reformats the result here: [`reformat`] re-indents or compacts JSON text without reordering
//! anything and copies strings and numbers verbatim. The pretty style is the one of
//! `serde_json::to_string_pretty`: two-space indents, `"key": value`, and `{}` / `[]` for empty
//! containers.
//!
//! The text must be one JSON value in which no object has two equal keys (compared after
//! unescaping) and every `\u` escape is a whole character, so that every reader sees the same
//! value. The same parse gives the ordered value tree behind the `Serialize` implementations of
//! [`crate::RawJson`] and [`crate::JsonObject`].

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::ser::{Error as _, SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

/// How [`reformat`] lays out JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonStyle {
    /// One value per line, two-space indents.
    #[default]
    Pretty,
    /// No whitespace at all.
    Compact,
}

/// Malformed JSON text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid JSON at byte {offset}: {reason}")]
pub struct JsonSyntaxError {
    /// Byte offset of the problem.
    pub offset: usize,
    /// What is wrong.
    pub reason: &'static str,
}

/// The deepest nesting [`reformat`] accepts.
pub const MAX_DEPTH: usize = 256;

/// Re-lays out one JSON value, keeping the order of keys and the text of scalars. An object
/// with two equal keys is an error (WS-REQ-0091).
pub fn reformat(text: &str, style: JsonStyle) -> Result<String, JsonSyntaxError> {
    let node = parse(text)?;
    let mut out = String::with_capacity(text.len() * 2);
    print(&node, style, 0, &mut out);
    Ok(out)
}

/// Checks that `text` is one JSON value without duplicate keys.
pub fn validate(text: &str) -> Result<(), JsonSyntaxError> {
    parse(text).map(drop)
}

/// A JSON string literal, escaped like `serde_json` does: `"`, `\`, and control characters
/// (`\b \f \n \r \t`, otherwise `\u00xx`); everything else verbatim.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A parsed JSON value that keeps the text of its scalars and the order of its keys.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node<'a> {
    /// `null`, `true`, `false` or a number, as written.
    Literal(&'a str),
    /// A string: its text with quotes and escapes, and its value.
    Str { raw: &'a str, value: String },
    /// An array.
    Array(Vec<Node<'a>>),
    /// An object: (key text with quotes, key value, value), in text order.
    Object(Vec<(&'a str, String, Node<'a>)>),
}

/// Parses one JSON value without duplicate keys.
pub(crate) fn parse(text: &str) -> Result<Node<'_>, JsonSyntaxError> {
    let mut parser = Parser { text, pos: 0 };
    let node = parser.value(0)?;
    parser.skip_ws();
    if parser.pos != text.len() {
        return Err(parser.error("trailing characters after the value"));
    }
    Ok(node)
}

fn newline(style: JsonStyle, depth: usize, out: &mut String) {
    if style == JsonStyle::Pretty {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    }
}

fn print(node: &Node<'_>, style: JsonStyle, depth: usize, out: &mut String) {
    match node {
        Node::Literal(text) => out.push_str(text),
        Node::Str { raw, .. } => out.push_str(raw),
        Node::Array(items) if items.is_empty() => out.push_str("[]"),
        Node::Object(fields) if fields.is_empty() => out.push_str("{}"),
        Node::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(style, depth + 1, out);
                print(item, style, depth + 1, out);
            }
            newline(style, depth, out);
            out.push(']');
        }
        Node::Object(fields) => {
            out.push('{');
            for (i, (key, _, value)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(style, depth + 1, out);
                out.push_str(key);
                out.push_str(if style == JsonStyle::Pretty {
                    ": "
                } else {
                    ":"
                });
                print(value, style, depth + 1, out);
            }
            newline(style, depth, out);
            out.push('}');
        }
    }
}

/// Serializes a JSON number from its text: an integer as `u64` or `i64` when it fits, anything
/// else as `f64` (the value is kept; the text may change, e.g. `1.50` becomes `1.5`).
fn serialize_number<S: Serializer>(text: &str, serializer: S) -> Result<S::Ok, S::Error> {
    if !text.contains(['.', 'e', 'E']) {
        if let Ok(v) = text.parse::<u64>() {
            return serializer.serialize_u64(v);
        }
        if let Ok(v) = text.parse::<i64>() {
            return serializer.serialize_i64(v);
        }
    }
    match text.parse::<f64>() {
        Ok(v) if v.is_finite() => serializer.serialize_f64(v),
        _ => Err(S::Error::custom(format!(
            "the number {text} is out of the range of f64"
        ))),
    }
}

impl Serialize for Node<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Node::Literal("null") => serializer.serialize_unit(),
            Node::Literal("true") => serializer.serialize_bool(true),
            Node::Literal("false") => serializer.serialize_bool(false),
            Node::Literal(number) => serialize_number(number, serializer),
            Node::Str { value, .. } => serializer.serialize_str(value),
            Node::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Node::Object(fields) => {
                let mut map = serializer.serialize_map(Some(fields.len()))?;
                for (_, key, value) in fields {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, reason: &'static str) -> JsonSyntaxError {
        JsonSyntaxError {
            offset: self.pos,
            reason,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    /// `text[start..pos]`; both ends are at ASCII bytes, hence char boundaries.
    fn since(&self, start: usize) -> &'a str {
        &self.text[start..self.pos]
    }

    fn value(&mut self, depth: usize) -> Result<Node<'a>, JsonSyntaxError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => {
                let (raw, value) = self.string()?;
                Ok(Node::Str { raw, value })
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b't') => self.literal("true"),
            Some(b'f') => self.literal("false"),
            Some(b'n') => self.literal("null"),
            Some(_) => Err(self.error("expected a value")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    /// After `[` or `{`: true if the container is empty (and consumed).
    fn open(&mut self, close: u8) -> bool {
        self.pos += 1;
        self.skip_ws();
        if self.peek() == Some(close) {
            self.pos += 1;
            return true;
        }
        false
    }

    /// After a member: true at the end of the container (consumed), false after a `,`.
    fn next_or_close(&mut self, close: u8, reason: &'static str) -> Result<bool, JsonSyntaxError> {
        self.skip_ws();
        match self.peek() {
            Some(b',') => {
                self.pos += 1;
                Ok(false)
            }
            Some(b) if b == close => {
                self.pos += 1;
                Ok(true)
            }
            _ => Err(self.error(reason)),
        }
    }

    fn array(&mut self, depth: usize) -> Result<Node<'a>, JsonSyntaxError> {
        let mut items = Vec::new();
        if self.open(b']') {
            return Ok(Node::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            if self.next_or_close(b']', "expected ',' or ']'")? {
                return Ok(Node::Array(items));
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Node<'a>, JsonSyntaxError> {
        let mut fields = Vec::new();
        if self.open(b'}') {
            return Ok(Node::Object(fields));
        }
        let mut keys = BTreeSet::new();
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a key"));
            }
            let at = self.pos;
            let (raw, key) = self.string()?;
            if !keys.insert(key.clone()) {
                return Err(JsonSyntaxError {
                    offset: at,
                    reason: "duplicate key in an object",
                });
            }
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.pos += 1;
            let value = self.value(depth + 1)?;
            fields.push((raw, key, value));
            if self.next_or_close(b'}', "expected ',' or '}'")? {
                return Ok(Node::Object(fields));
            }
        }
    }

    /// A string literal: its text with quotes, and its value.
    fn string(&mut self) -> Result<(&'a str, String), JsonSyntaxError> {
        let start = self.pos;
        self.pos += 1;
        let mut value = String::new();
        loop {
            let Some(c) = self.text[self.pos..].chars().next() else {
                return Err(self.error("unterminated string"));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok((self.since(start), value));
                }
                '\\' => {
                    self.pos += 1;
                    value.push(self.escape()?);
                }
                c if u32::from(c) < 0x20 => {
                    return Err(self.error("control character in a string"));
                }
                c => {
                    self.pos += c.len_utf8();
                    value.push(c);
                }
            }
        }
    }

    /// The character of an escape, after its backslash.
    fn escape(&mut self) -> Result<char, JsonSyntaxError> {
        let c = match self.peek() {
            Some(b'"') => '"',
            Some(b'\\') => '\\',
            Some(b'/') => '/',
            Some(b'b') => '\u{8}',
            Some(b'f') => '\u{c}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b't') => '\t',
            Some(b'u') => {
                self.pos += 1;
                let unit = self.hex4()?;
                return self.code_point(unit);
            }
            _ => return Err(self.error("bad escape")),
        };
        self.pos += 1;
        Ok(c)
    }

    fn hex4(&mut self) -> Result<u32, JsonSyntaxError> {
        let digits = self.text.get(self.pos..self.pos + 4).unwrap_or("");
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.error("bad \\u escape"));
        }
        self.pos += 4;
        u32::from_str_radix(digits, 16).map_err(|_| self.error("bad \\u escape"))
    }

    /// The character of `\u` code unit `unit`, reading the low half of a surrogate pair; a
    /// lone surrogate is an error.
    fn code_point(&mut self, unit: u32) -> Result<char, JsonSyntaxError> {
        let lone = "lone surrogate in a \\u escape";
        let code = match unit {
            0xD800..=0xDBFF => {
                if !self.text[self.pos..].starts_with("\\u") {
                    return Err(self.error(lone));
                }
                self.pos += 2;
                let low = self.hex4()?;
                if !(0xDC00..=0xDFFF).contains(&low) {
                    return Err(self.error(lone));
                }
                0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
            }
            0xDC00..=0xDFFF => return Err(self.error(lone)),
            _ => unit,
        };
        char::from_u32(code).ok_or_else(|| self.error(lone))
    }

    fn number(&mut self) -> Result<Node<'a>, JsonSyntaxError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.error("bad number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            self.required_digits()?;
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            self.required_digits()?;
        }
        Ok(Node::Literal(self.since(start)))
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn required_digits(&mut self) -> Result<(), JsonSyntaxError> {
        let start = self.pos;
        self.digits();
        if self.pos == start {
            Err(self.error("expected digits"))
        } else {
            Ok(())
        }
    }

    fn literal(&mut self, word: &'static str) -> Result<Node<'a>, JsonSyntaxError> {
        if !self.text.as_bytes()[self.pos..].starts_with(word.as_bytes()) {
            return Err(self.error("bad literal"));
        }
        self.pos += word.len();
        Ok(Node::Literal(word))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn samples() -> Vec<Value> {
        vec![
            json!(null),
            json!(true),
            json!(-0.0),
            json!(1.5e300),
            json!("a \"q\" \\ \n \u{1} é ✓ 😀"),
            json!([]),
            json!({}),
            json!([1, [2, []], {}]),
            json!({"b": {"c": [1, 2, {"d": null}], "e": {}}, "a": [[]], "z": "x"}),
            json!([{"k": [{"k": [{"k": []}]}]}]),
            json!([u64::MAX, i64::MIN, 0.1, -2.5e-7]),
        ]
    }

    // WS-REQ-0092: the pretty style is the one of serde_json; compact is to_string.
    #[test]
    fn matches_serde_json_layout() {
        for v in samples() {
            let compact = serde_json::to_string(&v).unwrap();
            let pretty = serde_json::to_string_pretty(&v).unwrap();
            assert_eq!(reformat(&compact, JsonStyle::Pretty).unwrap(), pretty);
            assert_eq!(reformat(&pretty, JsonStyle::Compact).unwrap(), compact);
            assert_eq!(reformat(&pretty, JsonStyle::Pretty).unwrap(), pretty);
        }
    }

    // WS-REQ-0091: keys keep their order; scalars keep their text.
    #[test]
    fn keeps_order_and_text() {
        let text = r#" { "z" : 1.50 , "a" : [ 1E3 , -0 , "\u00e9\/" ] , "m" : { } } "#;
        assert_eq!(
            reformat(text, JsonStyle::Compact).unwrap(),
            r#"{"z":1.50,"a":[1E3,-0,"\u00e9\/"],"m":{}}"#
        );
        assert_eq!(
            reformat(text, JsonStyle::Pretty).unwrap(),
            "{\n  \"z\": 1.50,\n  \"a\": [\n    1E3,\n    -0,\n    \"\\u00e9\\/\"\n  ],\n  \"m\": {}\n}"
        );
    }

    #[test]
    fn rejects_malformed_text() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\"}",
            "{\"a\":}",
            "{a:1}",
            "[1 2]",
            "01",
            "1.",
            "1e",
            "-",
            "+1",
            "tru",
            "nul",
            "\"a",
            "\"\\x\"",
            "\"\\u12g4\"",
            "\"\u{1}\"",
            "1 2",
            "[]]",
        ] {
            assert!(validate(bad).is_err(), "{bad:?}");
        }
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 2), "]".repeat(MAX_DEPTH + 2));
        assert_eq!(validate(&deep).unwrap_err().reason, "nesting too deep");
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        validate(&ok).unwrap();
    }

    // WS-REQ-0091 (review CR2-7): an object with two equal keys, also when one of them is
    // written with escapes, is rejected at any depth; equal keys in different objects are fine.
    #[test]
    fn rejects_duplicate_keys() {
        for bad in [
            r#"{"corner":"a","corner":"b"}"#,
            r#"{"a":1,"\u0061":2}"#,
            r#"{"x":[{"k":1,"k":1}]}"#,
            r#"{"é":1,"\u00e9":2}"#,
        ] {
            let err = validate(bad).unwrap_err();
            assert_eq!(err.reason, "duplicate key in an object", "{bad}");
        }
        validate(r#"{"k":{"k":1},"j":[{"k":1},{"k":2}]}"#).unwrap();
        // Lone surrogates do not decode to a character.
        for bad in [r#""\ud800""#, r#""\udc00""#, r#""\ud800\u0041""#] {
            assert!(validate(bad).is_err(), "{bad}");
        }
        assert_eq!(
            parse(r#""\ud83d\ude00""#).unwrap(),
            Node::Str {
                raw: r#""\ud83d\ude00""#,
                value: "😀".into()
            }
        );
    }

    // WS-REQ-0091 (review CR2-9): the tree serializes through serde in text order, with the
    // values of its scalars.
    #[test]
    fn tree_serializes_in_text_order() {
        let text = r#"{"z":1.50,"a":[1E3,-0,"\u00e9\/",null,true,false],"m":{},"n":-7,"u":18446744073709551615}"#;
        let node = parse(text).unwrap();
        assert_eq!(
            serde_json::to_string(&node).unwrap(),
            r#"{"z":1.5,"a":[1000.0,0,"é/",null,true,false],"m":{},"n":-7,"u":18446744073709551615}"#
        );
        for v in samples() {
            let compact = serde_json::to_string(&v).unwrap();
            assert_eq!(
                serde_json::to_string(&parse(&compact).unwrap()).unwrap(),
                compact
            );
        }
        let err = serde_json::to_string(&parse("1e400").unwrap()).unwrap_err();
        assert!(err.to_string().contains("out of the range"), "{err}");
    }

    #[test]
    fn quoting_matches_serde_json() {
        let mut all: String = (0u32..0x80).filter_map(char::from_u32).collect();
        all.push_str("é✓\u{2028}😀");
        assert_eq!(quote(&all), serde_json::to_string(&all).unwrap());
        for c in all.chars() {
            let s = c.to_string();
            assert_eq!(quote(&s), serde_json::to_string(&s).unwrap(), "{c:?}");
        }
    }
}
