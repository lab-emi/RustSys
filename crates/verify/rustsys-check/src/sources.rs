// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Sources other than RTL (CHECK-REQ-0100 to CHECK-REQ-0107): which reader a check takes, the
//! initial-value mode, and the names of the generated ports, written from the specification
//! (SYNTH-REQ-0900, SYNTH-REQ-0904, SYNTH-REQ-0906), not from engine code.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Input;
use crate::rtl::RtlModel;

/// The code of a source error of the checker's readers (CHECK-REQ-0101 to CHECK-REQ-0103).
pub(crate) const SOURCE: &str = "C0900";
/// The code of a construct that the checker's BLIF reader skips (CHECK-REQ-0101).
pub(crate) const SKIPPED: &str = "C0901";
/// The code of a cell outside the checker's model of BLIF cells and gate-level references
/// (CHECK-REQ-0103, CHECK-REQ-0104).
pub(crate) const CELL: &str = "C0902";
/// The code of the warning about BLIF signals without a driver, which read as 0
/// (CHECK-REQ-0101).
pub(crate) const UNDRIVEN: &str = "C0903";

/// What declared initial values of BLIF and AIGER state become (CHECK-REQ-0105,
/// SYNTH-REQ-0906).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InitialValues {
    /// An asynchronous control from a generated port.
    #[default]
    Reset,
    /// Not implemented: obligations for every initial state.
    Arbitrary,
}

impl InitialValues {
    pub fn name(self) -> &'static str {
        match self {
            InitialValues::Reset => "reset",
            InitialValues::Arbitrary => "arbitrary",
        }
    }
}

/// The kind of the reference sources of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Rtl,
    Blif,
    Aiger,
    Gates,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Rtl => "systemverilog",
            Kind::Blif => "blif",
            Kind::Aiger => "aiger",
            Kind::Gates => "gate-netlist",
        }
    }
}

/// The kind of the sources: a gate-level reference when asked, else by the extensions of the
/// names, ignoring letter case; sources of several kinds, or several AIGER files, are an error
/// (CHECK-REQ-0100).
pub(crate) fn kind(input: &Input, gate_netlist: bool) -> Result<Kind, String> {
    let names: Vec<String> = match input {
        Input::Files(f) => f.iter().map(|p| p.display().to_string()).collect(),
        Input::Texts(t) => t.iter().map(|(n, _)| n.clone()).collect(),
    };
    if gate_netlist {
        return Ok(Kind::Gates);
    }
    let of = |n: &str| match Path::new(n)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .as_deref()
    {
        Some("blif") => Kind::Blif,
        Some("aag") | Some("aig") => Kind::Aiger,
        _ => Kind::Rtl,
    };
    let kinds: Vec<Kind> = names.iter().map(|n| of(n)).collect();
    let Some(&k) = kinds.first() else {
        return Ok(Kind::Rtl);
    };
    if kinds.iter().any(|x| *x != k) {
        return Err(format!(
            "error[{SOURCE}]: the sources mix formats: {}",
            names.join(", ")
        ));
    }
    if k == Kind::Aiger && names.len() != 1 {
        return Err(format!(
            "error[{SOURCE}]: an AIGER check reads one file, not {}",
            names.len()
        ));
    }
    Ok(k)
}

/// The bytes of each source, with its name.
pub(crate) fn bytes(input: &Input) -> Result<Vec<(String, Vec<u8>)>, String> {
    match input {
        Input::Texts(t) => Ok(t
            .iter()
            .map(|(n, s)| (n.clone(), s.as_bytes().to_vec()))
            .collect()),
        Input::Files(f) => f
            .iter()
            .map(|p| {
                std::fs::read(p)
                    .map(|b| (p.display().to_string(), b))
                    .map_err(|e| format!("error[{SOURCE}]: cannot read {}: {e}", p.display()))
            })
            .collect(),
    }
}

/// `base`, or `base_<n>` with the smallest n from 1, that no name in `used` is (SYNTH-REQ-0904,
/// SYNTH-REQ-0906).
pub(crate) fn generated(base: &str, used: &dyn Fn(&str) -> bool) -> String {
    let mut name = base.to_string();
    let mut n = 0u64;
    while used(&name) {
        n += 1;
        name = format!("{base}_{n}");
    }
    name
}

/// A reference model of a source other than RTL, with what the report records about it.
#[derive(Debug)]
pub(crate) struct SourceModel {
    pub model: RtlModel,
    /// Declared initial values, by state name, in state order.
    pub initial_values: Vec<(String, bool)>,
    /// Constructs skipped with a warning (CHECK-REQ-0101).
    pub warnings: Vec<String>,
}
