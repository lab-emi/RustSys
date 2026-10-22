// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The JSON files of a workspace: `manifest.json`, `meta.json`, `history.jsonl` lines and the
//! reserved `ui_state.json`.
//!
//! Each struct declares its fields in the contract's key order, so serialization writes the keys
//! in that order; maps are `BTreeMap`s and serialize sorted (WS-REQ-0005). Readers accept the
//! major schema version they know (any minor version, unknown keys ignored) and reject a newer
//! major version with `SCHEMA_TOO_NEW` (WS-REQ-0004).

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::error::{Corruption, Error, ErrorCode, Result};
use crate::hash::{Hash64, hash_records};
use crate::rev::{BadHandle, Handle, Rev, is_handle};

/// Schema of `manifest.json`.
pub const MANIFEST_SCHEMA: &str = "rusteda.manifest.v1";
/// Schema of `meta.json`.
pub const REVISION_SCHEMA: &str = "rusteda.revision.v1";
/// Schema of the lines of `history.jsonl`.
pub const HISTORY_SCHEMA: &str = "rusteda.history.v1";
/// Schema of `ui_state.json`.
pub const UI_STATE_SCHEMA: &str = "rusteda.ui_state.v1";

/// The pseudo-table whose hash is the list hash of a revision's libraries (WS-REQ-0013).
pub const LIBRARIES: &str = "libraries";

/// The prefix of the per-corner pseudo-tables `libraries.<corner>`, whose hash is the list hash
/// of the libraries of one corner (WS-REQ-0014).
pub const CORNER_LIBRARIES_PREFIX: &str = "libraries.";

/// A JSON object with sorted keys. (Serialized, it lists its keys sorted, whatever the order of
/// insertion; see [`crate::JsonObject`] for an object in a chosen order.)
pub type JsonMap = BTreeMap<String, Value>;

/// Values per table: a table name to a JSON object of values that describe that table, as the
/// `summary` and `status` of `meta.json` hold them (contract §3, CA-STATUS; WS-REQ-0045), and
/// the `metric_deltas` of a history line (WS-REQ-0044).
pub type TableValues = BTreeMap<String, JsonMap>;

/// The upstream hashes of a table: table name to the hash of the table it was computed from.
pub type Upstream = BTreeMap<String, Hash64>;

/// Whether `name` is a libraries pseudo-table: `libraries` or `libraries.<corner>` with a
/// corner of `[A-Za-z0-9_-]+` (WS-REQ-0013, WS-REQ-0014).
pub fn is_libraries_table(name: &str) -> bool {
    name == LIBRARIES
        || name
            .strip_prefix(CORNER_LIBRARIES_PREFIX)
            .is_some_and(is_handle)
}

/// The hash of a libraries pseudo-table among `libraries`: `Some(Some(hash))` for `libraries`
/// and for `libraries.<corner>` when the corner has libraries, `Some(None)` for a corner
/// without libraries, and `None` when `name` is not a libraries pseudo-table.
pub(crate) fn libraries_table_hash(
    name: &str,
    libraries: &[LibraryEntry],
) -> Option<Option<Hash64>> {
    if name == LIBRARIES {
        return Some(Some(LibraryEntry::list_hash(libraries)));
    }
    let corner = name.strip_prefix(CORNER_LIBRARIES_PREFIX)?;
    Some(LibraryEntry::corner_hash(libraries, corner))
}

/// `manifest.json`: the workspace HEAD (WS-REQ-0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// `rusteda.manifest.v1`.
    pub schema: String,
    /// The current revision.
    pub head: Rev,
    /// The lowest revision number never used by a published revision.
    pub next_rev: u32,
    /// The corner names of HEAD's libraries, in first-appearance order; derived, rewritten at
    /// every commit and revert.
    pub corners: Vec<String>,
}

impl Manifest {
    /// A manifest of this schema.
    pub fn new(head: Rev, next_rev: u32, corners: Vec<String>) -> Manifest {
        Manifest {
            schema: MANIFEST_SCHEMA.to_string(),
            head,
            next_rev,
            corners,
        }
    }
}

/// `meta.json`: what a revision holds and how it was made (WS-REQ-0041).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionMeta {
    /// `rusteda.revision.v1`.
    pub schema: String,
    /// This revision.
    pub rev: Rev,
    /// The base revision of the command that made it; `null` for `rev0`.
    pub parent: Option<Rev>,
    /// Commit time, milliseconds since the Unix epoch.
    pub created_unix_ms: u64,
    /// The tool that made the revision, e.g. `rusteda`.
    pub tool: String,
    /// Its version.
    pub tool_version: String,
    /// The command, e.g. `rusteda import`.
    pub command: String,
    /// The full argument vector.
    pub argv: Vec<String>,
    /// A message, or `null`.
    pub message: Option<String>,
    /// The tables, by name.
    pub tables: BTreeMap<String, TableEntry>,
    /// The libraries, in order.
    pub libraries: Vec<LibraryEntry>,
    /// Source files of tables, with their hashes.
    pub sources: Vec<SourceEntry>,
    /// Tables of the parent that this revision does not carry, with the reason.
    pub dropped: Vec<DroppedEntry>,
    /// Tool-defined summary values per table, `{table: {key: value}}`, keys only for tables of
    /// the revision (contract §3, CA-STATUS; WS-REQ-0045): they describe their table, are
    /// carried forward with it and dropped with it; numeric ones give the history's metric
    /// deltas.
    #[serde(deserialize_with = "table_values")]
    pub summary: TableValues,
    /// Tool-defined status values per table, `{table: {key: value}}`, keys only for tables of
    /// the revision (CA-STATUS, WS-REQ-0045), carried and dropped with their table, so a status
    /// never outlives the table it describes and an unchecked property is never reported as
    /// passing: a property whose table is absent is unchecked (`rusteda show` renders a missing
    /// `sta` status as timing `not_analyzed`, a missing `check` status as equivalence
    /// `unchecked`).
    #[serde(deserialize_with = "table_values")]
    pub status: TableValues,
}

/// Reads `summary` or `status`: an object of objects, one per table (contract §3, CA-STATUS).
/// A value that is not an object, as in a flat `{"equivalence": "unchecked"}`, is refused
/// with a reason that names the rule.
fn table_values<'de, D: Deserializer<'de>>(deserializer: D) -> Result<TableValues, D::Error> {
    let raw = BTreeMap::<String, Value>::deserialize(deserializer)?;
    raw.into_iter()
        .map(|(table, values)| match values {
            Value::Object(values) => Ok((table, values.into_iter().collect())),
            other => Err(serde::de::Error::custom(format!(
                "the key {table} has the value {other}, not an object of values: summary and \
                 status hold values per table, {{table: {{key: value}}}}, and only for tables \
                 present (contract §3, CA-STATUS)"
            ))),
        })
        .collect()
}

impl RevisionMeta {
    /// The hash of a table, including the pseudo-tables `libraries` and `libraries.<corner>`.
    pub fn table_hash(&self, name: &str) -> Option<Hash64> {
        match libraries_table_hash(name, &self.libraries) {
            Some(hash) => hash,
            None => self.tables.get(name).map(|t| t.hash),
        }
    }

    /// Why `summary` or `status` breaks the contract's rule that they hold keys only for
    /// tables of the revision (CA-STATUS, WS-REQ-0045): the first key that is not one.
    pub(crate) fn values_problem(&self) -> Option<String> {
        for (what, values) in [("summary", &self.summary), ("status", &self.status)] {
            if let Some(key) = values.keys().find(|key| !self.tables.contains_key(*key)) {
                return Some(format!(
                    "{what} has the key {key}, which is not a table of the revision: {what} \
                     holds values per table, {{table: {{key: value}}}}, and only for tables \
                     present (contract §3, CA-STATUS)"
                ));
            }
        }
        None
    }
}

/// A table of a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableEntry {
    /// The path relative to the revision directory, `/`-separated.
    pub path: String,
    /// The file or directory hash.
    pub hash: Hash64,
    /// The hashes of the tables it was computed from.
    pub upstream: Upstream,
}

/// A library of a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// The library name.
    pub name: String,
    /// Its corner, `default` unless given.
    pub corner: String,
    /// The hash of the library file.
    pub hash: Hash64,
    /// Where it was imported from, as given.
    pub source: String,
    /// The compiled library, relative to the workspace: `cache/<name>-<h16>.rlib`.
    pub compiled: String,
}

impl LibraryEntry {
    /// The corner of a library imported without `--corner`.
    pub const DEFAULT_CORNER: &'static str = "default";

    /// The list hash of libraries: one record `name NUL corner NUL hash-hex LF` per library,
    /// in order (WS-REQ-0013). Source and compiled paths do not count.
    pub fn list_hash(libraries: &[LibraryEntry]) -> Hash64 {
        hash_records(
            libraries
                .iter()
                .map(|l| [l.name.clone(), l.corner.clone(), l.hash.hex()]),
        )
    }

    /// The list hash of the libraries of `corner`, in order: the hash of the pseudo-table
    /// `libraries.<corner>` (WS-REQ-0014), or `None` when no library belongs to the corner. A
    /// table computed from one corner names this pseudo-table as its upstream, so a library
    /// added for another corner does not drop it.
    pub fn corner_hash(libraries: &[LibraryEntry], corner: &str) -> Option<Hash64> {
        let members: Vec<&LibraryEntry> = libraries.iter().filter(|l| l.corner == corner).collect();
        if members.is_empty() {
            return None;
        }
        Some(hash_records(
            members
                .iter()
                .map(|l| [l.name.clone(), l.corner.clone(), l.hash.hex()]),
        ))
    }

    /// The corner names of libraries, in first-appearance order (WS-REQ-0040).
    pub fn corners(libraries: &[LibraryEntry]) -> Vec<String> {
        let mut corners: Vec<String> = Vec::new();
        for library in libraries {
            if !corners.contains(&library.corner) {
                corners.push(library.corner.clone());
            }
        }
        corners
    }
}

/// A source of a table: a file, or an environment value (contract §3, CA-SDCENV).
///
/// A file has its path as given and its file hash. An environment value that a table was
/// computed with, such as `rusteda import --sdc top.sdc --sdc-env PDK_ROOT=/pdk`, has the path
/// `env:NAME=VALUE` and the xxh3-64 of `VALUE` as its hash (WS-REQ-0047, extension), so it fits
/// the entry shape `{table, path, hash}` of `meta.json`; [`SourceEntry::env`] reads it back. A
/// file path never starts with `env:` ([`crate::Txn::add_source`] refuses one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEntry {
    /// The table built from it.
    pub table: String,
    /// The path as given, or `env:NAME=VALUE` for an environment value.
    pub path: String,
    /// The file hash, or the hash of the value.
    pub hash: Hash64,
}

/// The prefix of the `path` of a source entry that records an environment value,
/// `env:NAME=VALUE` (WS-REQ-0047).
pub const ENV_SOURCE_PREFIX: &str = "env:";

impl SourceEntry {
    /// The source entry of environment value `name`=`value` of `table` (WS-REQ-0047): path
    /// `env:NAME=VALUE`, hash the xxh3-64 of `value`. `name` is not empty and holds no `=`, and
    /// neither holds a NUL character; otherwise `INPUT_REJECTED`.
    pub fn env_value(table: &str, name: &str, value: &str) -> Result<SourceEntry> {
        if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
            return Err(Error::input_rejected(format!(
                "environment value {name:?}={value:?} of table {table}: the name must not be \
                 empty or hold '=', and neither part may hold a NUL character"
            )));
        }
        Ok(SourceEntry {
            table: table.to_string(),
            path: format!("{ENV_SOURCE_PREFIX}{name}={value}"),
            hash: Hash64::of(value.as_bytes()),
        })
    }

    /// The name and value of an environment source (`env:NAME=VALUE`, WS-REQ-0047), or `None`
    /// for a source file.
    pub fn env(&self) -> Option<(&str, &str)> {
        let (name, value) = self.path.strip_prefix(ENV_SOURCE_PREFIX)?.split_once('=')?;
        (!name.is_empty()).then_some((name, value))
    }
}

/// A table of the parent revision that a revision does not carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedEntry {
    /// The table name.
    pub table: String,
    /// Why it was dropped.
    pub reason: String,
}

/// One line of `history.jsonl` (WS-REQ-0042).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// `rusteda.history.v1`.
    pub schema: String,
    /// When the line was written, milliseconds since the Unix epoch.
    pub time_unix_ms: u64,
    /// The tool.
    pub tool: String,
    /// Its version.
    pub tool_version: String,
    /// The command.
    pub command: String,
    /// The full argument vector.
    pub argv: Vec<String>,
    /// The base revision, if the command named a valid one.
    pub base_rev: Option<Rev>,
    /// The revision the command created, if any.
    pub new_rev: Option<Rev>,
    /// HEAD after the command.
    pub head_after: Rev,
    /// Whether the command completed.
    pub ok: bool,
    /// The process exit code: 0, 3 (negative verdict) or 5 (inconclusive) for a command that
    /// completed, else the exit code of its error.
    pub exit: i32,
    /// The error of a failed command.
    pub error: Option<HistoryError>,
    /// The metrics the command reported.
    pub metrics: JsonMap,
    /// (extension of the inner shape) Per table the command changed, the numeric summary
    /// values after minus before, `{table: {key: delta}}` (WS-REQ-0044).
    pub metric_deltas: TableValues,
}

/// The error of a failed command in `history.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryError {
    /// The envelope error code.
    pub code: ErrorCode,
    /// The message.
    pub message: String,
}

/// `ui_state.json`, reserved: the selection a future GUI shares with the command line
/// (WS-REQ-0043). Every handle in it is shell-safe: [`UiState::validate`] holds for every
/// state the workspace reads or writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiState {
    /// `rusteda.ui_state.v1`.
    pub schema: String,
    /// The revision the selection belongs to.
    pub revision: Rev,
    /// The selection handle, `sel<N>`.
    pub selection_handle: String,
    /// Selected instance handles, `inst<N>`.
    pub selected_instances: Vec<String>,
    /// Selected path handles, `path<N>`.
    pub selected_paths: Vec<String>,
}

impl UiState {
    /// A selection of this schema.
    pub fn new(
        revision: Rev,
        selection_handle: impl Into<String>,
        selected_instances: Vec<String>,
        selected_paths: Vec<String>,
    ) -> UiState {
        UiState {
            schema: UI_STATE_SCHEMA.to_string(),
            revision,
            selection_handle: selection_handle.into(),
            selected_instances,
            selected_paths,
        }
    }

    /// Checks the handles (WS-REQ-0043, contract §2 and §7, Rule 4): `selection_handle` is
    /// `sel<N>`, every selected instance an instance handle `inst<N>` and every selected path
    /// a path handle `path<N>`, each with `N >= 1` and of `[A-Za-z0-9_-]` only, so the command
    /// line never passes on text like `$HOME` or `#p1`, nor a handle of another kind as a
    /// validated selection.
    pub fn validate(&self) -> std::result::Result<(), BadHandle> {
        Handle::parse_as(&self.selection_handle, "sel")?;
        for handle in &self.selected_instances {
            Handle::parse_as(handle, "inst")?;
        }
        for handle in &self.selected_paths {
            Handle::parse_as(handle, "path")?;
        }
        Ok(())
    }
}

/// `new - base` for every key whose value is a number in both maps (WS-REQ-0044). Integer
/// differences stay integers; anything else is computed in `f64`.
pub fn metric_deltas(new: &JsonMap, base: &JsonMap) -> JsonMap {
    let mut deltas = JsonMap::new();
    for (key, new_value) in new {
        let (Value::Number(n), Some(Value::Number(b))) = (new_value, base.get(key)) else {
            continue;
        };
        let integer = match (n.as_i64(), b.as_i64()) {
            (Some(n), Some(b)) => n.checked_sub(b).map(Value::from),
            _ => None,
        };
        let delta = integer.or_else(|| match (n.as_f64(), b.as_f64()) {
            (Some(n), Some(b)) => serde_json::Number::from_f64(n - b).map(Value::Number),
            _ => None,
        });
        if let Some(delta) = delta {
            deltas.insert(key.clone(), delta);
        }
    }
    deltas
}

/// Reads a workspace JSON file: checks its schema against `supported`, then parses it.
pub(crate) fn parse_json<T: DeserializeOwned>(
    path: &Path,
    bytes: &[u8],
    supported: &str,
) -> Result<T> {
    #[derive(Deserialize)]
    struct Probe {
        schema: Option<String>,
    }
    let malformed = |e: serde_json::Error| {
        Error::corrupt(
            path,
            Corruption::Json {
                reason: e.to_string(),
            },
        )
    };
    let probe: Probe = serde_json::from_slice(bytes).map_err(malformed)?;
    let Some(found) = probe.schema else {
        let reason = format!("no \"schema\" key (expected {supported})");
        return Err(Error::corrupt(path, Corruption::Json { reason }));
    };
    check_schema(path, &found, supported)?;
    serde_json::from_slice(bytes).map_err(malformed)
}

/// Accepts `found` if it is `supported` up to a minor version; a newer major version is
/// `SCHEMA_TOO_NEW`, anything else `CORRUPT_TABLE` (WS-REQ-0004).
pub(crate) fn check_schema(path: &Path, found: &str, supported: &str) -> Result<()> {
    let Some((family, major)) = split_schema(supported) else {
        return Err(Error::internal(format!("bad schema id {supported}")));
    };
    match split_schema(found) {
        Some((f, m)) if f == family && m == major => Ok(()),
        Some((f, m)) if f == family && m > major => Err(Error::SchemaTooNew {
            path: path.to_path_buf(),
            found: found.to_string(),
            supported: supported.to_string(),
        }),
        _ => {
            let reason = format!("schema \"{found}\" is not {supported}");
            Err(Error::corrupt(path, Corruption::Json { reason }))
        }
    }
}

/// `family.v<major>` or `family.v<major>.<minor>` to `(family, major)`.
fn split_schema(id: &str) -> Option<(&str, u32)> {
    let digits = |s: &str| -> Option<u32> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
    };
    let (head, last) = id.rsplit_once('.')?;
    if let Some(major) = last.strip_prefix('v') {
        return Some((head, digits(major)?));
    }
    digits(last)?;
    let (family, version) = head.rsplit_once('.')?;
    Some((family, digits(version.strip_prefix('v')?)?))
}

/// Pretty JSON with a final newline, as workspace files are written (WS-REQ-0005).
pub(crate) fn to_pretty_json<T: Serialize>(value: &T) -> Result<String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::internal(format!("serializing JSON: {e}")))?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn path() -> &'static Path {
        Path::new("f.json")
    }

    // WS-REQ-0004
    #[test]
    fn schema_versions() {
        let s = MANIFEST_SCHEMA;
        assert!(check_schema(path(), "rusteda.manifest.v1", s).is_ok());
        assert!(check_schema(path(), "rusteda.manifest.v1.3", s).is_ok());
        let err = check_schema(path(), "rusteda.manifest.v2", s).unwrap_err();
        assert_eq!(err.code(), ErrorCode::SchemaTooNew);
        let err = check_schema(path(), "rusteda.manifest.v10.1", s).unwrap_err();
        assert_eq!(err.code(), ErrorCode::SchemaTooNew);
        for bad in [
            "rusteda.revision.v1",
            "rusteda.manifest.v0",
            "rusteda.manifest",
            "v1",
            "",
        ] {
            let err = check_schema(path(), bad, s).unwrap_err();
            assert_eq!(err.code(), ErrorCode::CorruptTable, "{bad}");
        }
        let m: Manifest = parse_json(
            path(),
            br#"{"schema":"rusteda.manifest.v1.1","head":"rev2","next_rev":3,"corners":[],"x":1}"#,
            s,
        )
        .unwrap();
        assert_eq!(m.head, Rev(2));
        let err = parse_json::<Manifest>(path(), br#"{"schema":"rusteda.manifest.v2"}"#, s);
        assert_eq!(err.unwrap_err().code(), ErrorCode::SchemaTooNew);
        let err = parse_json::<Manifest>(path(), b"{\"head\":", s).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
        let err = parse_json::<Manifest>(path(), br#"{"next_rev":1}"#, s).unwrap_err();
        assert!(err.to_string().contains("no \"schema\" key"), "{err}");
    }

    // WS-REQ-0040, WS-REQ-0005
    #[test]
    fn manifest_key_order() {
        let m = Manifest::new(Rev(3), 4, vec!["default".into()]);
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"schema":"rusteda.manifest.v1","head":"rev3","next_rev":4,"corners":["default"]}"#
        );
    }

    // WS-REQ-0041, WS-REQ-0043
    #[test]
    fn meta_and_ui_state_key_order() {
        let meta = RevisionMeta {
            schema: REVISION_SCHEMA.into(),
            rev: Rev(1),
            parent: Some(Rev(0)),
            created_unix_ms: 5,
            tool: "t".into(),
            tool_version: "1".into(),
            command: "t run".into(),
            argv: vec!["t".into()],
            message: None,
            tables: BTreeMap::from([(
                "b".to_string(),
                TableEntry {
                    path: "b.rbin".into(),
                    hash: Hash64(1),
                    upstream: Upstream::from([("a".to_string(), Hash64(2))]),
                },
            )]),
            libraries: vec![LibraryEntry {
                name: "l".into(),
                corner: "default".into(),
                hash: Hash64(3),
                source: "l.lib".into(),
                compiled: "cache/l-0000000000000003.rlib".into(),
            }],
            sources: vec![SourceEntry {
                table: "b".into(),
                path: "s".into(),
                hash: Hash64(4),
            }],
            dropped: vec![DroppedEntry {
                table: "c".into(),
                reason: "r".into(),
            }],
            summary: TableValues::from([(
                "b".into(),
                JsonMap::from([("z".into(), json!(1)), ("a".into(), json!(2))]),
            )]),
            status: TableValues::new(),
        };
        let text = serde_json::to_string(&meta).unwrap();
        let h = |n: u64| Hash64(n).to_string();
        let expected = format!(
            "{{\"schema\":\"rusteda.revision.v1\",\"rev\":\"rev1\",\"parent\":\"rev0\",\
             \"created_unix_ms\":5,\"tool\":\"t\",\"tool_version\":\"1\",\"command\":\"t run\",\
             \"argv\":[\"t\"],\"message\":null,\"tables\":{{\"b\":{{\"path\":\"b.rbin\",\
             \"hash\":\"{}\",\"upstream\":{{\"a\":\"{}\"}}}}}},\"libraries\":[{{\"name\":\"l\",\
             \"corner\":\"default\",\"hash\":\"{}\",\"source\":\"l.lib\",\
             \"compiled\":\"cache/l-0000000000000003.rlib\"}}],\"sources\":[{{\"table\":\"b\",\
             \"path\":\"s\",\"hash\":\"{}\"}}],\"dropped\":[{{\"table\":\"c\",\"reason\":\"r\"}}],\
             \"summary\":{{\"b\":{{\"a\":2,\"z\":1}}}},\"status\":{{}}}}",
            h(1),
            h(2),
            h(3),
            h(4)
        );
        assert_eq!(text, expected);
        let back: RevisionMeta = serde_json::from_str(&text).unwrap();
        assert_eq!(back, meta);
        assert_eq!(back.values_problem(), None);

        // Review CR2-1, contract §3 CA-STATUS: summary and status hold keys only for tables of
        // the revision. A flat object, such as the status-honesty example of contract draft 4,
        // has keys that are no tables.
        let mut flat = meta.clone();
        flat.status = TableValues::from([("equivalence".into(), JsonMap::new())]);
        let problem = flat.values_problem().unwrap();
        assert!(problem.contains("key equivalence") && problem.contains("CA-STATUS"));
        let mut dropped = meta.clone();
        dropped.summary.insert("sta".into(), JsonMap::new());
        assert!(
            dropped
                .values_problem()
                .unwrap()
                .starts_with("summary has the key sta")
        );

        let ui = UiState::new(Rev(2), "sel4", vec!["inst8".into()], vec!["path1".into()]);
        assert_eq!(
            serde_json::to_string(&ui).unwrap(),
            r#"{"schema":"rusteda.ui_state.v1","revision":"rev2","selection_handle":"sel4","selected_instances":["inst8"],"selected_paths":["path1"]}"#
        );
    }

    // WS-REQ-0044
    #[test]
    fn deltas() {
        let base = JsonMap::from([
            ("cells".into(), json!(10)),
            ("area_um2".into(), json!(1.5)),
            ("name".into(), json!("x")),
            ("only_base".into(), json!(1)),
            ("big".into(), json!(u64::MAX)),
        ]);
        let new = JsonMap::from([
            ("cells".into(), json!(7)),
            ("area_um2".into(), json!(2)),
            ("name".into(), json!("y")),
            ("only_new".into(), json!(1)),
            ("big".into(), json!(u64::MAX)),
        ]);
        let d = metric_deltas(&new, &base);
        assert_eq!(
            d,
            JsonMap::from([
                ("cells".into(), json!(-3)),
                ("area_um2".into(), json!(0.5)),
                ("big".into(), json!(0.0)),
            ])
        );
    }

    // WS-REQ-0013, WS-REQ-0040
    #[test]
    fn libraries_hash_and_corners() {
        let lib = |name: &str, corner: &str, h: u64| LibraryEntry {
            name: name.into(),
            corner: corner.into(),
            hash: Hash64(h),
            source: format!("{name}.lib"),
            compiled: String::new(),
        };
        let libs = vec![lib("a", "ss", 1), lib("b", "tt", 2), lib("c", "ss", 3)];
        assert_eq!(LibraryEntry::corners(&libs), ["ss", "tt"]);
        let expected = format!(
            "a\0ss\0{}\nb\0tt\0{}\nc\0ss\0{}\n",
            Hash64(1).hex(),
            Hash64(2).hex(),
            Hash64(3).hex()
        );
        assert_eq!(
            LibraryEntry::list_hash(&libs),
            Hash64::of(expected.as_bytes())
        );
        let mut moved = libs.clone();
        moved[0].source = "elsewhere/a.lib".into();
        assert_eq!(
            LibraryEntry::list_hash(&moved),
            LibraryEntry::list_hash(&libs)
        );
        moved.swap(0, 1);
        assert_ne!(
            LibraryEntry::list_hash(&moved),
            LibraryEntry::list_hash(&libs)
        );
        assert_eq!(LibraryEntry::list_hash(&[]), Hash64::of(b""));
    }

    // WS-REQ-0014 (review CW-4): the per-corner pseudo-table hashes the records of its corner
    // only, so another corner's libraries do not change it.
    #[test]
    fn corner_pseudo_tables() {
        let lib = |name: &str, corner: &str, h: u64| LibraryEntry {
            name: name.into(),
            corner: corner.into(),
            hash: Hash64(h),
            source: String::new(),
            compiled: String::new(),
        };
        let libs = vec![lib("a", "ss", 1), lib("b", "tt", 2), lib("c", "ss", 3)];
        let ss = format!("a\0ss\0{}\nc\0ss\0{}\n", Hash64(1).hex(), Hash64(3).hex());
        assert_eq!(
            LibraryEntry::corner_hash(&libs, "ss"),
            Some(Hash64::of(ss.as_bytes()))
        );
        assert_eq!(LibraryEntry::corner_hash(&libs, "ff"), None);
        let mut more = libs.clone();
        more.push(lib("d", "ff", 4));
        assert_eq!(
            LibraryEntry::corner_hash(&more, "ss"),
            LibraryEntry::corner_hash(&libs, "ss")
        );
        assert_ne!(
            LibraryEntry::list_hash(&more),
            LibraryEntry::list_hash(&libs)
        );
        assert_eq!(
            libraries_table_hash("libraries.ss", &libs),
            Some(LibraryEntry::corner_hash(&libs, "ss"))
        );
        assert_eq!(libraries_table_hash("libraries.ff", &libs), Some(None));
        assert_eq!(
            libraries_table_hash("libraries", &libs),
            Some(Some(LibraryEntry::list_hash(&libs)))
        );
        assert_eq!(libraries_table_hash("netlist", &libs), None);
        for yes in ["libraries", "libraries.ss", "libraries.tt_025C-1v80"] {
            assert!(is_libraries_table(yes), "{yes}");
        }
        for no in [
            "librarie",
            "libraries.",
            "libraries.a b",
            "libraries.x.y",
            "netlist",
        ] {
            assert!(!is_libraries_table(no), "{no}");
        }
    }

    // WS-REQ-0043, WS-REQ-0047 (review CW-6): every handle of a selection is shell-safe and
    // matches its handle prefix (`sel<N>`, `inst<N>`, `path<N>`).
    #[test]
    fn ui_state_handles() {
        let ok = UiState::new(
            Rev(2),
            "sel4",
            vec!["inst841".into(), "inst8".into()],
            vec!["path1".into()],
        );
        ok.validate().unwrap();
        let env_src = SourceEntry::env_value("constraints", "PVT", "tt").unwrap();
        assert_eq!(env_src.env(), Some(("PVT", "tt")));
        assert!(SourceEntry::env_value("constraints", "", "tt").is_err());
        assert!(SourceEntry::env_value("constraints", "A=B", "tt").is_err());
        for (selection, instances, paths) in [
            ("sel4", vec!["inst 1"], vec!["path1"]),
            ("sel4", vec!["inst_841"], vec!["path1"]),
            ("sel4", vec!["path1"], vec!["path1"]),
            ("sel4", vec!["$HOME"], vec![]),
            ("sel4", vec![], vec!["#p1"]),
            ("sel4", vec![], vec!["inst1"]),
            ("sel4", vec![], vec!["path;rm"]),
            ("sel4", vec![], vec![""]),
            ("path4", vec![], vec![]),
            ("sel 4", vec![], vec![]),
        ] {
            let state = UiState::new(
                Rev(2),
                selection,
                instances.into_iter().map(String::from).collect(),
                paths.into_iter().map(String::from).collect(),
            );
            assert!(state.validate().is_err(), "{state:?}");
        }
    }
}
