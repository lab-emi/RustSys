// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Errors of the netlist mutation API. Names in the messages are resolved when the error is
//! made, so an error can outlive its netlist.

use std::fmt;

use crate::names::NameError;

/// A rejected netlist operation; the netlist is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetlistError {
    /// An id that addresses no live entry of this netlist: its entry was removed, or it comes
    /// from another netlist. An id of another netlist that happens to address a live entry of
    /// this one is not detected (see [`Id`](crate::arena::Id)).
    StaleId {
        kind: &'static str,
        index: u32,
        generation: u32,
    },
    /// An empty name.
    EmptyName { kind: &'static str },
    /// A name that breaks the rules of [`names`](crate::names) for its kind (CORE-REQ-0123),
    /// shown with every character outside printable ASCII as `\u{...}`.
    InvalidName {
        kind: &'static str,
        name: String,
        problem: NameError,
    },
    /// A cell of this name exists with other pins.
    CellConflict { cell: String },
    /// A cell type lists a pin name twice.
    DuplicateCellPin { cell: String, pin: String },
    /// A port or instance of this name exists.
    DuplicateName { kind: &'static str, name: String },
    /// The net name and index are taken, or the name belongs to a port or to nets of the other
    /// kind (scalar versus indexed).
    NetConflict { net: String, reason: &'static str },
    /// A bit index or port range uses the reserved value `i32::MIN`, a port range is wider than
    /// [`MAX_RANGE_WIDTH`](super::MAX_RANGE_WIDTH), or an anonymous net has an index.
    InvalidIndex { what: String },
    /// The cell has no such pin.
    UnknownPin { cell: String, pin: String },
    /// The pin is connected already.
    AlreadyConnected { pin: String },
    /// `swap_cell` to a cell whose pins differ.
    IncompatibleCell {
        inst: String,
        cell: String,
        reason: String,
    },
    /// A buffer sink is not connected to the buffered net.
    NotOnNet { pin: String, net: String },
    /// A buffer sink drives its net (output or inout pin).
    SinkIsDriver { pin: String },
    /// A buffer sink is listed twice.
    DuplicateSink { pin: String },
    /// A buffer pin has the wrong direction.
    BufferPin {
        cell: String,
        pin: String,
        expected: &'static str,
    },
    /// The pin is not connected.
    NotConnected { pin: String },
    /// A net to remove has pins, is a bit of a port or appears in an assignment.
    NetInUse { net: String, reason: &'static str },
    /// No assignment at this position.
    NoAssign { index: usize, count: usize },
    /// `remove_buffer` of an instance that is not a buffer between two nets.
    NotABuffer { inst: String, reason: String },
}

impl fmt::Display for NetlistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetlistError::StaleId {
                kind,
                index,
                generation,
            } => write!(
                f,
                "stale {kind} id (slot {index}, generation {generation}): it addresses no live \
                 {kind} of this netlist"
            ),
            NetlistError::EmptyName { kind } => write!(f, "empty {kind} name"),
            NetlistError::InvalidName {
                kind,
                name,
                problem,
            } => write!(f, "invalid {kind} name '{name}': {problem}"),
            NetlistError::CellConflict { cell } => {
                write!(f, "cell '{cell}' exists with different pins")
            }
            NetlistError::DuplicateCellPin { cell, pin } => {
                write!(f, "cell '{cell}' lists pin '{pin}' twice")
            }
            NetlistError::DuplicateName { kind, name } => {
                write!(f, "{kind} '{name}' exists already")
            }
            NetlistError::NetConflict { net, reason } => write!(f, "net '{net}': {reason}"),
            NetlistError::InvalidIndex { what } => write!(f, "{what}"),
            NetlistError::UnknownPin { cell, pin } => write!(f, "cell '{cell}' has no pin {pin}"),
            NetlistError::AlreadyConnected { pin } => write!(f, "pin '{pin}' is connected already"),
            NetlistError::IncompatibleCell { inst, cell, reason } => {
                write!(f, "instance '{inst}' cannot take cell '{cell}': {reason}")
            }
            NetlistError::NotOnNet { pin, net } => {
                write!(f, "pin '{pin}' is not connected to net '{net}'")
            }
            NetlistError::SinkIsDriver { pin } => {
                write!(
                    f,
                    "pin '{pin}' drives its net and cannot move behind a buffer"
                )
            }
            NetlistError::DuplicateSink { pin } => write!(f, "pin '{pin}' is listed twice"),
            NetlistError::BufferPin {
                cell,
                pin,
                expected,
            } => write!(
                f,
                "pin {pin} of buffer cell '{cell}' is not an {expected} pin"
            ),
            NetlistError::NotConnected { pin } => write!(f, "pin '{pin}' is not connected"),
            NetlistError::NetInUse { net, reason } => {
                write!(f, "net '{net}' cannot be removed: {reason}")
            }
            NetlistError::NoAssign { index, count } => {
                write!(
                    f,
                    "no assignment at position {index}: the netlist has {count}"
                )
            }
            NetlistError::NotABuffer { inst, reason } => {
                write!(f, "instance '{inst}' is not a removable buffer: {reason}")
            }
        }
    }
}

impl std::error::Error for NetlistError {}
