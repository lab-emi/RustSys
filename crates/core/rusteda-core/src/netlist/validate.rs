// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Checks of a frozen netlist (CORE-REQ-0135): every length, row, index, name and encoding rule
//! of the format, the canonical order of [`GateNetlist::freeze`](super::GateNetlist::freeze) and
//! the bound on what thawing allocates (CORE-REQ-0137), without panicking on any input. Values
//! read from the tables are compared with a bound before any arithmetic, so no sum overflows on
//! 32-bit targets either.

use std::cmp::Ordering;
use std::ops::Range;

use rustc_hash::{FxHashMap, FxHashSet};

use super::frozen::{
    ANONYMOUS, CONST0, CONST1, FrozenError, FrozenNetlist, SCALAR, module_entry, section,
};
use super::{MAX_RANGE_WIDTH, range_indices, range_width};
use crate::names;

type Check = Result<(), FrozenError>;

/// The pins that the instances of any tables may have together, connected or not; larger
/// tables may have one per byte (see [`FrozenNetlist`]).
const MIN_PIN_BUDGET: u64 = 1 << 24;

fn bad(section: u32, message: impl Into<String>) -> FrozenError {
    FrozenError::new(section, message)
}

/// Checks the start section of `rows` rows over a section of `len` elements: `rows + 1`
/// entries, starting at 0, never decreasing, ending at `len`.
fn rows(id: u32, start: &[u32], rows: usize, len: usize) -> Check {
    if start.len() != rows + 1 {
        return Err(bad(
            id,
            format!("{} entries, expected {}", start.len(), rows + 1),
        ));
    }
    if start[0] != 0 {
        return Err(bad(id, format!("first entry is {}, expected 0", start[0])));
    }
    if let Some(k) = start.windows(2).position(|w| w[1] < w[0]) {
        return Err(bad(id, format!("row {k} ends before it starts")));
    }
    if start[rows] as usize != len {
        return Err(bad(
            id,
            format!("last entry is {}, expected {len}", start[rows]),
        ));
    }
    Ok(())
}

/// Row `k` of a checked start section.
fn row(start: &[u32], k: usize) -> Range<usize> {
    start[k] as usize..start[k + 1] as usize
}

/// The row of cell pin `cp` among the rows `cell_pins` of its cell's pins, or `None` when the
/// cell has no pin `cp`. The pin is compared with the number of rows before it is added to
/// their start, so the sum stays within the rows on 32-bit targets too.
pub(super) fn cell_pin_slot(cell_pins: &Range<usize>, cp: u32) -> Option<usize> {
    let cp = usize::try_from(cp).ok()?;
    (cp < cell_pins.len()).then(|| cell_pins.start + cp)
}

/// String references must introduce new strings in table order: the next new string is the
/// next table entry, so the table is the one `freeze` builds and has no unused strings.
struct FirstUse {
    next: u32,
    count: usize,
}

impl FirstUse {
    fn see(&mut self, id: u32, what: &str, s: u32) -> Check {
        if s as usize >= self.count {
            return Err(bad(
                id,
                format!("{what}: string {s} out of range ({} strings)", self.count),
            ));
        }
        match s.cmp(&self.next) {
            Ordering::Less => Ok(()),
            Ordering::Equal => {
                // `s < count`, and the distinct non-empty strings of a table of at most
                // `u32::MAX` bytes number at most `u32::MAX`, so this cannot overflow.
                self.next += 1;
                Ok(())
            }
            Ordering::Greater => Err(bad(
                id,
                format!(
                    "{what}: string {s} is used before string {}, so the string table is not in \
                     first-use order",
                    self.next
                ),
            )),
        }
    }
}

/// Names (string ids) must not repeat.
fn unique(id: u32, what: &str, names: impl Iterator<Item = u32>) -> Check {
    let mut seen = FxHashSet::default();
    for (k, s) in names.enumerate() {
        if !seen.insert(s) {
            return Err(bad(id, format!("{what} {k} repeats an earlier name")));
        }
    }
    Ok(())
}

/// A net index or a constant code.
fn conn_ok(v: u32, n_nets: usize) -> bool {
    (v as usize) < n_nets || v == CONST0 || v == CONST1
}

impl FrozenNetlist<'_> {
    /// Checks every rule of the format (CORE-REQ-0135): section lengths, rows, index bounds,
    /// direction and flag codes, UTF-8 and uniqueness of strings and names, the name rules of
    /// every use of a string (CORE-REQ-0110, 0111), bus widths and the naming of port bits,
    /// distinct cell pins within an instance, the canonical order of strings and net pins, and
    /// the bound on the pins of all instances (CORE-REQ-0137, see [`FrozenNetlist`]). Accepts
    /// exactly what [`GateNetlist::freeze`](super::GateNetlist::freeze) produces within that
    /// bound, and never panics, whatever the tables hold.
    pub fn validate(&self) -> Result<(), FrozenError> {
        self.check_strings()?;
        let Ok(m) = <&[u32; module_entry::COUNT]>::try_from(&self.module[..]) else {
            return Err(bad(
                section::MODULE,
                format!(
                    "{} entries, expected {}",
                    self.module.len(),
                    module_entry::COUNT
                ),
            ));
        };
        for (what, k, len) in [
            ("ports", module_entry::N_PORTS, self.ports.len()),
            ("nets", module_entry::N_NETS, self.nets.len()),
            ("instances", module_entry::N_INSTS, self.instances.len()),
            ("pins", module_entry::N_PINS, self.pin_net.len()),
            ("assignments", module_entry::N_ASSIGNS, self.assigns.len()),
        ] {
            let count = m[k];
            if count as usize != len {
                return Err(bad(
                    section::MODULE,
                    format!("{count} {what} recorded, the section has {len}"),
                ));
            }
        }
        if self.nets.len() >= CONST0 as usize {
            return Err(bad(section::NETS, "too many nets for the constant codes"));
        }
        let n_cells = self.cells.len();
        rows(
            section::CELL_PIN_START,
            &self.cell_pin_start,
            n_cells,
            self.cell_pin_name.len(),
        )?;
        if self.cell_pin_dir.len() != self.cell_pin_name.len() {
            return Err(bad(
                section::CELL_PIN_DIR,
                format!(
                    "{} entries for {} cell pins",
                    self.cell_pin_dir.len(),
                    self.cell_pin_name.len()
                ),
            ));
        }
        if let Some(k) = self.cell_pin_dir.iter().position(|&d| d > 3) {
            return Err(bad(
                section::CELL_PIN_DIR,
                format!("cell pin {k} has direction code {}", self.cell_pin_dir[k]),
            ));
        }
        rows(
            section::PORT_BIT_START,
            &self.port_bit_start,
            self.ports.len(),
            self.port_bit_net.len(),
        )?;
        rows(
            section::INST_PIN_START,
            &self.inst_pin_start,
            self.instances.len(),
            self.pin_cellpin.len(),
        )?;
        if self.pin_net.len() != self.pin_cellpin.len() {
            return Err(bad(
                section::PIN_NET,
                format!(
                    "{} entries for {} pins",
                    self.pin_net.len(),
                    self.pin_cellpin.len()
                ),
            ));
        }
        rows(
            section::NET_PIN_START,
            &self.net_pin_start,
            self.nets.len(),
            self.net_pins.len(),
        )?;
        self.check_first_use(m[module_entry::NAME])?;
        self.check_hierarchical_names()?;
        unique(section::CELLS, "cell", self.cells.iter().copied())?;
        for k in 0..n_cells {
            let names = self.cell_pin_name[row(&self.cell_pin_start, k)].iter();
            unique(section::CELL_PIN_NAME, "pin of a cell", names.copied())?;
        }
        unique(section::PORTS, "port", self.ports.iter().map(|p| p.name))?;
        unique(
            section::INSTANCES,
            "instance",
            self.instances.iter().map(|i| i.name),
        )?;
        self.check_ports()?;
        self.check_nets()?;
        self.check_pins()?;
        for (k, a) in self.assigns.iter().enumerate() {
            if a.lhs as usize >= self.nets.len() || !conn_ok(a.rhs, self.nets.len()) {
                return Err(bad(
                    section::ASSIGNS,
                    format!("assignment {k} refers to a missing net"),
                ));
            }
        }
        self.check_net_pins()
    }

    /// Offsets, and distinct strings that follow the rules of every name (CORE-REQ-0110):
    /// UTF-8, non-empty, printable ASCII. Every string of a valid table names something, since
    /// the first-use order leaves no string unused.
    fn check_strings(&self) -> Check {
        let Some(n) = self.str_offs.len().checked_sub(1) else {
            return Err(bad(section::STR_OFFS, "no entries"));
        };
        rows(section::STR_OFFS, &self.str_offs, n, self.str_bytes.len())?;
        let mut seen = FxHashSet::default();
        for k in 0..n {
            let b = &self.str_bytes[row(&self.str_offs, k)];
            let Ok(s) = std::str::from_utf8(b) else {
                return Err(bad(section::STR_BYTES, format!("string {k} is not UTF-8")));
            };
            if let Err(e) = names::check_verbatim(s) {
                return Err(bad(section::STR_BYTES, format!("string {k}: {e}")));
            }
            if !seen.insert(b) {
                return Err(bad(
                    section::STR_BYTES,
                    format!("string {k} repeats an earlier string"),
                ));
            }
        }
        Ok(())
    }

    /// String ids in the traversal order of `freeze`.
    fn check_first_use(&self, module: u32) -> Check {
        let mut f = FirstUse {
            next: 0,
            count: self.str_offs.len() - 1,
        };
        f.see(section::MODULE, "module name", module)?;
        for (k, &c) in self.cells.iter().enumerate() {
            f.see(section::CELLS, "cell name", c)?;
            for &p in &self.cell_pin_name[row(&self.cell_pin_start, k)] {
                f.see(section::CELL_PIN_NAME, "cell pin name", p)?;
            }
        }
        for p in self.ports.iter() {
            f.see(section::PORTS, "port name", p.name)?;
        }
        for n in self.nets.iter().filter(|n| n.name != ANONYMOUS) {
            f.see(section::NETS, "net name", n.name)?;
        }
        for i in self.instances.iter() {
            f.see(section::INSTANCES, "instance name", i.name)?;
        }
        if f.next as usize != f.count {
            return Err(bad(
                section::STR_OFFS,
                format!("strings {} to {} are never used", f.next, f.count - 1),
            ));
        }
        Ok(())
    }

    /// The names of ports, nets and instances are hierarchical (CORE-REQ-0111): valid escapes
    /// and no empty segment. A string that also names a cell or a pin, or the module, must follow
    /// both rule sets, so every such use is checked, each string once. The first-use order has
    /// checked that the string ids are in range, and the strings are printable ASCII.
    fn check_hierarchical_names(&self) -> Check {
        let mut checked = vec![false; self.str_offs.len() - 1];
        let mut check = |id: u32, what: &str, k: usize, s: u32| -> Check {
            match checked.get_mut(s as usize) {
                Some(done) if !*done => *done = true,
                _ => return Ok(()),
            }
            let name = self.string(s)?;
            names::check_hierarchical(name)
                .map_err(|e| bad(id, format!("{what} {k}: name '{name}' (string {s}): {e}")))
        };
        for (k, p) in self.ports.iter().enumerate() {
            check(section::PORTS, "port", k, p.name)?;
        }
        for (k, n) in self.nets.iter().enumerate() {
            if n.name != ANONYMOUS {
                check(section::NETS, "net", k, n.name)?;
            }
        }
        for (k, i) in self.instances.iter().enumerate() {
            check(section::INSTANCES, "instance", k, i.name)?;
        }
        Ok(())
    }

    /// Codes and ranges of the ports, and their bits: nets named after the port with the bit
    /// indices of the range, from msb to lsb.
    fn check_ports(&self) -> Check {
        for (k, p) in self.ports.iter().enumerate() {
            let id = section::PORTS;
            if p.dir > 2 || p.is_bus > 1 || p._pad != 0 {
                return Err(bad(id, format!("port {k} has an invalid code")));
            }
            let bits = &self.port_bit_net[row(&self.port_bit_start, k)];
            let wrong_bits = || {
                bad(
                    section::PORT_BIT_NET,
                    format!("bits of port {k} are not the nets of its name and range"),
                )
            };
            let net_is = |b: u32, index: i32| {
                self.nets
                    .get(b as usize)
                    .is_some_and(|n| n.name == p.name && n.index == index)
            };
            if p.is_bus == 0 {
                if (p.msb, p.lsb) != (0, 0) {
                    return Err(bad(id, format!("scalar port {k} has a range")));
                }
                if bits.len() != 1 || !net_is(bits[0], SCALAR) {
                    return Err(wrong_bits());
                }
                continue;
            }
            if p.msb == SCALAR || p.lsb == SCALAR {
                return Err(bad(id, format!("port {k} uses a reserved range bound")));
            }
            let width = range_width(p.msb, p.lsb);
            if width > u64::from(MAX_RANGE_WIDTH) {
                return Err(bad(
                    id,
                    format!("port {k} is wider than {MAX_RANGE_WIDTH} bits"),
                ));
            }
            if bits.len() as u64 != width {
                return Err(wrong_bits());
            }
            if !bits
                .iter()
                .zip(range_indices(p.msb, p.lsb))
                .all(|(&b, index)| net_is(b, index))
            {
                return Err(wrong_bits());
            }
        }
        Ok(())
    }

    /// Net names: anonymous nets have no index, (name, index) pairs are unique, a name has one
    /// scalar net or indexed nets only, and the nets of a port's name are exactly its bits.
    fn check_nets(&self) -> Check {
        let id = section::NETS;
        let mut keys = FxHashSet::default();
        let mut names: FxHashMap<u32, (bool, usize)> = FxHashMap::default();
        for (k, n) in self.nets.iter().enumerate() {
            if n.name == ANONYMOUS {
                if n.index != SCALAR {
                    return Err(bad(id, format!("anonymous net {k} has a bit index")));
                }
                continue;
            }
            if !keys.insert((n.name, n.index)) {
                return Err(bad(
                    id,
                    format!("net {k} repeats an earlier name and index"),
                ));
            }
            let scalar = n.index == SCALAR;
            let e = names.entry(n.name).or_insert((scalar, 0));
            if e.0 != scalar {
                return Err(bad(
                    id,
                    format!("net {k}: its name has both scalar and indexed nets"),
                ));
            }
            e.1 += 1;
        }
        for (k, p) in self.ports.iter().enumerate() {
            let count = names.get(&p.name).map_or(0, |e| e.1);
            if count != row(&self.port_bit_start, k).len() {
                return Err(bad(
                    id,
                    format!("nets named after port {k} that are not bits of the port"),
                ));
            }
        }
        Ok(())
    }

    /// Instance cells, and their connected pins: cell pin indices in range and distinct within
    /// an instance (in any order, the order of connection), targets nets or constants. The
    /// pins of all instances together, connected or not, are within the bound of
    /// [`FrozenNetlist`]: one per byte of the tables, at least [`MIN_PIN_BUDGET`] and at most
    /// `u32::MAX`.
    fn check_pins(&self) -> Check {
        let n_nets = self.nets.len();
        let bytes: u64 = self.sections().iter().map(|s| s.2.len() as u64).sum();
        let budget = bytes.max(MIN_PIN_BUDGET).min(u64::from(u32::MAX));
        let mut all_pins = 0u64;
        // For each cell pin, the last instance (plus one) that connects it.
        let mut user = vec![0u32; self.cell_pin_name.len()];
        for (k, i) in self.instances.iter().enumerate() {
            let cell = i.cell as usize;
            if cell >= self.cells.len() {
                return Err(bad(
                    section::INSTANCES,
                    format!("instance {k} has cell {cell} of {}", self.cells.len()),
                ));
            }
            let cell_pins = row(&self.cell_pin_start, cell);
            all_pins += cell_pins.len() as u64;
            if all_pins > budget {
                return Err(bad(
                    section::INSTANCES,
                    format!(
                        "instances 0 to {k} have {all_pins} pins, connected or not, more than \
                         the {budget} that tables of {bytes} bytes may have (one per byte, at \
                         least {MIN_PIN_BUDGET}, at most {})",
                        u32::MAX
                    ),
                ));
            }
            // The instance count is a u32 entry of the module section, so `k + 1` fits.
            let me = k as u32 + 1;
            for p in row(&self.inst_pin_start, k) {
                let cp = self.pin_cellpin[p];
                let Some(slot) = cell_pin_slot(&cell_pins, cp).filter(|&s| user[s] != me) else {
                    return Err(bad(
                        section::PIN_CELLPIN,
                        format!(
                            "pin {p} of instance {k}: cell pin {cp} is out of range or connected \
                             twice"
                        ),
                    ));
                };
                user[slot] = me;
                if !conn_ok(self.pin_net[p], n_nets) {
                    return Err(bad(
                        section::PIN_NET,
                        format!("pin {p} refers to net {} of {n_nets}", self.pin_net[p]),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The pins of each net are exactly the pins whose `pin_net` is that net, increasing.
    fn check_net_pins(&self) -> Check {
        let id = section::NET_PINS;
        let n_nets = self.nets.len();
        let connected = self
            .pin_net
            .iter()
            .filter(|&&v| (v as usize) < n_nets)
            .count();
        if self.net_pins.len() != connected {
            return Err(bad(
                id,
                format!(
                    "{} entries for {connected} pins on nets",
                    self.net_pins.len()
                ),
            ));
        }
        for n in 0..n_nets {
            let mut prev: Option<u32> = None;
            for &p in &self.net_pins[row(&self.net_pin_start, n)] {
                let on_net = self
                    .pin_net
                    .get(p as usize)
                    .is_some_and(|&v| v as usize == n);
                if !on_net || prev.is_some_and(|q| p <= q) {
                    return Err(bad(
                        id,
                        format!("net {n} lists pin {p}, which is not its next pin"),
                    ));
                }
                prev = Some(p);
            }
        }
        Ok(())
    }
}
