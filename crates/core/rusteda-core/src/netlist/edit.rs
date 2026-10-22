// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Connection edits, removal of instances, nets and assignments, cell swaps, and buffer
//! insertion and removal. Each operation checks all its arguments before it changes anything.

use rustc_hash::FxHashSet;

use super::{
    Assign, CellId, Conn, GateNetlist, InstId, NetId, NetlistError, PinDir, PinId, PinRef, stale,
};

fn dir_name(d: PinDir) -> &'static str {
    match d {
        PinDir::Input => "input",
        PinDir::Output => "output",
        PinDir::Inout => "inout",
        PinDir::Other => "other",
    }
}

impl GateNetlist {
    /// Connects an unconnected pin of `inst`, given by cell pin name or index, to a net or a
    /// constant; it becomes the instance's last connected pin. Returns the pin.
    pub fn connect<'p>(
        &mut self,
        inst: InstId,
        pin: impl Into<PinRef<'p>>,
        conn: Conn,
    ) -> Result<PinId, NetlistError> {
        let pin = self.find_pin(inst, pin)?;
        self.connect_pin(pin, conn)?;
        Ok(pin)
    }

    /// Connects an unconnected pin to a net or a constant; it becomes its instance's last
    /// connected pin.
    pub fn connect_pin(&mut self, pin: PinId, conn: Conn) -> Result<(), NetlistError> {
        let p = self.pin(pin)?;
        if p.conn.is_some() {
            return Err(NetlistError::AlreadyConnected {
                pin: self.pin_label(pin),
            });
        }
        let inst = p.inst;
        if let Conn::Net(n) = conn {
            self.net(n)?;
            self.nets[n].pins.push(pin);
        }
        self.pins[pin].conn = Some(conn);
        self.insts[inst].connected.push(pin);
        Ok(())
    }

    /// Disconnects a pin and returns what it was connected to (`None`: it was unconnected).
    pub fn disconnect(&mut self, pin: PinId) -> Result<Option<Conn>, NetlistError> {
        let p = self.pin(pin)?;
        let (old, inst) = (p.conn, p.inst);
        if let Some(Conn::Net(n)) = old {
            let pins = &mut self.nets[n].pins;
            if let Some(k) = pins.iter().position(|&p| p == pin) {
                pins.remove(k);
            }
        }
        if old.is_some() {
            self.insts[inst].connected.retain(|&p| p != pin);
        }
        self.pins[pin].conn = None;
        Ok(old)
    }

    /// Moves a connected pin to a net or a constant. Unlike `disconnect` followed by
    /// `connect_pin`, which makes it the last connected pin of its instance, the pin keeps its
    /// place in its instance's connection order; it becomes the last pin of its new net.
    /// Returns what it was connected to.
    pub fn move_pin(&mut self, pin: PinId, to: Conn) -> Result<Conn, NetlistError> {
        let Some(old) = self.pin(pin)?.conn else {
            return Err(NetlistError::NotConnected {
                pin: self.pin_label(pin),
            });
        };
        if let Conn::Net(n) = to {
            self.net(n)?;
        }
        if old == to {
            return Ok(old);
        }
        if let Conn::Net(n) = old {
            self.nets[n].pins.retain(|&p| p != pin);
        }
        if let Conn::Net(n) = to {
            self.nets[n].pins.push(pin);
        }
        self.pins[pin].conn = Some(to);
        Ok(old)
    }

    /// Why a live net cannot be removed, apart from its pins: it is a bit of a port, or an
    /// assignment refers to it.
    fn net_in_use(&self, net: NetId) -> Option<&'static str> {
        let port_bit = self.nets[net]
            .name
            .is_some_and(|s| self.port_by_name.contains_key(&s));
        if port_bit {
            Some("it is a bit of a port")
        } else if self
            .assigns
            .iter()
            .any(|a| a.lhs == net || a.rhs == Conn::Net(net))
        {
            Some("an assignment refers to it")
        } else {
            None
        }
    }

    /// Removes a net that nothing uses: no pin is connected to it, it is not a bit of a port,
    /// and no assignment refers to it. Its id becomes stale, and its name and index are free
    /// again.
    pub fn remove_net(&mut self, net: NetId) -> Result<(), NetlistError> {
        let reason = if self.net(net)?.pins.is_empty() {
            self.net_in_use(net)
        } else {
            Some("pins are connected to it")
        };
        if let Some(reason) = reason {
            return Err(NetlistError::NetInUse {
                net: self.net_label(net),
                reason,
            });
        }
        let removed = self.nets.remove(net).map_err(stale("net"))?;
        if let Some(s) = removed.name {
            self.net_by_key.remove(&(s, removed.index));
            if let Some(e) = self.net_names.get_mut(&s) {
                e.1 -= 1;
                if e.1 == 0 {
                    self.net_names.remove(&s);
                }
            }
        }
        Ok(())
    }

    /// Removes the assignment at position `k` of [`GateNetlist::assigns`] and returns it; the
    /// later assignments move up.
    pub fn remove_assign(&mut self, k: usize) -> Result<Assign, NetlistError> {
        if k >= self.assigns.len() {
            return Err(NetlistError::NoAssign {
                index: k,
                count: self.assigns.len(),
            });
        }
        Ok(self.assigns.remove(k))
    }

    /// Removes an instance after disconnecting its pins. The ids of the instance and of its pins
    /// become stale.
    pub fn remove_instance(&mut self, inst: InstId) -> Result<(), NetlistError> {
        let pins = self.instance(inst)?.pins.clone();
        for &p in &pins {
            self.disconnect(p)?;
            self.pins.remove(p).map_err(stale("pin"))?;
        }
        let removed = self.insts.remove(inst).map_err(stale("instance"))?;
        self.inst_by_name.remove(&removed.name);
        Ok(())
    }

    /// Gives an instance another cell that is pin-compatible: the same pin names with the same
    /// directions, in any order. Each connection stays with its pin name, the pin ids stay valid,
    /// and the connection order is kept.
    pub fn swap_cell(&mut self, inst: InstId, cell: CellId) -> Result<(), NetlistError> {
        let old_cell = self.instance(inst)?.cell;
        let new = self.cell(cell)?;
        let old = &self.cells[old_cell];
        let reject = |reason: String| NetlistError::IncompatibleCell {
            inst: self.resolve(self.insts[inst].name).to_string(),
            cell: self.resolve(new.name).to_string(),
            reason,
        };
        if new.pins.len() != old.pins.len() {
            return Err(reject(format!(
                "it has {} pins, '{}' has {}",
                new.pins.len(),
                self.resolve(old.name),
                old.pins.len()
            )));
        }
        // Position in the old pin list of each new pin; a permutation, since pin names are
        // unique within a cell and the counts agree.
        let mut order = Vec::with_capacity(new.pins.len());
        for np in &new.pins {
            let name = self.resolve(np.name);
            match old.pins.iter().position(|op| op.name == np.name) {
                None => {
                    return Err(reject(format!(
                        "pin '{name}' is not a pin of '{}'",
                        self.resolve(old.name)
                    )));
                }
                Some(k) if old.pins[k].dir != np.dir => {
                    return Err(reject(format!(
                        "pin '{name}' has direction {}, not {} as in '{}'",
                        dir_name(np.dir),
                        dir_name(old.pins[k].dir),
                        self.resolve(old.name)
                    )));
                }
                Some(k) => order.push(k),
            }
        }
        let old_pins = self.insts[inst].pins.clone();
        let pins: Vec<PinId> = order.iter().map(|&k| old_pins[k]).collect();
        for (j, &p) in pins.iter().enumerate() {
            self.pins[p].cell_pin = j as u32;
        }
        let i = &mut self.insts[inst];
        i.cell = cell;
        i.pins = pins;
        Ok(())
    }

    /// Splits `net` with a buffer: a new instance `inst_name` of `cell` gets its `input` pin
    /// connected to `net` and its `output` pin to a new scalar net `net_name` (`None`:
    /// anonymous), and the `sinks` move from `net` to the new net, each keeping its place in its
    /// instance's connection order. Every sink must be connected to `net` and must not drive it
    /// (output and inout pins never move). Returns the buffer instance and the new net.
    pub fn insert_buffer<'p>(
        &mut self,
        net: NetId,
        sinks: &[PinId],
        cell: CellId,
        input: impl Into<PinRef<'p>>,
        output: impl Into<PinRef<'p>>,
        inst_name: &str,
        net_name: Option<&str>,
    ) -> Result<(InstId, NetId), NetlistError> {
        self.net(net)?;
        let k_in = self.cell_pin_index(cell, input.into())?;
        let k_out = self.cell_pin_index(cell, output.into())?;
        let c = &self.cells[cell];
        for (k, dir, expected) in [
            (k_in, PinDir::Input, "input"),
            (k_out, PinDir::Output, "output"),
        ] {
            let p = &c.pins[k as usize];
            if p.dir != dir {
                return Err(NetlistError::BufferPin {
                    cell: self.resolve(c.name).to_string(),
                    pin: format!("'{}'", self.resolve(p.name)),
                    expected,
                });
            }
        }
        let mut moved = FxHashSet::default();
        for &p in sinks {
            let pin = self.pin(p)?;
            if !moved.insert(p) {
                return Err(NetlistError::DuplicateSink {
                    pin: self.pin_label(p),
                });
            }
            if pin.conn != Some(Conn::Net(net)) {
                return Err(NetlistError::NotOnNet {
                    pin: self.pin_label(p),
                    net: self.net_label(net),
                });
            }
            if self.pin_dir(p).drives() {
                return Err(NetlistError::SinkIsDriver {
                    pin: self.pin_label(p),
                });
            }
        }
        self.check_instance_name(inst_name)?;
        if let Some(n) = net_name {
            self.check_net_name(n, None)?;
        }
        // Everything is checked; the steps below cannot fail.
        let out = self.add_net(net_name, None)?;
        let buf = self.add_instance(inst_name, cell)?;
        let (pin_in, pin_out) = {
            let pins = &self.insts[buf].pins;
            (pins[k_in as usize], pins[k_out as usize])
        };
        self.connect_pin(pin_in, Conn::Net(net))?;
        self.connect_pin(pin_out, Conn::Net(out))?;
        self.nets[net].pins.retain(|p| !moved.contains(p));
        for &p in sinks {
            self.pins[p].conn = Some(Conn::Net(out));
            self.nets[out].pins.push(p);
        }
        Ok((buf, out))
    }

    /// Undoes [`GateNetlist::insert_buffer`]: `inst` must connect exactly one input pin, to a
    /// net `a`, and one output pin, to another net `b`; `b` must not be a bit of a port or
    /// appear in an assignment, and no other pin may drive it. The other pins of `b` move to
    /// `a`, each keeping its place in its instance's connection order, and `inst` and `b` are
    /// removed. So a buffer insertion followed by the removal of the buffer leaves the frozen
    /// tables as they were. Returns `a`.
    pub fn remove_buffer(&mut self, inst: InstId) -> Result<NetId, NetlistError> {
        let i = self.instance(inst)?;
        let reject = |reason: String| NetlistError::NotABuffer {
            inst: self.resolve(i.name).to_string(),
            reason,
        };
        let cell = &self.cells[i.cell];
        let (mut input, mut output) = (None, None);
        for &p in &i.connected {
            let pin = &self.pins[p];
            let problem = match (pin.conn, cell.pins[pin.cell_pin as usize].dir) {
                (Some(Conn::Const(_)), _) => Some("is connected to a constant"),
                (_, PinDir::Input) if input.is_some() => Some("is a second input"),
                (_, PinDir::Output) if output.is_some() => Some("is a second output"),
                (Some(Conn::Net(n)), PinDir::Input) => {
                    input = Some(n);
                    None
                }
                (Some(Conn::Net(n)), PinDir::Output) => {
                    output = Some((p, n));
                    None
                }
                _ => Some("is neither an input nor an output"),
            };
            if let Some(problem) = problem {
                return Err(reject(format!("pin '{}' {problem}", self.pin_label(p))));
            }
        }
        let (Some(a), Some((pin_out, b))) = (input, output) else {
            return Err(reject(
                "it does not connect both an input and an output".into(),
            ));
        };
        if a == b {
            let net = self.net_label(a);
            return Err(reject(format!(
                "its input and output are on the same net '{net}'"
            )));
        }
        if let Some(reason) = self.net_in_use(b) {
            return Err(NetlistError::NetInUse {
                net: self.net_label(b),
                reason,
            });
        }
        let sinks: Vec<PinId> = self.nets[b]
            .pins
            .iter()
            .copied()
            .filter(|&p| p != pin_out)
            .collect();
        if let Some(&d) = sinks.iter().find(|&&p| self.pin_dir(p).drives()) {
            return Err(reject(format!(
                "pin '{}' drives its output net '{}' too",
                self.pin_label(d),
                self.net_label(b)
            )));
        }
        // Everything is checked; the steps below cannot fail.
        self.nets[b].pins.retain(|&p| p == pin_out);
        for &p in &sinks {
            self.pins[p].conn = Some(Conn::Net(a));
            self.nets[a].pins.push(p);
        }
        self.remove_instance(inst)?;
        self.remove_net(b)?;
        Ok(a)
    }

    /// The direction of a live pin's cell pin.
    fn pin_dir(&self, pin: PinId) -> PinDir {
        let p = &self.pins[pin];
        self.cells[self.insts[p.inst].cell].pins[p.cell_pin as usize].dir
    }
}
