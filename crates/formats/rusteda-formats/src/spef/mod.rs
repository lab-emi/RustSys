// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Bounded IEEE 1481 detailed RC reader. Grounded inter-net coupling is explicit;
//! unsupported semantic constructs fail instead of silently changing timing.

mod parse;
mod source;

use serde::{Deserialize, Serialize};

pub use parse::read;
pub use source::{DecodedSource, SourceEncoding, decode_source};

/// Hard parser budgets, retained with the imported source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_nets: usize,
    pub max_nodes: usize,
    pub max_elements: usize,
    pub max_name_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 32 << 20,
            max_nets: 100_000,
            max_nodes: 500_000,
            max_elements: 1_000_000,
            max_name_bytes: 16 << 20,
        }
    }
}

impl Limits {
    /// Absolute supported parser ceilings, also used for public typed inputs.
    pub fn absolute_max() -> Self {
        let d = Self::default();
        Self {
            max_bytes: d.max_bytes * 8,
            max_nets: d.max_nets * 8,
            max_nodes: d.max_nodes * 8,
            max_elements: d.max_elements * 8,
            max_name_bytes: d.max_name_bytes * 8,
        }
    }
    /// Reject zero or excessive budgets even for an on-disk table.
    pub fn validate(&self) -> Result<(), Error> {
        let d = Self::default();
        for (value, default) in [
            (self.max_bytes, d.max_bytes),
            (self.max_nets, d.max_nets),
            (self.max_nodes, d.max_nodes),
            (self.max_elements, d.max_elements),
            (self.max_name_bytes, d.max_name_bytes),
        ] {
            if value == 0 || value > default * 8 {
                return Err(Error::new(
                    0,
                    "T0072",
                    "invalid SPEF budget (allowed 1..8x default)",
                ));
            }
        }
        Ok(())
    }
}

/// A structured parse/budget error with a one-based source line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code} at SPEF line {line}: {message}")]
pub struct Error {
    pub line: usize,
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub(crate) fn new(line: usize, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            line,
            code,
            message: message.into(),
        }
    }
}

/// A local RC node (per-net indices never escape their net).
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    pub cap_pf: f64,
}

/// A physical connection, with the source's declared direction.
#[derive(Debug, Clone, PartialEq)]
pub struct Connection {
    pub node: u32,
    pub port: bool,
    pub direction: char,
}

/// Resistance in kOhm between local node indices.
#[derive(Debug, Clone, PartialEq)]
pub struct Resistor {
    pub a: u32,
    pub b: u32,
    pub kohm: f64,
}

/// One detailed net, with inter-net coupling grounded at factor one.
#[derive(Debug, Clone, PartialEq)]
pub struct Net {
    pub name: String,
    pub total_cap_pf: f64,
    pub nodes: Vec<Node>,
    pub connections: Vec<Connection>,
    pub resistors: Vec<Resistor>,
}

/// Immutable parsed parasitics. The whole object can be shared between corners.
#[derive(Debug, Clone, PartialEq)]
pub struct Spef {
    pub design: String,
    pub divider: char,
    pub delimiter: char,
    pub bus_delimiter: (char, char),
    pub nets: Vec<Net>,
    pub coupling_records: usize,
    pub metadata_records: usize,
    pub node_count: usize,
    pub element_count: usize,
}

impl Spef {
    /// Validate public in-memory input before indexing or allocating engine state.
    /// Parser counters are not trusted merely because the struct is public.
    pub fn validate(&self, limits: Limits) -> Result<(), Error> {
        limits.validate()?;
        let fail = |message| Error::new(0, "T0071", message);
        if self.design.is_empty() || self.design.len() > 4096 {
            return Err(fail("invalid RC design name"));
        }
        if self.nets.is_empty() || self.nets.len() > limits.max_nets {
            return Err(Error::new(0, "T0072", "invalid RC net count"));
        }
        let mut count = 0usize;
        let mut resistors = 0usize;
        let mut bytes = self.design.len();
        let mut names = std::collections::BTreeSet::new();
        for net in &self.nets {
            count = count
                .checked_add(net.nodes.len())
                .ok_or_else(|| fail("RC count overflow"))?;
            resistors = resistors
                .checked_add(net.resistors.len())
                .ok_or_else(|| fail("RC count overflow"))?;
            if count > limits.max_nodes || resistors > limits.max_elements {
                return Err(Error::new(0, "T0072", "in-memory RC budget exceeded"));
            }
            if net.name.is_empty()
                || net.name.len() > 4096
                || !names.insert(net.name.as_str())
                || net.nodes.is_empty()
                || !net.total_cap_pf.is_finite()
                || net.total_cap_pf < 0.0
            {
                return Err(fail("invalid or duplicate RC net"));
            }
            bytes = bytes.saturating_add(net.name.len());
            let mut node_names = std::collections::BTreeSet::new();
            let mut sum = 0.0;
            for node in &net.nodes {
                if node.name.is_empty()
                    || node.name.len() > 4096
                    || !node_names.insert(node.name.as_str())
                    || !node.cap_pf.is_finite()
                    || node.cap_pf < 0.0
                {
                    return Err(fail("invalid or duplicate RC node"));
                }
                bytes = bytes.saturating_add(node.name.len());
                sum += node.cap_pf;
            }
            if !sum.is_finite()
                || (sum - net.total_cap_pf).abs() > 1e-8_f64.max(net.total_cap_pf * 1e-4)
            {
                return Err(fail("RC total capacitance mismatch"));
            }
            let mut connections = std::collections::BTreeSet::new();
            if net.connections.iter().any(|c| {
                c.node as usize >= net.nodes.len()
                    || !matches!(c.direction, 'I' | 'O' | 'B')
                    || !connections.insert(c.node)
            }) {
                return Err(fail("invalid or duplicate RC connection"));
            }
            if net.resistors.iter().any(|r| {
                r.a as usize >= net.nodes.len()
                    || r.b as usize >= net.nodes.len()
                    || !r.kohm.is_finite()
                    || r.kohm < 0.0
            }) {
                return Err(fail("invalid RC resistor"));
            }
            if bytes > limits.max_name_bytes {
                return Err(Error::new(0, "T0072", "RC name budget exceeded"));
            }
        }
        if count != self.node_count
            || self.element_count < resistors
            || self.element_count > limits.max_elements
        {
            return Err(fail("inconsistent RC allocation counters"));
        }
        Ok(())
    }

    /// Conservative additional peak state, including parser and RC tree temporaries.
    pub fn estimated_state_bytes(&self) -> u64 {
        let nodes = self
            .nets
            .iter()
            .fold(0u64, |n, net| n.saturating_add(net.nodes.len() as u64));
        let resistors = self
            .nets
            .iter()
            .fold(0u64, |n, net| n.saturating_add(net.resistors.len() as u64));
        nodes
            .saturating_mul(448)
            .saturating_add(
                (self.element_count as u64)
                    .max(resistors)
                    .saturating_mul(64),
            )
            .saturating_add((self.nets.len() as u64).saturating_mul(256))
    }
}

/// Metadata of the PARASIT v1 source container (section 2; source is section 1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub corner: String,
    pub limits: Limits,
}

/// One corner of a PARASIT v2 container (RCLI-REQ-0075): its limits and the identity of the
/// file it was imported from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CornerSource {
    pub corner: String,
    pub limits: Limits,
    /// The source path as given at import.
    pub source_path: String,
    /// The hash of the source file, as the workspace writes source hashes.
    pub source_hash: String,
}

/// Metadata of the PARASIT v2 container (section 1, RCLI-REQ-0075): the corners in ascending
/// name order; the decoded source of entry `i` is section `i + 2`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CornerSetMetadata {
    pub corners: Vec<CornerSource>,
}

#[cfg(test)]
mod tests;
