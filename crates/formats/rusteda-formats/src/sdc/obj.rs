// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Unresolved SDC design-object query AST (`Obj` and `ObjQuery`; SDC-REQ-0005).

use super::filter::Filter;

/// Arguments common to `get_ports`, `get_pins`, `get_cells`, `get_nets`, `get_clocks`,
/// `get_lib_cells`, and `get_lib_pins` (SDC-REQ-0005).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ObjQuery {
    /// Positional name patterns (glob by default, or regular expressions when `regexp` is true).
    pub patterns: Vec<String>,
    /// `-hierarchical` (`-hier`).
    pub hierarchical: bool,
    /// `-regexp`.
    pub regexp: bool,
    /// `-nocase`.
    pub nocase: bool,
    /// `-of_objects <obj>`.
    pub of_objects: Option<Box<Obj>>,
    /// `-filter <expr>`.
    pub filter: Option<Filter>,
    /// `-quiet`.
    pub quiet: bool,
}

impl ObjQuery {
    /// Creates a simple pattern query with default flags.
    pub fn from_patterns(patterns: Vec<String>) -> Self {
        Self {
            patterns,
            ..Self::default()
        }
    }
}

/// Symbolic design-object query tree stored in `Sdc` and `CONSTR v1` and resolved against the
/// netlist at analysis time (SDC-REQ-0005).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Obj {
    /// `get_ports` (or alias `get_port`).
    Ports(ObjQuery),
    /// `get_pins`.
    Pins(ObjQuery),
    /// `get_cells`.
    Cells(ObjQuery),
    /// `get_nets`.
    Nets(ObjQuery),
    /// `get_clocks`.
    Clocks(ObjQuery),
    /// `get_lib_cells`.
    LibCells(ObjQuery),
    /// `get_lib_pins`.
    LibPins(ObjQuery),
    /// `all_inputs` (`-no_clocks`).
    AllInputs { no_clocks: bool },
    /// `all_outputs`.
    AllOutputs,
    /// `all_clocks`.
    AllClocks,
    /// `all_registers`.
    AllRegisters,
    /// `current_design` used as an object query.
    CurrentDesign,
    /// Bare name or list of names (`core_clock`, `{a[0] a[1]}`).
    Names(Vec<String>),
    /// Symbolic union of object collections (`[list [get_ports a] [get_ports b]]`,
    /// `add_to_collection`).
    Union(Vec<Obj>),
    /// Symbolic difference of object collections (`remove_from_collection`,
    /// `lsearch`/`lreplace`).
    Diff(Box<Obj>, Box<Obj>),
}

impl Obj {
    /// Builds a simple `get_ports` query from pattern strings.
    pub fn ports<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Obj::Ports(ObjQuery::from_patterns(
            patterns.into_iter().map(Into::into).collect(),
        ))
    }

    /// Builds a simple `get_pins` query from pattern strings.
    pub fn pins<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Obj::Pins(ObjQuery::from_patterns(
            patterns.into_iter().map(Into::into).collect(),
        ))
    }

    /// Builds a simple `get_cells` query from pattern strings.
    pub fn cells<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Obj::Cells(ObjQuery::from_patterns(
            patterns.into_iter().map(Into::into).collect(),
        ))
    }

    /// Builds a simple `get_nets` query from pattern strings.
    pub fn nets<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Obj::Nets(ObjQuery::from_patterns(
            patterns.into_iter().map(Into::into).collect(),
        ))
    }

    /// Builds a simple `get_clocks` query from pattern strings.
    pub fn clocks<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Obj::Clocks(ObjQuery::from_patterns(
            patterns.into_iter().map(Into::into).collect(),
        ))
    }

    /// Builds a simple `get_lib_cells` query from pattern strings.
    pub fn lib_cells<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Obj::LibCells(ObjQuery::from_patterns(
            patterns.into_iter().map(Into::into).collect(),
        ))
    }

    /// Builds a simple `get_lib_pins` query from pattern strings.
    pub fn lib_pins<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Obj::LibPins(ObjQuery::from_patterns(
            patterns.into_iter().map(Into::into).collect(),
        ))
    }

    /// Builds a `Names` node from one or more strings.
    pub fn names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Obj::Names(names.into_iter().map(Into::into).collect())
    }

    /// Flattens and normalizes a union of object queries.
    pub fn union(items: Vec<Obj>) -> Self {
        let mut flat = Vec::new();
        for item in items {
            match item {
                Obj::Union(sub) => flat.extend(sub),
                other => flat.push(other),
            }
        }
        if flat.len() == 1 {
            flat.into_iter().next().unwrap()
        } else {
            Obj::Union(flat)
        }
    }

    /// Builds a symbolic collection difference `lhs \ rhs`.
    pub fn diff(lhs: Obj, rhs: Obj) -> Self {
        Obj::Diff(Box::new(lhs), Box::new(rhs))
    }
}
