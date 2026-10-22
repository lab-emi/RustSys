// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The `rusteda.v1` envelope: exactly one per invocation on stdout in JSON mode, including
//! usage errors (WS-REQ-0090).
//!
//! Keys, in this order: `schema`, `ok`, `command`, `workspace`, `base_rev`, `rev`, `new_rev`,
//! `scope`, `completeness`, `metrics`, then the tool-specific fields in insertion order, then
//! `diagnostics`, `error` and `next_actions`. The standard keys are always present (`null`,
//! `{}` or `[]` when they do not apply).
//!
//! Nested values are kept as [`RawJson`], JSON text spliced into the output, which
//! [`crate::json::reformat`] then lays out (WS-REQ-0091, WS-REQ-0092). `serde_json` is built
//! without `preserve_order`, so the order of object keys survives only where the text is made
//! in order:
//!
//! - a struct serializes its fields in declaration order ([`RawJson::new`]);
//! - a [`JsonObject`] keeps its keys in insertion order, also for keys known only at run time
//!   (per corner, per clock);
//! - [`RawJson::parse`] keeps the text it is given;
//! - [`JsonObject`] and [`RawJson`] implement `Serialize` in that order, so they keep it as
//!   fields of a struct too (a per-corner slack inside each `top_paths` entry); their numbers
//!   are then written from their values (`1.50` becomes `1.5`);
//! - but a map (`BTreeMap`, [`crate::JsonMap`], `serde_json::Value`, `json!`) serializes its
//!   keys sorted, and a `HashMap` in an arbitrary order that differs from run to run.
//!
//! The parts the contract defines are checked when they are set: `scope`, `completeness` and
//! `metrics` are JSON objects ([`Envelope::set_scope`] and its siblings), no object of a value
//! has two equal keys, and every diagnostic code is a [`DiagCode`] (WS-REQ-0090, WS-REQ-0094).
//!
//! The pretty layout is the one of `serde_json::to_string_pretty`: every array element and
//! object member on its own line, and numbers as they were serialized (an `f64` of -14.210
//! prints as `-14.21`). The envelope of plan Example 1 is illustrative in this respect: the
//! keys, their order and the values are binding, the whitespace is not (WS-REQ-0092).

use std::fmt;
use std::io::{self, Write};

use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, ErrorCode, Result};
use crate::json::{self, JsonStyle};
use crate::rev::Rev;
use crate::text::shell_join;

/// The envelope schema.
pub const SCHEMA: &str = "rusteda.v1";

/// The standard keys, in output order; tool-specific fields go between `metrics` and
/// `diagnostics`.
pub const STANDARD_KEYS: [&str; 13] = [
    "schema",
    "ok",
    "command",
    "workspace",
    "base_rev",
    "rev",
    "new_rev",
    "scope",
    "completeness",
    "metrics",
    "diagnostics",
    "error",
    "next_actions",
];

/// A JSON value kept as compact text, so that the order of object keys survives (WS-REQ-0091).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RawJson(String);

impl RawJson {
    /// Serializes `value` with `serde_json`. A struct keeps its field order; a map does not
    /// (its keys come out sorted, or for a `HashMap` in an arbitrary order), so build an object
    /// whose key order matters with [`JsonObject`] (WS-REQ-0091).
    /// An object with two equal keys (from a hand-written `Serialize`) is `INTERNAL`.
    pub fn new<T: Serialize + ?Sized>(value: &T) -> Result<RawJson> {
        let text = serde_json::to_string(value)
            .map_err(|e| Error::internal(format!("serializing an envelope value: {e}")))?;
        json::validate(&text).map_err(|e| Error::internal(format!("envelope value: {e}")))?;
        Ok(RawJson(text))
    }

    /// Takes JSON text as it is (order kept), validated and compacted. Malformed text, or an
    /// object with two equal keys, is `INTERNAL` (WS-REQ-0091).
    pub fn parse(text: &str) -> Result<RawJson> {
        json::reformat(text, JsonStyle::Compact)
            .map(RawJson)
            .map_err(|e| Error::internal(format!("envelope value: {e}")))
    }

    /// `null`.
    pub fn null() -> RawJson {
        RawJson("null".to_string())
    }

    /// `{}`.
    pub fn empty_object() -> RawJson {
        RawJson("{}".to_string())
    }

    /// `[]`.
    pub fn empty_array() -> RawJson {
        RawJson("[]".to_string())
    }

    /// The compact JSON text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is `null`.
    pub fn is_null(&self) -> bool {
        self.0 == "null"
    }

    /// Whether this is a JSON object.
    pub fn is_object(&self) -> bool {
        // The text is compact JSON, so an object starts with its brace.
        self.0.starts_with('{')
    }
}

/// Serializes the value with its object keys in text order (WS-REQ-0091), so a `RawJson` field
/// of a serialized struct keeps the order of its keys.
impl Serialize for RawJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        json::parse(&self.0)
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl fmt::Display for RawJson {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A JSON object built key by key that keeps the insertion order of its keys (WS-REQ-0091),
/// for objects whose keys are known only at run time, such as metrics per corner.
///
/// ```
/// use rusteda_workspace::{JsonObject, RawJson};
///
/// let mut metrics = JsonObject::new();
/// metrics.insert("wns_ns", &-0.382)?.insert("tns_ns", &-14.21)?;
/// metrics.insert_raw("clock_domains", RawJson::parse(r#"["core_clk"]"#)?);
/// assert_eq!(
///     RawJson::from(metrics).as_str(),
///     r#"{"wns_ns":-0.382,"tns_ns":-14.21,"clock_domains":["core_clk"]}"#
/// );
/// # Ok::<(), rusteda_workspace::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonObject {
    fields: Vec<(String, RawJson)>,
}

impl JsonObject {
    /// An empty object.
    pub fn new() -> JsonObject {
        JsonObject::default()
    }

    /// Sets `key` to the JSON of `value`. A new key goes after the existing ones; an existing
    /// key keeps its place and gets the new value.
    pub fn insert<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<&mut Self> {
        Ok(self.insert_raw(key, RawJson::new(value)?))
    }

    /// Sets `key` to JSON text, like [`JsonObject::insert`].
    pub fn insert_raw(&mut self, key: &str, value: RawJson) -> &mut Self {
        match self.fields.iter_mut().find(|(k, _)| k == key) {
            Some((_, slot)) => *slot = value,
            None => self.fields.push((key.to_string(), value)),
        }
        self
    }

    /// The value of `key`.
    pub fn get(&self, key: &str) -> Option<&RawJson> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The number of keys.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Whether the object has no keys.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// The object as compact JSON text, keys in insertion order.
    pub fn to_raw(&self) -> RawJson {
        let mut o = ObjectText::new();
        for (key, value) in &self.fields {
            o.field(key, value.as_str());
        }
        RawJson(o.finish())
    }
}

/// Serializes the object with its keys in insertion order (WS-REQ-0091).
impl Serialize for JsonObject {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for (key, value) in &self.fields {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl From<JsonObject> for RawJson {
    fn from(object: JsonObject) -> RawJson {
        object.to_raw()
    }
}

impl From<&JsonObject> for RawJson {
    fn from(object: &JsonObject) -> RawJson {
        object.to_raw()
    }
}

/// The severity of a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// `note`.
    Note,
    /// `warning`.
    Warning,
    /// `error`.
    Error,
}

impl Severity {
    /// `note`, `warning` or `error`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }

    /// Reads [`Severity::as_str`].
    pub fn parse(text: &str) -> Option<Severity> {
        [Severity::Note, Severity::Warning, Severity::Error]
            .into_iter()
            .find(|s| s.as_str() == text)
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a diagnostic points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagLocation {
    /// The file, as given.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub col: u32,
}

/// A diagnostic code: one letter of the contract's areas and four digits (WS-REQ-0094). The
/// letter names the area: A application, B blif, C check, D sdc, G aiger, L liberty, S synth,
/// T timing, V verilog, W workspace (WS-REQ-0190). A `DiagCode` holds a valid code only, so every [`Diagnostic`] has
/// one, and its text line is one the shared diagnostic reader parses.
///
/// ```
/// use rusteda_workspace::DiagCode;
///
/// const UNCONSTRAINED: DiagCode = DiagCode::of("T0007"); // checked at compile time
/// assert_eq!(DiagCode::parse("T0007")?, UNCONSTRAINED);
/// assert!(DiagCode::parse("timing").is_err());
/// assert!(DiagCode::parse("X0001").is_err());
/// # Ok::<(), rusteda_workspace::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagCode([u8; 5]);

impl DiagCode {
    /// The area letters of the contract (§7).
    pub const LETTERS: &'static str = "ABCDGLSTVW";

    const fn check(text: &[u8]) -> Option<DiagCode> {
        if text.len() != 5 {
            return None;
        }
        let letters = DiagCode::LETTERS.as_bytes();
        let mut known = false;
        let mut i = 0;
        while i < letters.len() {
            known |= letters[i] == text[0];
            i += 1;
        }
        let mut digits = true;
        let mut i = 1;
        while i < 5 {
            digits &= text[i].is_ascii_digit();
            i += 1;
        }
        if known && digits {
            Some(DiagCode([text[0], text[1], text[2], text[3], text[4]]))
        } else {
            None
        }
    }

    /// A code written in the source, e.g. `DiagCode::of("T0007")`. In a `const` item an invalid
    /// code is a compile error.
    ///
    /// # Panics
    ///
    /// If `text` is not a letter of [`DiagCode::LETTERS`] and four digits; use
    /// [`DiagCode::parse`] for text known only at run time.
    pub const fn of(text: &'static str) -> DiagCode {
        match DiagCode::check(text.as_bytes()) {
            Some(code) => code,
            None => panic!("a diagnostic code is a letter of ABCDGLSTVW and four digits"),
        }
    }

    /// Reads a code; anything but a letter of [`DiagCode::LETTERS`] and four digits is
    /// `INTERNAL` (codes are fixed in the tools' sources).
    pub fn parse(text: &str) -> Result<DiagCode> {
        DiagCode::check(text.as_bytes()).ok_or_else(|| {
            Error::internal(format!(
                "'{text}' is not a diagnostic code: a letter of {} and four digits",
                DiagCode::LETTERS
            ))
        })
    }

    /// The code, e.g. `W0101`.
    pub fn as_str(&self) -> &str {
        // Five ASCII bytes, checked when the code was made.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }

    /// The area letter.
    pub const fn letter(self) -> char {
        self.0[0] as char
    }
}

impl fmt::Display for DiagCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for DiagCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DiagCode({})", self.as_str())
    }
}

impl Serialize for DiagCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DiagCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        DiagCode::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// A diagnostic of the envelope: `{severity, code, location, message, help}` with `location`
/// `{file, line, col}` or `null` (WS-REQ-0094). The code is a [`DiagCode`]: one letter and
/// four digits, the letter naming the area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The severity.
    pub severity: Severity,
    /// The code, e.g. `W0101`.
    pub code: DiagCode,
    /// The location, if any.
    pub location: Option<DiagLocation>,
    /// The message.
    pub message: String,
    /// A hint, if any.
    pub help: Option<String>,
}

impl Diagnostic {
    /// A diagnostic without location or help.
    pub fn new(severity: Severity, code: DiagCode, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity,
            code,
            location: None,
            message: message.into(),
            help: None,
        }
    }

    /// A diagnostic from the parts of a coded message: severity, code, optional file/line/column,
    /// message and optional help. A code given as text is checked with [`DiagCode::parse`].
    pub fn from_parts(
        severity: Severity,
        code: DiagCode,
        location: Option<(String, u32, u32)>,
        message: impl Into<String>,
        help: Option<String>,
    ) -> Diagnostic {
        Diagnostic {
            severity,
            code,
            location: location.map(|(file, line, col)| DiagLocation { file, line, col }),
            message: message.into(),
            help,
        }
    }

    /// Sets the location.
    pub fn at(mut self, file: impl Into<String>, line: u32, col: u32) -> Diagnostic {
        self.location = Some(DiagLocation {
            file: file.into(),
            line,
            col,
        });
        self
    }

    /// Sets the help.
    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }

    /// The diagnostic as compact JSON.
    pub fn to_json(&self) -> String {
        let location = match &self.location {
            Some(l) => {
                let mut o = ObjectText::new();
                o.field("file", &json::quote(&l.file));
                o.field("line", &l.line.to_string());
                o.field("col", &l.col.to_string());
                o.finish()
            }
            None => "null".to_string(),
        };
        let mut o = ObjectText::new();
        o.field("severity", &json::quote(self.severity.as_str()));
        o.field("code", &json::quote(self.code.as_str()));
        o.field("location", &location);
        o.field("message", &json::quote(&self.message));
        o.field(
            "help",
            &self.help.as_deref().map_or("null".to_string(), json::quote),
        );
        o.finish()
    }
}

/// The envelope's `error`: `{code, message, details}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    /// The error code.
    pub code: ErrorCode,
    /// The message.
    pub message: String,
    /// Structured details, or `null`.
    pub details: RawJson,
}

impl ErrorInfo {
    /// An error without details.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> ErrorInfo {
        ErrorInfo {
            code,
            message: message.into(),
            details: RawJson::null(),
        }
    }

    /// Sets the details.
    pub fn with_details(mut self, details: RawJson) -> ErrorInfo {
        self.details = details;
        self
    }

    /// The process exit code.
    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }

    /// The error as compact JSON.
    pub fn to_json(&self) -> String {
        let mut o = ObjectText::new();
        o.field("code", &json::quote(self.code.as_str()));
        o.field("message", &json::quote(&self.message));
        o.field("details", self.details.as_str());
        o.finish()
    }
}

impl From<&Error> for ErrorInfo {
    fn from(error: &Error) -> ErrorInfo {
        let details = RawJson::new(&error.details()).unwrap_or_else(|_| RawJson::null());
        ErrorInfo {
            code: error.code(),
            message: error.to_string(),
            details,
        }
    }
}

impl From<Error> for ErrorInfo {
    fn from(error: Error) -> ErrorInfo {
        ErrorInfo::from(&error)
    }
}

/// A `rusteda.v1` envelope. `ok` is derived: true unless `error` is set (a command that
/// completed with a negative verdict is `ok`; its exit code is the caller's choice, 3 or 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The command, e.g. `ruststa run`.
    pub command: String,
    /// The workspace as given with `-w`, if known.
    pub workspace: Option<String>,
    /// The base revision of a mutation.
    pub base_rev: Option<Rev>,
    /// The revision a query read.
    pub rev: Option<Rev>,
    /// The revision a mutation created.
    pub new_rev: Option<Rev>,
    scope: RawJson,
    completeness: RawJson,
    metrics: RawJson,
    fields: Vec<(String, RawJson)>,
    /// Diagnostics; each has a valid [`DiagCode`].
    pub diagnostics: Vec<Diagnostic>,
    /// The error of a failed command.
    pub error: Option<ErrorInfo>,
    /// Canonical follow-up commands, POSIX-quoted.
    pub next_actions: Vec<String>,
}

impl Envelope {
    /// An envelope of a completed command, every other key empty.
    pub fn new(command: impl Into<String>) -> Envelope {
        Envelope {
            command: command.into(),
            workspace: None,
            base_rev: None,
            rev: None,
            new_rev: None,
            scope: RawJson::empty_object(),
            completeness: RawJson::empty_object(),
            metrics: RawJson::empty_object(),
            fields: Vec::new(),
            diagnostics: Vec::new(),
            error: None,
            next_actions: Vec::new(),
        }
    }

    /// An envelope of a failed command.
    pub fn failure(command: impl Into<String>, error: impl Into<ErrorInfo>) -> Envelope {
        let mut envelope = Envelope::new(command);
        envelope.error = Some(error.into());
        envelope
    }

    /// Whether the command completed: no error.
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    /// Sets the analysis scope, a JSON object (WS-REQ-0090); anything else is `INTERNAL`.
    pub fn set_scope(&mut self, scope: impl Into<RawJson>) -> Result<()> {
        self.scope = object("scope", scope.into())?;
        Ok(())
    }

    /// Sets the completeness, a JSON object (WS-REQ-0090); anything else is `INTERNAL`.
    pub fn set_completeness(&mut self, completeness: impl Into<RawJson>) -> Result<()> {
        self.completeness = object("completeness", completeness.into())?;
        Ok(())
    }

    /// Sets the metrics, a JSON object with units in its key names (`_ns`, `_pf`, `_um2`)
    /// (WS-REQ-0090); anything else is `INTERNAL`.
    pub fn set_metrics(&mut self, metrics: impl Into<RawJson>) -> Result<()> {
        self.metrics = object("metrics", metrics.into())?;
        Ok(())
    }

    /// The analysis scope; `{}` unless set.
    pub fn scope(&self) -> &RawJson {
        &self.scope
    }

    /// The completeness; `{}` unless set.
    pub fn completeness(&self) -> &RawJson {
        &self.completeness
    }

    /// The metrics; `{}` unless set.
    pub fn metrics(&self) -> &RawJson {
        &self.metrics
    }

    /// Sets a tool-specific field. Keys are `[a-z][a-z0-9_]*` and not standard keys; a new key
    /// goes after the existing ones, an existing key keeps its place.
    pub fn set_raw(&mut self, key: &str, value: RawJson) -> Result<()> {
        let valid = key.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            && !STANDARD_KEYS.contains(&key);
        if !valid {
            return Err(Error::internal(format!(
                "envelope field '{key}' must match [a-z][a-z0-9_]* and not be a standard key"
            )));
        }
        match self.fields.iter_mut().find(|(k, _)| k == key) {
            Some((_, slot)) => *slot = value,
            None => self.fields.push((key.to_string(), value)),
        }
        Ok(())
    }

    /// Sets a tool-specific field from any serializable value (see [`Envelope::set_raw`]).
    pub fn set<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<()> {
        self.set_raw(key, RawJson::new(value)?)
    }

    /// The tool-specific field `key`.
    pub fn field(&self, key: &str) -> Option<&RawJson> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The tool-specific fields, in insertion order.
    pub fn fields(&self) -> &[(String, RawJson)] {
        &self.fields
    }

    /// Appends a next action from its words, POSIX-quoted (WS-REQ-0096).
    pub fn push_next_action<S: AsRef<str>>(&mut self, words: &[S]) {
        self.next_actions.push(shell_join(words));
    }

    /// The envelope as JSON text, without a final newline.
    pub fn to_json(&self, style: JsonStyle) -> String {
        let compact = self.compact();
        match style {
            JsonStyle::Compact => compact,
            // Every piece is valid JSON, so reformatting cannot fail.
            JsonStyle::Pretty => {
                json::reformat(&compact, JsonStyle::Pretty).unwrap_or_else(|_| compact.clone())
            }
        }
    }

    /// Writes the envelope and a newline.
    pub fn write_to<W: Write>(&self, mut out: W, style: JsonStyle) -> io::Result<()> {
        let mut text = self.to_json(style);
        text.push('\n');
        out.write_all(text.as_bytes())
    }

    fn compact(&self) -> String {
        let string = |s: Option<&str>| s.map_or("null".to_string(), json::quote);
        let rev = |r: Option<Rev>| r.map_or("null".to_string(), |r| json::quote(&r.to_string()));
        let mut o = ObjectText::new();
        o.field("schema", &json::quote(SCHEMA));
        o.field("ok", if self.ok() { "true" } else { "false" });
        o.field("command", &json::quote(&self.command));
        o.field("workspace", &string(self.workspace.as_deref()));
        o.field("base_rev", &rev(self.base_rev));
        o.field("rev", &rev(self.rev));
        o.field("new_rev", &rev(self.new_rev));
        o.field("scope", self.scope.as_str());
        o.field("completeness", self.completeness.as_str());
        o.field("metrics", self.metrics.as_str());
        for (key, value) in &self.fields {
            o.field(key, value.as_str());
        }
        let diagnostics: Vec<String> = self.diagnostics.iter().map(Diagnostic::to_json).collect();
        o.field("diagnostics", &format!("[{}]", diagnostics.join(",")));
        o.field(
            "error",
            &self
                .error
                .as_ref()
                .map_or("null".to_string(), ErrorInfo::to_json),
        );
        let actions: Vec<String> = self.next_actions.iter().map(|a| json::quote(a)).collect();
        o.field("next_actions", &format!("[{}]", actions.join(",")));
        o.finish()
    }
}

/// `value` if it is a JSON object, else `INTERNAL`: the contract makes `key` an object.
fn object(key: &str, value: RawJson) -> Result<RawJson> {
    if value.is_object() {
        return Ok(value);
    }
    Err(Error::internal(format!(
        "the envelope's {key} is a JSON object, not {value}"
    )))
}

/// Compact JSON object text from keys and value texts, in order.
struct ObjectText {
    text: String,
}

impl ObjectText {
    fn new() -> ObjectText {
        ObjectText {
            text: "{".to_string(),
        }
    }

    fn field(&mut self, key: &str, value: &str) {
        if self.text.len() > 1 {
            self.text.push(',');
        }
        self.text.push_str(&json::quote(key));
        self.text.push(':');
        self.text.push_str(value);
    }

    fn finish(mut self) -> String {
        self.text.push('}');
        self.text
    }
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
