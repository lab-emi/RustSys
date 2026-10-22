// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Workspace report query command (`rustsys -w <WS> report [--rev <REV>] [--section <NAME>...]`).
//!
//! Covers: CLI-REQ-0203.

use std::fmt::Write as _;
use std::process::ExitCode;

use rusteda_workspace::text::next_line;
use rusteda_workspace::{Corruption, Envelope, Error, JsonObject, Workspace};
use rustsys::view::Report as SynthReport;
use rustsys_check::CheckReport;

use crate::args::{FormatArg, ReportArgs};
use crate::proto::fields::fields;
use crate::proto::filter::Filter;
use crate::proto::format::exact;
use crate::proto::load::Report;
use crate::proto::order::{SortSpec, build};
use crate::proto::table::{
    CellRows, MismatchRows, PhaseRows, TableModel, UnusedRows, json_rows, keys, text as table_text,
};
use crate::proto::{Ctx, Product, text as render};
use crate::ws::{OutputOutcome, finish_error, finish_ok};

fn valid_sections(p: Product) -> (&'static [&'static str], &'static [&'static str]) {
    match p {
        Product::Synth => (
            &["summary", "cells", "unused", "phases", "command"],
            &["summary"],
        ),
        Product::CheckSynth => (
            &["mismatches", "summary", "command"],
            &["mismatches", "summary"],
        ),
    }
}

fn table_model(r: &Report, name: &str) -> Option<Box<dyn TableModel>> {
    Some(match (r, name) {
        (Report::Synth(s), "cells") => Box::new(CellRows::new(s)),
        (Report::Synth(s), "unused") => Box::new(UnusedRows::new(s)),
        (Report::Synth(s), "phases") => Box::new(PhaseRows(s.phase_s.clone())),
        (Report::CheckSynth(s), "mismatches") => Box::new(MismatchRows::new(s)),
        _ => return None,
    })
}

pub(crate) fn run_ws_report(
    ws: &Workspace,
    ws_arg: &str,
    args: &ReportArgs,
    cmd_name: &str,
    format: FormatArg,
    quiet: bool,
) -> ExitCode {
    match execute_ws_report(ws, ws_arg, args, cmd_name) {
        Ok(outcome) => finish_ok(outcome, format, quiet),
        Err(e) => finish_error(cmd_name, Some(ws_arg), None, e, Vec::new(), format, quiet),
    }
}

fn execute_ws_report(
    ws: &Workspace,
    ws_arg: &str,
    args: &ReportArgs,
    cmd_name: &str,
) -> Result<OutputOutcome, Error> {
    let rev = ws.resolve_query(&args.rev)?;
    let revision = ws.revision(rev)?;

    let table_name = match args.table.as_deref() {
        Some("synth") => "synth",
        Some("check") => "check",
        Some(other) => {
            return Err(Error::usage(format!(
                "unknown --table '{other}'; expected 'synth' or 'check'"
            )));
        }
        None if args.sections.iter().any(|s| s == "mismatches") && revision.has_table("check") => {
            "check"
        }
        None if revision.has_table("synth") => "synth",
        None if revision.has_table("check") => "check",
        None => {
            return Err(Error::MissingTable {
                rev,
                table: "synth".to_string(),
            });
        }
    };

    revision.verify_table(table_name)?;
    let bytes = revision.read_table(table_name)?;
    let table_path = revision.table_path(table_name)?;

    let report = match table_name {
        "synth" => {
            let sr: SynthReport = serde_json::from_slice(&bytes).map_err(|e| {
                Error::corrupt(
                    &table_path,
                    Corruption::Json {
                        reason: format!("invalid synth.json: {e}"),
                    },
                )
            })?;
            Report::Synth(sr)
        }
        "check" => {
            let cr: CheckReport = serde_json::from_slice(&bytes).map_err(|e| {
                Error::corrupt(
                    &table_path,
                    Corruption::Json {
                        reason: format!("invalid check.json: {e}"),
                    },
                )
            })?;
            Report::CheckSynth(cr)
        }
        _ => unreachable!(),
    };

    let product = report.product();
    let (valid, default) = valid_sections(product);
    for s in &args.sections {
        if !valid.contains(&s.as_str()) {
            return Err(Error::usage(format!(
                "no section '{s}' in a {} report; sections: {}",
                product.name(),
                valid.join(", ")
            )));
        }
    }

    let explicit = !args.sections.is_empty();
    let names: Vec<&str> = if explicit {
        args.sections.iter().map(String::as_str).collect()
    } else {
        default.to_vec()
    };

    let mut out_text = String::new();
    let mut sections_json = serde_json::Map::new();
    let mut primary_rows = Vec::<serde_json::Value>::new();

    for name in &names {
        match *name {
            "summary" => {
                let fs = fields(&report);
                let mut o = serde_json::Map::new();
                for f in &fs {
                    let v = match f.value {
                        Some(x) => serde_json::json!(x),
                        None => serde_json::Value::String(f.text.clone()),
                    };
                    o.insert(f.label.into(), v);
                }
                sections_json.insert("summary".into(), serde_json::Value::Object(o));

                if !args.sources && !args.exact {
                    out_text.push_str(&match &report {
                        Report::Synth(s) => render::synth_summary(s),
                        Report::CheckSynth(s) => render::check_synth_verdict(s),
                    });
                } else {
                    let w = fs.iter().map(|f| f.label.len()).max().unwrap_or(0);
                    for f in fs {
                        let v = match (args.exact, f.value) {
                            (true, Some(x)) => exact(x),
                            _ => crate::proto::format::name(&f.text).into_owned(),
                        };
                        let src = if args.sources {
                            format!("  [{}]", f.source)
                        } else {
                            String::new()
                        };
                        let _ = writeln!(out_text, "{:<w$}  {v}{}{src}", f.label, f.unit.suffix());
                    }
                }
            }
            "command" => {
                let cmd_str = revision.meta().argv.join(" ");
                sections_json.insert("command".into(), serde_json::json!(cmd_str));
                let _ = writeln!(out_text, "{}", crate::proto::format::name(&cmd_str));
            }
            t => {
                if let Some(m) = table_model(&report, t) {
                    let f = Filter::parse(args.filter.as_deref().unwrap_or(""), m.as_ref())
                        .map_err(Error::usage)?;
                    let sort = match &args.sort {
                        Some(k) => Some(SortSpec {
                            col: m.column(k).ok_or_else(|| {
                                Error::usage(format!(
                                    "unknown key '{k}'; keys: {}",
                                    keys(m.as_ref())
                                ))
                            })?,
                            descending: args.reverse,
                        }),
                        None if args.reverse => Some(SortSpec {
                            col: 0,
                            descending: true,
                        }),
                        None => None,
                    };
                    let all = build(m.as_ref(), &f, sort, Ctx::NONE).unwrap_or_default();
                    let n = all.len();
                    let limit = args.limit.unwrap_or(20);
                    let shown = if limit == 0 { n } else { n.min(limit) };
                    let order = &all[..shown];
                    let more = n - shown;

                    out_text.push_str(&table_text(m.as_ref(), order));
                    if more > 0 {
                        let _ = writeln!(out_text, "… {more} more rows (--limit 0 prints all)");
                    }
                    let prefix = match t {
                        "cells" => "cell",
                        "mismatches" => "mm",
                        "phases" => "phase",
                        "unused" => "unused",
                        other => other,
                    };
                    let mut rows_val = json_rows(m.as_ref(), order);
                    if let serde_json::Value::Array(arr) = &mut rows_val {
                        for (idx, item) in arr.iter_mut().enumerate() {
                            if let serde_json::Value::Object(map) = item {
                                map.insert(
                                    "handle".to_string(),
                                    serde_json::Value::String(format!("{prefix}{}", idx + 1)),
                                );
                            }
                        }
                        if primary_rows.is_empty() {
                            primary_rows = arr.clone();
                        }
                    }
                    sections_json.insert(t.into(), rows_val);
                }
            }
        }
    }

    if !explicit && !args.sources && !args.exact {
        if let Report::CheckSynth(r) = &report {
            out_text = render::check_synth(r);
        }
    }

    let mut env = Envelope::new(cmd_name);
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = revision.meta().parent;
    env.rev = Some(rev);

    let corners = revision.corners();
    let mut scope = JsonObject::new();
    scope.insert("table", &table_name)?;
    scope.insert("corners", &corners)?;
    env.set_scope(scope)?;

    let mut metrics = JsonObject::new();
    if let Some(vals) = revision.meta().summary.get(table_name) {
        for (k, v) in vals {
            metrics.insert(k, v)?;
        }
    }
    env.set_metrics(metrics)?;

    env.set("table", &table_name)?;
    env.set("rows", &primary_rows)?;
    env.set("sections", &sections_json)?;
    match &report {
        Report::Synth(sr) => env.set("report", sr)?,
        Report::CheckSynth(cr) => env.set("report", cr)?,
    }

    let rev_s = rev.to_string();
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &rev_s]);
    if table_name == "synth" && revision.has_table("check") {
        env.push_next_action(&[
            "rustsys", "-w", ws_arg, "report", "--rev", &rev_s, "--table", "check",
        ]);
    }

    let mut text_lines: Vec<String> = out_text.lines().map(String::from).collect();
    for next in &env.next_actions {
        text_lines.push(next_line(next));
    }

    Ok(OutputOutcome {
        envelope: env,
        text_lines,
        exit_code: 0,
    })
}
