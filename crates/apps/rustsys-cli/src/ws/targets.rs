// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Targets of the post-mapping commands (CLI-REQ-0206, CLI-REQ-0207): instance names and the
//! handles `inst<N>`, `net<N>`, `sel<N>`, and `path<N>` and `ep<N>` of the timing table of the
//! base revision, resolved to instances of its netlist; and the default targets of
//! `opt-timing`, the instances on the ranked setup paths with negative slack.

use std::collections::{BTreeSet, HashMap};

use rusteda_core::names::unescape;
use rusteda_core::netlist::GateNetlist;
use rusteda_formats::verilog::write::{NlModule, NlRef};
use rusteda_workspace::sta::{CheckKind, RankedPath, StaTable};
use rusteda_workspace::{Diagnostic, Error, Handle, Revision, Severity, Workspace};

use crate::ws::app_diag_code;

/// Where the targets came from (CLI-REQ-0206).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// `--target` handles and names.
    Cli,
    /// The ranked setup paths with negative slack of the timing table (no `--target`).
    TimingTable,
    /// Every instance: no `--target` and no timing table.
    AllInstances,
}

impl Source {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Source::Cli => "cli",
            Source::TimingTable => "timing_table",
            Source::AllInstances => "all_instances",
        }
    }
}

/// A ranked path that contributed targets.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PathUse {
    pub handle: String,
    pub kind: &'static str,
    pub corner: String,
    pub slack_ns: f64,
    /// Whether `slack_ns` is the exact slack of a materialized path (else the `f32` rank).
    pub slack_exact: bool,
    pub endpoint: String,
    pub instances: usize,
}

/// The resolved targets.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    /// Rows of the netlist's instances, each once, in resolution order.
    pub instances: Vec<usize>,
    pub source: Source,
    /// The ranked paths that contributed, in resolution order.
    pub paths: Vec<PathUse>,
    /// Whether the timing table of the base revision was read.
    pub table_used: bool,
}

/// The timing table of the base revision, opened on first use: content hash verified, and
/// computed from the revision's netlist (CLI-REQ-0207).
struct Table<'r> {
    rev: &'r Revision,
    table: Option<StaTable>,
}

impl Table<'_> {
    fn get(&mut self, why: &str, diags: &mut Vec<Diagnostic>) -> Result<&StaTable, Error> {
        if self.table.is_none() {
            let rev = self.rev.rev();
            let opened = StaTable::open_rev(self.rev).inspect_err(|e| {
                if let Error::MissingTable { .. } = e {
                    let mut d = e.to_diagnostic(app_diag_code(e.code()));
                    d.message = format!("{why} needs the timing table of {rev}, which has none");
                    d.help = Some(format!(
                        "run the static timing analysis tool of the suite on {rev} first: it \
                         commits the timing table (sta) that path<N> and ep<N> handles name"
                    ));
                    diags.push(d);
                }
            })?;
            opened.verify_hash()?;
            let from_netlist = self
                .rev
                .table(rusteda_workspace::sta::TABLE_NAME)?
                .upstream
                .get("netlist")
                .is_some_and(|h| Some(*h) == self.rev.table_hash("netlist"));
            if !from_netlist {
                return Err(Error::input_rejected(format!(
                    "the timing table of {rev} was not computed from the netlist of {rev}; run the \
                     static timing analysis tool of the suite on {rev} again"
                )));
            }
            self.table = Some(opened);
        }
        Ok(self.table.as_ref().expect("opened above"))
    }
}

/// Maps the instance names of a timing table, as the netlist table stores them, to netlist rows.
struct Names {
    rows: HashMap<String, usize>,
}

impl Names {
    fn new(nl: &NlModule) -> Names {
        Names {
            rows: nl
                .instances
                .iter()
                .enumerate()
                .map(|(i, x)| (x.name.clone(), i))
                .collect(),
        }
    }

    fn row(&self, stored: &str, rev: &Revision) -> Result<usize, Error> {
        self.rows
            .get(stored)
            .or_else(|| self.rows.get(&unescape(stored)))
            .copied()
            .ok_or_else(|| {
                Error::input_rejected(format!(
                    "the timing table of {} names instance '{stored}', which its netlist lacks",
                    rev.rev()
                ))
            })
    }
}

fn path_use(p: &RankedPath) -> PathUse {
    PathUse {
        handle: p.handle(),
        kind: p.kind.as_str(),
        corner: p.corner_name.clone(),
        slack_ns: p.slack_ns,
        slack_exact: p.slack_exact,
        endpoint: p.endpoint_name.clone(),
        instances: p.instances().len(),
    }
}

/// Collects targets in resolution order without repeats.
struct Collector {
    rows: Vec<usize>,
    seen: BTreeSet<usize>,
    paths: Vec<PathUse>,
}

impl Collector {
    fn add(&mut self, row: usize) {
        if self.seen.insert(row) {
            self.rows.push(row);
        }
    }

    fn add_path(&mut self, p: &RankedPath, names: &Names, rev: &Revision) -> Result<(), Error> {
        for inst in p.instances() {
            let row = names.row(inst, rev)?;
            self.add(row);
        }
        self.paths.push(path_use(p));
        Ok(())
    }
}

fn unknown(handle: &str, reason: impl Into<String>) -> Error {
    Error::UnknownHandle {
        handle: handle.to_string(),
        reason: reason.into(),
    }
}

/// Resolves `raw` targets against `rev` (CLI-REQ-0206, CLI-REQ-0207); without targets and with
/// `defaults`, the instances on the ranked setup paths with negative slack of the timing table,
/// or every instance when the revision has no timing table.
pub(crate) fn resolve(
    ws: &Workspace,
    rev: &Revision,
    g: &GateNetlist,
    nl: &NlModule,
    raw: &[String],
    defaults: bool,
    diags: &mut Vec<Diagnostic>,
) -> Result<Resolved, Error> {
    let names = Names::new(nl);
    let mut table = Table { rev, table: None };
    let mut out = Collector {
        rows: Vec::new(),
        seen: BTreeSet::new(),
        paths: Vec::new(),
    };
    if raw.is_empty() && defaults {
        if !rev.has_table(rusteda_workspace::sta::TABLE_NAME) {
            diags.push(Diagnostic::new(
                Severity::Note,
                rusteda_workspace::DiagCode::of("S0537"),
                format!(
                    "{} has no timing table: every instance may change, chosen by the estimate",
                    rev.rev()
                ),
            ));
            return Ok(Resolved {
                instances: Vec::new(),
                source: Source::AllInstances,
                paths: Vec::new(),
                table_used: false,
            });
        }
        let t = table.get("choosing the default targets", diags)?;
        for rank in t.path_ranks()? {
            if rank.kind != CheckKind::Setup {
                continue;
            }
            let p = t.path(rank.row)?;
            if p.slack_ns < 0.0 {
                out.add_path(&p, &names, rev)?;
            }
        }
        if out.rows.is_empty() {
            diags.push(Diagnostic::new(
                Severity::Warning,
                rusteda_workspace::DiagCode::of("S0537"),
                format!(
                    "the timing table of {} has no ranked setup path with negative slack: no \
                     instance is a target",
                    rev.rev()
                ),
            ));
        }
        return Ok(Resolved {
            instances: out.rows,
            source: Source::TimingTable,
            paths: out.paths,
            table_used: true,
        });
    }
    let mut expanded: Vec<String> = Vec::new();
    for t in raw {
        if Handle::parse_as(t, "sel").is_ok() {
            let ui = ws.resolve_selection(t, rev.rev())?;
            if ui.selected_instances.is_empty() && ui.selected_paths.is_empty() {
                return Err(unknown(t, "selection contains no objects"));
            }
            expanded.extend(ui.selected_instances);
            expanded.extend(ui.selected_paths);
        } else {
            expanded.push(t.clone());
        }
    }
    let mut nets: Option<Vec<(NlRef, NlRef)>> = None;
    for target in &expanded {
        if let Some(&row) = names.rows.get(target.as_str()) {
            out.add(row);
            continue;
        }
        let Ok(handle) = Handle::parse(target) else {
            return Err(unknown(
                target,
                format!("no such handle or instance in {}", rev.rev()),
            ));
        };
        let row = handle.row() as usize;
        match handle.prefix() {
            "inst" => {
                if row >= nl.instances.len() {
                    return Err(unknown(
                        target,
                        format!(
                            "instance index {} is out of range for {} ({} instances)",
                            handle.index(),
                            rev.rev(),
                            nl.instances.len()
                        ),
                    ));
                }
                out.add(row);
            }
            "net" => {
                let refs = nets.get_or_insert_with(|| net_refs(g));
                let Some((bit, whole)) = refs.get(row) else {
                    return Err(unknown(
                        target,
                        format!(
                            "net index {} is out of range for {} ({} nets)",
                            handle.index(),
                            rev.rev(),
                            refs.len()
                        ),
                    ));
                };
                for (i, inst) in nl.instances.iter().enumerate() {
                    if inst.conns.iter().any(|(_, r)| r == bit || r == whole) {
                        out.add(i);
                    }
                }
            }
            "path" => {
                let t = table.get(&format!("--target {target}"), diags)?;
                let p = t.path(row)?;
                out.add_path(&p, &names, rev)?;
            }
            "ep" => {
                let t = table.get(&format!("--target {target}"), diags)?;
                let order = t.endpoint_order(0, false)?;
                let Some(&e) = order.get(row) else {
                    return Err(unknown(
                        target,
                        format!(
                            "the timing table of {} has {} endpoints",
                            rev.rev(),
                            order.len()
                        ),
                    ));
                };
                let ep = t.endpoint(e, 0)?;
                if let Some(path_row) = ep.worst_setup_path {
                    let p = t.path(path_row)?;
                    out.add_path(&p, &names, rev)?;
                } else if let Some(inst) = ep.instance.as_deref() {
                    out.add(names.row(inst, rev)?);
                } else {
                    return Err(unknown(
                        target,
                        format!("endpoint {} is a port without a ranked setup path", ep.name),
                    ));
                }
            }
            other => {
                return Err(unknown(
                    target,
                    format!("unknown handle prefix '{other}' in {}", rev.rev()),
                ));
            }
        }
    }
    if out.rows.is_empty() {
        return Err(Error::input_rejected(format!(
            "the targets name no cell instance of {}",
            rev.rev()
        )));
    }
    Ok(Resolved {
        instances: out.rows,
        source: Source::Cli,
        paths: out.paths,
        table_used: table.table.is_some(),
    })
}

/// The reference of every net of `g` in net order, as a bit and as a whole net.
fn net_refs(g: &GateNetlist) -> Vec<(NlRef, NlRef)> {
    g.nets()
        .map(|(_, rec)| {
            let name = rec.name.map(|s| unescape(g.resolve(s))).unwrap_or_default();
            let bit = match rec.index {
                Some(i) if i >= 0 => NlRef::Bit(name.clone(), i as u32),
                _ => NlRef::Net(name.clone()),
            };
            (bit, NlRef::Net(name))
        })
        .collect()
}
