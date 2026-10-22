// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Frozen netlist: the dense tables of a [`GateNetlist`] in a canonical order, their section
//! bytes, and the conversions from and to the editable netlist.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use bytemuck::{Pod, Zeroable};

use super::{Conn, GateNetlist, NetId, NetlistError, PinDir, PinRef, PortDir};
use crate::symbol::{SymbolId, SymbolInterner};

/// Version of the frozen layout.
pub const FORMAT_VERSION: u32 = 1;
/// `pin_net` and assignment right-hand-side value of the constant 0 (CORE-REQ-0132).
pub const CONST0: u32 = 0xFFFF_FFFE;
/// `pin_net` and assignment right-hand-side value of the constant 1 (CORE-REQ-0132).
pub const CONST1: u32 = 0xFFFF_FFFF;
/// Net name of an anonymous net (CORE-REQ-0132).
pub const ANONYMOUS: u32 = u32::MAX;
/// Bit index of a scalar net (CORE-REQ-0132).
pub const SCALAR: i32 = i32::MIN;

/// Section ids of the netlist file, in the order of the [`FrozenNetlist`] fields.
pub mod section {
    pub const STR_BYTES: u32 = 1;
    pub const STR_OFFS: u32 = 2;
    pub const MODULE: u32 = 3;
    pub const CELLS: u32 = 4;
    pub const CELL_PIN_START: u32 = 5;
    pub const CELL_PIN_NAME: u32 = 6;
    pub const CELL_PIN_DIR: u32 = 7;
    pub const PORTS: u32 = 8;
    pub const PORT_BIT_START: u32 = 9;
    pub const PORT_BIT_NET: u32 = 10;
    pub const NETS: u32 = 11;
    pub const INSTANCES: u32 = 12;
    pub const INST_PIN_START: u32 = 13;
    pub const PIN_CELLPIN: u32 = 14;
    pub const PIN_NET: u32 = 15;
    pub const ASSIGNS: u32 = 16;
    pub const NET_PIN_START: u32 = 17;
    pub const NET_PINS: u32 = 18;
    /// Number of sections.
    pub const COUNT: usize = 18;

    /// The element size in bytes of a section id, `None` for an unknown id; a netlist file
    /// declares these sizes in its section table.
    pub const fn elem_size(id: u32) -> Option<u32> {
        const SIZES: [u32; COUNT] = [1, 4, 4, 4, 4, 4, 1, 16, 4, 4, 8, 8, 4, 4, 4, 8, 4, 4];
        if id >= 1 && id as usize <= COUNT {
            Some(SIZES[id as usize - 1])
        } else {
            None
        }
    }

    /// The name of a section id.
    pub fn name(id: u32) -> &'static str {
        const NAMES: [&str; COUNT] = [
            "STR_BYTES",
            "STR_OFFS",
            "MODULE",
            "CELLS",
            "CELL_PIN_START",
            "CELL_PIN_NAME",
            "CELL_PIN_DIR",
            "PORTS",
            "PORT_BIT_START",
            "PORT_BIT_NET",
            "NETS",
            "INSTANCES",
            "INST_PIN_START",
            "PIN_CELLPIN",
            "PIN_NET",
            "ASSIGNS",
            "NET_PIN_START",
            "NET_PINS",
        ];
        (id as usize)
            .checked_sub(1)
            .and_then(|k| NAMES.get(k))
            .copied()
            .unwrap_or("unknown")
    }
}

/// Positions of the entries of the `module` section, which has exactly [`module_entry::COUNT`]
/// entries: the module name and the numbers of entities.
pub mod module_entry {
    /// String id of the module name.
    pub const NAME: usize = 0;
    pub const N_PORTS: usize = 1;
    pub const N_NETS: usize = 2;
    pub const N_INSTS: usize = 3;
    /// Connected pins: the rows of `pin_net`.
    pub const N_PINS: usize = 4;
    pub const N_ASSIGNS: usize = 5;
    /// Number of entries.
    pub const COUNT: usize = 6;
}

/// A port: name, direction (0 input, 1 output, 2 inout) and, for a bus (`is_bus == 1`), its
/// range; a scalar port has `msb == lsb == 0` (CORE-REQ-0131).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct PortRec {
    pub name: u32,
    pub dir: u8,
    pub is_bus: u8,
    /// Always 0.
    pub _pad: u16,
    pub msb: i32,
    pub lsb: i32,
}

/// A net: name or [`ANONYMOUS`], bit index or [`SCALAR`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct NetRec {
    pub name: u32,
    pub index: i32,
}

/// An instance: name and cell.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct InstRec {
    pub name: u32,
    pub cell: u32,
}

/// An assignment: left-hand net and right-hand net or constant ([`CONST0`], [`CONST1`]).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct AssignRec {
    pub lhs: u32,
    pub rhs: u32,
}

/// The dense tables of a [`GateNetlist`] in a canonical order (NETLIST format version
/// [`FORMAT_VERSION`]).
///
/// Each field is one section of a netlist file (CORE-REQ-0130): a slice of plain records in host
/// byte order, borrowed (for example from a mapped file) or owned. Entities are numbered densely:
/// the live entries in id index order (CORE-REQ-0133). Every string is stored once, and the
/// string table lists the strings in the order of their first use when visiting the module name,
/// each cell name followed by its pin names, the port names, the net names and the instance
/// names (CORE-REQ-0134). Every string is a name of printable ASCII characters, and a string that
/// names a port, net or instance is a hierarchical name, whatever else it names (CORE-REQ-0110,
/// 0111). A `*_start` section holds the start of each row of the section after it, plus its end
/// (rows + 1 entries, compressed sparse rows).
///
/// | id | section | element | content |
/// |---|---|---|---|
/// | 1 | `str_bytes` | u8 | bytes of the strings (printable ASCII) |
/// | 2 | `str_offs` | u32 | start of each string, plus the end |
/// | 3 | `module` | u32 | module name and numbers of entities ([`module_entry`]) |
/// | 4 | `cells` | u32 | cell name |
/// | 5 | `cell_pin_start` | u32 | rows of cell pins, per cell |
/// | 6 | `cell_pin_name` | u32 | pin name |
/// | 7 | `cell_pin_dir` | u8 | 0 input, 1 output, 2 inout, 3 other |
/// | 8 | `ports` | [`PortRec`] | name, direction, bus range |
/// | 9 | `port_bit_start` | u32 | rows of bits, per port |
/// | 10 | `port_bit_net` | u32 | net of each bit, from msb to lsb |
/// | 11 | `nets` | [`NetRec`] | name ([`ANONYMOUS`]) and bit index ([`SCALAR`]) |
/// | 12 | `instances` | [`InstRec`] | name and cell |
/// | 13 | `inst_pin_start` | u32 | rows of connected pins, per instance |
/// | 14 | `pin_cellpin` | u32 | cell pin index, distinct within an instance |
/// | 15 | `pin_net` | u32 | net, or [`CONST0`] / [`CONST1`] |
/// | 16 | `assigns` | [`AssignRec`] | assignments in order |
/// | 17 | `net_pin_start` | u32 | rows of connected pins, per net |
/// | 18 | `net_pins` | u32 | pin (row of `pin_net`), increasing within a net |
///
/// Only connected pins are stored, in the order they were connected; the other pins of an
/// instance are unconnected. [`section::elem_size`] gives the element size of each section.
/// Thawing gives every instance all the pins of its cell, so validation bounds the pins of all
/// instances together, connected or not: at most one per byte of the tables (or 2^24 for
/// smaller tables), never more than `u32::MAX`. A short file therefore cannot make `thaw`
/// exhaust memory (CORE-REQ-0137). [`FrozenNetlist::validate`] accepts exactly the tables that
/// [`GateNetlist::freeze`] can produce within this bound (CORE-REQ-0135), so `thaw` followed by
/// `freeze` gives back every valid frozen netlist unchanged (CORE-REQ-0136).
///
/// Queries can use the tables without thawing (CORE-REQ-0138): [`FrozenNetlist::string`] reads
/// a string, and [`FrozenNetlist::cell_pins`], [`FrozenNetlist::instance_pins`],
/// [`FrozenNetlist::net_pins_of`], [`FrozenNetlist::port_bits`],
/// [`FrozenNetlist::pin_instance`] and [`FrozenNetlist::pin_dir`] give the rows and relations.
/// These assume validated tables: like slice indexing, they panic on an index out of range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenNetlist<'a> {
    pub str_bytes: Cow<'a, [u8]>,
    pub str_offs: Cow<'a, [u32]>,
    pub module: Cow<'a, [u32]>,
    pub cells: Cow<'a, [u32]>,
    pub cell_pin_start: Cow<'a, [u32]>,
    pub cell_pin_name: Cow<'a, [u32]>,
    pub cell_pin_dir: Cow<'a, [u8]>,
    pub ports: Cow<'a, [PortRec]>,
    pub port_bit_start: Cow<'a, [u32]>,
    pub port_bit_net: Cow<'a, [u32]>,
    pub nets: Cow<'a, [NetRec]>,
    pub instances: Cow<'a, [InstRec]>,
    pub inst_pin_start: Cow<'a, [u32]>,
    pub pin_cellpin: Cow<'a, [u32]>,
    pub pin_net: Cow<'a, [u32]>,
    pub assigns: Cow<'a, [AssignRec]>,
    pub net_pin_start: Cow<'a, [u32]>,
    pub net_pins: Cow<'a, [u32]>,
}

/// A frozen netlist that breaks a rule of its format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenError {
    /// The section id ([`section`]) where the problem was found.
    pub section: u32,
    pub message: String,
}

impl FrozenError {
    pub(crate) fn new(section: u32, message: impl Into<String>) -> FrozenError {
        FrozenError {
            section,
            message: message.into(),
        }
    }
}

impl fmt::Display for FrozenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "netlist section {} ({}): {}",
            self.section,
            section::name(self.section),
            self.message
        )
    }
}

impl std::error::Error for FrozenError {}

pub(crate) fn pin_dir_code(d: PinDir) -> u8 {
    match d {
        PinDir::Input => 0,
        PinDir::Output => 1,
        PinDir::Inout => 2,
        PinDir::Other => 3,
    }
}

fn pin_dir_of(code: u8) -> PinDir {
    match code {
        0 => PinDir::Input,
        1 => PinDir::Output,
        2 => PinDir::Inout,
        _ => PinDir::Other,
    }
}

fn port_dir_code(d: PortDir) -> u8 {
    match d {
        PortDir::Input => 0,
        PortDir::Output => 1,
        PortDir::Inout => 2,
    }
}

fn port_dir_of(code: u8) -> PortDir {
    match code {
        0 => PortDir::Input,
        1 => PortDir::Output,
        _ => PortDir::Inout,
    }
}

/// Converts a table length or offset to its stored width.
///
/// # Panics
///
/// When the value does not fit 32 bits (a netlist with more than 4 GiB of names, for example).
fn u32_of(n: usize) -> u32 {
    u32::try_from(n).expect("netlist table exceeds 32-bit offsets")
}

/// The string table under construction: strings numbered by first use.
struct Strings<'s> {
    symbols: &'s SymbolInterner,
    ids: Vec<u32>,
    bytes: Vec<u8>,
    offs: Vec<u32>,
}

impl Strings<'_> {
    fn id(&mut self, s: SymbolId) -> u32 {
        let slot = &mut self.ids[s.index() as usize];
        if *slot == u32::MAX {
            *slot = u32_of(self.offs.len() - 1);
            self.bytes
                .extend_from_slice(self.symbols.resolve(s).as_bytes());
            self.offs.push(u32_of(self.bytes.len()));
        }
        *slot
    }
}

impl GateNetlist {
    /// The dense, canonical tables of this netlist (see [`FrozenNetlist`]).
    ///
    /// # Panics
    ///
    /// When a table outgrows 32-bit offsets (more than 4 GiB of names).
    pub fn freeze(&self) -> FrozenNetlist<'static> {
        let mut st = Strings {
            symbols: &self.symbols,
            ids: vec![u32::MAX; self.symbols.len()],
            bytes: Vec::new(),
            offs: vec![0],
        };
        let module_name = st.id(self.name);
        let mut cell_ix = vec![u32::MAX; self.cells.slot_count()];
        let mut cells = Vec::with_capacity(self.cells.len());
        let mut cell_pin_start = vec![0];
        let mut cell_pin_name = Vec::new();
        let mut cell_pin_dir = Vec::new();
        for (k, (id, c)) in self.cells.iter().enumerate() {
            cell_ix[id.index() as usize] = u32_of(k);
            cells.push(st.id(c.name));
            for p in &c.pins {
                cell_pin_name.push(st.id(p.name));
                cell_pin_dir.push(pin_dir_code(p.dir));
            }
            cell_pin_start.push(u32_of(cell_pin_name.len()));
        }
        let mut net_ix = vec![u32::MAX; self.nets.slot_count()];
        for (k, id) in self.nets.ids().enumerate() {
            net_ix[id.index() as usize] = u32_of(k);
        }
        let net = |id: NetId| net_ix[id.index() as usize];
        let encode = |c: Conn| match c {
            Conn::Net(n) => net(n),
            Conn::Const(false) => CONST0,
            Conn::Const(true) => CONST1,
        };
        let mut ports = Vec::with_capacity(self.ports.len());
        let mut port_bit_start = vec![0];
        let mut port_bit_net = Vec::new();
        for p in &self.ports {
            let (msb, lsb) = p.range.unwrap_or((0, 0));
            ports.push(PortRec {
                name: st.id(p.name),
                dir: port_dir_code(p.dir),
                is_bus: u8::from(p.range.is_some()),
                _pad: 0,
                msb,
                lsb,
            });
            port_bit_net.extend(p.bits.iter().map(|&b| net(b)));
            port_bit_start.push(u32_of(port_bit_net.len()));
        }
        let nets: Vec<NetRec> = self
            .nets
            .iter()
            .map(|(_, n)| NetRec {
                name: n.name.map_or(ANONYMOUS, |s| st.id(s)),
                index: n.index.unwrap_or(SCALAR),
            })
            .collect();
        let mut instances = Vec::with_capacity(self.insts.len());
        let mut inst_pin_start = vec![0];
        let mut pin_cellpin = Vec::new();
        let mut pin_net = Vec::new();
        for (_, i) in self.insts.iter() {
            instances.push(InstRec {
                name: st.id(i.name),
                cell: cell_ix[i.cell.index() as usize],
            });
            for &p in &i.connected {
                let pin = &self.pins[p];
                if let Some(c) = pin.conn {
                    pin_cellpin.push(pin.cell_pin);
                    pin_net.push(encode(c));
                }
            }
            inst_pin_start.push(u32_of(pin_net.len()));
        }
        let assigns: Vec<AssignRec> = self
            .assigns
            .iter()
            .map(|a| AssignRec {
                lhs: net(a.lhs),
                rhs: encode(a.rhs),
            })
            .collect();
        // Pins per net by a counting sort, which keeps pin order within a net.
        let n_nets = nets.len();
        let mut net_pin_start = vec![0u32; n_nets + 1];
        for &v in &pin_net {
            if (v as usize) < n_nets {
                net_pin_start[v as usize + 1] += 1;
            }
        }
        for k in 0..n_nets {
            net_pin_start[k + 1] += net_pin_start[k];
        }
        let mut fill = net_pin_start.clone();
        let mut net_pins = vec![0u32; net_pin_start[n_nets] as usize];
        for (p, &v) in pin_net.iter().enumerate() {
            if (v as usize) < n_nets {
                net_pins[fill[v as usize] as usize] = u32_of(p);
                fill[v as usize] += 1;
            }
        }
        // In the order of `module_entry`.
        let module = vec![
            module_name,
            u32_of(ports.len()),
            u32_of(n_nets),
            u32_of(instances.len()),
            u32_of(pin_net.len()),
            u32_of(assigns.len()),
        ];
        FrozenNetlist {
            str_bytes: Cow::Owned(st.bytes),
            str_offs: Cow::Owned(st.offs),
            module: Cow::Owned(module),
            cells: Cow::Owned(cells),
            cell_pin_start: Cow::Owned(cell_pin_start),
            cell_pin_name: Cow::Owned(cell_pin_name),
            cell_pin_dir: Cow::Owned(cell_pin_dir),
            ports: Cow::Owned(ports),
            port_bit_start: Cow::Owned(port_bit_start),
            port_bit_net: Cow::Owned(port_bit_net),
            nets: Cow::Owned(nets),
            instances: Cow::Owned(instances),
            inst_pin_start: Cow::Owned(inst_pin_start),
            pin_cellpin: Cow::Owned(pin_cellpin),
            pin_net: Cow::Owned(pin_net),
            assigns: Cow::Owned(assigns),
            net_pin_start: Cow::Owned(net_pin_start),
            net_pins: Cow::Owned(net_pins),
        }
    }
}

/// Reads one section from its declared element size and its bytes: borrowed when aligned,
/// copied otherwise.
fn section_of<'a, T: Pod>(
    id: u32,
    (elem_size, bytes): (u32, &'a [u8]),
) -> Result<Cow<'a, [T]>, FrozenError> {
    let size = size_of::<T>();
    debug_assert_eq!(section::elem_size(id), Some(size as u32));
    if elem_size as usize != size {
        return Err(FrozenError::new(
            id,
            format!("element size {elem_size}, expected {size}"),
        ));
    }
    if !bytes.len().is_multiple_of(size) {
        return Err(FrozenError::new(
            id,
            format!(
                "{} bytes are not a whole number of {size}-byte elements",
                bytes.len()
            ),
        ));
    }
    Ok(match bytemuck::try_cast_slice(bytes) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => {
            let mut v = vec![T::zeroed(); bytes.len() / size];
            bytemuck::cast_slice_mut::<T, u8>(&mut v).copy_from_slice(bytes);
            Cow::Owned(v)
        }
    })
}

/// Row `k` of a start section. Entry `k` is read first, so `k + 1` cannot overflow.
fn csr_row(start: &[u32], k: u32) -> Range<usize> {
    let k = k as usize;
    start[k] as usize..start[k + 1] as usize
}

/// The bytes of string `k`: from entry `k` of the offsets to entry `k + 1`, or `None` when an
/// entry is missing. `k` comes from the tables, so `k + 1` is checked: on a 32-bit target
/// [`ANONYMOUS`] as an index is `usize::MAX`.
pub(super) fn string_bytes(offs: &[u32], k: usize) -> Option<Range<usize>> {
    let a = *offs.get(k)?;
    let b = *offs.get(k.checked_add(1)?)?;
    Some(a as usize..b as usize)
}

impl<'a> FrozenNetlist<'a> {
    /// The tables as owned data.
    pub fn into_owned(self) -> FrozenNetlist<'static> {
        FrozenNetlist {
            str_bytes: Cow::Owned(self.str_bytes.into_owned()),
            str_offs: Cow::Owned(self.str_offs.into_owned()),
            module: Cow::Owned(self.module.into_owned()),
            cells: Cow::Owned(self.cells.into_owned()),
            cell_pin_start: Cow::Owned(self.cell_pin_start.into_owned()),
            cell_pin_name: Cow::Owned(self.cell_pin_name.into_owned()),
            cell_pin_dir: Cow::Owned(self.cell_pin_dir.into_owned()),
            ports: Cow::Owned(self.ports.into_owned()),
            port_bit_start: Cow::Owned(self.port_bit_start.into_owned()),
            port_bit_net: Cow::Owned(self.port_bit_net.into_owned()),
            nets: Cow::Owned(self.nets.into_owned()),
            instances: Cow::Owned(self.instances.into_owned()),
            inst_pin_start: Cow::Owned(self.inst_pin_start.into_owned()),
            pin_cellpin: Cow::Owned(self.pin_cellpin.into_owned()),
            pin_net: Cow::Owned(self.pin_net.into_owned()),
            assigns: Cow::Owned(self.assigns.into_owned()),
            net_pin_start: Cow::Owned(self.net_pin_start.into_owned()),
            net_pins: Cow::Owned(self.net_pins.into_owned()),
        }
    }

    /// The sections as `(id, element size, bytes)` in id order, for writing a netlist file.
    pub fn sections(&self) -> [(u32, u32, &[u8]); section::COUNT] {
        fn s<T: Pod>(id: u32, v: &[T]) -> (u32, u32, &[u8]) {
            let size = size_of::<T>() as u32;
            debug_assert_eq!(section::elem_size(id), Some(size));
            (id, size, bytemuck::cast_slice(v))
        }
        [
            s(section::STR_BYTES, &self.str_bytes),
            s(section::STR_OFFS, &self.str_offs),
            s(section::MODULE, &self.module),
            s(section::CELLS, &self.cells),
            s(section::CELL_PIN_START, &self.cell_pin_start),
            s(section::CELL_PIN_NAME, &self.cell_pin_name),
            s(section::CELL_PIN_DIR, &self.cell_pin_dir),
            s(section::PORTS, &self.ports),
            s(section::PORT_BIT_START, &self.port_bit_start),
            s(section::PORT_BIT_NET, &self.port_bit_net),
            s(section::NETS, &self.nets),
            s(section::INSTANCES, &self.instances),
            s(section::INST_PIN_START, &self.inst_pin_start),
            s(section::PIN_CELLPIN, &self.pin_cellpin),
            s(section::PIN_NET, &self.pin_net),
            s(section::ASSIGNS, &self.assigns),
            s(section::NET_PIN_START, &self.net_pin_start),
            s(section::NET_PINS, &self.net_pins),
        ]
    }

    /// A frozen netlist over the sections of a netlist file, for example a mapped one: `section`
    /// returns the declared element size and the bytes of a section id. Each declared size must
    /// be the [`section::elem_size`] of its section and each length a whole number of elements.
    /// Aligned sections are borrowed, others are copied. The content is not checked: call
    /// [`FrozenNetlist::validate`] before using it.
    pub fn from_sections(
        section: impl Fn(u32) -> Option<(u32, &'a [u8])>,
    ) -> Result<FrozenNetlist<'a>, FrozenError> {
        let get = |id: u32| section(id).ok_or_else(|| FrozenError::new(id, "section missing"));
        Ok(FrozenNetlist {
            str_bytes: section_of(section::STR_BYTES, get(section::STR_BYTES)?)?,
            str_offs: section_of(section::STR_OFFS, get(section::STR_OFFS)?)?,
            module: section_of(section::MODULE, get(section::MODULE)?)?,
            cells: section_of(section::CELLS, get(section::CELLS)?)?,
            cell_pin_start: section_of(section::CELL_PIN_START, get(section::CELL_PIN_START)?)?,
            cell_pin_name: section_of(section::CELL_PIN_NAME, get(section::CELL_PIN_NAME)?)?,
            cell_pin_dir: section_of(section::CELL_PIN_DIR, get(section::CELL_PIN_DIR)?)?,
            ports: section_of(section::PORTS, get(section::PORTS)?)?,
            port_bit_start: section_of(section::PORT_BIT_START, get(section::PORT_BIT_START)?)?,
            port_bit_net: section_of(section::PORT_BIT_NET, get(section::PORT_BIT_NET)?)?,
            nets: section_of(section::NETS, get(section::NETS)?)?,
            instances: section_of(section::INSTANCES, get(section::INSTANCES)?)?,
            inst_pin_start: section_of(section::INST_PIN_START, get(section::INST_PIN_START)?)?,
            pin_cellpin: section_of(section::PIN_CELLPIN, get(section::PIN_CELLPIN)?)?,
            pin_net: section_of(section::PIN_NET, get(section::PIN_NET)?)?,
            assigns: section_of(section::ASSIGNS, get(section::ASSIGNS)?)?,
            net_pin_start: section_of(section::NET_PIN_START, get(section::NET_PIN_START)?)?,
            net_pins: section_of(section::NET_PINS, get(section::NET_PINS)?)?,
        })
    }

    /// The string of a string id, checked for bounds and UTF-8: unlike the other accessors, it
    /// never panics, also for unvalidated tables or [`ANONYMOUS`] (CORE-REQ-0138).
    pub fn string(&self, k: u32) -> Result<&str, FrozenError> {
        let Some(range) = usize::try_from(k)
            .ok()
            .and_then(|i| string_bytes(&self.str_offs, i))
        else {
            return Err(FrozenError::new(
                section::STR_OFFS,
                format!("no string {k}"),
            ));
        };
        self.str_bytes
            .get(range)
            .and_then(|b| std::str::from_utf8(b).ok())
            .ok_or_else(|| FrozenError::new(section::STR_BYTES, format!("string {k} is invalid")))
    }

    /// The rows of cell `c`'s pins in `cell_pin_name` and `cell_pin_dir`, in library order.
    pub fn cell_pins(&self, c: u32) -> Range<usize> {
        csr_row(&self.cell_pin_start, c)
    }

    /// The rows of instance `i`'s connected pins in `pin_cellpin` and `pin_net`, in connection
    /// order.
    pub fn instance_pins(&self, i: u32) -> Range<usize> {
        csr_row(&self.inst_pin_start, i)
    }

    /// The connected pins of net `n` (rows of `pin_net`), increasing.
    pub fn net_pins_of(&self, n: u32) -> &[u32] {
        &self.net_pins[csr_row(&self.net_pin_start, n)]
    }

    /// The nets of port `p`'s bits, from msb to lsb.
    pub fn port_bits(&self, p: u32) -> &[u32] {
        &self.port_bit_net[csr_row(&self.port_bit_start, p)]
    }

    /// The instance of pin `p` (a row of `pin_net`).
    pub fn pin_instance(&self, p: u32) -> u32 {
        assert!(
            (p as usize) < self.pin_net.len(),
            "pin {p} out of range ({} pins)",
            self.pin_net.len()
        );
        // The last row that starts at or before `p`; rows of unconnected instances are empty.
        // Validated rows start at 0, so there is one; otherwise this panics rather than wraps.
        let rows = self.inst_pin_start.partition_point(|&s| s <= p);
        let k = rows
            .checked_sub(1)
            .unwrap_or_else(|| panic!("pin {p}: the instance pin rows do not start at 0"));
        u32::try_from(k).unwrap_or_else(|_| panic!("pin {p}: instance {k} out of range"))
    }

    /// The direction of pin `p`'s cell pin.
    pub fn pin_dir(&self, p: u32) -> PinDir {
        let cell = self.instances[self.pin_instance(p) as usize].cell;
        // Indexing within the cell's row, never adding to its start, cannot overflow on 32-bit
        // targets, and a cell pin outside the row panics instead of reading another cell's pin.
        let dirs = &self.cell_pin_dir[self.cell_pins(cell)];
        pin_dir_of(dirs[self.pin_cellpin[p as usize] as usize])
    }

    /// Rebuilds the editable netlist after [`FrozenNetlist::validate`] (CORE-REQ-0136). Entities
    /// keep their order, so `freeze` of the result equals `self`.
    pub fn thaw(&self) -> Result<GateNetlist, FrozenError> {
        self.validate()?;
        let ctx = |section: u32| move |e: NetlistError| FrozenError::new(section, e.to_string());
        let name = self.string(self.module[module_entry::NAME])?;
        let mut g = GateNetlist::new(name).map_err(ctx(section::MODULE))?;
        // String k becomes symbol k: the table is canonical, so the module name is string 0.
        let mut syms = Vec::with_capacity(self.str_offs.len() - 1);
        for k in 0..self.str_offs.len() - 1 {
            syms.push(g.symbols.intern(self.string(k as u32)?));
        }
        let row = |start: &[u32], k: usize| start[k] as usize..start[k + 1] as usize;
        let mut cells = Vec::with_capacity(self.cells.len());
        for (k, &name) in self.cells.iter().enumerate() {
            let r = row(&self.cell_pin_start, k);
            let mut pins = Vec::with_capacity(r.len());
            for p in r {
                pins.push((
                    self.string(self.cell_pin_name[p])?,
                    pin_dir_of(self.cell_pin_dir[p]),
                ));
            }
            cells.push(
                g.add_cell_type(self.string(name)?, &pins)
                    .map_err(ctx(section::CELLS))?,
            );
        }
        let nets: Vec<NetId> = self
            .nets
            .iter()
            .map(|n| {
                let name = (n.name != ANONYMOUS).then(|| syms[n.name as usize]);
                g.insert_net(name, (n.index != SCALAR).then_some(n.index))
            })
            .collect();
        for (k, p) in self.ports.iter().enumerate() {
            let bits = self.port_bit_net[row(&self.port_bit_start, k)]
                .iter()
                .map(|&b| nets[b as usize])
                .collect();
            let range = (p.is_bus == 1).then_some((p.msb, p.lsb));
            g.insert_port(syms[p.name as usize], port_dir_of(p.dir), range, bits);
        }
        let decode = |v: u32| match v {
            CONST0 => Conn::Const(false),
            CONST1 => Conn::Const(true),
            n => Conn::Net(nets[n as usize]),
        };
        for (k, i) in self.instances.iter().enumerate() {
            let inst = g
                .add_instance(self.string(i.name)?, cells[i.cell as usize])
                .map_err(ctx(section::INSTANCES))?;
            for p in row(&self.inst_pin_start, k) {
                g.connect(
                    inst,
                    PinRef::Index(self.pin_cellpin[p]),
                    decode(self.pin_net[p]),
                )
                .map_err(ctx(section::PIN_NET))?;
            }
        }
        for a in self.assigns.iter() {
            g.add_assign(nets[a.lhs as usize], decode(a.rhs))
                .map_err(ctx(section::ASSIGNS))?;
        }
        Ok(g)
    }
}
