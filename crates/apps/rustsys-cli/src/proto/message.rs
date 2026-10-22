// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Messages as JSON (JOB-REQ-0007, PROTO-REQ-0053): severity, code, file location, text and
//! help, each optional key absent when the message has no such part.

use rusteda_core::job::{Diag, Location, Severity};
use serde_json::{Map, Value, json};

/// The JSON object of a message: `severity`, `code`, `message` and, when present, `file`,
/// `line`, `column` and `help`.
pub fn to_json(d: &Diag) -> Value {
    let mut m = Map::new();
    m.insert("severity".into(), json!(d.severity.as_str()));
    m.insert("code".into(), json!(d.code));
    if let Some(l) = &d.location {
        m.insert("file".into(), json!(l.file));
        m.insert("line".into(), json!(l.line));
        m.insert("column".into(), json!(l.column));
    }
    m.insert("message".into(), json!(d.message));
    if let Some(h) = &d.help {
        m.insert("help".into(), json!(h));
    }
    Value::Object(m)
}

/// Reads the JSON object of a message back; `None` when a required key is missing or a value
/// is malformed.
pub fn from_json(v: &Value) -> Option<Diag> {
    let severity = match v.get("severity")?.as_str()? {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "note" => Severity::Note,
        _ => return None,
    };
    let code = v.get("code")?.as_str()?;
    let mut d = Diag::new(severity, code, v.get("message")?.as_str()?);
    if let Some(file) = v.get("file") {
        let num = |k: &str| {
            v.get(k)
                .and_then(Value::as_u64)
                .and_then(|x| u32::try_from(x).ok())
        };
        d.location = Some(Location {
            file: file.as_str()?.to_string(),
            line: num("line")?,
            column: num("column")?,
        });
    }
    if let Some(h) = v.get("help") {
        d.help = Some(h.as_str()?.to_string());
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // PROTO-REQ-0053: the parts of a message in JSON, each optional key absent when the message
    // has no such part, and read back unchanged.
    #[test]
    fn message_keys_in_json() {
        let d = Diag::warning("V0100", "expected ';'")
            .at("top.sv", 4, 9)
            .with_help("end the statement");
        let v = to_json(&d);
        assert_eq!(v["file"], "top.sv");
        assert_eq!(v["line"], 4);
        assert_eq!(v["column"], 9);
        assert_eq!(v["help"], "end the statement");
        assert_eq!(from_json(&v), Some(d));
        let plain = Diag::error("S0703", "no output netlist");
        let v = to_json(&plain);
        assert!(v.get("file").is_none() && v.get("help").is_none());
        assert_eq!(from_json(&v), Some(plain));
        let mut bad = to_json(&Diag::note("A0110", "x"));
        bad["severity"] = json!("fatal");
        assert_eq!(from_json(&bad), None);
    }
}
