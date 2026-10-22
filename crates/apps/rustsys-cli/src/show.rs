// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `rustsys show` (CLI-REQ-0100 to CLI-REQ-0102).

use std::fmt::Write as _;
use std::process::ExitCode;

use crate::proto::fields::fields;
use crate::proto::filter::Filter;
use crate::proto::format::exact;
use crate::proto::load::{Loaded, Report, load};
use crate::proto::order::{SortSpec, build};
use crate::proto::record::resolve;
use crate::proto::table::{
    CellRows, MismatchRows, PhaseRows, TableModel, UnusedRows, json_rows, text, tsv,
};
use crate::proto::{Ctx, Diag, Product, text as render};

use crate::Globals;
use crate::args::{ShowArgs, ShowPrint};
use crate::out::stdout;

/// The sections of a report and its default sections (CLI-REQ-0100).
fn sections(p: Product) -> (&'static [&'static str], &'static [&'static str]) {
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

/// The table sections.
const TABLES: [&str; 4] = ["cells", "unused", "phases", "mismatches"];

/// A table section's model.
fn table(r: &Report, name: &str) -> Option<Box<dyn TableModel>> {
    Some(match (r, name) {
        (Report::Synth(s), "cells") => Box::new(CellRows::new(s)),
        (Report::Synth(s), "unused") => Box::new(UnusedRows::new(s)),
        (Report::Synth(s), "phases") => Box::new(PhaseRows(s.phase_s.clone())),
        (Report::CheckSynth(s), "mismatches") => Box::new(MismatchRows::new(s)),
        _ => return None,
    })
}

struct Ctxt<'a> {
    a: &'a ShowArgs,
    l: &'a Loaded,
    out: String,
    json: serde_json::Map<String, serde_json::Value>,
}

impl Ctxt<'_> {
    fn order(&self, m: &dyn TableModel) -> Result<(Vec<u32>, usize), Diag> {
        let f = Filter::parse(self.a.filter.as_deref().unwrap_or(""), m)
            .map_err(|e| Diag::error("A0022", e))?;
        let sort = match &self.a.sort {
            Some(k) => Some(SortSpec {
                col: m.column(k).ok_or_else(|| {
                    Diag::error(
                        "A0022",
                        format!("unknown key '{k}'; keys: {}", crate::proto::table::keys(m)),
                    )
                })?,
                descending: self.a.reverse,
            }),
            None if self.a.reverse => Some(SortSpec {
                col: 0,
                descending: true,
            }),
            None => None,
        };
        let all = build(m, &f, sort, Ctx::NONE).unwrap_or_default();
        let n = all.len();
        let limit = self.a.limit.unwrap_or(20);
        let shown = if limit == 0 { n } else { n.min(limit) };
        Ok((all[..shown].to_vec(), n - shown))
    }

    fn table_section(&mut self, name: &str, m: &dyn TableModel) -> Result<(), Diag> {
        let (order, more) = self.order(m)?;
        match self.a.print {
            ShowPrint::Text => {
                self.out.push_str(&text(m, &order));
                if more > 0 {
                    let _ = writeln!(self.out, "… {more} more rows (--limit 0 prints all)");
                }
            }
            ShowPrint::Tsv => self.out.push_str(&tsv(m, &order)),
            ShowPrint::Json => {
                self.json.insert(name.into(), json_rows(m, &order));
            }
        }
        Ok(())
    }

    fn summary(&mut self) {
        let r = &self.l.report;
        if !self.a.sources && !self.a.exact && self.a.print == ShowPrint::Text {
            self.out.push_str(&match r {
                Report::Synth(s) => render::synth_summary(s),
                Report::CheckSynth(s) => render::check_synth_verdict(s),
            });
            return;
        }
        let fs = fields(r);
        match self.a.print {
            ShowPrint::Json => {
                let mut o = serde_json::Map::new();
                for f in &fs {
                    let v = match f.value {
                        Some(x) => serde_json::json!(x),
                        None => serde_json::Value::String(f.text.clone()),
                    };
                    o.insert(f.label.into(), v);
                }
                self.json
                    .insert("summary".into(), serde_json::Value::Object(o));
            }
            _ => {
                let w = fs.iter().map(|f| f.label.len()).max().unwrap_or(0);
                for f in fs {
                    let v = match (self.a.exact, f.value) {
                        (true, Some(x)) => exact(x),
                        _ => crate::proto::format::name(&f.text).into_owned(),
                    };
                    let src = if self.a.sources {
                        format!("  [{}]", f.source)
                    } else {
                        String::new()
                    };
                    let _ = writeln!(self.out, "{:<w$}  {v}{}{src}", f.label, f.unit.suffix());
                }
            }
        }
    }
}

/// `rustsys show` (CLI-REQ-0100).
pub fn show(g: &Globals, a: &ShowArgs) -> ExitCode {
    let err = g.err("show");
    let reference = a.run.as_deref().unwrap_or("@");
    let loaded = match resolve(reference, g.runs_root.as_deref()).and_then(|l| load(&l)) {
        Ok(l) => l,
        Err(d) => {
            err.diag(&d);
            return ExitCode::from(1);
        }
    };
    let product = loaded.report.product();
    let (valid, default) = sections(product);
    for s in &a.sections {
        if !valid.contains(&s.as_str()) {
            err.diag(&Diag::error(
                "A0022",
                format!(
                    "no section '{s}' in a {} report; sections: {}",
                    product.name(),
                    valid.join(", ")
                ),
            ));
            return ExitCode::from(1);
        }
    }
    let explicit = !a.sections.is_empty();
    // Without sections, JSON is the report file itself (CLI-REQ-0102).
    if !explicit && a.print == ShowPrint::Json {
        return match stdout(loaded.report_text.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(d) => {
                err.diag(&d);
                ExitCode::from(1)
            }
        };
    }
    let names: Vec<&str> = if explicit {
        a.sections.iter().map(String::as_str).collect()
    } else {
        default.to_vec()
    };
    if a.print == ShowPrint::Tsv && (names.len() != 1 || !TABLES.contains(&names[0])) {
        err.diag(&Diag::error(
            "A0022",
            "--print tsv needs exactly one table section",
        ));
        return ExitCode::from(1);
    }
    let mut c = Ctxt {
        a,
        l: &loaded,
        out: String::new(),
        json: serde_json::Map::new(),
    };
    // Default text of a check report: exactly the command's standard output (CLI-REQ-0101).
    if !explicit && a.print == ShowPrint::Text && !a.sources && !a.exact {
        if let Report::CheckSynth(r) = &loaded.report {
            c.out = render::check_synth(r);
            return finish(&err, &c);
        }
    }
    for name in names {
        let r = match name {
            "summary" => {
                c.summary();
                Ok(())
            }
            "command" => match &loaded.record {
                Some(r) => {
                    if a.print == ShowPrint::Json {
                        c.json
                            .insert("command".into(), serde_json::json!(r.command));
                    } else {
                        let _ = writeln!(c.out, "{}", crate::proto::format::name(&r.command));
                    }
                    Ok(())
                }
                None => Err(Diag::error(
                    "A0010",
                    "a report file has no command; give its run directory",
                )),
            },
            t => match table(&loaded.report, t) {
                Some(m) => c.table_section(t, m.as_ref()),
                None => Ok(()),
            },
        };
        if let Err(d) = r {
            err.diag(&d);
            return ExitCode::from(1);
        }
    }
    finish(&err, &c)
}

fn finish(err: &crate::out::Err, c: &Ctxt<'_>) -> ExitCode {
    let text = if c.a.print == ShowPrint::Json {
        let v = if c.json.len() == 1 {
            c.json.values().next().cloned().unwrap_or_default()
        } else {
            serde_json::Value::Object(c.json.clone())
        };
        serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
    } else {
        c.out.clone()
    };
    match stdout(text.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(d) => {
            err.diag(&d);
            ExitCode::from(1)
        }
    }
}
