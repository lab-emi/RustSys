// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! AIGER reader (`docs/specs/aiger/`, AIGER-REQ-0001 to AIGER-REQ-0032) [@biere2007aiger;
//! @biere2011aiger]: the ASCII (`aag`) and binary (`aig`) forms with the version 1.9 header,
//! latch reset values, the symbol table and the comment section.

mod read;

use std::collections::BTreeMap;
use std::path::Path;

pub use rusteda_core::diag::{Diag, Severity};

/// Bounds checked against the header before anything is allocated (AIGER-REQ-0030).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest maximum variable index `M`.
    pub max_vars: u32,
    /// The most outputs and properties together.
    pub max_items: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_vars: 1 << 24,
            max_items: 1 << 24,
        }
    }
}

/// Options of a read (AIGER-REQ-0001, AIGER-REQ-0022).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Accept bad-state, constraint, justice and fairness sections instead of rejecting them.
    pub allow_properties: bool,
    pub limits: Limits,
}

/// The reset value of a latch (AIGER-REQ-0021).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    Zero,
    One,
    /// The reset is the latch's own literal.
    Uninitialized,
}

/// A latch: its current-state literal (even), next-state literal and reset value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latch {
    pub lit: u32,
    pub next: u32,
    pub reset: Reset,
}

/// Symbol names by position (AIGER-REQ-0023), kept only for the positions that have one, so that
/// their memory follows the symbol lines, not the header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Symbols {
    pub inputs: BTreeMap<u32, String>,
    pub latches: BTreeMap<u32, String>,
    pub outputs: BTreeMap<u32, String>,
    pub bad: BTreeMap<u32, String>,
    pub constraints: BTreeMap<u32, String>,
    pub justice: BTreeMap<u32, String>,
    pub fairness: BTreeMap<u32, String>,
}

/// An and-inverter graph (AIGER-REQ-0002).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Aig {
    pub max_var: u32,
    /// Input literals (even), in input order.
    pub inputs: Vec<u32>,
    pub latches: Vec<Latch>,
    pub outputs: Vec<u32>,
    pub bad: Vec<u32>,
    pub constraints: Vec<u32>,
    pub justice: Vec<Vec<u32>>,
    pub fairness: Vec<u32>,
    /// `[lhs, rhs0, rhs1]`, every gate after the gates it reads.
    pub ands: Vec<[u32; 3]>,
    pub symbols: Symbols,
    pub binary: bool,
}

impl Aig {
    /// Evaluates the outputs and next-state literals for input and latch values, for tests and
    /// simple consumers: `values[v]` is the value of variable `v`, filled for the inputs and
    /// latches; the AND gates are filled in.
    pub fn eval(&self, values: &mut [bool]) {
        for &[lhs, a, b] in &self.ands {
            let v = lit(values, a) && lit(values, b);
            values[(lhs >> 1) as usize] = v;
        }
    }
}

/// The value of literal `l` under variable values `values` (variable 0 is false).
pub fn lit(values: &[bool], l: u32) -> bool {
    let v = (l >> 1) as usize;
    let x = if v == 0 { false } else { values[v] };
    x ^ (l & 1 == 1)
}

pub(crate) const UNREADABLE: &str = "G0001";
pub(crate) const HEADER: &str = "G0002";
pub(crate) const DEFINITION: &str = "G0003";
pub(crate) const UNDEFINED: &str = "G0004";
pub(crate) const CYCLE: &str = "G0005";
pub(crate) const BINARY: &str = "G0006";
pub(crate) const SYMBOLS: &str = "G0007";
pub(crate) const LIMIT: &str = "G0008";
pub(crate) const LINE: &str = "G0009";
pub(crate) const PROPERTIES: &str = "G0010";

/// Reads an AIGER file from disk (AIGER-REQ-0001).
pub fn read_file(path: &Path, opts: &Options) -> Result<Aig, Vec<Diag>> {
    let name = path.display().to_string();
    match std::fs::read(path) {
        Ok(bytes) => read_bytes(&bytes, &name, opts),
        Err(e) => Err(vec![Diag::error(UNREADABLE, format!("{name}: {e}"))]),
    }
}

/// Reads AIGER `bytes`, located as `name` in messages (AIGER-REQ-0001).
pub fn read_bytes(bytes: &[u8], name: &str, opts: &Options) -> Result<Aig, Vec<Diag>> {
    read::read(bytes, name, opts).map_err(|d| vec![d])
}
