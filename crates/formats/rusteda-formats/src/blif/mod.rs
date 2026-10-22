// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! BLIF reader (`docs/specs/blif/`, BLIF-REQ-0001 to BLIF-REQ-0062) [@blif1992]: models with
//! `.names` covers, `.latch` latches, `.subckt` hierarchy (flattened) and `.gate`/`.mlatch`
//! library cells (recorded for the consumer to resolve), returned as one flat [`Network`].

mod flatten;
mod lex;
mod parse;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use rusteda_core::diag::{Diag, Severity};

/// Bounds of the flattened network (BLIF-REQ-0060).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Nodes, latches and cell instances together.
    pub max_items: usize,
    /// Signals and signal references (node inputs, cell connections) together.
    pub max_refs: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_items: 1 << 22,
            max_refs: 1 << 26,
        }
    }
}

/// Options of a read (BLIF-REQ-0001).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// The top model; default the first model of the first file (BLIF-REQ-0042).
    pub top: Option<String>,
    pub limits: Limits,
}

/// Where a construct was written: an index into [`Network::files`], line and column (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Loc {
    pub file: u32,
    pub line: u32,
    pub col: u32,
}

/// A single-output cover (BLIF-REQ-0030), shared by the instances of a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    /// Number of inputs N.
    pub inputs: usize,
    /// Number of rows.
    pub count: usize,
    /// The input planes of the rows, N bytes each from `0`, `1` and `-`.
    pub rows: Vec<u8>,
    /// The rows cover the on-set (`1`) or the off-set (`0`); `true` for a cover without rows.
    pub onset: bool,
}

impl Cover {
    /// Number of rows.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Row `r`: N bytes from `0`, `1` and `-` (empty for N = 0).
    pub fn row(&self, r: usize) -> &[u8] {
        &self.rows[r * self.inputs..(r + 1) * self.inputs]
    }

    /// The function value for input values `x` (BLIF-REQ-0030).
    pub fn eval(&self, x: &[bool]) -> bool {
        let hit = (0..self.len()).any(|r| {
            self.row(r).iter().zip(x).all(|(c, v)| match c {
                b'1' => *v,
                b'0' => !*v,
                _ => true,
            })
        });
        if self.is_empty() {
            false
        } else {
            hit == self.onset
        }
    }
}

/// A `.names` node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub inputs: Vec<u32>,
    pub output: u32,
    pub cover: Arc<Cover>,
    pub at: Loc,
}

/// The type of a `.latch` (BLIF-REQ-0031).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatchType {
    /// `fe`
    FallingEdge,
    /// `re`
    RisingEdge,
    /// `ah`
    ActiveHigh,
    /// `al`
    ActiveLow,
}

/// The initial value of a latch as written (BLIF-REQ-0031).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Init {
    Zero,
    One,
    /// `2`
    DontCare,
    /// `3`, the default.
    Unknown,
}

impl Init {
    /// The declared value, for `0` and `1`.
    pub fn value(self) -> Option<bool> {
        match self {
            Init::Zero => Some(false),
            Init::One => Some(true),
            Init::DontCare | Init::Unknown => None,
        }
    }
}

/// A `.latch` (BLIF-REQ-0032).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latch {
    pub input: u32,
    pub output: u32,
    /// `None` without type and control: the global clock.
    pub kind: Option<LatchType>,
    /// The control signal; `None` for the global clock (`NIL` or no control).
    pub control: Option<u32>,
    pub init: Init,
    pub at: Loc,
}

/// A `.gate` or `.mlatch` instance of a library cell (BLIF-REQ-0033).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellInst {
    pub cell: String,
    /// Formal pin and signal, in source order.
    pub conns: Vec<(String, u32)>,
    /// For `.mlatch`: the control (`None` for `NIL`) and the initial value.
    pub mlatch: Option<(Option<u32>, Init)>,
    pub at: Loc,
}

/// The flattened top model (BLIF-REQ-0003).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Network {
    pub name: String,
    /// The files read, which [`Loc::file`] indexes.
    pub files: Vec<String>,
    /// Signal names; a signal is an index into them.
    pub signals: Vec<String>,
    /// Primary inputs: `.inputs` lines first, then `.clock` lines.
    pub inputs: Vec<u32>,
    /// The inputs that `.clock` declared.
    pub clocks: Vec<u32>,
    pub outputs: Vec<u32>,
    /// Where each output was declared.
    pub output_locs: Vec<Loc>,
    pub nodes: Vec<Node>,
    pub latches: Vec<Latch>,
    pub cells: Vec<CellInst>,
}

impl Network {
    /// `file:line:col` of a location.
    pub fn describe(&self, at: Loc) -> String {
        let file = self
            .files
            .get(at.file as usize)
            .map_or("<blif>", String::as_str);
        format!("{file}:{}:{}", at.line, at.col)
    }

    /// The name of signal `s`.
    pub fn name(&self, s: u32) -> &str {
        &self.signals[s as usize]
    }
}

/// A successful read: the network and the warnings (BLIF-REQ-0001).
#[derive(Debug, Clone)]
pub struct Read {
    pub network: Network,
    pub warnings: Vec<Diag>,
}

/// The code of an unreadable file (BLIF-REQ-0062).
pub(crate) const UNREADABLE: &str = "B0001";
pub(crate) const SYNTAX: &str = "B0002";
pub(crate) const COVER: &str = "B0003";
pub(crate) const LATCH: &str = "B0004";
pub(crate) const TWICE: &str = "B0005";
pub(crate) const DRIVERS: &str = "B0006";
pub(crate) const UNDRIVEN: &str = "B0007";
pub(crate) const INSTANCE: &str = "B0008";
pub(crate) const RECURSION: &str = "B0009";
pub(crate) const UNSUPPORTED: &str = "B0010";
pub(crate) const LIMIT: &str = "B0011";
pub(crate) const COLLISION: &str = "B0012";
pub(crate) const TOP: &str = "B0013";
pub(crate) const IGNORED: &str = "B0020";
pub(crate) const EXDC: &str = "B0021";

/// The most errors a read reports before it stops.
pub(crate) const MAX_ERRORS: usize = 20;

/// Reads BLIF files from disk (BLIF-REQ-0001).
pub fn read_files(paths: &[PathBuf], opts: &Options) -> Result<Read, Vec<Diag>> {
    let mut texts = Vec::with_capacity(paths.len());
    let mut errors = Vec::new();
    for p in paths {
        let name = p.display().to_string();
        match std::fs::read(p) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(t) => texts.push((name, t)),
                Err(e) => errors.push(
                    Diag::error(UNREADABLE, format!("the text is not UTF-8: {e}")).at(name, 1, 1),
                ),
            },
            Err(e) => errors.push(Diag::error(UNREADABLE, format!("{name}: {e}"))),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let refs: Vec<(&str, &str)> = texts
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    read_texts(&refs, opts)
}

/// Reads BLIF texts given as `(name, text)` (BLIF-REQ-0001).
pub fn read_texts(texts: &[(&str, &str)], opts: &Options) -> Result<Read, Vec<Diag>> {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut models = parse::Models::default();
    let files: Vec<String> = texts.iter().map(|(n, _)| n.to_string()).collect();
    for (k, (name, text)) in texts.iter().enumerate() {
        let stem = Path::new(name)
            .file_stem()
            .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned());
        parse::parse_file(
            &mut models,
            k as u32,
            &files,
            &stem,
            text,
            &mut warnings,
            &mut errors,
        );
        if errors.len() >= MAX_ERRORS {
            break;
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let (network, undriven) = flatten::flatten(&models, files, opts)?;
    warnings.extend(undriven);
    Ok(Read { network, warnings })
}

/// A located error at `at` of file `file`.
pub(crate) fn err(files: &[String], at: Loc, code: &str, msg: impl Into<String>) -> Diag {
    let file = files.get(at.file as usize).map_or("<blif>", String::as_str);
    Diag::error(code, msg).at(file, at.line.max(1), at.col.max(1))
}
