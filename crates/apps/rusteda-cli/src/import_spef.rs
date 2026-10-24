// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Atomic, bounded detailed-parasitic import (RCLI-REQ-0070 to RCLI-REQ-0072, RCLI-REQ-0075).
//!
//! One import binds one or more SPEF files to named corners (`--spef FILE --spef-corner NAME`,
//! repeatable). The `parasitics` table is a `PARASIT` v2 container that holds the decoded source
//! of every corner: the corners of the base revision whose table still describes the netlist of
//! the new revision are kept, and a corner imported again is replaced.
use rusteda_formats::spef::{self, CornerSetMetadata, CornerSource, Limits, Metadata};
use rusteda_workspace::{
    DiagCode, Diagnostic, Error, Hash64, JsonMap, RbinWriter, Revision, Severity, Txn,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The table name and file of the parasitics.
pub(crate) const TABLE: &str = "parasitics";
const TABLE_PATH: &str = "parasitics.rbin";
/// Largest metadata section of a container: 4 KiB per corner, for at most 64 corners.
const MAX_METADATA_BYTES: usize = 64 * 4096;
/// The decoded sources of all corners of a table, which an import holds at once, total at most
/// this many per-file byte budgets (RCLI-REQ-0075).
const MAX_TABLE_SOURCES: usize = 8;

/// One corner of the new table: its metadata and decoded source.
pub(crate) struct CornerEntry {
    pub(crate) source: CornerSource,
    text: Vec<u8>,
}

fn check_corner(corner: &str) -> Result<(), Error> {
    if corner.is_empty()
        || corner.len() > 128
        || !corner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::usage(
            "--spef-corner must be a shell-safe corner handle",
        ));
    }
    Ok(())
}

/// Reads the raw bytes of one SPEF file, bounded by `limits` (RCLI-REQ-0073).
fn read_source(path: &Path, limits: Limits) -> Result<Vec<u8>, Error> {
    let meta = fs::symlink_metadata(path)
        .map_err(|e| Error::input_rejected(format!("{}: {e}", path.display())))?;
    if !meta.file_type().is_file() {
        return Err(Error::input_rejected(
            "SPEF source must be a regular file, not a symlink/FIFO/device",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|e| Error::input_rejected(format!("{}: {e}", path.display())))?;
    if !file
        .metadata()
        .map_err(|e| Error::input_rejected(e.to_string()))?
        .is_file()
    {
        return Err(Error::input_rejected("SPEF source is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limits.max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::input_rejected(format!("{}: {e}", path.display())))?;
    if bytes.len() > limits.max_bytes {
        return Err(Error::input_rejected(
            "T0072: SPEF source byte budget exceeded",
        ));
    }
    Ok(bytes)
}

/// The corners of a `parasitics` table of `rev`, read back for carry-forward (RCLI-REQ-0075).
/// A version 1 table holds one corner without a recorded source path, which is taken from the
/// revision's sources of the table.
pub(crate) fn read_corners(rev: &Revision) -> Result<Vec<CornerEntry>, Error> {
    let rbin = rev.open_rbin(TABLE, "PARASIT", 1..=2)?;
    rbin.verify_hash()?;
    let bad = |e: String| Error::input_rejected(format!("invalid PARASIT table: {e}"));
    if rbin.format_version() == 1 {
        let metadata = rbin.section_bytes(2)?;
        if metadata.len() > 4096 {
            return Err(bad("metadata budget exceeded".into()));
        }
        let meta: Metadata = serde_json::from_slice(metadata).map_err(|e| bad(e.to_string()))?;
        let (source_path, source_hash) = rev
            .meta()
            .sources
            .iter()
            .find(|s| s.table == TABLE)
            .map_or_else(
                || (String::new(), String::new()),
                |s| (s.path.clone(), s.hash.to_string()),
            );
        return Ok(vec![CornerEntry {
            source: CornerSource {
                corner: meta.corner,
                limits: meta.limits,
                source_path,
                source_hash,
            },
            text: rbin.section_bytes(1)?.to_vec(),
        }]);
    }
    let metadata = rbin.section_bytes(1)?;
    if metadata.len() > MAX_METADATA_BYTES {
        return Err(bad("metadata budget exceeded".into()));
    }
    let set: CornerSetMetadata =
        serde_json::from_slice(metadata).map_err(|e| bad(e.to_string()))?;
    let mut out = Vec::with_capacity(set.corners.len());
    for (i, source) in set.corners.into_iter().enumerate() {
        let text = rbin.section_bytes(i as u32 + 2)?.to_vec();
        out.push(CornerEntry { source, text });
    }
    Ok(out)
}

/// Imports the SPEF files `inputs`, `(path, corner)` in command-line order, into the
/// `parasitics` table of `txn` (RCLI-REQ-0070, RCLI-REQ-0075).
pub(crate) fn import_spefs(
    txn: &mut Txn,
    inputs: &[(PathBuf, String)],
    limits: Limits,
    metrics: &mut JsonMap,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    limits
        .validate()
        .map_err(|e| Error::input_rejected(e.to_string()))?;
    if inputs.len() > 64 {
        return Err(Error::usage("at most 64 --spef corners per import"));
    }
    for (i, (_, corner)) in inputs.iter().enumerate() {
        check_corner(corner)?;
        if inputs[..i].iter().any(|(_, c)| c == corner) {
            return Err(Error::usage(format!(
                "--spef-corner {corner} is given more than once"
            )));
        }
        if !txn.libraries().iter().any(|l| l.corner == *corner) {
            return Err(Error::input_rejected(format!(
                "SPEF corner {corner} has no imported library"
            )));
        }
    }

    // The corners of the base table that still describe the netlist of the new revision.
    let mut corners: BTreeMap<String, CornerEntry> = BTreeMap::new();
    let base = txn.base();
    if base.has_table(TABLE) {
        let base_netlist = base
            .open_rbin(TABLE, "PARASIT", 1..=2)?
            .upstream_hash("netlist");
        if base_netlist.is_some() && base_netlist == txn.table_hash("netlist") {
            for entry in read_corners(base)? {
                corners.insert(entry.source.corner.clone(), entry);
            }
        }
    }
    let kept: Vec<String> = corners
        .keys()
        .filter(|c| !inputs.iter().any(|(_, n)| n == *c))
        .cloned()
        .collect();
    // The decoded sources held at once: the kept corners and every imported one so far
    // (RCLI-REQ-0075).
    let table_budget = limits.max_bytes.saturating_mul(MAX_TABLE_SOURCES);
    let mut held: usize = corners
        .iter()
        .filter(|(c, _)| kept.contains(c))
        .map(|(_, e)| e.text.len())
        .sum();
    let mut hold = |n: usize| -> Result<(), Error> {
        held = held.saturating_add(n);
        if held > table_budget {
            return Err(Error::input_rejected(format!(
                "T0072: the decoded SPEF sources of the parasitics table exceed {table_budget} \
                 bytes ({MAX_TABLE_SOURCES} times the per-file byte budget)"
            )));
        }
        Ok(())
    };
    hold(0)?;

    let mut per_corner = serde_json::Map::new();
    let mut coupling_records = 0usize;
    for (path, corner) in inputs {
        let bytes = read_source(path, limits)?;
        let mut reject = |e: spef::Error| {
            diags.push(Diagnostic::from_parts(
                Severity::Error,
                DiagCode::of(e.code),
                Some((path.display().to_string(), e.line.max(1) as u32, 1)),
                e.message.clone(),
                None,
            ));
            Error::input_rejected(e.to_string())
        };
        let decoded = spef::decode_source(&bytes, limits).map_err(&mut reject)?;
        hold(decoded.text.len())?;
        let parsed = spef::read(&decoded.text, limits).map_err(&mut reject)?;
        coupling_records += parsed.coupling_records;
        per_corner.insert(
            corner.clone(),
            json!({
                "nets": parsed.nets.len(),
                "nodes": parsed.node_count,
                "elements": parsed.element_count,
                "compression": decoded.encoding.as_str(),
                "source_bytes": bytes.len(),
                "decoded_bytes": decoded.text.len(),
            }),
        );
        if inputs.len() == 1 {
            metrics.insert("spef_nets".into(), json!(parsed.nets.len()));
            metrics.insert("spef_nodes".into(), json!(parsed.node_count));
            metrics.insert("spef_elements".into(), json!(parsed.element_count));
            metrics.insert("spef_corner".into(), json!(corner));
            metrics.insert("spef_compression".into(), json!(decoded.encoding.as_str()));
            metrics.insert("spef_source_bytes".into(), json!(bytes.len()));
            metrics.insert("spef_decoded_bytes".into(), json!(decoded.text.len()));
        }
        corners.insert(
            corner.clone(),
            CornerEntry {
                source: CornerSource {
                    corner: corner.clone(),
                    limits,
                    source_path: path.display().to_string(),
                    source_hash: Hash64::of(&bytes).to_string(),
                },
                text: decoded.text.into_bytes(),
            },
        );
    }

    let set = CornerSetMetadata {
        corners: corners.values().map(|c| c.source.clone()).collect(),
    };
    let metadata = serde_json::to_vec(&set).map_err(|e| Error::internal(e.to_string()))?;
    if metadata.len() > MAX_METADATA_BYTES {
        return Err(Error::input_rejected(
            "T0072: parasitic metadata budget exceeded",
        ));
    }
    let mut writer = RbinWriter::new("PARASIT", 2)?;
    writer.section_bytes(1, 1, metadata)?;
    for (i, entry) in corners.values().enumerate() {
        writer.section_bytes(i as u32 + 2, 1, entry.text.as_slice())?;
    }
    let upstream = txn.upstream(&["netlist"])?;
    txn.add_rbin(TABLE, TABLE_PATH, &writer, upstream)?;
    for entry in corners.values() {
        let hash = Hash64::parse(&entry.source.source_hash);
        if let Some(hash) = hash
            && !entry.source.source_path.is_empty()
        {
            txn.add_source(TABLE, entry.source.source_path.clone(), hash)?;
        }
    }
    // A pre-extraction timing table cannot depend on a table that did not yet exist.
    for table in ["sta", "sta_check", "limits"] {
        if txn.table_hash(table).is_some() {
            txn.drop_table(table, "extracted parasitics imported")?;
        }
    }
    metrics.insert(
        "spef_corners".into(),
        json!(corners.keys().collect::<Vec<_>>()),
    );
    metrics.insert(
        "spef_imported".into(),
        serde_json::Value::Object(per_corner),
    );
    metrics.insert("spef_kept_corners".into(), json!(kept));
    if coupling_records > 0 {
        diags.push(Diagnostic::new(
            Severity::Warning,
            DiagCode::of("T0074"),
            format!(
                "{coupling_records} inter-net coupling records grounded at factor 1; crosstalk \
                 not analyzed"
            ),
        ));
    }
    Ok(())
}
