// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Workspace table export (`rusteda export --netlist`; RCLI-REQ-0001, RCLI-REQ-0008, RCLI-REQ-0020).

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write as _};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use rusteda_core::names::unescape;
#[cfg(test)]
use rusteda_core::netlist::{Conn, GateNetlist, NetId, PortDir};
#[cfg(test)]
use rusteda_formats::verilog::netlist::from_gate_netlist;
#[cfg(test)]
use rusteda_formats::verilog::write::{ident, write_netlist};
use rusteda_workspace::text::next_line;
use rusteda_workspace::{Envelope, Error, JsonObject, Workspace, netlist};

use crate::OutputOutcome;

mod frozen;

/// Formats a [`GateNetlist`] as structural Verilog text, using [`from_gate_netlist`] and
/// [`write_netlist`] when the module uses `[w-1:0]` input/output ports and wires, and falling back
/// to direct `ident`-escaped rendering when the netlist has `inout` ports or general `[msb:lsb]`
/// bus ranges (`RCLI-REQ-0020`).
#[cfg(test)]
fn format_gate_netlist(g: &GateNetlist) -> Result<String, Error> {
    if let Ok(m) = from_gate_netlist(g) {
        return Ok(write_netlist(&m));
    }

    let mut taken: HashMap<String, (&'static str, String)> = HashMap::new();
    let mut declare = |kind: &'static str, stored: &str| -> Result<String, Error> {
        let raw = unescape(stored);
        match taken.get(&raw) {
            None => {
                taken.insert(raw.clone(), (kind, stored.to_string()));
                Ok(raw)
            }
            Some((k, s)) if s == stored => Err(Error::input_rejected(format!(
                "{kind} '{stored}' has the name of a {k}; Verilog needs distinct names"
            ))),
            Some((k, s)) => Err(Error::input_rejected(format!(
                "{kind} '{stored}' and {k} '{s}' are both written as '{raw}'; Verilog needs distinct names"
            ))),
        }
    };

    let mut refs: HashMap<NetId, String> = HashMap::new();
    let mut s = String::new();
    let _ = writeln!(s, "module {}(", ident(g.name()));

    for (i, p) in g.ports().iter().enumerate() {
        let stored = g.resolve(p.name);
        let raw = declare("port", stored)?;
        let dir = match p.dir {
            PortDir::Input => "input",
            PortDir::Output => "output",
            PortDir::Inout => "inout",
        };
        let range = p
            .range
            .map(|(msb, lsb)| format!(" [{msb}:{lsb}]"))
            .unwrap_or_default();
        let sep = if i + 1 == g.ports().len() { "" } else { "," };
        let _ = writeln!(s, "  {dir} wire{range} {}{sep}", ident(&raw));
        for &bit in &p.bits {
            let net = g
                .net(bit)
                .map_err(|e| Error::input_rejected(e.to_string()))?;
            let r = match net.index {
                None => ident(&raw),
                Some(k) => format!("{}[{k}]", ident(&raw)),
            };
            refs.insert(bit, r);
        }
    }
    let _ = writeln!(s, ");");

    let mut groups: Vec<(&str, Vec<(NetId, Option<i32>)>)> = Vec::new();
    let mut group_of = HashMap::new();
    for (id, n) in g.nets() {
        if refs.contains_key(&id) {
            continue;
        }
        let Some(sym) = n.name else {
            return Err(Error::input_rejected(format!(
                "net #{} has no name; the writer needs a name for every net",
                id.index()
            )));
        };
        let k = *group_of.entry(sym).or_insert_with(|| {
            groups.push((g.resolve(sym), Vec::new()));
            groups.len() - 1
        });
        groups[k].1.push((id, n.index));
    }

    for (stored, bits) in groups {
        let raw = declare("net", stored)?;
        if let [(id, None)] = bits[..] {
            refs.insert(id, ident(&raw));
            let _ = writeln!(s, "  wire {};", ident(&raw));
            continue;
        }
        let Some((_, Some(first_idx))) = bits.first().copied() else {
            return Err(Error::input_rejected(format!(
                "net '{stored}' mixes scalar and bit-indexed declarations"
            )));
        };
        let Some((_, Some(last_idx))) = bits.last().copied() else {
            return Err(Error::input_rejected(format!(
                "net '{stored}' mixes scalar and bit-indexed declarations"
            )));
        };
        let expected_len = (i64::from(first_idx) - i64::from(last_idx)).unsigned_abs() as usize + 1;
        let step = if first_idx >= last_idx { -1i64 } else { 1i64 };
        let contiguous = bits.len() == expected_len
            && bits.iter().enumerate().all(|(pos, &(_, idx))| {
                idx.map(i64::from) == Some(i64::from(first_idx) + (pos as i64) * step)
            });
        if !contiguous {
            return Err(Error::input_rejected(format!(
                "the {} bits of net '{stored}' do not form a contiguous bus range",
                bits.len()
            )));
        }
        for &(id, idx) in &bits {
            let k = idx.expect("checked Some above");
            refs.insert(id, format!("{}[{k}]", ident(&raw)));
        }
        let _ = writeln!(s, "  wire [{first_idx}:{last_idx}] {};", ident(&raw));
    }

    let fmt_conn = |c: Conn| -> Result<String, Error> {
        match c {
            Conn::Const(false) => Ok("1'b0".to_string()),
            Conn::Const(true) => Ok("1'b1".to_string()),
            Conn::Net(n) => refs
                .get(&n)
                .cloned()
                .ok_or_else(|| Error::input_rejected(format!("net #{} is missing", n.index()))),
        }
    };

    for (_, inst) in g.instances() {
        let stored = g.resolve(inst.name);
        let raw = declare("instance", stored)?;
        let cell = g
            .cell(inst.cell)
            .map_err(|e| Error::input_rejected(e.to_string()))?;
        let cell_name = g.resolve(cell.name);
        if cell_name == g.name() {
            return Err(Error::input_rejected(format!(
                "instance '{stored}' has the module's name '{cell_name}' as its cell"
            )));
        }
        let _ = write!(s, "  {} {} (", ident(cell_name), ident(&raw));
        let mut connected_pairs = Vec::new();
        for &p in &inst.connected {
            let pin = g.pin(p).map_err(|e| Error::input_rejected(e.to_string()))?;
            if let Some(c) = pin.conn {
                let pin_name = g.resolve(cell.pins[pin.cell_pin as usize].name);
                connected_pairs.push((pin_name, fmt_conn(c)?));
            }
        }
        for (i, (pin_name, target)) in connected_pairs.iter().enumerate() {
            let sep = if i + 1 == connected_pairs.len() {
                ""
            } else {
                ", "
            };
            let _ = write!(s, ".{}({target}){sep}", ident(pin_name));
        }
        let _ = writeln!(s, ");");
    }

    for a in g.assigns() {
        let lhs = fmt_conn(Conn::Net(a.lhs))?;
        let rhs = fmt_conn(a.rhs)?;
        let _ = writeln!(s, "  assign {lhs} = {rhs};");
    }
    let _ = writeln!(s, "endmodule");
    Ok(s)
}

/// Exports the committed `netlist` table from `ws` at `rev_spec` to `out_file` (`RCLI-REQ-0020`).
pub(crate) fn run_export(
    ws: &Workspace,
    ws_arg: &str,
    rev_spec: &str,
    out_file: &Path,
) -> Result<OutputOutcome, Error> {
    let rev = ws.resolve_query(rev_spec)?;
    let revision = ws.revision(rev)?;
    let limits = frozen::Limits::default();
    let table_path = revision.table_path(netlist::TABLE_NAME)?;
    if fs::metadata(&table_path)
        .map_err(|e| Error::io(&table_path, e))?
        .len()
        > limits.max_table_bytes
    {
        return Err(Error::input_rejected(
            "EXPORT_BUDGET: netlist table exceeds the export byte limit",
        ));
    }
    let rbin = netlist::open_rev(&revision)?;
    let f = netlist::decode(&rbin)?;
    let plan = frozen::Plan::new(&f, limits)?;

    if let Some(parent) = out_file.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
    }
    let parent = out_file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pending = parent.join(format!(
        ".rusteda-export-{}-{nonce}.tmp",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|e| Error::io(&pending, e))?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    let written = plan
        .write(&mut writer)
        .and_then(|()| writer.flush())
        .and_then(|()| writer.get_ref().sync_all());
    drop(writer);
    if let Err(e) = written {
        let _ = fs::remove_file(&pending);
        return Err(Error::io(out_file, e));
    }
    if let Err(e) = fs::rename(&pending, out_file) {
        let _ = fs::remove_file(&pending);
        return Err(Error::io(out_file, e));
    }

    let out_str = out_file.display().to_string();
    let mut env = Envelope::new("rusteda export");
    env.workspace = Some(ws_arg.to_string());
    env.rev = Some(rev);

    let mut metrics = JsonObject::new();
    metrics.insert("instances", &f.instances.len())?;
    metrics.insert("nets", &f.nets.len())?;
    metrics.insert("ports", &f.ports.len())?;
    env.set_metrics(metrics)?;

    env.set("table", &netlist::TABLE_NAME)?;
    env.set("output", &out_str)?;

    let rev_s = rev.to_string();
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &rev_s]);

    let mut lines = Vec::new();
    lines.push(format!("{rev}  rusteda export  netlist -> {out_str}"));
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok(OutputOutcome {
        envelope: env,
        text_lines: lines,
    })
}
