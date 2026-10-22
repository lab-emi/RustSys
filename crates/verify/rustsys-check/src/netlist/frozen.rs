// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Independent gate semantics directly from validated disk tables.
//!
//! C910 plan: ~720 MiB mapped tables, 6M dense variable ids *4 B, <=200 MiB
//! lookup/group records, <=1 GiB names/state, and circuit definitions below the
//! configurable node cap. The RTL model and proof scratch share an 8 GiB OS
//! guard. Every cell/connection is evaluated; only the *name lookup index* is
//! projected to ports and requested RTL state probes. No gate cone is omitted.
//! No mutable synthesis graph, Verilog string or whole-netlist AST is built.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use rusteda_core::names::unescape;
use rusteda_core::netlist::{ANONYMOUS, CONST0, CONST1, FrozenNetlist, SCALAR, module_entry};
use rusteda_formats::liberty::{Direction, Library};
use rusteda_formats::verilog::ast::{Ident, Instance};
use rusteda_formats::verilog::{SourceMap, Span};

use super::{MAX_NET_BITS, NetModel, cells};
use crate::circ::{B, Circuit};
use crate::liberty::Unread;

/// Entity, allocation and graph-work guards, checked before large allocations.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_nets: usize,
    pub max_instances: usize,
    pub max_pins: usize,
    pub max_strings: usize,
    pub max_table_bytes: u64,
    pub max_aux_bytes: u64,
    pub max_definitions: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nets: 20_000_000,
            max_instances: 10_000_000,
            max_pins: 128_000_000,
            max_strings: 32_000_000,
            max_table_bytes: 2 * 1024 * 1024 * 1024,
            max_aux_bytes: 2 * 1024 * 1024 * 1024,
            max_definitions: 128_000_000,
        }
    }
}

fn budget(what: &str) -> String {
    format!("warning[C0005]: frozen checker budget reached: {what}")
}

fn raw(name: &str) -> Cow<'_, str> {
    if name.contains('\\') {
        Cow::Owned(unescape(name))
    } else {
        Cow::Borrowed(name)
    }
}

fn string<'a>(f: &'a FrozenNetlist<'_>, k: u32) -> &'a str {
    f.string(k).expect("validated string")
}

struct Group {
    sym: u32,
    /// An unambiguous lookup proposal only; physical nets remain unchanged.
    probe_name: Option<String>,
    min: i32,
    max: i32,
    count: usize,
    vars: Vec<u32>,
}

/// Foreign frontends may insert anonymous generate scopes. This proposes a
/// name match only; all state/event functions still have to be proved.
fn without_anonymous_scopes(name: &str) -> Option<String> {
    let anonymous = |part: &str| {
        part.strip_prefix("genblk")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if !name.split('.').any(anonymous) {
        return None;
    }
    let mut result = String::with_capacity(name.len());
    for part in name.split('.').filter(|part| !anonymous(part)) {
        if !result.is_empty() {
            result.push('.');
        }
        result.push_str(part);
    }
    Some(result)
}

/// Reads every cell and alias. `wanted` contains RTL state names and their
/// optional vector bases; ports are always included. Library functions and
/// directions are authoritative, never the mapper's serialized pin directions.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn build(
    c: &mut Circuit,
    f: &FrozenNetlist<'_>,
    libs: &[Library],
    top: &str,
    input_vars: Option<&HashMap<String, Vec<u32>>>,
    supply_ports: &[String],
    unread: &[Unread],
    wanted: &HashSet<&str>,
    limits: Limits,
) -> Result<NetModel, String> {
    let strings = f.str_offs.len().saturating_sub(1);
    if f.nets.len() > limits.max_nets
        || f.instances.len() > limits.max_instances
        || f.pin_net.len() > limits.max_pins
        || strings > limits.max_strings
    {
        return Err(budget("entity count"));
    }
    if f.sections()
        .into_iter()
        .map(|(_, _, bytes)| bytes.len() as u128)
        .sum::<u128>()
        > limits.max_table_bytes as u128
    {
        return Err(budget("table bytes"));
    }
    let base_aux = strings as u128 * 4 + f.nets.len() as u128 * 4;
    if base_aux > limits.max_aux_bytes as u128
        || c.defs.len().saturating_add(f.nets.len()) > limits.max_definitions
    {
        return Err(budget("initial variable/index allocation"));
    }
    f.validate()
        .map_err(|e| format!("invalid frozen netlist: {e}"))?;
    let mut nm = NetModel {
        name: raw(string(f, f.module[module_entry::NAME])).into_owned(),
        ..NetModel::default()
    };
    if nm.name != top {
        return Err(format!("frozen top {} differs from RTL top {top}", nm.name));
    }
    let mut sm = SourceMap::new();
    sm.add("<frozen-netlist>", "");
    let at = Span::default();
    let table = cells::table(libs);
    let mut unread_of = HashMap::new();
    for u in unread {
        unread_of.entry(u.cell.as_str()).or_insert(u);
    }
    let mut catalog = Vec::new();
    let mut costs = Vec::new();
    for (id, &symbol) in f.cells.iter().enumerate() {
        let name = string(f, symbol);
        let cell = match table.get(name) {
            Some(Ok(cell)) => *cell,
            Some(Err(e)) => return Err(format!("cell {name}: {e}")),
            None => return Err(format!("unknown cell {name}")),
        };
        let site = Instance {
            name: Ident {
                name: name.into(),
                span: at,
            },
            dims: vec![],
            conns: vec![],
            span: at,
        };
        cells::modeled(&sm, &site, cell, unread_of.get(name).copied())?;
        for pin in f.cell_pins(id as u32) {
            let name = string(f, f.cell_pin_name[pin]);
            if cell.pg_pins.iter().any(|p| p == name) {
                continue;
            }
            let p = cell
                .pin(name)
                .ok_or_else(|| format!("cell {} has no pin {name}", cell.name))?;
            let dir = match p.direction {
                Direction::Input => 0,
                Direction::Output => 1,
                Direction::Inout => 2,
                Direction::Internal => 3,
            };
            if f.cell_pin_dir[pin] != dir {
                return Err(format!(
                    "cell {} pin {name}: serialized direction differs from Liberty",
                    cell.name
                ));
            }
        }
        // Each function character bounds parser/operator work and possible
        // circuit additions; constants and shared inversions only reduce it.
        let text_bytes = cell
            .pins
            .iter()
            .filter_map(|p| p.function_text.as_ref())
            .map(String::len)
            .sum::<usize>()
            + cell
                .sequentials
                .iter()
                .map(|s| {
                    s.data_text.len()
                        + s.clock_text.len()
                        + s.clear_text.as_ref().map_or(0, String::len)
                        + s.preset_text.as_ref().map_or(0, String::len)
                })
                .sum::<usize>();
        costs.push(text_bytes.saturating_mul(4).saturating_add(16));
        catalog.push(cell);
    }
    let mut seq_count = 0usize;
    let mut seq_names = 0usize;
    for instance in f.instances.iter() {
        let cell = catalog[instance.cell as usize];
        if !cell.sequentials.is_empty() {
            seq_count += 1;
            seq_names = seq_names.saturating_add(string(f, instance.name).len() + cell.name.len());
        }
    }
    let pg: HashSet<&str> = libs
        .iter()
        .flat_map(|l| &l.cells)
        .flat_map(|c| &c.pg_pins)
        .map(String::as_str)
        .chain(supply_ports.iter().map(String::as_str))
        .collect();
    let mut vars: Vec<u32> = (0..f.nets.len()).map(|_| c.free()).collect();
    let mut inputs = HashSet::new();
    let mut supplies = HashSet::new();
    let mut port_symbols = HashSet::new();
    let mut port_ranges = HashMap::new();
    for (id, port) in f.ports.iter().enumerate() {
        let name = raw(string(f, port.name)).into_owned();
        port_symbols.insert(port.name);
        if port.is_bus != 0 {
            port_ranges.insert(port.name, (i64::from(port.msb), i64::from(port.lsb)));
        }
        let bits = f.port_bits(id as u32);
        let supply =
            pg.contains(name.as_str()) && input_vars.is_none_or(|v| !v.contains_key(&name));
        if supply {
            supplies.extend(bits.iter().map(|&n| vars[n as usize]));
            nm.supply_ports.push(name);
            continue;
        }
        if port.dir == 2 {
            return Err(format!(
                "logic port {name} is bidirectional; the checker models input/output ports only"
            ));
        }
        if port.dir == 0 {
            if let Some(ports) = input_vars {
                let rtl = ports
                    .get(&name)
                    .ok_or_else(|| format!("netlist input {name} is not an RTL input"))?;
                if rtl.len() != bits.len() {
                    return Err(format!("input {name} width differs"));
                }
                for (&net, &v) in bits.iter().rev().zip(rtl) {
                    vars[net as usize] = v;
                }
            }
            inputs.extend(bits.iter().map(|&n| vars[n as usize]));
        }
        nm.ports.push((
            name.clone(),
            port.dir == 0,
            (port.is_bus != 0).then_some(bits.len() as u32),
        ));
        if port.dir == 1 {
            nm.outputs.insert(
                name,
                bits.iter().rev().map(|&n| B::V(vars[n as usize])).collect(),
            );
        }
    }
    const UNKNOWN: u32 = u32::MAX;
    const OMIT_NAME: u32 = u32::MAX - 1;
    let mut group_index = vec![UNKNOWN; strings];
    let mut groups: Vec<Group> = Vec::new();
    let mut name_bytes = 0usize;
    let mut exact_probes: Option<HashSet<&str>> = None;
    let mut alias_claims: HashMap<&str, u32> = HashMap::new();
    let mut alias_aux = 0u128;
    for net in f.nets.iter() {
        if net.name == ANONYMOUS {
            continue;
        }
        let mut index = group_index[net.name as usize];
        if index == UNKNOWN {
            let name = raw(string(f, net.name));
            let exact = port_symbols.contains(&net.name) || wanted.contains(name.as_ref());
            let mut probe_name = None;
            if !exact && name.contains("genblk") {
                // Bound a temporary normalization before allocating its text.
                if name.len() > 65536 {
                    return Err(budget("anonymous-scope probe name bytes"));
                }
                if let Some(normalized) = without_anonymous_scopes(&name)
                    && let Some(&probe) = wanted.get(normalized.as_str())
                {
                    if exact_probes.is_none() {
                        alias_aux = wanted.len() as u128 * 64;
                        if base_aux + alias_aux > limits.max_aux_bytes as u128 {
                            return Err(budget("anonymous-scope exact-name index"));
                        }
                        // Borrow requested names; don't duplicate the string table.
                        let mut found = HashSet::new();
                        for candidate in f.nets.iter() {
                            if candidate.name != ANONYMOUS {
                                let raw = raw(string(f, candidate.name));
                                if let Some(&p) = wanted.get(raw.as_ref()) {
                                    found.insert(p);
                                }
                            }
                        }
                        exact_probes = Some(found);
                    }
                    if !exact_probes.as_ref().expect("exact probes").contains(probe) {
                        if base_aux + alias_aux + (alias_claims.len() + 1) as u128 * 64
                            > limits.max_aux_bytes as u128
                        {
                            return Err(budget("anonymous-scope alias index"));
                        }
                        if alias_claims.insert(probe, net.name).is_some() {
                            return Err(format!(
                                "warning[C0005]: anonymous-scope state lookup is ambiguous for {probe}"
                            ));
                        }
                        probe_name = Some(probe.to_owned());
                    }
                }
            }
            index = if exact || probe_name.is_some() {
                name_bytes = name_bytes.saturating_add(name.len());
                let next_aux = base_aux
                    + alias_aux
                    + alias_claims.len() as u128 * 64
                    + (groups.len() + 1) as u128 * 192
                    + name_bytes as u128 * 2
                    + seq_count as u128 * 160
                    + seq_names as u128 * 2
                    + f.cells.len() as u128 * 65536;
                if next_aux > limits.max_aux_bytes as u128 {
                    return Err(budget("state/name/connection auxiliary storage"));
                }
                groups.push(Group {
                    sym: net.name,
                    probe_name,
                    min: net.index,
                    max: net.index,
                    count: 0,
                    vars: Vec::new(),
                });
                (groups.len() - 1) as u32
            } else {
                OMIT_NAME
            };
            group_index[net.name as usize] = index;
        }
        if index != OMIT_NAME {
            let g = &mut groups[index as usize];
            g.min = g.min.min(net.index);
            g.max = g.max.max(net.index);
            g.count += 1;
        }
    }
    let aux = base_aux
        + alias_aux
        + alias_claims.len() as u128 * 64
        + groups.len() as u128 * 192
        + name_bytes as u128 * 2
        + seq_count as u128 * 160
        + seq_names as u128 * 2
        + f.cells.len() as u128 * 65536;
    if aux > limits.max_aux_bytes as u128 {
        return Err(budget("state/name/connection auxiliary storage"));
    }
    for g in &mut groups {
        let width = if g.min == SCALAR && g.max == SCALAR {
            1
        } else {
            if g.min == SCALAR {
                return Err("frozen net mixes scalar/vector bits".into());
            }
            (i64::from(g.max) - i64::from(g.min)).unsigned_abs() as usize + 1
        };
        if width as u64 > MAX_NET_BITS || width != g.count {
            return Err("frozen probe vector is sparse or exceeds the width limit".into());
        }
        g.vars.resize(width, u32::MAX);
    }
    for (id, net) in f.nets.iter().enumerate() {
        if net.name == ANONYMOUS {
            continue;
        }
        let index = group_index[net.name as usize];
        if index == OMIT_NAME {
            continue;
        }
        let g = &mut groups[index as usize];
        let offset = if net.index == SCALAR {
            0
        } else {
            (i64::from(net.index) - i64::from(g.min)) as usize
        };
        if g.vars[offset] != u32::MAX {
            return Err("duplicate frozen probe bit".into());
        }
        g.vars[offset] = vars[id];
    }
    for mut g in groups {
        let name = g
            .probe_name
            .take()
            .unwrap_or_else(|| raw(string(f, g.sym)).into_owned());
        if g.min != SCALAR {
            let range = port_ranges
                .get(&g.sym)
                .copied()
                .unwrap_or((i64::from(g.max), i64::from(g.min)));
            if range.0 < range.1 {
                g.vars.reverse();
            }
            nm.ranges.insert(name.clone(), range);
        }
        if nm.nets.insert(name.clone(), g.vars).is_some() {
            return Err(format!("frozen probe name collision: {name}"));
        }
    }
    drop(group_index);
    let reference = |net: u32| match net {
        CONST0 => B::F,
        CONST1 => B::T,
        n => B::V(vars[n as usize]),
    };
    let define = |c: &mut Circuit, target: B, value: B, what: &str, _at: Span| {
        let B::V(v) = target else {
            return Err(format!("{what}: constant output connection"));
        };
        if inputs.contains(&v) || supplies.contains(&v) {
            return Err(format!("{what} drives an input/supply port"));
        }
        if !c.define(v, value) {
            return Err(format!("{what}: net has more than one driver"));
        }
        Ok(())
    };
    for a in f.assigns.iter() {
        let value = reference(a.rhs);
        if matches!(value, B::V(v) if supplies.contains(&v)) {
            return Err("supply port is read as logic".into());
        }
        define(c, reference(a.lhs), value, "frozen assignment", at)?;
    }
    nm.seq.reserve(seq_count);
    for (id, instance) in f.instances.iter().enumerate() {
        let cell_id = instance.cell as usize;
        let cell = catalog[cell_id];
        if c.defs.len().saturating_add(costs[cell_id]) > limits.max_definitions {
            return Err(budget("circuit definitions"));
        }
        let site = Instance {
            name: Ident {
                name: raw(string(f, instance.name)).into_owned(),
                span: at,
            },
            dims: vec![],
            conns: vec![],
            span: at,
        };
        let mut conn = HashMap::new();
        for pin in f.instance_pins(id as u32) {
            let name = string(
                f,
                f.cell_pin_name[f.cell_pin_start[cell_id] as usize + f.pin_cellpin[pin] as usize],
            );
            if cell.pg_pins.iter().any(|p| p == name) {
                continue;
            }
            let p = cell
                .pin(name)
                .ok_or_else(|| format!("unknown pin {name}"))?;
            if !matches!(p.direction, Direction::Input | Direction::Output) {
                return Err(format!(
                    "instance {} pin {name} is not an input/output",
                    site.name.name
                ));
            }
            let value = reference(f.pin_net[pin]);
            if matches!(value,B::V(v) if supplies.contains(&v)) {
                return Err("supply port is read as logic".into());
            }
            conn.insert(name, (value, at));
        }
        if let Some(seq) = cells::evaluate(c, &sm, &site, cell, &conn, &define)? {
            nm.seq.push(seq);
        }
    }
    Ok(nm)
}
