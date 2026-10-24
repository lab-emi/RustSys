// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Applying an edit to a [`GateNetlist`] by a fixed sequence of netlist operations
//! (WS-REQ-0146 to WS-REQ-0151).
//!
//! Every edit checks its arguments before it changes the netlist, and an edit that fails leaves
//! the netlist unchanged. The sequence of operations is part of the definition of each edit: it
//! fixes the connection order of every pin and net, so that two tools that apply the same edits
//! to the same netlist get equal frozen netlists.

use rusteda_core::names;
use rusteda_core::netlist::{
    CellId, Conn, GateNetlist, InstId, NetId, NetlistError, PinDir, PinId, PortDir,
};

use super::{Edit, EditsError, Sink, codes};
use crate::envelope::DiagCode;

/// What an applied edit changed (WS-REQ-0151): the instances whose cell or connections changed
/// and the nets that gained or lost a pin, the new ones included, each once, in the order the edit
/// touched them. Removed instances and nets are not listed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// Changed and new instances.
    pub instances: Vec<InstId>,
    /// Changed and new nets.
    pub nets: Vec<NetId>,
}

impl Applied {
    fn inst(&mut self, i: InstId) {
        if !self.instances.contains(&i) {
            self.instances.push(i);
        }
    }

    fn net(&mut self, n: NetId) {
        if !self.nets.contains(&n) {
            self.nets.push(n);
        }
    }

    fn pin_owner(&mut self, nl: &GateNetlist, p: PinId) {
        if let Ok(pin) = nl.pin(p) {
            self.inst(pin.inst);
        }
    }
}

/// Applies `edit` to `netlist` (WS-REQ-0146 to WS-REQ-0151). A cell that the netlist does not
/// have yet is added with the pins `cell_pins` gives for it, in library order; `cell_pins`
/// returns `None` for a cell it does not know. On an error the netlist is unchanged.
pub fn apply(
    netlist: &mut GateNetlist,
    edit: &Edit,
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
) -> Result<Applied, EditsError> {
    match edit {
        Edit::Resize { instance, cell } => resize(netlist, instance, cell, cell_pins),
        Edit::InsertBuffer {
            net,
            cell,
            instance,
            net_out,
            sinks,
        } => insert_buffer(netlist, net, cell, instance, net_out, sinks, cell_pins),
        Edit::RemoveBuffer { instance } => remove_buffer(netlist, instance),
        Edit::SwapPins {
            instance,
            pin_a,
            pin_b,
        } => swap_pins(netlist, instance, pin_a, pin_b),
        Edit::Clone {
            instance,
            new_instance,
            new_net,
            sinks,
        } => clone(netlist, instance, new_instance, new_net, sinks),
    }
}

/// The net named `name` (WS-REQ-0141): the scalar net `name`, or else, when `name` is
/// `base[index]`, the bit `index` of the bus `base`. A name that names both is ambiguous.
pub fn find_net(netlist: &GateNetlist, name: &str) -> Result<NetId, EditsError> {
    let scalar = netlist.find_net(name, None);
    let bit = split_bit(name).and_then(|(base, k)| netlist.find_net(base, Some(k)));
    match (scalar, bit) {
        (Some(_), Some(_)) => Err(EditsError::new(
            codes::UNKNOWN_NET,
            "net",
            format!(
                "'{}' names both a scalar net and a bit of a bus",
                names::for_display(name)
            ),
        )),
        (Some(n), None) | (None, Some(n)) => Ok(n),
        (None, None) => Err(EditsError::new(
            codes::UNKNOWN_NET,
            "net",
            format!("no net '{}'", names::for_display(name)),
        )),
    }
}

/// The net of the port bit `name` and the direction of its port (WS-REQ-0141): a scalar port, or
/// `base[index]` of a bus port.
pub fn port_bit_net(netlist: &GateNetlist, name: &str) -> Option<(NetId, PortDir)> {
    if let Some(p) = netlist.find_port(name)
        && p.range.is_none()
    {
        return p.bits.first().map(|&n| (n, p.dir));
    }
    let (base, k) = split_bit(name)?;
    let p = netlist.find_port(base)?;
    p.range?;
    let n = netlist.find_net(base, Some(k))?;
    p.bits.contains(&n).then_some((n, p.dir))
}

/// `base[index]` split into its base and index.
fn split_bit(name: &str) -> Option<(&str, i32)> {
    let inner = name.strip_suffix(']')?;
    let open = inner.rfind('[')?;
    let (base, k) = (&inner[..open], &inner[open + 1..]);
    if base.is_empty() {
        return None;
    }
    k.parse::<i32>().ok().map(|k| (base, k))
}

fn find_instance(nl: &GateNetlist, name: &str, at: &str) -> Result<InstId, EditsError> {
    nl.find_instance(name).ok_or_else(|| {
        EditsError::new(
            codes::UNKNOWN_INSTANCE,
            at,
            format!("no instance '{}'", names::for_display(name)),
        )
    })
}

fn free_instance_name(nl: &GateNetlist, name: &str, at: &str) -> Result<(), EditsError> {
    if nl.find_instance(name).is_some() {
        return Err(EditsError::new(
            codes::NAME_IN_USE,
            at,
            format!("an instance '{}' exists", names::for_display(name)),
        ));
    }
    Ok(())
}

fn find_pin(nl: &GateNetlist, inst: InstId, pin: &str, at: &str) -> Result<PinId, EditsError> {
    nl.find_pin(inst, pin)
        .map_err(|e| EditsError::new(codes::UNKNOWN_PIN, at, format!("{e}")))
}

/// The direction of the cell pin of `pin`.
fn pin_dir(nl: &GateNetlist, pin: PinId) -> PinDir {
    nl.pin(pin)
        .ok()
        .and_then(|p| {
            let inst = nl.instance(p.inst).ok()?;
            let cell = nl.cell(inst.cell).ok()?;
            cell.pins.get(p.cell_pin as usize).map(|cp| cp.dir)
        })
        .unwrap_or(PinDir::Other)
}

/// True when `net` is a bit of a port or an assignment refers to it.
fn net_in_use(nl: &GateNetlist, net: NetId) -> bool {
    let port_bit = nl.ports().iter().any(|p| p.bits.contains(&net));
    port_bit
        || nl
            .assigns()
            .iter()
            .any(|a| a.lhs == net || a.rhs == Conn::Net(net))
}

fn netlist_error(code: DiagCode, at: &str, e: &NetlistError) -> EditsError {
    EditsError::new(code, at, format!("{e}"))
}

/// Checks that a new scalar net may take `name`: no port, scalar net or bus has it.
fn free_net_name(nl: &GateNetlist, name: &str, at: &str) -> Result<(), EditsError> {
    let bus = nl.symbols().get(name).is_some_and(|s| {
        nl.nets()
            .any(|(_, n)| n.name == Some(s) && n.index.is_some())
    });
    if nl.find_port(name).is_some() || nl.find_net(name, None).is_some() || bus {
        return Err(EditsError::new(
            codes::NAME_IN_USE,
            at,
            format!("a port or net '{}' exists", names::for_display(name)),
        ));
    }
    Ok(())
}

/// The id of cell `name`, added with the pins of `cell_pins` when the netlist does not have it;
/// `check` sees the pins first and may refuse them, before the cell is added.
fn cell_id(
    nl: &mut GateNetlist,
    name: &str,
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
    check: &dyn Fn(&[(String, PinDir)]) -> Result<(), EditsError>,
) -> Result<CellId, EditsError> {
    if let Some(c) = nl.find_cell(name) {
        let pins: Vec<(String, PinDir)> = nl
            .cell(c)
            .map(|ct| {
                ct.pins
                    .iter()
                    .map(|p| (nl.symbols().resolve(p.name).to_string(), p.dir))
                    .collect()
            })
            .unwrap_or_default();
        check(&pins)?;
        return Ok(c);
    }
    let Some(pins) = cell_pins(name) else {
        return Err(EditsError::new(
            codes::UNKNOWN_CELL,
            "cell",
            format!("no cell '{}'", names::for_display(name)),
        ));
    };
    check(&pins)?;
    nl.add_cell_type(name, &pins)
        .map_err(|e| netlist_error(codes::UNKNOWN_CELL, "cell", &e))
}

/// The input and output cell pin indices of a buffer cell: exactly two pins, one input and one
/// output (WS-REQ-0147).
fn buffer_pins(pins: &[(String, PinDir)]) -> Option<(u32, u32)> {
    if pins.len() != 2 {
        return None;
    }
    let i = pins.iter().position(|p| p.1 == PinDir::Input)?;
    let o = pins.iter().position(|p| p.1 == PinDir::Output)?;
    Some((i as u32, o as u32))
}

/// Resolves the sinks of `net` (WS-REQ-0147, WS-REQ-0150): every pin sink is an input pin
/// connected to `net`; a port sink is an output port whose bit is `net` itself. Returns the pins
/// and whether a port is among the sinks.
fn resolve_sinks(
    nl: &GateNetlist,
    net: NetId,
    sinks: &[Sink],
) -> Result<(Vec<PinId>, bool), EditsError> {
    let mut pins = Vec::with_capacity(sinks.len());
    let mut port = false;
    for (k, s) in sinks.iter().enumerate() {
        let at = format!("sinks[{k}]");
        match s {
            Sink::Pin { instance, pin } => {
                let inst = find_instance(nl, instance, &at)?;
                let p = find_pin(nl, inst, pin, &at)?;
                if nl.pin(p).ok().and_then(|x| x.conn) != Some(Conn::Net(net)) {
                    return Err(EditsError::new(
                        codes::SINK,
                        at,
                        format!(
                            "pin '{}/{}' is not on the net",
                            names::for_display(instance),
                            names::for_display(pin)
                        ),
                    ));
                }
                if pin_dir(nl, p) != PinDir::Input {
                    return Err(EditsError::new(
                        codes::SINK,
                        at,
                        format!(
                            "pin '{}/{}' is not an input pin",
                            names::for_display(instance),
                            names::for_display(pin)
                        ),
                    ));
                }
                if pins.contains(&p) {
                    return Err(EditsError::new(codes::SINK, at, "the sink is listed twice"));
                }
                pins.push(p);
            }
            Sink::Port { port: name } => {
                let Some((pnet, dir)) = port_bit_net(nl, name) else {
                    return Err(EditsError::new(
                        codes::UNKNOWN_PIN,
                        at,
                        format!("no port bit '{}'", names::for_display(name)),
                    ));
                };
                if pnet != net {
                    return Err(EditsError::new(
                        codes::SINK,
                        at,
                        format!("port '{}' is not on the net", names::for_display(name)),
                    ));
                }
                if dir != PortDir::Output {
                    return Err(EditsError::new(
                        codes::SINK,
                        at,
                        format!("port '{}' is not an output port", names::for_display(name)),
                    ));
                }
                if port {
                    return Err(EditsError::new(codes::SINK, at, "the sink is listed twice"));
                }
                port = true;
            }
        }
    }
    Ok((pins, port))
}

/// Adds the net `name`, or reports why its name cannot be taken.
fn add_named_net(nl: &mut GateNetlist, name: &str, at: &str) -> Result<NetId, EditsError> {
    nl.add_net(Some(name), None)
        .map_err(|e| netlist_error(codes::NAME_IN_USE, at, &e))
}

/// `resize` (WS-REQ-0146): `swap_cell` of the instance to the cell.
fn resize(
    nl: &mut GateNetlist,
    instance: &str,
    cell: &str,
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
) -> Result<Applied, EditsError> {
    let inst = find_instance(nl, instance, "instance")?;
    let old: Vec<(String, PinDir)> = {
        let i = nl
            .instance(inst)
            .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
        let ct = nl
            .cell(i.cell)
            .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
        ct.pins
            .iter()
            .map(|p| (nl.symbols().resolve(p.name).to_string(), p.dir))
            .collect()
    };
    let compatible = |pins: &[(String, PinDir)]| {
        let mut a = old.clone();
        let mut b = pins.to_vec();
        a.sort_by(|x, y| x.0.cmp(&y.0));
        b.sort_by(|x, y| x.0.cmp(&y.0));
        if a == b {
            Ok(())
        } else {
            Err(EditsError::new(
                codes::NETLIST,
                "cell",
                format!(
                    "cell '{}' does not have the pins of instance '{}'",
                    names::for_display(cell),
                    names::for_display(instance)
                ),
            ))
        }
    };
    let c = cell_id(nl, cell, cell_pins, &compatible)?;
    nl.swap_cell(inst, c)
        .map_err(|e| netlist_error(codes::NETLIST, "cell", &e))?;
    let mut applied = Applied::default();
    applied.inst(inst);
    Ok(applied)
}

/// `insert_buffer` (WS-REQ-0147).
#[allow(clippy::too_many_arguments)]
fn insert_buffer(
    nl: &mut GateNetlist,
    net: &str,
    cell: &str,
    instance: &str,
    net_out: &str,
    sinks: &[Sink],
    cell_pins: &dyn Fn(&str) -> Option<Vec<(String, PinDir)>>,
) -> Result<Applied, EditsError> {
    let n = find_net(nl, net)?;
    free_instance_name(nl, instance, "instance")?;
    free_net_name(nl, net_out, "net_out")?;
    let (pins, port) = resolve_sinks(nl, n, sinks)?;
    let is_buffer = |pins: &[(String, PinDir)]| match buffer_pins(pins) {
        Some(_) => Ok(()),
        None => Err(EditsError::new(
            codes::BUFFER,
            "cell",
            format!(
                "cell '{}' does not have exactly one input and one output pin",
                names::for_display(cell)
            ),
        )),
    };
    let c = cell_id(nl, cell, cell_pins, &is_buffer)?;
    let pin_list: Vec<(String, PinDir)> = nl
        .cell(c)
        .map(|ct| {
            ct.pins
                .iter()
                .map(|p| (nl.symbols().resolve(p.name).to_string(), p.dir))
                .collect()
        })
        .unwrap_or_default();
    let Some((k_in, k_out)) = buffer_pins(&pin_list) else {
        return Err(EditsError::new(codes::BUFFER, "cell", "not a buffer cell"));
    };
    let mut applied = Applied::default();
    if !port {
        let (buf, out) = nl
            .insert_buffer(n, &pins, c, k_in, k_out, instance, Some(net_out))
            .map_err(|e| {
                let code = match e {
                    NetlistError::NetConflict { .. } | NetlistError::DuplicateName { .. } => {
                        codes::NAME_IN_USE
                    }
                    NetlistError::InvalidName { .. } | NetlistError::EmptyName { .. } => {
                        codes::NAME
                    }
                    _ => codes::NETLIST,
                };
                netlist_error(code, "net_out", &e)
            })?;
        applied.inst(buf);
        for &p in &pins {
            applied.pin_owner(nl, p);
        }
        applied.net(n);
        applied.net(out);
        return Ok(applied);
    }
    // A port bit cannot change nets: the buffer drives `net`, and the pins that stay with the
    // driver move to the new net (WS-REQ-0147).
    let m = add_named_net(nl, net_out, "net_out")?;
    let buf = match nl.add_instance(instance, c) {
        Ok(b) => b,
        Err(e) => {
            let _ = nl.remove_net(m);
            return Err(netlist_error(codes::NAME_IN_USE, "instance", &e));
        }
    };
    let stay: Vec<PinId> = nl
        .net(n)
        .map(|x| {
            x.pins
                .iter()
                .copied()
                .filter(|p| !pins.contains(p))
                .collect()
        })
        .unwrap_or_default();
    let (pin_in, pin_out) = {
        let b = nl
            .instance(buf)
            .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
        (b.pins[k_in as usize], b.pins[k_out as usize])
    };
    for &p in &stay {
        nl.move_pin(p, Conn::Net(m))
            .map_err(|e| netlist_error(codes::NETLIST, "net", &e))?;
        applied.pin_owner(nl, p);
    }
    nl.connect_pin(pin_in, Conn::Net(m))
        .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
    nl.connect_pin(pin_out, Conn::Net(n))
        .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
    applied.inst(buf);
    applied.net(n);
    applied.net(m);
    Ok(applied)
}

/// The connected input and output pins of a buffer instance and their nets.
struct BufferNets {
    pin_in: PinId,
    pin_out: PinId,
    a: NetId,
    b: NetId,
}

fn buffer_nets(nl: &GateNetlist, inst: InstId, instance: &str) -> Result<BufferNets, EditsError> {
    let reject = |why: &str| {
        EditsError::new(
            codes::BUFFER,
            "instance",
            format!("instance '{}' {why}", names::for_display(instance)),
        )
    };
    let i = nl
        .instance(inst)
        .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
    let ct = nl
        .cell(i.cell)
        .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
    let pins: Vec<(String, PinDir)> = ct.pins.iter().map(|p| (String::new(), p.dir)).collect();
    let Some((k_in, k_out)) = buffer_pins(&pins) else {
        return Err(reject(
            "is not an instance of a cell with one input and one output pin",
        ));
    };
    let (pin_in, pin_out) = (i.pins[k_in as usize], i.pins[k_out as usize]);
    let conn = |p: PinId| nl.pin(p).ok().and_then(|x| x.conn);
    let (Some(Conn::Net(a)), Some(Conn::Net(b))) = (conn(pin_in), conn(pin_out)) else {
        return Err(reject("does not connect its input and its output to nets"));
    };
    if a == b {
        return Err(reject("connects its input and its output to the same net"));
    }
    Ok(BufferNets {
        pin_in,
        pin_out,
        a,
        b,
    })
}

/// `remove_buffer` (WS-REQ-0148).
fn remove_buffer(nl: &mut GateNetlist, instance: &str) -> Result<Applied, EditsError> {
    let inst = find_instance(nl, instance, "instance")?;
    let BufferNets {
        pin_in,
        pin_out,
        a,
        b,
    } = buffer_nets(nl, inst, instance)?;
    let mut applied = Applied::default();
    let others: Vec<PinId> = nl
        .net(b)
        .map(|x| x.pins.iter().copied().filter(|&p| p != pin_out).collect())
        .unwrap_or_default();
    if others.iter().any(|&p| pin_dir(nl, p) != PinDir::Input) {
        return Err(EditsError::new(
            codes::BUFFER,
            "instance",
            format!(
                "another pin than the output of '{}' drives its output net",
                names::for_display(instance)
            ),
        ));
    }
    if !net_in_use(nl, b) {
        let kept = nl
            .remove_buffer(inst)
            .map_err(|e| netlist_error(codes::BUFFER, "instance", &e))?;
        for &p in &others {
            applied.pin_owner(nl, p);
        }
        applied.net(kept);
        return Ok(applied);
    }
    if net_in_use(nl, a) {
        return Err(EditsError::new(
            codes::BUFFER,
            "instance",
            format!(
                "both nets of '{}' are bits of ports or appear in assignments",
                names::for_display(instance)
            ),
        ));
    }
    // The output net is a port bit, which cannot be removed: the pins of the input net move to
    // it, and the input net goes (WS-REQ-0148).
    let moved: Vec<PinId> = nl
        .net(a)
        .map(|x| x.pins.iter().copied().filter(|&p| p != pin_in).collect())
        .unwrap_or_default();
    for &p in &moved {
        nl.move_pin(p, Conn::Net(b))
            .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
        applied.pin_owner(nl, p);
    }
    nl.remove_instance(inst)
        .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
    nl.remove_net(a)
        .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
    applied.net(b);
    Ok(applied)
}

/// `swap_pins` (WS-REQ-0149).
fn swap_pins(
    nl: &mut GateNetlist,
    instance: &str,
    pin_a: &str,
    pin_b: &str,
) -> Result<Applied, EditsError> {
    let inst = find_instance(nl, instance, "instance")?;
    if pin_a == pin_b {
        return Err(EditsError::new(
            codes::PIN_SWAP,
            "pin_b",
            "the two pins of a pin swap are the same pin",
        ));
    }
    let pa = find_pin(nl, inst, pin_a, "pin_a")?;
    let pb = find_pin(nl, inst, pin_b, "pin_b")?;
    for (p, name, at) in [(pa, pin_a, "pin_a"), (pb, pin_b, "pin_b")] {
        if pin_dir(nl, p) != PinDir::Input {
            return Err(EditsError::new(
                codes::PIN_SWAP,
                at,
                format!("pin '{}' is not an input pin", names::for_display(name)),
            ));
        }
    }
    let conn = |p: PinId| nl.pin(p).ok().and_then(|x| x.conn);
    let (Some(ca), Some(cb)) = (conn(pa), conn(pb)) else {
        return Err(EditsError::new(
            codes::PIN_SWAP,
            "instance",
            "both pins of a pin swap must be connected",
        ));
    };
    let mut applied = Applied::default();
    applied.inst(inst);
    if ca != cb {
        nl.move_pin(pa, cb)
            .map_err(|e| netlist_error(codes::NETLIST, "pin_a", &e))?;
        nl.move_pin(pb, ca)
            .map_err(|e| netlist_error(codes::NETLIST, "pin_b", &e))?;
        for c in [ca, cb] {
            if let Conn::Net(n) = c {
                applied.net(n);
            }
        }
    }
    Ok(applied)
}

/// `clone` (WS-REQ-0150).
fn clone(
    nl: &mut GateNetlist,
    instance: &str,
    new_instance: &str,
    new_net: &str,
    sinks: &[Sink],
) -> Result<Applied, EditsError> {
    let inst = find_instance(nl, instance, "instance")?;
    free_instance_name(nl, new_instance, "new_instance")?;
    free_net_name(nl, new_net, "new_net")?;
    let (cell, connected, out_pin) = {
        let i = nl
            .instance(inst)
            .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
        let ct = nl
            .cell(i.cell)
            .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
        let outs: Vec<usize> = ct
            .pins
            .iter()
            .enumerate()
            .filter(|(_, p)| p.dir.drives())
            .map(|(k, _)| k)
            .collect();
        if outs.len() != 1 || ct.pins[outs[0]].dir != PinDir::Output {
            return Err(EditsError::new(
                codes::CLONE,
                "instance",
                format!(
                    "instance '{}' does not have exactly one output pin and no inout pin",
                    names::for_display(instance)
                ),
            ));
        }
        (i.cell, i.connected.clone(), i.pins[outs[0]])
    };
    let Some(Conn::Net(n)) = nl.pin(out_pin).ok().and_then(|p| p.conn) else {
        return Err(EditsError::new(
            codes::CLONE,
            "instance",
            format!(
                "the output pin of '{}' is not on a net",
                names::for_display(instance)
            ),
        ));
    };
    let (pins, port) = resolve_sinks(nl, n, sinks)?;
    let m = add_named_net(nl, new_net, "new_net")?;
    let k = match nl.add_instance(new_instance, cell) {
        Ok(k) => k,
        Err(e) => {
            let _ = nl.remove_net(m);
            return Err(netlist_error(codes::NAME_IN_USE, "new_instance", &e));
        }
    };
    let stay: Vec<PinId> = if port {
        nl.net(n)
            .map(|x| {
                x.pins
                    .iter()
                    .copied()
                    .filter(|p| !pins.contains(p))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut applied = Applied::default();
    for &p in &connected {
        let (cell_pin, conn) = match nl.pin(p) {
            Ok(x) => (x.cell_pin, x.conn),
            Err(e) => return Err(netlist_error(codes::NETLIST, "instance", &e)),
        };
        let target = if p == out_pin {
            Conn::Net(if port { n } else { m })
        } else {
            match conn {
                Some(c) => c,
                None => continue,
            }
        };
        let q = nl
            .find_pin(k, cell_pin)
            .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
        nl.connect_pin(q, target)
            .map_err(|e| netlist_error(codes::NETLIST, "instance", &e))?;
        if let Conn::Net(x) = target {
            applied.net(x);
        }
    }
    applied.inst(k);
    let moved = if port { stay } else { pins };
    for &p in &moved {
        nl.move_pin(p, Conn::Net(m))
            .map_err(|e| netlist_error(codes::NETLIST, "sinks", &e))?;
        applied.pin_owner(nl, p);
    }
    applied.net(n);
    applied.net(m);
    Ok(applied)
}
