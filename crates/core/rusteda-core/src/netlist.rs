// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Gate-level netlist of one flat module: cell types, ports, nets, cell instances with their
//! pins, and continuous assignments.
//!
//! [`GateNetlist`] is the editable form (CORE-REQ-0120). Its entities live in generational
//! arenas and are addressed by typed ids ([`InstId`], [`NetId`], [`PinId`], [`CellId`]); every
//! operation checks the ids and names it is given and either succeeds or returns a
//! [`NetlistError`] and leaves the netlist unchanged, without interning a name (CORE-REQ-0122).
//! Names are interned per netlist. [`GateNetlist::freeze`] produces the dense [`FrozenNetlist`]
//! (compressed-sparse-row tables in a canonical order) that file codecs and queries use;
//! [`FrozenNetlist::thaw`] turns it back into a `GateNetlist`.
//!
//! Every operation keeps these invariants (CORE-REQ-0121):
//! - names follow the rules of [`names`](crate::names) (CORE-REQ-0123): printable ASCII; port,
//!   net and instance names are hierarchical, with valid escapes and no empty segment; module,
//!   cell and cell pin names are verbatim;
//! - cell, port and instance names are unique;
//! - a net is anonymous, or has a name and an optional bit index; named nets have unique
//!   (name, index) pairs, and a name belongs either to one scalar net or to indexed nets only;
//! - the nets named after a port are exactly its bits: index per bit for a bus, none for a
//!   scalar port; a bus has at most [`MAX_RANGE_WIDTH`] bits;
//! - an instance has one pin per pin of its cell, in cell pin order, and lists its connected
//!   pins once each in the order they were connected;
//! - a pin connected to a net is listed among that net's pins, and only then.
//!
//! Ids are checked against their slot and its generation only (see [`Id`]): an id that another
//! netlist made is detected when it addresses no live entry of this one, so ids must stay with
//! the netlist that made them.
//!
//! ```
//! use rusteda_core::netlist::{Conn, GateNetlist, PinDir, PortDir};
//!
//! let mut g = GateNetlist::new("top")?;
//! let inv = g.add_cell_type("inv", &[("A", PinDir::Input), ("Y", PinDir::Output)])?;
//! g.add_port("a", PortDir::Input, None)?;
//! g.add_port("y", PortDir::Output, Some((1, 0)))?;
//! let a = g.find_net("a", None).unwrap();
//! let y0 = g.find_net("y", Some(0)).unwrap();
//! let u1 = g.add_instance("u1", inv)?;
//! g.connect(u1, "A", Conn::Net(a))?;
//! g.connect(u1, "Y", Conn::Net(y0))?;
//! let y1 = g.find_net("y", Some(1)).unwrap();
//! g.add_assign(y1, Conn::Const(true))?;
//!
//! let frozen = g.freeze();
//! frozen.validate()?;
//! assert_eq!(frozen.thaw()?.freeze(), frozen);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod edit;
mod error;
mod frozen;
mod validate;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_frozen;
#[cfg(test)]
mod tests_random;

use std::fmt;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::arena::{Arena, Id, StaleId};
use crate::names::{self, NameError};
use crate::symbol::{SymbolId, SymbolInterner};

pub use error::NetlistError;
pub use frozen::{
    ANONYMOUS, AssignRec, CONST0, CONST1, FORMAT_VERSION, FrozenError, FrozenNetlist, InstRec,
    NetRec, PortRec, SCALAR, module_entry, section,
};

/// The widest port range: 2^24 bits. A wider range is refused before any of its nets is made,
/// so that a short declaration cannot exhaust memory.
pub const MAX_RANGE_WIDTH: u32 = 1 << 24;

/// Id of an instance.
pub type InstId = Id<Instance>;
/// Id of a net.
pub type NetId = Id<Net>;
/// Id of an instance pin.
pub type PinId = Id<Pin>;
/// Id of a cell type.
pub type CellId = Id<CellType>;

/// Direction of a cell pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PinDir {
    Input,
    Output,
    Inout,
    /// Any other pin, for example an internal one.
    Other,
}

impl PinDir {
    /// True for the directions that can drive a net (output, inout).
    pub fn drives(self) -> bool {
        matches!(self, PinDir::Output | PinDir::Inout)
    }
}

/// Direction of a module port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortDir {
    Input,
    Output,
    Inout,
}

/// A pin of a cell type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellPin {
    pub name: SymbolId,
    pub dir: PinDir,
}

/// A library cell as the netlist sees it: its name and its pins in library order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellType {
    pub name: SymbolId,
    pub pins: Vec<CellPin>,
}

/// A module port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub name: SymbolId,
    pub dir: PortDir,
    /// `(msb, lsb)` of a bus; `None` for a scalar port.
    pub range: Option<(i32, i32)>,
    /// One net per bit in declaration order, from `msb` to `lsb`.
    pub bits: Vec<NetId>,
}

/// A net: anonymous, or named with an optional bit index (a bit of a bus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Net {
    pub name: Option<SymbolId>,
    pub index: Option<i32>,
    /// The pins connected to the net, in connection order.
    pub pins: Vec<PinId>,
}

/// A cell instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub name: SymbolId,
    pub cell: CellId,
    /// One pin per cell pin, in cell pin order.
    pub pins: Vec<PinId>,
    /// The connected pins in the order they were connected: the order of the instance's pins in
    /// the frozen view and of its connections in a written netlist.
    pub connected: Vec<PinId>,
}

/// A pin of an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub inst: InstId,
    /// Index of the pin in its cell's pin list.
    pub cell_pin: u32,
    pub conn: Option<Conn>,
}

/// What a pin or the right-hand side of an assignment connects to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Conn {
    Net(NetId),
    Const(bool),
}

/// A continuous assignment `lhs = rhs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assign {
    pub lhs: NetId,
    pub rhs: Conn,
}

/// A cell pin given by name or by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinRef<'a> {
    Name(&'a str),
    Index(u32),
}

impl<'a> From<&'a str> for PinRef<'a> {
    fn from(name: &'a str) -> PinRef<'a> {
        PinRef::Name(name)
    }
}

impl<'a> From<u32> for PinRef<'a> {
    fn from(index: u32) -> PinRef<'a> {
        PinRef::Index(index)
    }
}

impl fmt::Display for PinRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PinRef::Name(n) => write!(f, "'{n}'"),
            PinRef::Index(i) => write!(f, "#{i}"),
        }
    }
}

/// How a name is used by nets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameUse {
    Scalar,
    Indexed,
}

/// The editable netlist of one module (see the module documentation).
#[derive(Debug, Clone)]
pub struct GateNetlist {
    symbols: SymbolInterner,
    name: SymbolId,
    cells: Arena<CellType>,
    ports: Vec<Port>,
    nets: Arena<Net>,
    insts: Arena<Instance>,
    pins: Arena<Pin>,
    assigns: Vec<Assign>,
    cell_by_name: FxHashMap<SymbolId, CellId>,
    port_by_name: FxHashMap<SymbolId, usize>,
    net_by_key: FxHashMap<(SymbolId, Option<i32>), NetId>,
    /// How each net name is used, and by how many nets.
    net_names: FxHashMap<SymbolId, (NameUse, usize)>,
    inst_by_name: FxHashMap<SymbolId, InstId>,
}

fn stale(kind: &'static str) -> impl Fn(StaleId) -> NetlistError {
    move |e| NetlistError::StaleId {
        kind,
        index: e.index,
        generation: e.generation,
    }
}

/// The error of a name of `kind` that breaks the rules of [`names`] (CORE-REQ-0123).
fn name_error(
    kind: &'static str,
    name: &str,
    r: Result<(), NameError>,
) -> Result<(), NetlistError> {
    r.map_err(|problem| match problem {
        NameError::Empty => NetlistError::EmptyName { kind },
        problem => NetlistError::InvalidName {
            kind,
            name: names::for_display(name),
            problem,
        },
    })
}

/// Checks a module, cell or cell pin name: printable ASCII, kept verbatim.
fn verbatim(kind: &'static str, name: &str) -> Result<(), NetlistError> {
    name_error(kind, name, names::check_verbatim(name))
}

/// Checks a port, net or instance name: printable ASCII, valid escapes, no empty segment.
fn hierarchical(kind: &'static str, name: &str) -> Result<(), NetlistError> {
    name_error(kind, name, names::check_hierarchical(name))
}

/// `name` or `name[index]`.
fn bit_name(name: &str, index: Option<i32>) -> String {
    match index {
        Some(i) => format!("{name}[{i}]"),
        None => name.to_string(),
    }
}

/// Number of bits of the range `[msb:lsb]` (up to 2^32).
pub(crate) fn range_width(msb: i32, lsb: i32) -> u64 {
    (i64::from(msb) - i64::from(lsb)).unsigned_abs() + 1
}

/// Bit indices of a range in declaration order, from `msb` to `lsb`; computed in 64 bits, so
/// any two bounds work.
pub(crate) fn range_indices(msb: i32, lsb: i32) -> impl DoubleEndedIterator<Item = i32> {
    let step = if msb >= lsb { -1 } else { 1 };
    (0..range_width(msb, lsb) as i64).map(move |k| (i64::from(msb) + step * k) as i32)
}

impl GateNetlist {
    /// An empty netlist of module `name`, a verbatim name (CORE-REQ-0123).
    pub fn new(name: &str) -> Result<GateNetlist, NetlistError> {
        verbatim("module", name)?;
        let mut symbols = SymbolInterner::new();
        let name = symbols.intern(name);
        Ok(GateNetlist {
            symbols,
            name,
            cells: Arena::new(),
            ports: Vec::new(),
            nets: Arena::new(),
            insts: Arena::new(),
            pins: Arena::new(),
            assigns: Vec::new(),
            cell_by_name: FxHashMap::default(),
            port_by_name: FxHashMap::default(),
            net_by_key: FxHashMap::default(),
            net_names: FxHashMap::default(),
            inst_by_name: FxHashMap::default(),
        })
    }

    /// The module name.
    pub fn name(&self) -> &str {
        self.symbols.resolve(self.name)
    }

    /// The names of this netlist.
    pub fn symbols(&self) -> &SymbolInterner {
        &self.symbols
    }

    /// The string of one of this netlist's symbols.
    ///
    /// # Panics
    ///
    /// When `s` comes from another netlist's interner.
    pub fn resolve(&self, s: SymbolId) -> &str {
        self.symbols.resolve(s)
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    pub fn port_count(&self) -> usize {
        self.ports.len()
    }

    pub fn net_count(&self) -> usize {
        self.nets.len()
    }

    pub fn instance_count(&self) -> usize {
        self.insts.len()
    }

    /// Number of instance pins, connected or not.
    pub fn pin_count(&self) -> usize {
        self.pins.len()
    }

    pub fn assign_count(&self) -> usize {
        self.assigns.len()
    }

    pub fn cell(&self, id: CellId) -> Result<&CellType, NetlistError> {
        self.cells.try_get(id).map_err(stale("cell"))
    }

    pub fn net(&self, id: NetId) -> Result<&Net, NetlistError> {
        self.nets.try_get(id).map_err(stale("net"))
    }

    pub fn instance(&self, id: InstId) -> Result<&Instance, NetlistError> {
        self.insts.try_get(id).map_err(stale("instance"))
    }

    pub fn pin(&self, id: PinId) -> Result<&Pin, NetlistError> {
        self.pins.try_get(id).map_err(stale("pin"))
    }

    /// Ports in declaration order.
    pub fn ports(&self) -> &[Port] {
        &self.ports
    }

    /// Assignments in the order they were added.
    pub fn assigns(&self) -> &[Assign] {
        &self.assigns
    }

    /// Cell types in id index order.
    pub fn cells(&self) -> impl Iterator<Item = (CellId, &CellType)> + '_ {
        self.cells.iter()
    }

    /// Nets in id index order.
    pub fn nets(&self) -> impl Iterator<Item = (NetId, &Net)> + '_ {
        self.nets.iter()
    }

    /// Instances in id index order.
    pub fn instances(&self) -> impl Iterator<Item = (InstId, &Instance)> + '_ {
        self.insts.iter()
    }

    /// Instance pins in id index order.
    pub fn pins(&self) -> impl Iterator<Item = (PinId, &Pin)> + '_ {
        self.pins.iter()
    }

    pub fn find_cell(&self, name: &str) -> Option<CellId> {
        let s = self.symbols.get(name)?;
        self.cell_by_name.get(&s).copied()
    }

    pub fn find_port(&self, name: &str) -> Option<&Port> {
        let s = self.symbols.get(name)?;
        self.port_by_name.get(&s).map(|&i| &self.ports[i])
    }

    /// The net `name` (`index == None`) or `name[index]`.
    pub fn find_net(&self, name: &str, index: Option<i32>) -> Option<NetId> {
        let s = self.symbols.get(name)?;
        self.net_by_key.get(&(s, index)).copied()
    }

    pub fn find_instance(&self, name: &str) -> Option<InstId> {
        let s = self.symbols.get(name)?;
        self.inst_by_name.get(&s).copied()
    }

    /// The pin of `inst` for a cell pin given by name or index.
    pub fn find_pin<'p>(
        &self,
        inst: InstId,
        pin: impl Into<PinRef<'p>>,
    ) -> Result<PinId, NetlistError> {
        let i = self.instance(inst)?;
        let k = self.cell_pin_index(i.cell, pin.into())?;
        Ok(i.pins[k as usize])
    }

    /// Index of a cell pin in its cell's pin list.
    fn cell_pin_index(&self, cell: CellId, pin: PinRef<'_>) -> Result<u32, NetlistError> {
        let c = self.cell(cell)?;
        let k = match pin {
            PinRef::Index(k) => ((k as usize) < c.pins.len()).then_some(k),
            PinRef::Name(n) => self
                .symbols
                .get(n)
                .and_then(|s| c.pins.iter().position(|p| p.name == s))
                .map(|k| k as u32),
        };
        k.ok_or_else(|| NetlistError::UnknownPin {
            cell: self.resolve(c.name).to_string(),
            pin: pin.to_string(),
        })
    }

    /// Adds a cell type with its pins in library order; cell and pin names are verbatim
    /// (CORE-REQ-0123). Adding a cell again with the same pins returns the existing id.
    pub fn add_cell_type<S: AsRef<str>>(
        &mut self,
        name: &str,
        pins: &[(S, PinDir)],
    ) -> Result<CellId, NetlistError> {
        verbatim("cell", name)?;
        // A set, so that thawing a cell with many pins takes time in proportion to them.
        let mut seen = FxHashSet::default();
        for (p, _) in pins {
            let p = p.as_ref();
            verbatim("cell pin", p)?;
            if !seen.insert(p) {
                return Err(NetlistError::DuplicateCellPin {
                    cell: name.to_string(),
                    pin: p.to_string(),
                });
            }
        }
        if let Some(id) = self.find_cell(name) {
            let c = &self.cells[id];
            let same = c.pins.len() == pins.len()
                && c.pins
                    .iter()
                    .zip(pins)
                    .all(|(a, (n, d))| a.dir == *d && self.resolve(a.name) == n.as_ref());
            return if same {
                Ok(id)
            } else {
                Err(NetlistError::CellConflict {
                    cell: name.to_string(),
                })
            };
        }
        let cell = CellType {
            name: self.symbols.intern(name),
            pins: pins
                .iter()
                .map(|(n, dir)| CellPin {
                    name: self.symbols.intern(n.as_ref()),
                    dir: *dir,
                })
                .collect(),
        };
        let name = cell.name;
        let id = self.cells.insert(cell);
        self.cell_by_name.insert(name, id);
        Ok(id)
    }

    /// Adds a port and its bit nets: a scalar port gets one net named after it, a bus port one
    /// net per bit named after it with the bit index. The name is hierarchical (CORE-REQ-0123);
    /// a bus has at most [`MAX_RANGE_WIDTH`] bits (CORE-REQ-0128). Returns the port's position.
    pub fn add_port(
        &mut self,
        name: &str,
        dir: PortDir,
        range: Option<(i32, i32)>,
    ) -> Result<usize, NetlistError> {
        hierarchical("port", name)?;
        if let Some((msb, lsb)) = range {
            if msb == i32::MIN || lsb == i32::MIN {
                return Err(NetlistError::InvalidIndex {
                    what: format!("port '{name}': range bound i32::MIN is reserved"),
                });
            }
            if range_width(msb, lsb) > u64::from(MAX_RANGE_WIDTH) {
                return Err(NetlistError::InvalidIndex {
                    what: format!(
                        "port '{name}': range [{msb}:{lsb}] is wider than {MAX_RANGE_WIDTH} bits"
                    ),
                });
            }
        }
        if let Some(s) = self.symbols.get(name) {
            if self.port_by_name.contains_key(&s) {
                return Err(NetlistError::DuplicateName {
                    kind: "port",
                    name: name.to_string(),
                });
            }
            if self.net_names.contains_key(&s) {
                return Err(NetlistError::NetConflict {
                    net: name.to_string(),
                    reason: "a port cannot take the name of existing nets",
                });
            }
        }
        let s = self.symbols.intern(name);
        let bits: Vec<NetId> = match range {
            None => vec![self.insert_net(Some(s), None)],
            Some((msb, lsb)) => range_indices(msb, lsb)
                .map(|k| self.insert_net(Some(s), Some(k)))
                .collect(),
        };
        Ok(self.insert_port(s, dir, range, bits))
    }

    /// Adds a net: `name == None` makes an anonymous net (without index), `index` makes a bit
    /// of a bus. The name is hierarchical (CORE-REQ-0123); names of ports are reserved for their
    /// bits.
    pub fn add_net(
        &mut self,
        name: Option<&str>,
        index: Option<i32>,
    ) -> Result<NetId, NetlistError> {
        let Some(name) = name else {
            if index.is_some() {
                return Err(NetlistError::InvalidIndex {
                    what: "an anonymous net has no bit index".into(),
                });
            }
            return Ok(self.insert_net(None, None));
        };
        self.check_net_name(name, index)?;
        let s = self.symbols.intern(name);
        Ok(self.insert_net(Some(s), index))
    }

    /// The name rules of a new named net.
    fn check_net_name(&self, name: &str, index: Option<i32>) -> Result<(), NetlistError> {
        hierarchical("net", name)?;
        if index == Some(i32::MIN) {
            return Err(NetlistError::InvalidIndex {
                what: format!("net '{name}': bit index i32::MIN is reserved"),
            });
        }
        let Some(s) = self.symbols.get(name) else {
            return Ok(());
        };
        let reason = if self.port_by_name.contains_key(&s) {
            Some("the name belongs to a port")
        } else if self.net_by_key.contains_key(&(s, index)) {
            Some("the net exists already")
        } else {
            match (self.net_names.get(&s).map(|e| e.0), index) {
                (Some(NameUse::Scalar), _) => Some("the name belongs to a scalar net"),
                (Some(NameUse::Indexed), None) => Some("the name belongs to indexed nets"),
                _ => None,
            }
        };
        match reason {
            Some(reason) => Err(NetlistError::NetConflict {
                net: bit_name(name, index),
                reason,
            }),
            None => Ok(()),
        }
    }

    /// Adds an instance of `cell` with all pins unconnected; the name is hierarchical
    /// (CORE-REQ-0123).
    pub fn add_instance(&mut self, name: &str, cell: CellId) -> Result<InstId, NetlistError> {
        let n_pins = self.cell(cell)?.pins.len();
        self.check_instance_name(name)?;
        let s = self.symbols.intern(name);
        let inst = self.insts.insert(Instance {
            name: s,
            cell,
            pins: Vec::with_capacity(n_pins),
            connected: Vec::new(),
        });
        for k in 0..n_pins {
            let pin = self.pins.insert(Pin {
                inst,
                cell_pin: k as u32,
                conn: None,
            });
            self.insts[inst].pins.push(pin);
        }
        self.inst_by_name.insert(s, inst);
        Ok(inst)
    }

    fn check_instance_name(&self, name: &str) -> Result<(), NetlistError> {
        hierarchical("instance", name)?;
        if self.find_instance(name).is_some() {
            return Err(NetlistError::DuplicateName {
                kind: "instance",
                name: name.to_string(),
            });
        }
        Ok(())
    }

    /// Appends the assignment `lhs = rhs`.
    pub fn add_assign(&mut self, lhs: NetId, rhs: Conn) -> Result<(), NetlistError> {
        self.net(lhs)?;
        if let Conn::Net(n) = rhs {
            self.net(n)?;
        }
        self.assigns.push(Assign { lhs, rhs });
        Ok(())
    }

    /// Inserts a net and indexes its name; the caller has checked the name rules.
    fn insert_net(&mut self, name: Option<SymbolId>, index: Option<i32>) -> NetId {
        let id = self.nets.insert(Net {
            name,
            index,
            pins: Vec::new(),
        });
        if let Some(s) = name {
            self.net_by_key.insert((s, index), id);
            let usage = if index.is_some() {
                NameUse::Indexed
            } else {
                NameUse::Scalar
            };
            self.net_names.entry(s).or_insert((usage, 0)).1 += 1;
        }
        id
    }

    /// Appends a port over existing bit nets; the caller has checked the port rules.
    fn insert_port(
        &mut self,
        name: SymbolId,
        dir: PortDir,
        range: Option<(i32, i32)>,
        bits: Vec<NetId>,
    ) -> usize {
        let k = self.ports.len();
        self.ports.push(Port {
            name,
            dir,
            range,
            bits,
        });
        self.port_by_name.insert(name, k);
        k
    }

    /// The display name of a net for messages: `name`, `name[index]`, or `#<slot> (anonymous)`.
    pub fn net_label(&self, id: NetId) -> String {
        match self.nets.get(id) {
            Some(Net {
                name: Some(s),
                index,
                ..
            }) => bit_name(self.resolve(*s), *index),
            Some(_) => format!("#{} (anonymous)", id.index()),
            None => format!("#{} (stale)", id.index()),
        }
    }

    /// The display name of a pin for messages: `instance.pin`.
    pub fn pin_label(&self, id: PinId) -> String {
        let Some(pin) = self.pins.get(id) else {
            return format!("#{} (stale)", id.index());
        };
        let inst = &self.insts[pin.inst];
        let cell = &self.cells[inst.cell];
        format!(
            "{}.{}",
            self.resolve(inst.name),
            self.resolve(cell.pins[pin.cell_pin as usize].name)
        )
    }
}
