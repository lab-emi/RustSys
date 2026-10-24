// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Structural netlist import (`rusteda import --netlist`; RCLI-REQ-0011, RCLI-REQ-0019).

mod ast;
mod fast;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use rusteda_core::liberty::{Direction as LibDir, Library};
use rusteda_core::names::escape_segment;
use rusteda_core::netlist::{GateNetlist, MAX_RANGE_WIDTH, PinDir};
use rusteda_formats::liberty;
use rusteda_formats::verilog::ast::{
    DataType, Expr, ExprKind, Item, Module, ModuleItem, NetKind, PortConn, PortList,
};
use rusteda_formats::verilog::diag::{Diagnostic as VDiag, Severity as VSev};
use rusteda_formats::verilog::netlist::read_structural;
use rusteda_formats::verilog::number::{Bit4, Literal};
use rusteda_formats::verilog::parse_tokens;
use rusteda_formats::verilog::pp::{PpOptions, Preprocessor};
use rusteda_formats::verilog::span::SourceMap;
use rusteda_workspace::cache::PutError;
use rusteda_workspace::{
    Corruption, DiagCode, Diagnostic, Error, Hash64, JsonMap, Severity, Txn, Upstream, Workspace,
    rlib,
};
use serde_json::json;

pub(super) const C_UNSUP: &str = "V0300";
pub(super) const C_DECL: &str = "V0301";
pub(super) const C_CELL: &str = "V0302";
pub(super) const C_WIDTH: &str = "V0303";
pub(super) const C_PG_DROP: &str = "V0310";

#[derive(Debug, Clone)]
pub(super) struct PinConflict {
    pub(super) pin: String,
    pub(super) corner_a: String,
    pub(super) dir_a: PinDir,
    pub(super) corner_b: String,
    pub(super) dir_b: PinDir,
}

#[derive(Debug, Clone)]
pub(super) struct CellInfo {
    pub(super) pins: Vec<(String, PinDir)>,
    pub(super) pin_index: HashMap<String, u32>,
    pub(super) pg_pins: HashSet<String>,
    pub(super) first_corner: String,
    pub(super) conflicts: Vec<PinConflict>,
}

#[derive(Debug, Default)]
pub(super) struct CellCatalog {
    pub(super) cells: HashMap<String, CellInfo>,
}

impl CellCatalog {
    fn from_txn(ws: &Workspace, txn: &Txn) -> Result<Self, Error> {
        let mut cat = Self::default();
        let cache = ws.cache();
        for entry in txn.libraries() {
            let rbin = cache
                .open_rlib_or_recompile(
                    &entry.name,
                    entry.hash,
                    rlib::KIND,
                    rlib::VERSIONS,
                    |raw| {
                        let text = std::str::from_utf8(raw)
                            .map_err(|_| Error::corrupt(&entry.source, Corruption::NotUtf8))?;
                        let lib = liberty::read_text(text, &entry.source).map_err(|e| {
                            Error::corrupt(
                                &entry.source,
                                Corruption::Json {
                                    reason: e.to_string(),
                                },
                            )
                        })?;
                        rlib::encode_with_source(&lib, entry.hash)
                    },
                )
                .map_err(|e| match e {
                    PutError::Compile(err) | PutError::Workspace(err) => err,
                })?;
            let lib: Library = rlib::decode_rbin(&rbin)?;
            cat.add_library(&lib, &entry.corner);
        }
        Ok(cat)
    }

    fn add_library(&mut self, lib: &Library, corner: &str) {
        for cell in &lib.cells {
            let mut pins = Vec::with_capacity(cell.pins.len());
            for p in &cell.pins {
                let dir = match p.direction {
                    LibDir::Input => PinDir::Input,
                    LibDir::Output => PinDir::Output,
                    LibDir::Inout => PinDir::Inout,
                    LibDir::Internal => continue,
                };
                pins.push((p.name.clone(), dir));
            }
            let mut pg_pins: HashSet<String> = cell.pg_pins.iter().cloned().collect();
            pg_pins.extend(cell.pg_pin_groups.iter().map(|pg| pg.name.clone()));

            if let Some(ex) = self.cells.get_mut(&cell.name) {
                ex.pg_pins.extend(pg_pins);
                for (pname, pdir) in pins {
                    if let Some(&idx) = ex.pin_index.get(&pname) {
                        let prev = ex.pins[idx as usize].1;
                        if prev != pdir && !ex.conflicts.iter().any(|c| c.pin == pname) {
                            ex.conflicts.push(PinConflict {
                                pin: pname,
                                corner_a: ex.first_corner.clone(),
                                dir_a: prev,
                                corner_b: corner.to_string(),
                                dir_b: pdir,
                            });
                        }
                    } else {
                        ex.pin_index.insert(pname.clone(), ex.pins.len() as u32);
                        ex.pins.push((pname, pdir));
                    }
                }
            } else {
                let pin_index = pins
                    .iter()
                    .enumerate()
                    .map(|(i, (n, _))| (n.clone(), i as u32))
                    .collect();
                self.cells.insert(
                    cell.name.clone(),
                    CellInfo {
                        pins,
                        pin_index,
                        pg_pins,
                        first_corner: corner.to_string(),
                        conflicts: Vec::new(),
                    },
                );
            }
        }
    }
}

pub(super) fn range_width(msb: i32, lsb: i32) -> u64 {
    (i64::from(msb) - i64::from(lsb)).unsigned_abs() + 1
}

pub(super) fn qualify_hier(prefix: Option<&str>, raw: &str) -> String {
    match prefix {
        None => escape_segment(raw),
        Some(p) => format!("{p}/{}", escape_segment(raw)),
    }
}

pub(super) fn collect_expr_idents<'a>(e: &'a Expr, out: &mut HashSet<&'a str>) {
    match &e.kind {
        ExprKind::Ident(p) if p.scope.is_none() => {
            out.insert(p.name.name.as_str());
        }
        ExprKind::Index(base, _) | ExprKind::Slice { base, .. } => {
            if let Some(id) = base.as_ident() {
                out.insert(id.name.as_str());
            }
        }
        ExprKind::Concat(items) | ExprKind::Repl(_, items) => {
            for item in items {
                collect_expr_idents(item, out);
            }
        }
        _ => {}
    }
}

pub(super) fn literal_i64(l: &Literal) -> Option<i64> {
    if l.signed && !l.plain {
        return None;
    }
    let bits = l.bits().ok()?;
    let mut v = 0i64;
    for (k, b) in bits.iter().enumerate() {
        match b {
            Bit4::Zero => {}
            Bit4::One if k < 62 => v |= 1 << k,
            _ => return None,
        }
    }
    Some(v)
}

fn convert_vdiag(sm: &SourceMap, d: &VDiag, fallback_file: &str) -> Diagnostic {
    let sev = match d.severity {
        VSev::Note => Severity::Note,
        VSev::Warning => Severity::Warning,
        VSev::Error => Severity::Error,
    };
    let code = DiagCode::parse(d.code).unwrap_or(DiagCode::of(C_UNSUP));
    let loc = match sm.get(d.span.file) {
        Some(f) => {
            let (line, col) = f.line_col(d.span.start);
            let fp = f.path.display().to_string();
            let file = if fp.is_empty() {
                fallback_file.to_string()
            } else {
                fp
            };
            Some((file, line.max(1), col.max(1)))
        }
        None => Some((fallback_file.to_string(), 1, 1)),
    };
    Diagnostic::from_parts(sev, code, loc, d.message.clone(), None)
}

fn reject_vdiags(sm: &SourceMap, vdiags: &[VDiag], file: &str, out: &mut Vec<Diagnostic>) -> Error {
    let mut first_err = None;
    for vd in vdiags {
        let conv = convert_vdiag(sm, vd, file);
        if conv.severity == Severity::Error && first_err.is_none() {
            first_err = Some(match &conv.location {
                Some(l) => format!("{}:{}:{}: {}", l.file, l.line, l.col, conv.message),
                None => format!("{file}:1:1: {}", conv.message),
            });
        }
        out.push(conv);
    }
    Error::input_rejected(first_err.unwrap_or_else(|| format!("{file}:1:1: invalid netlist")))
}

fn expr_uses_extension(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Concat(_)
        | ExprKind::Repl(..)
        | ExprKind::Slice { .. }
        | ExprKind::UnbasedUnsized(_) => true,
        ExprKind::Number(l) => l.size != Some(1) || l.plain,
        _ => false,
    }
}

fn uses_extended_features(items: &[Item], catalog: &CellCatalog) -> bool {
    let mods: Vec<&Module> = items
        .iter()
        .filter_map(|it| {
            if let Item::Module(m) = it {
                Some(m.as_ref())
            } else {
                None
            }
        })
        .collect();
    if mods.len() > 1 {
        return true;
    }
    let Some(m) = mods.first() else { return false };
    let is_ext_net = |nk: Option<NetKind>, ty: &DataType| {
        matches!(nk, Some(NetKind::Supply0 | NetKind::Supply1)) || ty.signing.is_some()
    };
    if let PortList::Ansi(ports) = &m.ports
        && ports.iter().any(|p| is_ext_net(p.net, &p.ty))
    {
        return true;
    }
    for item in &m.items {
        match item {
            ModuleItem::PortDecl(d) if is_ext_net(d.net, &d.ty) => return true,
            ModuleItem::Data(d) if is_ext_net(d.net, &d.ty) => return true,
            ModuleItem::Instance(inst) => {
                if let Some(info) = catalog.cells.get(&inst.module.name) {
                    if !info.conflicts.is_empty() {
                        return true;
                    }
                    for i in &inst.instances {
                        for c in &i.conns {
                            if let PortConn::Named(pin, opt_e) = c {
                                if info.pg_pins.contains(&pin.name) {
                                    return true;
                                }
                                if let Some(Some(e)) = opt_e
                                    && expr_uses_extension(e)
                                {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            ModuleItem::Assign(assigns)
                if assigns
                    .iter()
                    .any(|a| expr_uses_extension(&a.lhs) || expr_uses_extension(&a.rhs)) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn read_netlist_ast(
    text: &str,
    file: &str,
    catalog: &CellCatalog,
    diags: &mut Vec<Diagnostic>,
) -> Result<GateNetlist, Error> {
    let mut sources = SourceMap::new();
    let id = sources.add(file, text);
    let mut pp = Preprocessor::new(PpOptions::default());
    let toks = pp.run(&mut sources, id);
    if pp.diags.iter().any(VDiag::is_error) {
        return Err(reject_vdiags(&sources, &pp.diags, file, diags));
    }
    let unit = match parse_tokens(&toks) {
        Ok(u) => u,
        Err(vd) => return Err(reject_vdiags(&sources, &[vd], file, diags)),
    };
    if !uses_extended_features(&unit.items, catalog) {
        return read_structural(text, file, &|c| {
            catalog.cells.get(c).map(|i| i.pins.clone())
        })
        .map_err(|re| reject_vdiags(&re.sources, &re.diagnostics, file, diags));
    }
    for vd in &pp.diags {
        diags.push(convert_vdiag(&sources, vd, file));
    }
    let budget = (text.len() as u64).max(u64::from(MAX_RANGE_WIDTH));
    match ast::elaborate_extended(&sources, &unit.items, catalog, budget) {
        Ok((g, ext_diags)) => {
            for vd in &ext_diags {
                diags.push(convert_vdiag(&sources, vd, file));
            }
            Ok(g)
        }
        Err(ext_diags) => Err(reject_vdiags(&sources, &ext_diags, file, diags)),
    }
}

/// Imports a structural Verilog netlist into `txn` (`RCLI-REQ-0011`, `RCLI-REQ-0019`).
pub(crate) fn import_netlist(
    ws: &Workspace,
    txn: &mut Txn,
    file: &Path,
    metrics: &mut JsonMap,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    let source_str = file.display().to_string();
    let meta = fs::symlink_metadata(file).map_err(|e| Error::io(file, e))?;
    if !meta.file_type().is_file() {
        let msg = format!("{source_str}: not a regular file");
        diags.push(
            Diagnostic::new(Severity::Error, DiagCode::of("V0001"), msg.clone())
                .at(source_str, 1, 1),
        );
        return Err(Error::input_rejected(msg));
    }

    let bytes = fs::read(file).map_err(|e| Error::io(file, e))?;
    let source_hash = Hash64::of(&bytes);
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("invalid UTF-8 in Verilog netlist: {e}");
            diags.push(
                Diagnostic::new(Severity::Error, DiagCode::of("V0001"), msg.clone()).at(
                    source_str.clone(),
                    1,
                    1,
                ),
            );
            return Err(Error::input_rejected(format!("{source_str}:1:1: {msg}")));
        }
    };

    let catalog = CellCatalog::from_txn(ws, txn)?;
    let frozen = if let Some(fast) = fast::try_fast_flat(&text, &catalog) {
        // Debug builds check small inputs against the reader that the fast path replaces: the
        // structural reader, or the extended elaborator for concatenations, part-selects and
        // sized constants.
        #[cfg(debug_assertions)]
        if text.len() <= 65_536
            && let Ok(g) = read_netlist_ast(&text, &source_str, &catalog, &mut Vec::new())
        {
            debug_assert_eq!(
                fast,
                g.freeze(),
                "fast path diverged from the netlist reader"
            );
        }
        drop(text);
        fast
    } else {
        // Whole-file token/AST expansion is unsuitable for multi-million-cell
        // flat inputs. Keep unsupported large syntax explicit and bounded.
        if text.len() > 64 * 1024 * 1024 {
            let msg = "large flat netlist uses syntax unsupported by the bounded streaming importer (64 MiB AST fallback budget)";
            diags.push(Diagnostic::new(Severity::Error, DiagCode::of(C_UNSUP), msg));
            return Err(Error::input_rejected(msg));
        }
        let g = read_netlist_ast(&text, &source_str, &catalog, diags)?;
        drop(text);
        g.freeze()
    };

    rusteda_workspace::netlist::add_to_txn(txn, &frozen, Upstream::new())?;
    txn.add_source(
        rusteda_workspace::netlist::TABLE_NAME,
        source_str,
        source_hash,
    )?;

    let top_id = frozen.module[rusteda_core::netlist::module_entry::NAME] as usize;
    let (ts, te) = (
        frozen.str_offs[top_id] as usize,
        frozen.str_offs[top_id + 1] as usize,
    );
    let top_name = std::str::from_utf8(&frozen.str_bytes[ts..te])
        .unwrap_or("")
        .to_string();
    let (n_insts, n_nets, n_ports, n_assigns) = (
        frozen.instances.len(),
        frozen.nets.len(),
        frozen.ports.len(),
        frozen.assigns.len(),
    );

    txn.set_summary("netlist", "top", top_name.clone())?;
    txn.set_summary("netlist", "cells", n_insts)?;
    txn.set_summary("netlist", "nets", n_nets)?;
    txn.set_summary("netlist", "ports", n_ports)?;
    txn.set_summary("netlist", "assigns", n_assigns)?;

    metrics.insert("top".to_string(), json!(top_name));
    metrics.insert("cells".to_string(), json!(n_insts));
    metrics.insert("nets".to_string(), json!(n_nets));
    metrics.insert("ports".to_string(), json!(n_ports));
    metrics.insert("assigns".to_string(), json!(n_assigns));
    Ok(())
}
