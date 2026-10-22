// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Query commands: `rusteda log`, `rusteda show`, and `rusteda diff` (RCLI-REQ-0015 to
//! RCLI-REQ-0017).

use std::collections::{BTreeMap, BTreeSet};

use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    Envelope, Error, Hash64, JsonObject, LibraryEntry, Rev, RevisionMeta, TableValues, Workspace,
};
use serde::Serialize;
use serde_json::Value;

use crate::OutputOutcome;

/// One entry in `rusteda log` output (RCLI-REQ-0015).
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub rev: Rev,
    pub parent: Option<Rev>,
    pub command: String,
    pub message: Option<String>,
    pub tables: Vec<String>,
    pub created_unix_ms: u64,
}

/// Runs `rusteda log -w <ws> [--limit <n>]` (RCLI-REQ-0015).
pub(crate) fn run_log(
    ws: &Workspace,
    ws_arg: &str,
    limit: Option<usize>,
) -> Result<OutputOutcome, Error> {
    let head = ws.head()?;
    let mut revs = ws.revisions()?;
    revs.reverse();
    if let Some(max) = limit {
        revs.truncate(max);
    }

    let mut entries = Vec::with_capacity(revs.len());
    for rev in revs {
        let r = ws.revision(rev)?;
        let meta = r.meta();
        entries.push(LogEntry {
            rev: meta.rev,
            parent: meta.parent,
            command: meta.command.clone(),
            message: meta.message.clone(),
            tables: meta.tables.keys().cloned().collect(),
            created_unix_ms: meta.created_unix_ms,
        });
    }

    let mut env = Envelope::new("rusteda log");
    env.workspace = Some(ws_arg.to_string());
    env.rev = Some(head);
    env.set("revisions", &entries)?;
    let head_s = head.to_string();
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &head_s]);

    let mut lines = Vec::new();
    for entry in &entries {
        let parent_s = entry
            .parent
            .map_or_else(|| "none".to_string(), |p| p.to_string());
        let tables_s = if entry.tables.is_empty() {
            "-".to_string()
        } else {
            entry.tables.join(",")
        };
        let msg_s = entry.message.as_deref().unwrap_or("");
        if msg_s.is_empty() {
            lines.push(format!(
                "{}  parent={}  command={}  tables={}  created={}",
                entry.rev, parent_s, entry.command, tables_s, entry.created_unix_ms
            ));
        } else {
            lines.push(format!(
                "{}  parent={}  command={}  tables={}  created={}  message={}",
                entry.rev, parent_s, entry.command, tables_s, entry.created_unix_ms, msg_s
            ));
        }
    }
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok(OutputOutcome {
        envelope: env,
        text_lines: lines,
    })
}

/// Extracts effective timing and equivalence statuses from `meta`, rendering a missing `sta`
/// status as `"not_analyzed"` and a missing `check` status as `"unchecked"` (CA-STATUS,
/// RCLI-REQ-0016).
fn effective_status(meta: &RevisionMeta) -> (String, String) {
    let timing = meta
        .status
        .get("sta")
        .and_then(|m| m.get("timing"))
        .and_then(Value::as_str)
        .unwrap_or("not_analyzed")
        .to_string();
    let equivalence = meta
        .status
        .get("check")
        .and_then(|m| m.get("equivalence"))
        .and_then(Value::as_str)
        .or_else(|| {
            meta.status
                .get("synth")
                .and_then(|m| m.get("equivalence"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            meta.status
                .get("netlist")
                .and_then(|m| m.get("equivalence"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unchecked")
        .to_string();
    (timing, equivalence)
}

/// Runs `rusteda show -w <ws> [--rev <rev>]` (RCLI-REQ-0016).
pub(crate) fn run_show(
    ws: &Workspace,
    ws_arg: &str,
    rev_spec: &str,
) -> Result<OutputOutcome, Error> {
    let rev = ws.resolve_query(rev_spec)?;
    let revision = ws.revision(rev)?;
    let meta = revision.meta();
    let (timing, equivalence) = effective_status(meta);
    let corners = LibraryEntry::corners(&meta.libraries);

    let mut env = Envelope::new("rusteda show");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = meta.parent;
    env.rev = Some(rev);

    let mut scope = JsonObject::new();
    scope.insert("corners", &corners)?;
    scope.insert("timing", &timing)?;
    scope.insert("equivalence", &equivalence)?;
    env.set_scope(scope)?;

    let mut metrics = JsonObject::new();
    for table_vals in meta.summary.values() {
        for (k, v) in table_vals {
            metrics.insert(k, v)?;
        }
    }
    env.set_metrics(metrics)?;

    let mut status_obj = JsonObject::new();
    status_obj.insert("timing", &timing)?;
    status_obj.insert("equivalence", &equivalence)?;
    for (tbl, vals) in &meta.status {
        status_obj.insert(tbl, vals)?;
    }

    env.set("parent", &meta.parent)?;
    env.set("created_unix_ms", &meta.created_unix_ms)?;
    env.set("tool", &meta.tool)?;
    env.set("tool_version", &meta.tool_version)?;
    env.set("revision_command", &meta.command)?;
    env.set("argv", &meta.argv)?;
    env.set("message", &meta.message)?;
    env.set("tables", &meta.tables)?;
    env.set("libraries", &meta.libraries)?;
    env.set("sources", &meta.sources)?;
    env.set("dropped", &meta.dropped)?;
    env.set("summary", &meta.summary)?;
    env.set_raw("status", status_obj.into())?;

    let rev_s = rev.to_string();
    env.push_next_action(&["rusteda", "log", "-w", ws_arg]);
    if let Some(parent) = meta.parent {
        let parent_s = parent.to_string();
        env.push_next_action(&[
            "rusteda", "diff", "-w", ws_arg, "--from", &parent_s, "--to", &rev_s,
        ]);
    }

    let mut lines = Vec::new();
    let parent_s = meta
        .parent
        .map_or_else(|| "none".to_string(), |p| p.to_string());
    lines.push(format!("{rev}  {}  (parent {parent_s})", meta.command));
    if let Some(msg) = &meta.message {
        lines.push(format!("  message: {msg}"));
    }
    lines.push(format!(
        "  status: timing={timing} equivalence={equivalence}"
    ));
    for (name, tbl) in &meta.tables {
        lines.push(format!("  table {name}: {} ({})", tbl.path, tbl.hash));
    }
    for lib in &meta.libraries {
        lines.push(format!(
            "  library {} ({}): {}",
            lib.name, lib.corner, lib.hash
        ));
    }
    for src in &meta.sources {
        lines.push(format!(
            "  source {} ({}): {}",
            src.path, src.table, src.hash
        ));
    }
    for drop in &meta.dropped {
        lines.push(format!("  dropped {}: {}", drop.table, drop.reason));
    }
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok(OutputOutcome {
        envelope: env,
        text_lines: lines,
    })
}

/// Hash comparison of a single table between two revisions (RCLI-REQ-0017).
#[derive(Debug, Clone, Serialize)]
pub struct TableDiffEntry {
    pub status: &'static str,
    pub from_hash: Option<Hash64>,
    pub to_hash: Option<Hash64>,
}

/// Hash change of a library present in both revisions under the same `(name, corner)`.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryHashChange {
    pub name: String,
    pub corner: String,
    pub from_hash: Hash64,
    pub to_hash: Hash64,
}

/// Library changes between two revisions (RCLI-REQ-0017).
#[derive(Debug, Clone, Serialize)]
pub struct LibraryDiff {
    pub added: Vec<LibraryEntry>,
    pub removed: Vec<LibraryEntry>,
    pub changed: Vec<LibraryHashChange>,
}

/// Runs `rusteda diff -w <ws> --from <from> --to <to>` (RCLI-REQ-0017).
pub(crate) fn run_diff(
    ws: &Workspace,
    ws_arg: &str,
    from_spec: &str,
    to_spec: &str,
) -> Result<OutputOutcome, Error> {
    let from_rev = ws.resolve_query(from_spec)?;
    let to_rev = ws.resolve_query(to_spec)?;
    let from = ws.revision(from_rev)?;
    let to = ws.revision(to_rev)?;
    let from_meta = from.meta();
    let to_meta = to.meta();

    let mut all_tables: BTreeSet<String> = BTreeSet::new();
    all_tables.extend(from_meta.tables.keys().cloned());
    all_tables.extend(to_meta.tables.keys().cloned());

    let mut added_tables = Vec::new();
    let mut removed_tables = Vec::new();
    let mut changed_tables = Vec::new();
    let mut unchanged_tables = Vec::new();
    let mut table_entries: BTreeMap<String, TableDiffEntry> = BTreeMap::new();

    for name in all_tables {
        match (from_meta.tables.get(&name), to_meta.tables.get(&name)) {
            (None, Some(t)) => {
                added_tables.push(name.clone());
                table_entries.insert(
                    name,
                    TableDiffEntry {
                        status: "added",
                        from_hash: None,
                        to_hash: Some(t.hash),
                    },
                );
            }
            (Some(f), None) => {
                removed_tables.push(name.clone());
                table_entries.insert(
                    name,
                    TableDiffEntry {
                        status: "removed",
                        from_hash: Some(f.hash),
                        to_hash: None,
                    },
                );
            }
            (Some(f), Some(t)) if f.hash != t.hash => {
                changed_tables.push(name.clone());
                table_entries.insert(
                    name,
                    TableDiffEntry {
                        status: "changed",
                        from_hash: Some(f.hash),
                        to_hash: Some(t.hash),
                    },
                );
            }
            (Some(f), Some(t)) => {
                unchanged_tables.push(name.clone());
                table_entries.insert(
                    name,
                    TableDiffEntry {
                        status: "unchanged",
                        from_hash: Some(f.hash),
                        to_hash: Some(t.hash),
                    },
                );
            }
            (None, None) => {}
        }
    }

    // Library diff by (name, corner).
    let mut added_libs = Vec::new();
    let mut removed_libs = Vec::new();
    let mut changed_libs = Vec::new();
    for to_lib in &to_meta.libraries {
        match from_meta
            .libraries
            .iter()
            .find(|l| l.name == to_lib.name && l.corner == to_lib.corner)
        {
            None => added_libs.push(to_lib.clone()),
            Some(from_lib) if from_lib.hash != to_lib.hash => {
                changed_libs.push(LibraryHashChange {
                    name: to_lib.name.clone(),
                    corner: to_lib.corner.clone(),
                    from_hash: from_lib.hash,
                    to_hash: to_lib.hash,
                });
            }
            Some(_) => {}
        }
    }
    for from_lib in &from_meta.libraries {
        if !to_meta
            .libraries
            .iter()
            .any(|l| l.name == from_lib.name && l.corner == from_lib.corner)
        {
            removed_libs.push(from_lib.clone());
        }
    }
    let lib_diff = LibraryDiff {
        added: added_libs,
        removed: removed_libs,
        changed: changed_libs,
    };

    // Numeric summary metric deltas for tables present in both revisions.
    let mut metric_deltas: TableValues = TableValues::new();
    let mut flat_deltas = JsonObject::new();
    for (tbl, to_vals) in &to_meta.summary {
        if let Some(from_vals) = from_meta.summary.get(tbl) {
            for (k, to_v) in to_vals {
                if let Some(from_v) = from_vals.get(k) {
                    if let (Some(a), Some(b)) = (from_v.as_i64(), to_v.as_i64()) {
                        let d = b.saturating_sub(a);
                        metric_deltas
                            .entry(tbl.clone())
                            .or_default()
                            .insert(k.clone(), Value::from(d));
                        flat_deltas.insert(k, &d)?;
                    } else if let (Some(a), Some(b)) = (from_v.as_f64(), to_v.as_f64()) {
                        let d = b - a;
                        if d.is_finite() {
                            metric_deltas
                                .entry(tbl.clone())
                                .or_default()
                                .insert(k.clone(), Value::from(d));
                            flat_deltas.insert(k, &d)?;
                        }
                    }
                }
            }
        }
    }

    let (from_timing, from_eq) = effective_status(from_meta);
    let (to_timing, to_eq) = effective_status(to_meta);
    let mut status_obj = JsonObject::new();
    status_obj.insert("timing", &to_timing)?;
    status_obj.insert("equivalence", &to_eq)?;
    status_obj.insert("from_timing", &from_timing)?;
    status_obj.insert("from_equivalence", &from_eq)?;

    let mut env = Envelope::new("rusteda diff");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(from_rev);
    env.rev = Some(to_rev);
    env.set_metrics(flat_deltas)?;
    env.set("from_rev", &from_rev)?;
    env.set("to_rev", &to_rev)?;
    env.set("added_tables", &added_tables)?;
    env.set("removed_tables", &removed_tables)?;
    env.set("changed_tables", &changed_tables)?;
    env.set("unchanged_tables", &unchanged_tables)?;
    env.set("tables", &table_entries)?;
    env.set("libraries", &lib_diff)?;
    env.set("metric_deltas", &metric_deltas)?;
    env.set_raw("status", status_obj.into())?;

    let to_s = to_rev.to_string();
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &to_s]);

    let mut lines = Vec::new();
    lines.push(format!("diff {from_rev} -> {to_rev}"));
    for (name, entry) in &table_entries {
        lines.push(format!("  table {name}: {}", entry.status));
    }
    for lib in &lib_diff.added {
        lines.push(format!(
            "  library +{} ({}): {}",
            lib.name, lib.corner, lib.hash
        ));
    }
    for lib in &lib_diff.removed {
        lines.push(format!(
            "  library -{} ({}): {}",
            lib.name, lib.corner, lib.hash
        ));
    }
    for lib in &lib_diff.changed {
        lines.push(format!(
            "  library ~{} ({}): {} -> {}",
            lib.name, lib.corner, lib.from_hash, lib.to_hash
        ));
    }
    lines.push(format!(
        "  status: timing={from_timing}->{to_timing} equivalence={from_eq}->{to_eq}"
    ));
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok(OutputOutcome {
        envelope: env,
        text_lines: lines,
    })
}
