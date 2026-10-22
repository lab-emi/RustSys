// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Direct canonical frozen tables for explicit, plain-name writer modules.
//! C910: <=1GiB final tables + <=1.5GiB name/range indexes, shared input module.
//! Connections use bounded8192-instance batches (<8MiB), not one graph/thread.
//! Escaped names, implicit nets and whole-bus connections use the general builder.

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

use rusteda_core::netlist::{
    AssignRec, CONST0, CONST1, FrozenNetlist, InstRec, NetRec, PinDir, PortRec, SCALAR,
};

use super::{ConvertError, convert_error, writer_range};
use super::{NlDir, NlModule, NlRef};

#[derive(Debug, Clone, Copy)]
pub struct FrozenBuildLimits {
    pub max_nets: usize,
    pub max_instances: usize,
    pub max_connections: usize,
    pub max_strings: usize,
    pub max_table_bytes: u64,
    pub max_aux_bytes: u64,
    pub max_name_bytes: usize,
}

impl Default for FrozenBuildLimits {
    fn default() -> Self {
        Self {
            max_nets: 20_000_000,
            max_instances: 10_000_000,
            max_connections: 80_000_000,
            max_strings: 32_000_000,
            max_table_bytes: 2 * 1024 * 1024 * 1024,
            max_aux_bytes: 2 * 1024 * 1024 * 1024,
            max_name_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy)]
struct NetRange {
    base: u32,
    width: u32,
    scalar: bool,
}

struct Cell<'a> {
    name: &'a str,
    pins: Vec<(String, PinDir)>,
}

struct Strings<'a> {
    ids: FxHashMap<&'a str, u32>,
    bytes: Vec<u8>,
    offs: Vec<u32>,
}

impl<'a> Strings<'a> {
    fn new() -> Self {
        Self {
            ids: FxHashMap::default(),
            bytes: Vec::new(),
            offs: vec![0],
        }
    }
    fn id(&mut self, text: &'a str) -> u32 {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = (self.offs.len() - 1) as u32;
        self.bytes.extend_from_slice(text.as_bytes());
        self.offs.push(self.bytes.len() as u32);
        self.ids.insert(text, id);
        id
    }
}

fn plain(name: &str, limit: usize) -> bool {
    !name.is_empty()
        && name.len() <= limit
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'/' && b != b'\\')
}

fn resolve(reference: &NlRef, nets: &FxHashMap<&str, NetRange>) -> Option<u32> {
    match reference {
        NlRef::Const(false) => Some(CONST0),
        NlRef::Const(true) => Some(CONST1),
        NlRef::Net(name) => {
            let n = nets.get(name.as_str())?;
            (n.width == 1).then_some(n.base)
        }
        NlRef::Bit(name, bit) => {
            let n = nets.get(name.as_str())?;
            (!n.scalar && *bit < n.width).then(|| n.base + n.width - 1 - *bit)
        }
    }
}

/// Returns None for layouts handled by the general converter. Budget errors
/// are explicit; no partial tables escape. On success, every frozen section is
/// identical to to_gate_netlist(...).freeze(), with complete table validation.
pub fn to_frozen_netlist(
    m: &NlModule,
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
    limits: FrozenBuildLimits,
) -> Result<Option<FrozenNetlist<'static>>, ConvertError> {
    let budget =
        || convert_error("NETLIST_FREEZE_BUDGET: direct frozen-table storage limit reached");
    if m.instances.len() > limits.max_instances {
        return Err(budget());
    }
    if !plain(&m.name, limits.max_name_bytes) {
        if m.name.len() > limits.max_name_bytes {
            return Err(budget());
        }
        return Ok(None);
    }
    let mut net_count = 0usize;
    let mut port_bits = 0usize;
    let mut name_bytes = m.name.len() as u128;
    for (name, width) in m
        .ports
        .iter()
        .map(|p| (&p.name, p.width))
        .chain(m.wires.iter().map(|(n, w)| (n, *w)))
    {
        if !plain(name, limits.max_name_bytes) {
            if name.len() > limits.max_name_bytes {
                return Err(budget());
            }
            return Ok(None);
        }
        writer_range(name, width)?;
        net_count = net_count
            .checked_add(width.unwrap_or(1) as usize)
            .ok_or_else(budget)?;
        name_bytes += name.len() as u128;
    }
    for port in &m.ports {
        port_bits += port.width.unwrap_or(1) as usize;
    }
    let mut connections = 0usize;
    let mut cell_of = FxHashMap::default();
    let mut catalog = Vec::<Cell<'_>>::new();
    let mut catalog_owned_bytes = 0u128;
    for instance in &m.instances {
        if !plain(&instance.name, limits.max_name_bytes)
            || !plain(&instance.cell, limits.max_name_bytes)
            || instance.cell == m.name
        {
            if instance.name.len() > limits.max_name_bytes
                || instance.cell.len() > limits.max_name_bytes
            {
                return Err(budget());
            }
            return Ok(None);
        }
        name_bytes += instance.name.len() as u128;
        connections = connections
            .checked_add(instance.conns.len())
            .ok_or_else(budget)?;
        if !cell_of.contains_key(instance.cell.as_str()) {
            if catalog.len() >= 65_536 {
                return Err(budget());
            }
            let Some(pins) = cell_pins(&instance.cell) else {
                return Ok(None);
            };
            if pins.len() > 64
                || pins.iter().enumerate().any(|(i, (name, _))| {
                    !plain(name, limits.max_name_bytes) || pins[..i].iter().any(|(n, _)| n == name)
                })
            {
                if pins
                    .iter()
                    .any(|(name, _)| name.len() > limits.max_name_bytes)
                {
                    return Err(budget());
                }
                return Ok(None);
            }
            name_bytes += instance.cell.len() as u128
                + pins.iter().map(|(n, _)| n.len() as u128).sum::<u128>();
            catalog_owned_bytes += pins.capacity() as u128 * size_of::<(String, PinDir)>() as u128
                + pins.iter().map(|(n, _)| n.capacity() as u128).sum::<u128>();
            // Check while collecting cells, before retaining another owned pin
            // catalog. The final table estimate alone would be too late here.
            if name_bytes > limits.max_table_bytes as u128
                || catalog_owned_bytes + (catalog.len() as u128 + 1) * 128
                    > limits.max_aux_bytes as u128
            {
                return Err(budget());
            }
            cell_of.insert(instance.cell.as_str(), catalog.len() as u32);
            catalog.push(Cell {
                name: &instance.cell,
                pins,
            });
        }
    }
    let cell_pin_count = catalog.iter().map(|c| c.pins.len()).sum::<usize>();
    let strings = 1u128
        + catalog.len() as u128
        + cell_pin_count as u128
        + m.ports.len() as u128
        + m.wires.len() as u128
        + m.instances.len() as u128;
    let table_bytes = name_bytes
        + (strings + 1) * 4
        + catalog.len() as u128 * 8
        + cell_pin_count as u128 * 5
        + m.ports.len() as u128 * 20
        + port_bits as u128 * 4
        + net_count as u128 * 12
        + m.instances.len() as u128 * 12
        + connections as u128 * 12
        + m.assigns.len() as u128 * 8
        + 128;
    let aux_bytes = catalog_owned_bytes
        + strings * 64
        + (m.ports.len() + m.wires.len()) as u128 * 64
        + m.instances.len() as u128 * 32
        + net_count as u128 * 4
        + 8 * 1024 * 1024;
    if net_count > limits.max_nets
        || connections > limits.max_connections
        || strings > limits.max_strings as u128
        || table_bytes > limits.max_table_bytes as u128
        || aux_bytes > limits.max_aux_bytes as u128
        || table_bytes >= u32::MAX as u128
    {
        return Err(budget());
    }
    let mut strings = Strings::new();
    let module_name = strings.id(&m.name);
    let mut cells = Vec::with_capacity(catalog.len());
    let mut cell_pin_start = vec![0];
    let mut cell_pin_name = Vec::with_capacity(cell_pin_count);
    let mut cell_pin_dir = Vec::with_capacity(cell_pin_count);
    for cell in &catalog {
        cells.push(strings.id(cell.name));
        for (name, dir) in &cell.pins {
            cell_pin_name.push(strings.id(name));
            cell_pin_dir.push(match dir {
                PinDir::Input => 0,
                PinDir::Output => 1,
                PinDir::Inout => 2,
                PinDir::Other => 3,
            });
        }
        cell_pin_start.push(cell_pin_name.len() as u32);
    }
    let mut declared: FxHashMap<&str, NetRange> = FxHashMap::default();
    let mut nets = Vec::with_capacity(net_count);
    let mut ports = Vec::with_capacity(m.ports.len());
    let mut port_bit_start = vec![0];
    let mut port_bit_net = Vec::with_capacity(port_bits);
    for port in &m.ports {
        if declared.contains_key(port.name.as_str()) {
            return Ok(None);
        }
        let name = strings.id(&port.name);
        let width = port.width.unwrap_or(1);
        let base = nets.len() as u32;
        declared.insert(
            &port.name,
            NetRange {
                base,
                width,
                scalar: port.width.is_none(),
            },
        );
        ports.push(PortRec {
            name,
            dir: match port.dir {
                NlDir::Input => 0,
                NlDir::Output => 1,
            },
            is_bus: u8::from(port.width.is_some()),
            _pad: 0,
            msb: if port.width.is_some() {
                width as i32 - 1
            } else {
                0
            },
            lsb: 0,
        });
        for offset in 0..width {
            nets.push(NetRec {
                name,
                index: if port.width.is_some() {
                    (width - 1 - offset) as i32
                } else {
                    SCALAR
                },
            });
            port_bit_net.push(base + offset);
        }
        port_bit_start.push(port_bit_net.len() as u32);
    }
    for (wire, width) in &m.wires {
        if declared.contains_key(wire.as_str()) {
            return Ok(None);
        }
        let name = strings.id(wire);
        let count = width.unwrap_or(1);
        let base = nets.len() as u32;
        declared.insert(
            wire,
            NetRange {
                base,
                width: count,
                scalar: width.is_none(),
            },
        );
        for offset in 0..count {
            nets.push(NetRec {
                name,
                index: if width.is_some() {
                    (count - 1 - offset) as i32
                } else {
                    SCALAR
                },
            });
        }
    }
    let mut instances = Vec::with_capacity(m.instances.len());
    let mut inst_pin_start = vec![0];
    let mut instance_names = FxHashSet::default();
    let mut pin_count = 0u32;
    for instance in &m.instances {
        if declared.contains_key(instance.name.as_str())
            || !instance_names.insert(instance.name.as_str())
        {
            return Ok(None);
        }
        instances.push(InstRec {
            name: strings.id(&instance.name),
            cell: cell_of[instance.cell.as_str()],
        });
        pin_count += instance.conns.len() as u32;
        inst_pin_start.push(pin_count);
    }
    drop(instance_names);
    drop(strings.ids);
    let mut pin_cellpin = Vec::with_capacity(connections);
    let mut pin_net = Vec::with_capacity(connections);
    for batch in m.instances.chunks(8192) {
        let build = |instance: &super::NlInstance| -> Option<SmallVec<[(u32, u32); 8]>> {
            let cell = &catalog[cell_of[instance.cell.as_str()] as usize];
            let mut used = 0u64;
            let mut pairs = SmallVec::new();
            for (name, reference) in &instance.conns {
                let pin = cell.pins.iter().position(|(p, _)| p == name)?;
                let mask = 1u64 << pin;
                if used & mask != 0 {
                    return None;
                }
                used |= mask;
                pairs.push((pin as u32, resolve(reference, &declared)?));
            }
            Some(pairs)
        };
        let rows: Vec<_> = if batch.len() < 64 || rayon::current_num_threads() == 1 {
            batch.iter().map(build).collect()
        } else {
            batch.par_iter().with_min_len(64).map(build).collect()
        };
        for row in rows {
            let Some(row) = row else {
                return Ok(None);
            };
            for (pin, net) in row {
                pin_cellpin.push(pin);
                pin_net.push(net);
            }
        }
    }
    let mut assigns = Vec::with_capacity(m.assigns.len());
    for (lhs, rhs) in &m.assigns {
        let (Some(lhs), Some(rhs)) = (resolve(lhs, &declared), resolve(rhs, &declared)) else {
            return Ok(None);
        };
        if lhs >= CONST0 {
            return Ok(None);
        }
        assigns.push(AssignRec { lhs, rhs });
    }
    drop(declared);
    let mut net_pin_start = vec![0u32; net_count + 1];
    for &net in &pin_net {
        if net < CONST0 {
            net_pin_start[net as usize + 1] += 1;
        }
    }
    for i in 0..net_count {
        net_pin_start[i + 1] += net_pin_start[i];
    }
    let mut cursor = net_pin_start.clone();
    let mut net_pins = vec![0; net_pin_start[net_count] as usize];
    for (pin, &net) in pin_net.iter().enumerate() {
        if net < CONST0 {
            let at = &mut cursor[net as usize];
            net_pins[*at as usize] = pin as u32;
            *at += 1;
        }
    }
    let module = vec![
        module_name,
        ports.len() as u32,
        nets.len() as u32,
        instances.len() as u32,
        pin_net.len() as u32,
        assigns.len() as u32,
    ];
    let frozen = FrozenNetlist {
        str_bytes: strings.bytes.into(),
        str_offs: strings.offs.into(),
        module: module.into(),
        cells: cells.into(),
        cell_pin_start: cell_pin_start.into(),
        cell_pin_name: cell_pin_name.into(),
        cell_pin_dir: cell_pin_dir.into(),
        ports: ports.into(),
        port_bit_start: port_bit_start.into(),
        port_bit_net: port_bit_net.into(),
        nets: nets.into(),
        instances: instances.into(),
        inst_pin_start: inst_pin_start.into(),
        pin_cellpin: pin_cellpin.into(),
        pin_net: pin_net.into(),
        assigns: assigns.into(),
        net_pin_start: net_pin_start.into(),
        net_pins: net_pins.into(),
    };
    frozen
        .validate()
        .map_err(|e| convert_error(format!("direct frozen netlist: {e}")))?;
    Ok(Some(frozen))
}

#[cfg(test)]
mod tests {
    use super::super::{NlInstance, NlPort, to_gate_netlist};
    use super::*;

    fn pins(name: &str) -> Option<Vec<(String, PinDir)>> {
        (name == "gate").then(|| {
            vec![
                ("Y".into(), PinDir::Output),
                ("B".into(), PinDir::Input),
                ("A".into(), PinDir::Input),
            ]
        })
    }

    fn module() -> NlModule {
        let mut m = NlModule {
            name: "top".into(),
            ports: vec![
                NlPort {
                    name: "A".into(),
                    dir: NlDir::Input,
                    width: Some(4),
                },
                NlPort {
                    name: "Y".into(),
                    dir: NlDir::Output,
                    width: Some(1),
                },
            ],
            ..Default::default()
        };
        m.wires.push(("bus".into(), Some(4)));
        for i in 0..256 {
            let wire = format!("stage.q[{i}]");
            m.wires.push((wire.clone(), None));
            m.instances.push(NlInstance {
                cell: "gate".into(),
                name: format!("g{i}"),
                conns: vec![
                    ("A".into(), NlRef::Bit("A".into(), i % 4)),
                    ("Y".into(), NlRef::Net(wire)),
                    ("B".into(), NlRef::Const(i % 2 == 0)),
                ],
            });
        }
        m.assigns
            .push((NlRef::Bit("Y".into(), 0), NlRef::Net("stage.q[0]".into())));
        m
    }

    // VERILOG-REQ-0301, VERILOG-REQ-0317: <5000 records, <32MiB including
    // independent mutable/frozen copies; all18sections and worker counts match.
    #[test]
    fn all_frozen_sections_match_independent_mutable_builder() {
        let m = module();
        let old = to_gate_netlist(&m, &pins).unwrap().freeze();
        for threads in [1, 2, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let new = pool.install(|| {
                to_frozen_netlist(&m, &pins, FrozenBuildLimits::default())
                    .unwrap()
                    .unwrap()
            });
            assert_eq!(old, new);
            new.validate().unwrap();
        }
    }

    // VERILOG-REQ-0317, VERILOG-REQ-0319: the optimization cannot reinterpret
    // escaped names, implicit nets or whole-vector assignments.
    #[test]
    fn general_layouts_and_diagnostics_keep_the_existing_converter() {
        let mut escaped = module();
        escaped.wires.push(("user/path".into(), None));
        let mut implicit = module();
        implicit.instances[0].conns[0].1 = NlRef::Net("implicit".into());
        let mut vector = module();
        vector
            .assigns
            .push((NlRef::Net("bus".into()), NlRef::Net("A".into())));
        for m in [escaped, implicit, vector] {
            assert!(
                to_frozen_netlist(&m, &pins, FrozenBuildLimits::default())
                    .unwrap()
                    .is_none()
            );
            to_gate_netlist(&m, &pins)
                .unwrap()
                .freeze()
                .validate()
                .unwrap();
        }
        let mut invalid = module();
        let duplicate = invalid.instances[0].conns[0].clone();
        invalid.instances[0].conns.push(duplicate);
        assert!(
            to_frozen_netlist(&invalid, &pins, FrozenBuildLimits::default())
                .unwrap()
                .is_none()
        );
        assert!(to_gate_netlist(&invalid, &pins).is_err());
    }

    // CORE-REQ-0137, VERILOG-REQ-0315: no partial result on an explicit budget.
    #[test]
    fn direct_freeze_budgets_fail_before_materializing_large_tables() {
        let m = module();
        for limits in [
            FrozenBuildLimits {
                max_nets: 0,
                ..Default::default()
            },
            FrozenBuildLimits {
                max_table_bytes: 0,
                ..Default::default()
            },
            FrozenBuildLimits {
                max_aux_bytes: 0,
                ..Default::default()
            },
            FrozenBuildLimits {
                max_name_bytes: 0,
                ..Default::default()
            },
        ] {
            let error = to_frozen_netlist(&m, &pins, limits).unwrap_err();
            assert!(error.message.contains("NETLIST_FREEZE_BUDGET"));
        }
    }

    // Charge retained catalog capacity before asking for the next cell type.
    #[test]
    fn pin_catalog_storage_is_bounded_during_preflight() {
        let mut m = module();
        let mut extra = m.instances[0].clone();
        extra.name = "extra".into();
        extra.cell = "another_cell".into();
        m.instances.push(extra);
        let calls = std::cell::Cell::new(0);
        let lookup = |_: &str| {
            calls.set(calls.get() + 1);
            let mut name = String::with_capacity(4096);
            name.push('A');
            Some(vec![(name, PinDir::Input)])
        };
        let error = to_frozen_netlist(
            &m,
            &lookup,
            FrozenBuildLimits {
                max_aux_bytes: 1024,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.message.contains("NETLIST_FREEZE_BUDGET"));
        assert_eq!(calls.get(), 1);
    }
}
