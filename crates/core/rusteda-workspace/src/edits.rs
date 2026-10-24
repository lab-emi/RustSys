// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Netlist edit files `rusteda.edits.v1`: the edits an optimizer proposes to a revision's netlist
//! and the queries it asks about them (WS-REQ-0140 to WS-REQ-0159, `docs/specs/workspace/edits.md`).
//!
//! An edit file holds `base` edits, accepted already, then `candidates`, each a list of edits that
//! is evaluated on its own after the base edits, and a `report` request: endpoints by name or
//! `ep<N>` handle and ranked paths by `path<N>` handle. Edits name netlist objects by name, never
//! by an id of one tool, so that any tool of the suite can write and read them. The five edit
//! kinds are `resize`, `insert_buffer`, `remove_buffer`, `swap_pins` and `clone`.
//!
//! [`parse`] reads and checks a file: its schema, every name against the naming rules of the
//! netlist (CORE-REQ-0123), the candidate ids against the handle rules, and the bounds of
//! [`Limits`]; [`EditFile::to_json`] writes the canonical form back. [`apply`] applies one edit to
//! a [`GateNetlist`](rusteda_core::netlist::GateNetlist) by a fixed sequence of netlist operations,
//! the same in every tool, so that the netlist a tool commits after accepting edits is the one
//! the edits were evaluated on. Whether an edit keeps the logic of the netlist (the function of a
//! cell) is a question for the tool that knows the cell libraries; this module checks structure
//! only. Every error has a `W03nn` code ([`codes`]) and the JSON path of what it is about.

mod apply;
#[cfg(test)]
mod tests;

use std::fmt;

use rusteda_core::names;
use serde_json::{Map, Value};

use crate::envelope::{DiagCode, Diagnostic, Severity};

pub use apply::{Applied, apply, find_net, port_bit_net};

/// The schema id of an edit file (WS-REQ-0140).
pub const SCHEMA: &str = "rusteda.edits.v1";

/// The diagnostic codes of edit files and edits (WS-REQ-0144, WS-REQ-0151).
pub mod codes {
    use crate::envelope::DiagCode;

    /// The file is not JSON, or a field is missing, of the wrong type or unknown.
    pub const INVALID: DiagCode = DiagCode::of("W0300");
    /// `schema` is missing or is not [`super::SCHEMA`].
    pub const SCHEMA: DiagCode = DiagCode::of("W0301");
    /// A name breaks the naming rules of the netlist.
    pub const NAME: DiagCode = DiagCode::of("W0302");
    /// A candidate id is not a handle-safe word, or two candidates share it.
    pub const CANDIDATE_ID: DiagCode = DiagCode::of("W0303");
    /// A bound of [`super::Limits`] is exceeded.
    pub const LIMIT: DiagCode = DiagCode::of("W0304");
    /// No instance has the name.
    pub const UNKNOWN_INSTANCE: DiagCode = DiagCode::of("W0305");
    /// No net has the name, or the name is ambiguous.
    pub const UNKNOWN_NET: DiagCode = DiagCode::of("W0306");
    /// No pin of the instance, or no port, has the name.
    pub const UNKNOWN_PIN: DiagCode = DiagCode::of("W0307");
    /// The netlist has no cell of the name and none is supplied.
    pub const UNKNOWN_CELL: DiagCode = DiagCode::of("W0308");
    /// A new instance or net would take a name in use.
    pub const NAME_IN_USE: DiagCode = DiagCode::of("W0309");
    /// A sink is not on the net, drives it, is listed twice, or is a port that cannot move.
    pub const SINK: DiagCode = DiagCode::of("W0310");
    /// The instance or cell does not have the shape of a buffer, or its nets cannot merge.
    pub const BUFFER: DiagCode = DiagCode::of("W0311");
    /// The pins of a pin swap are not two distinct connected input pins.
    pub const PIN_SWAP: DiagCode = DiagCode::of("W0312");
    /// The instance of a clone does not drive exactly one net from exactly one output pin.
    pub const CLONE: DiagCode = DiagCode::of("W0313");
    /// The cell of a resize is not pin-compatible, or another netlist operation failed.
    pub const NETLIST: DiagCode = DiagCode::of("W0314");
}

/// Bounds of an edit file (WS-REQ-0144): a file beyond them is rejected before any edit is
/// applied, so that reading it never allocates beyond them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of the file text.
    pub max_bytes: usize,
    /// Candidates.
    pub max_candidates: usize,
    /// Edits of `base` and of each candidate.
    pub max_edits: usize,
    /// Sinks of one edit.
    pub max_sinks: usize,
    /// Entries of `report.endpoints` and of `report.paths`.
    pub max_report: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_bytes: 64 << 20,
            max_candidates: 1024,
            max_edits: 4096,
            max_sinks: 65_536,
            max_report: 4096,
        }
    }
}

/// A sink of a net: a pin of an instance or a port (WS-REQ-0142).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sink {
    /// The pin `pin` of the instance `instance`.
    Pin {
        /// The instance name.
        instance: String,
        /// The cell pin name.
        pin: String,
    },
    /// The port `port`: a scalar port, or a bit `name[index]` of a bus port.
    Port {
        /// The port name.
        port: String,
    },
}

/// One netlist edit (WS-REQ-0142).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Gives the instance another cell with the same pins and the same function.
    Resize {
        /// The instance.
        instance: String,
        /// The new cell.
        cell: String,
    },
    /// Splits the net with a new buffer that drives the sinks on a new net.
    InsertBuffer {
        /// The net to split.
        net: String,
        /// The cell of the buffer.
        cell: String,
        /// The name of the new buffer instance.
        instance: String,
        /// The name of the new net.
        net_out: String,
        /// The sinks of `net` that move behind the buffer.
        sinks: Vec<Sink>,
    },
    /// Removes a buffer and merges its output net into its input net.
    RemoveBuffer {
        /// The buffer instance.
        instance: String,
    },
    /// Exchanges the connections of two input pins of an instance.
    SwapPins {
        /// The instance.
        instance: String,
        /// The first input pin.
        pin_a: String,
        /// The second input pin.
        pin_b: String,
    },
    /// Duplicates a driver with the same input connections and moves sinks to the copy.
    Clone {
        /// The instance to duplicate.
        instance: String,
        /// The name of the copy.
        new_instance: String,
        /// The name of the net the copy drives.
        new_net: String,
        /// The sinks of the instance's output net that move to the copy.
        sinks: Vec<Sink>,
    },
}

impl Edit {
    /// The `op` of the edit: `resize`, `insert_buffer`, `remove_buffer`, `swap_pins` or `clone`.
    pub fn op(&self) -> &'static str {
        match self {
            Edit::Resize { .. } => "resize",
            Edit::InsertBuffer { .. } => "insert_buffer",
            Edit::RemoveBuffer { .. } => "remove_buffer",
            Edit::SwapPins { .. } => "swap_pins",
            Edit::Clone { .. } => "clone",
        }
    }

    /// The edit as a JSON object in canonical form (WS-REQ-0145).
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert("op".into(), Value::from(self.op()));
        let mut put = |k: &str, v: &str| {
            o.insert(k.into(), Value::from(v));
        };
        match self {
            Edit::Resize { instance, cell } => {
                put("instance", instance);
                put("cell", cell);
            }
            Edit::InsertBuffer {
                net,
                cell,
                instance,
                net_out,
                sinks,
            } => {
                put("net", net);
                put("cell", cell);
                put("instance", instance);
                put("net_out", net_out);
                o.insert("sinks".into(), sinks_value(sinks));
            }
            Edit::RemoveBuffer { instance } => put("instance", instance),
            Edit::SwapPins {
                instance,
                pin_a,
                pin_b,
            } => {
                put("instance", instance);
                put("pin_a", pin_a);
                put("pin_b", pin_b);
            }
            Edit::Clone {
                instance,
                new_instance,
                new_net,
                sinks,
            } => {
                put("instance", instance);
                put("new_instance", new_instance);
                put("new_net", new_net);
                o.insert("sinks".into(), sinks_value(sinks));
            }
        }
        Value::Object(o)
    }
}

fn sinks_value(sinks: &[Sink]) -> Value {
    Value::Array(
        sinks
            .iter()
            .map(|s| {
                let mut o = Map::new();
                match s {
                    Sink::Pin { instance, pin } => {
                        o.insert("instance".into(), Value::from(instance.as_str()));
                        o.insert("pin".into(), Value::from(pin.as_str()));
                    }
                    Sink::Port { port } => {
                        o.insert("port".into(), Value::from(port.as_str()));
                    }
                }
                Value::Object(o)
            })
            .collect(),
    )
}

/// A candidate: edits evaluated together, after the base edits (WS-REQ-0143).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Its id, a handle-safe word unique in the file.
    pub id: String,
    /// Its edits, in order.
    pub edits: Vec<Edit>,
}

/// The values a reader of the file reports for the base and for each candidate (WS-REQ-0143).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportRequest {
    /// Endpoints: a pin `inst/pin`, a port, or an endpoint handle `ep<N>`.
    pub endpoints: Vec<String>,
    /// Ranked paths: path handles `path<N>`.
    pub paths: Vec<String>,
}

/// An edit file (WS-REQ-0140).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditFile {
    /// Edits applied first, in order.
    pub base: Vec<Edit>,
    /// Candidates, each evaluated after `base` and independently of the others.
    pub candidates: Vec<Candidate>,
    /// What to report.
    pub report: ReportRequest,
}

impl EditFile {
    /// The file as canonical JSON text: two-space indentation, the keys of each object in
    /// byte order, a final newline (WS-REQ-0145). [`parse`] reads it back unchanged.
    pub fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".into(), Value::from(SCHEMA));
        o.insert(
            "base".into(),
            Value::Array(self.base.iter().map(Edit::to_value).collect()),
        );
        o.insert(
            "candidates".into(),
            Value::Array(
                self.candidates
                    .iter()
                    .map(|c| {
                        let mut co = Map::new();
                        co.insert("id".into(), Value::from(c.id.as_str()));
                        co.insert(
                            "edits".into(),
                            Value::Array(c.edits.iter().map(Edit::to_value).collect()),
                        );
                        Value::Object(co)
                    })
                    .collect(),
            ),
        );
        let mut r = Map::new();
        r.insert(
            "endpoints".into(),
            Value::from(self.report.endpoints.clone()),
        );
        r.insert("paths".into(), Value::from(self.report.paths.clone()));
        o.insert("report".into(), Value::Object(r));
        let mut text =
            serde_json::to_string_pretty(&canonical(Value::Object(o))).unwrap_or_default();
        text.push('\n');
        text
    }
}

/// `value` with the keys of every object inserted in byte order, so that the text is the same
/// whatever the map type of `serde_json`.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(o) => {
            let mut entries: Vec<(String, Value)> = o.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.into_iter().map(canonical).collect()),
        v => v,
    }
}

/// An error of an edit file or of an edit (WS-REQ-0144, WS-REQ-0151).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditsError {
    /// Its `W03nn` code.
    pub code: DiagCode,
    /// The JSON path of the value it is about, such as `candidates[2].edits[0].cell`; empty for
    /// the whole file.
    pub at: String,
    /// What is wrong.
    pub message: String,
}

impl EditsError {
    /// An error with `code` at `at`.
    pub fn new(code: DiagCode, at: impl Into<String>, message: impl Into<String>) -> EditsError {
        EditsError {
            code,
            at: at.into(),
            message: message.into(),
        }
    }

    /// The error at `prefix` followed by its own path.
    pub fn within(mut self, prefix: &str) -> EditsError {
        self.at = if self.at.is_empty() {
            prefix.to_string()
        } else if prefix.is_empty() {
            self.at
        } else {
            format!("{prefix}.{}", self.at)
        };
        self
    }

    /// The error as an error diagnostic of the envelope.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::new(Severity::Error, self.code, self.to_string())
    }
}

impl fmt::Display for EditsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.at.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.at, self.message)
        }
    }
}

impl std::error::Error for EditsError {}

/// Reads and checks an edit file (WS-REQ-0140 to WS-REQ-0144): the JSON structure, the schema,
/// every name, the candidate ids and the bounds of `limits`. Unknown keys are errors, so that a
/// misspelt field is never ignored.
pub fn parse(text: &str, limits: &Limits) -> Result<EditFile, EditsError> {
    if text.len() > limits.max_bytes {
        return Err(EditsError::new(
            codes::LIMIT,
            "",
            format!(
                "the file has {} bytes, above the limit of {}",
                text.len(),
                limits.max_bytes
            ),
        ));
    }
    let value: Value = serde_json::from_str(text)
        .map_err(|e| EditsError::new(codes::INVALID, "", format!("not valid JSON: {e}")))?;
    let o = object(&value, "")?;
    known_keys(o, "", &["schema", "base", "candidates", "report"])?;
    match o.get("schema") {
        Some(Value::String(s)) if s == SCHEMA => {}
        Some(Value::String(s)) => {
            return Err(EditsError::new(
                codes::SCHEMA,
                "schema",
                format!("schema '{s}' is not '{SCHEMA}'"),
            ));
        }
        _ => {
            return Err(EditsError::new(
                codes::SCHEMA,
                "schema",
                format!("the file has no schema '{SCHEMA}'"),
            ));
        }
    }
    let base = match o.get("base") {
        None => Vec::new(),
        Some(v) => edit_list(v, "base", limits)?,
    };
    let mut candidates = Vec::new();
    if let Some(v) = o.get("candidates") {
        let list = array(v, "candidates")?;
        if list.len() > limits.max_candidates {
            return Err(EditsError::new(
                codes::LIMIT,
                "candidates",
                format!(
                    "{} candidates, above the limit of {}",
                    list.len(),
                    limits.max_candidates
                ),
            ));
        }
        for (k, c) in list.iter().enumerate() {
            let at = format!("candidates[{k}]");
            let co = object(c, &at)?;
            known_keys(co, &at, &["id", "edits"])?;
            let id = string(co, &at, "id")?;
            if !is_handle_word(&id) {
                return Err(EditsError::new(
                    codes::CANDIDATE_ID,
                    format!("{at}.id"),
                    format!(
                        "candidate id '{}' is not 1 to 64 of [A-Za-z0-9_-]",
                        names::for_display(&id)
                    ),
                ));
            }
            if candidates.iter().any(|c: &Candidate| c.id == id) {
                return Err(EditsError::new(
                    codes::CANDIDATE_ID,
                    format!("{at}.id"),
                    format!("two candidates have the id '{id}'"),
                ));
            }
            let edits = match co.get("edits") {
                Some(v) => edit_list(v, &format!("{at}.edits"), limits)?,
                None => {
                    return Err(EditsError::new(
                        codes::INVALID,
                        format!("{at}.edits"),
                        "a candidate needs its list of edits",
                    ));
                }
            };
            candidates.push(Candidate { id, edits });
        }
    }
    let mut report = ReportRequest::default();
    if let Some(v) = o.get("report") {
        let ro = object(v, "report")?;
        known_keys(ro, "report", &["endpoints", "paths"])?;
        for (key, out) in [
            ("endpoints", &mut report.endpoints),
            ("paths", &mut report.paths),
        ] {
            let Some(list) = ro.get(key) else {
                continue;
            };
            let at = format!("report.{key}");
            let list = array(list, &at)?;
            if list.len() > limits.max_report {
                return Err(EditsError::new(
                    codes::LIMIT,
                    at,
                    format!(
                        "{} entries, above the limit of {}",
                        list.len(),
                        limits.max_report
                    ),
                ));
            }
            for (k, e) in list.iter().enumerate() {
                let eat = format!("{at}[{k}]");
                let Value::String(s) = e else {
                    return Err(EditsError::new(codes::INVALID, eat, "expected a string"));
                };
                if key == "paths" {
                    if !is_numbered_handle(s, "path") {
                        return Err(EditsError::new(
                            codes::NAME,
                            eat,
                            format!("'{}' is not a path handle path<N>", names::for_display(s)),
                        ));
                    }
                } else {
                    hierarchical(s, &eat)?;
                }
                out.push(s.clone());
            }
        }
    }
    Ok(EditFile {
        base,
        candidates,
        report,
    })
}

/// Parses one edit from its JSON object (WS-REQ-0142).
pub fn parse_edit(value: &Value, limits: &Limits) -> Result<Edit, EditsError> {
    let o = object(value, "")?;
    let op = string(o, "", "op")?;
    let edit = match op.as_str() {
        "resize" => {
            known_keys(o, "", &["op", "instance", "cell"])?;
            Edit::Resize {
                instance: name_field(o, "instance")?,
                cell: verbatim_field(o, "cell")?,
            }
        }
        "insert_buffer" => {
            known_keys(
                o,
                "",
                &["op", "net", "cell", "instance", "net_out", "sinks"],
            )?;
            Edit::InsertBuffer {
                net: name_field(o, "net")?,
                cell: verbatim_field(o, "cell")?,
                instance: name_field(o, "instance")?,
                net_out: name_field(o, "net_out")?,
                sinks: sinks_field(o, limits)?,
            }
        }
        "remove_buffer" => {
            known_keys(o, "", &["op", "instance"])?;
            Edit::RemoveBuffer {
                instance: name_field(o, "instance")?,
            }
        }
        "swap_pins" => {
            known_keys(o, "", &["op", "instance", "pin_a", "pin_b"])?;
            Edit::SwapPins {
                instance: name_field(o, "instance")?,
                pin_a: verbatim_field(o, "pin_a")?,
                pin_b: verbatim_field(o, "pin_b")?,
            }
        }
        "clone" => {
            known_keys(
                o,
                "",
                &["op", "instance", "new_instance", "new_net", "sinks"],
            )?;
            Edit::Clone {
                instance: name_field(o, "instance")?,
                new_instance: name_field(o, "new_instance")?,
                new_net: name_field(o, "new_net")?,
                sinks: sinks_field(o, limits)?,
            }
        }
        other => {
            return Err(EditsError::new(
                codes::INVALID,
                "op",
                format!(
                    "unknown edit '{}': expected resize, insert_buffer, remove_buffer, \
                     swap_pins or clone",
                    names::for_display(other)
                ),
            ));
        }
    };
    Ok(edit)
}

fn edit_list(value: &Value, at: &str, limits: &Limits) -> Result<Vec<Edit>, EditsError> {
    let list = array(value, at)?;
    if list.len() > limits.max_edits {
        return Err(EditsError::new(
            codes::LIMIT,
            at,
            format!(
                "{} edits, above the limit of {}",
                list.len(),
                limits.max_edits
            ),
        ));
    }
    list.iter()
        .enumerate()
        .map(|(k, v)| parse_edit(v, limits).map_err(|e| e.within(&format!("{at}[{k}]"))))
        .collect()
}

fn sinks_field(o: &Map<String, Value>, limits: &Limits) -> Result<Vec<Sink>, EditsError> {
    let Some(v) = o.get("sinks") else {
        return Err(EditsError::new(
            codes::INVALID,
            "sinks",
            "the edit needs its sinks",
        ));
    };
    let list = array(v, "sinks")?;
    if list.is_empty() {
        return Err(EditsError::new(
            codes::SINK,
            "sinks",
            "the edit moves no sink",
        ));
    }
    if list.len() > limits.max_sinks {
        return Err(EditsError::new(
            codes::LIMIT,
            "sinks",
            format!(
                "{} sinks, above the limit of {}",
                list.len(),
                limits.max_sinks
            ),
        ));
    }
    let mut out = Vec::with_capacity(list.len());
    for (k, s) in list.iter().enumerate() {
        let at = format!("sinks[{k}]");
        let so = object(s, &at)?;
        let sink = if so.contains_key("port") {
            known_keys(so, &at, &["port"])?;
            Sink::Port {
                port: name_field(so, "port").map_err(|e| e.within(&at))?,
            }
        } else {
            known_keys(so, &at, &["instance", "pin"])?;
            Sink::Pin {
                instance: name_field(so, "instance").map_err(|e| e.within(&at))?,
                pin: verbatim_field(so, "pin").map_err(|e| e.within(&at))?,
            }
        };
        if out.contains(&sink) {
            return Err(EditsError::new(codes::SINK, at, "the sink is listed twice"));
        }
        out.push(sink);
    }
    Ok(out)
}

fn object<'v>(value: &'v Value, at: &str) -> Result<&'v Map<String, Value>, EditsError> {
    value
        .as_object()
        .ok_or_else(|| EditsError::new(codes::INVALID, at, "expected an object"))
}

fn array<'v>(value: &'v Value, at: &str) -> Result<&'v Vec<Value>, EditsError> {
    value
        .as_array()
        .ok_or_else(|| EditsError::new(codes::INVALID, at, "expected an array"))
}

fn join(at: &str, key: &str) -> String {
    if at.is_empty() {
        key.to_string()
    } else {
        format!("{at}.{key}")
    }
}

fn known_keys(o: &Map<String, Value>, at: &str, keys: &[&str]) -> Result<(), EditsError> {
    match o.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(EditsError::new(
            codes::INVALID,
            join(at, k),
            format!("unknown key '{}'", names::for_display(k)),
        )),
        None => Ok(()),
    }
}

fn string(o: &Map<String, Value>, at: &str, key: &str) -> Result<String, EditsError> {
    match o.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(EditsError::new(
            codes::INVALID,
            join(at, key),
            "expected a string",
        )),
        None => Err(EditsError::new(
            codes::INVALID,
            join(at, key),
            format!("missing '{key}'"),
        )),
    }
}

fn hierarchical(name: &str, at: &str) -> Result<(), EditsError> {
    names::check_hierarchical(name).map_err(|e| {
        EditsError::new(
            codes::NAME,
            at,
            format!("'{}': {e}", names::for_display(name)),
        )
    })
}

/// A port, net or instance name (CORE-REQ-0123).
fn name_field(o: &Map<String, Value>, key: &str) -> Result<String, EditsError> {
    let s = string(o, "", key)?;
    hierarchical(&s, key)?;
    Ok(s)
}

/// A cell or cell pin name (CORE-REQ-0123).
fn verbatim_field(o: &Map<String, Value>, key: &str) -> Result<String, EditsError> {
    let s = string(o, "", key)?;
    names::check_verbatim(&s).map_err(|e| {
        EditsError::new(
            codes::NAME,
            key,
            format!("'{}': {e}", names::for_display(&s)),
        )
    })?;
    Ok(s)
}

/// A handle-safe word of 1 to 64 characters: `[A-Za-z0-9_-]+` (WS-REQ-0022).
fn is_handle_word(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// True for `<prefix><N>` with `N >= 1` written without leading zeros.
pub fn is_numbered_handle(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix).is_some_and(|n| {
        !n.is_empty()
            && n.len() <= 10
            && !n.starts_with('0')
            && n.bytes().all(|b| b.is_ascii_digit())
    })
}
