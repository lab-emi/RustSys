// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Timing constraint reader, canonical writer, and `CONSTR v1` binary section
//! codec for Synopsys Design Constraints (`SDC-REQ-0001` to `SDC-REQ-0055`) `[@sdc2016]`.

pub mod cmd;
pub mod constr;
mod eval;
mod expr;
pub mod filter;
pub mod obj;
pub mod schema;
pub mod write;

use std::collections::BTreeMap;

pub use cmd::{
    AnalysisType, CaseValue, ClockGroupsKind, Cmd, CmdKind, EarlyLateScope, EdgeScope,
    ExceptionEnd, MinMaxScope, SetupHoldScope, StartEndScope, WireLoadMode,
};
pub use constr::{ConstrError, ConstrSection, section_id, xxh3_64};
pub use filter::{Filter, FilterAtom, FilterExpr, FilterOp};
pub use obj::{Obj, ObjQuery};
pub use rusteda_core::diag::{Diag, Location, Severity};
pub use schema::{CmdSchema, CmdStatus, OptSpec, OptValueKind, find_query_schema, find_schema};

/// Canonical unit scaling recorded in `CONSTR v1` `HEADER` (`SDC-REQ-0004`, `SDC-REQ-0043`).
///
/// Numeric values inside [`Sdc::cmds`] are scaled on ingestion to canonical
/// internal units (`ns`, `pF`, `kOhm`, `V`, `mA`, `nW`). Each field is `None`
/// (`NaN` in `HEADER`) when the script left that dimension at library default,
/// or `Some(1.0)` when `set_units` explicitly configured that dimension.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Units {
    pub time_to_ns: Option<f64>,
    pub cap_to_pf: Option<f64>,
    pub res_to_kohm: Option<f64>,
    pub voltage_to_v: Option<f64>,
    pub current_to_ma: Option<f64>,
    pub power_to_nw: Option<f64>,
}

/// Top-level input source buffer passed to [`read`] (`SDC-REQ-0001`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub text: String,
    pub xxh3: u64,
}

impl Source {
    /// Creates a [`Source`] and computes its `XXH3_64` content hash.
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        let name = name.into();
        let text = text.into();
        let xxh3 = xxh3_64(text.as_bytes());
        Self { name, text, xxh3 }
    }

    /// Creates a [`Source`] with a pre-computed `XXH3_64` hash.
    pub fn with_hash(name: impl Into<String>, text: impl Into<String>, xxh3: u64) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
            xxh3,
        }
    }
}

/// Provenance record in [`Sdc::sources`] and `CONSTR v1` `SOURCES` (`SDC-REQ-0044`, `SDC-REQ-0055`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub path: String,
    /// `0` for top-level `--sdc` inputs, [`SourceRecord::FLAG_SOURCED`] (`1`) for files loaded via `source`.
    pub flags: u32,
    pub xxh3: u64,
}

impl SourceRecord {
    /// Flag bit indicating that this file was included via a relative `source` command.
    pub const FLAG_SOURCED: u32 = 1;
}

/// Evaluation environment passed to [`read`] (`SDC-REQ-0009`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    pub vars: Vec<(String, String)>,
}

impl Env {
    /// Creates an empty [`Env`].
    pub fn new() -> Self {
        Self { vars: Vec::new() }
    }

    /// Creates an [`Env`] from `(name, value)` pairs.
    pub fn from_pairs<I, K, V>(iter: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            vars: iter
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }
}

/// Recorded `::env(...)` binding in [`Sdc::env`] and `CONSTR v1` `ENV` (`SDC-REQ-0009`, `SDC-REQ-0045`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
    pub name: String,
    pub value: String,
    pub written: bool,
}

/// Resource budgets and optional virtual filesystem for SDC evaluation
/// (`SDC-REQ-0014`, `SDC-REQ-0015`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Maximum command-evaluation steps across top-level and `source`d scripts (`D0008`).
    pub max_steps: u64,
    /// Maximum recursion / nesting depth for `[...]` and `source` (`D0008`).
    pub max_depth: u32,
    /// Optional in-memory map of relative paths for `source` resolution in hermetic tests.
    pub virtual_sources: BTreeMap<String, String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 100_000,
            max_depth: 32,
            virtual_sources: BTreeMap::new(),
        }
    }
}

/// Evaluated SDC constraint model (`SDC-REQ-0001` to `SDC-REQ-0055`) `[@sdc2016]`.
///
/// Semantic equality (`PartialEq`) compares [`Sdc::units`], [`Sdc::current_design`],
/// and [`Sdc::cmds`] so that canonical round-tripping (`read(write(s)) == s`),
/// binary section round-tripping (`Sdc::from_sections(&s.to_sections()) == Ok(s)`),
/// and unit-scaling equivalences hold directly. Use [`Sdc::exact_eq`] to compare
/// source provenance, environment records, diagnostics, and per-command spans as well.
#[derive(Debug, Clone, Default)]
pub struct Sdc {
    pub units: Units,
    pub current_design: Option<String>,
    pub sources: Vec<SourceRecord>,
    pub env: Vec<EnvEntry>,
    pub cmds: Vec<Cmd>,
    pub diags: Vec<Diag>,
}

impl PartialEq for Sdc {
    fn eq(&self, other: &Self) -> bool {
        self.units == other.units
            && self.current_design == other.current_design
            && self.cmds == other.cmds
    }
}

impl Sdc {
    /// Returns `true` if any diagnostic has [`Severity::Error`].
    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(|d| d.severity == Severity::Error)
    }

    /// Compares all fields of two [`Sdc`] models, including source provenance,
    /// diagnostics, environment bindings, and per-command spans.
    pub fn exact_eq(&self, other: &Self) -> bool {
        self.units == other.units
            && self.current_design == other.current_design
            && self.sources == other.sources
            && self.env == other.env
            && self.diags == other.diags
            && self.cmds.len() == other.cmds.len()
            && self
                .cmds
                .iter()
                .zip(&other.cmds)
                .all(|(a, b)| a.exact_eq(b))
    }

    /// Encodes this [`Sdc`] into the 13 `CONSTR v1` binary sections (`SDC-REQ-0040`, `SDC-REQ-0041`).
    pub fn to_sections(&self) -> Vec<ConstrSection> {
        constr::encode_sections(self)
    }

    /// Decodes an [`Sdc`] from the 13 `CONSTR v1` binary sections (`SDC-REQ-0049`, `SDC-REQ-0050`).
    pub fn from_sections(sections: &[ConstrSection]) -> Result<Self, ConstrError> {
        constr::decode_sections(sections)
    }
}

/// Evaluates an ordered slice of SDC [`Source`] files under the given [`Env`]
/// and resource [`Limits`] (`SDC-REQ-0001`).
pub fn read(sources: &[Source], env: &Env, limits: &Limits) -> (Sdc, Vec<Diag>) {
    let mut interp = eval::Interpreter::new(env, limits);
    for src in sources {
        interp.eval_top_source(src);
    }
    interp.finish()
}

/// Convenience reader for a single in-memory SDC script with empty [`Env`]
/// and default [`Limits`].
pub fn read_str(text: &str) -> (Sdc, Vec<Diag>) {
    let sources = [Source::new("<memory>", text)];
    read(&sources, &Env::default(), &Limits::default())
}

/// Formats an evaluated [`Sdc`] into canonical, variable-free SDC text
/// (`SDC-REQ-0003`).
pub fn write(sdc: &Sdc) -> String {
    write::write_sdc(sdc)
}

/// Glob match with `*` (any sequence) and `?` (any character) (`SDC-REQ-0005`).
pub fn glob_match(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut i, mut j) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(si) = star {
            i = si + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

/// Case-insensitive glob match with `*` and `?` (`SDC-REQ-0005`).
pub fn glob_match_nocase(pat: &str, s: &str) -> bool {
    let p_low: String = pat.chars().map(|c| c.to_ascii_lowercase()).collect();
    let s_low: String = s.chars().map(|c| c.to_ascii_lowercase()).collect();
    glob_match(&p_low, &s_low)
}

#[cfg(test)]
mod tests {
    use super::*;

    // SDC-REQ-0001, SDC-REQ-0002, SDC-REQ-0003, SDC-REQ-0005, SDC-REQ-0040
    #[test]
    fn basic_read_write_and_sections_roundtrip() {
        let text = r#"
set clk_name clk
set clk_period 10.0
create_clock -name $clk_name -period $clk_period -waveform {0 5} [get_ports clk_i]
set_input_delay -clock $clk_name -max [expr {$clk_period * 0.25}] [get_ports {a b* c[3]}]
set_load 0.01 [all_outputs]
set_output_delay -clock [get_clocks $clk_name] -clock_fall 1.0 [get_ports "out_o"]
set_clock_uncertainty -setup 0.1 [get_clocks $clk_name]
"#;
        let (sdc, diags) = read_str(text);
        assert!(diags.is_empty(), "unexpected diags: {diags:?}");
        assert_eq!(sdc.cmds.len(), 5);

        let canon = write(&sdc);
        let (sdc2, diags2) = read_str(&canon);
        assert!(diags2.is_empty(), "unexpected diags2: {diags2:?}");
        assert_eq!(sdc, sdc2);

        let sections = sdc.to_sections();
        assert_eq!(sections.len(), 13);
        let sdc3 = Sdc::from_sections(&sections).unwrap();
        assert_eq!(sdc, sdc3);
    }

    // SDC-REQ-0005
    #[test]
    fn glob_matcher_handles_wildcards() {
        assert!(glob_match("a*b?c", "axxbyc"));
        assert!(!glob_match("a*b", "ac"));
        assert!(glob_match("*", ""));
        assert!(glob_match("data[*]", "data[12]"));
        assert!(glob_match_nocase("CLK_*", "clk_main"));
    }
}
