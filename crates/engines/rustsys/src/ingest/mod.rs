// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Sources other than RTL (`docs/specs/synth/inputs.md`, SYNTH-REQ-0900 to SYNTH-REQ-0914): BLIF
//! networks, AIGER graphs and gate-level netlists of other library cells, read with the shared
//! readers of `rusteda-formats` and lowered into the design that optimization and mapping take.

mod aiger;
mod blif;
mod cells;
mod gates;

use std::path::Path;

use rusteda_core::netlist::GateNetlist;
use rusteda_formats::aiger::Aig as AigerGraph;
use rusteda_formats::blif::Network;
use serde::{Deserialize, Serialize};

use crate::aig::{Aig, Lit};
use crate::elab::Design;
use crate::{Sources, SynthOptions};

/// The format of the sources of a run (SYNTH-REQ-0900).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    SystemVerilog,
    Blif,
    Aiger,
    GateNetlist,
}

impl SourceFormat {
    /// The name in reports (SYNTH-REQ-0913).
    pub fn name(self) -> &'static str {
        match self {
            SourceFormat::SystemVerilog => "systemverilog",
            SourceFormat::Blif => "blif",
            SourceFormat::Aiger => "aiger",
            SourceFormat::GateNetlist => "gate-netlist",
        }
    }

    /// The format of one file name by its extension, ignoring letter case (SYNTH-REQ-0900).
    pub fn of_name(name: &str) -> SourceFormat {
        let ext = Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        match ext.as_deref() {
            Some("blif") => SourceFormat::Blif,
            Some("aag" | "aig") => SourceFormat::Aiger,
            _ => SourceFormat::SystemVerilog,
        }
    }
}

/// What declared initial values become (SYNTH-REQ-0906).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InitialValues {
    /// An asynchronous control from a generated port establishes them.
    #[default]
    Reset,
    /// They are not implemented; every state starts arbitrarily.
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

/// The names of the sources.
fn names(src: &Sources) -> Vec<String> {
    match src {
        Sources::Files(f) => f.iter().map(|p| p.display().to_string()).collect(),
        Sources::Texts(t) => t.iter().map(|(n, _)| n.clone()).collect(),
    }
}

/// The format of the sources, or the error `S0910` (SYNTH-REQ-0900).
pub fn format_of(src: &Sources, gate_netlist: bool) -> Result<SourceFormat, String> {
    let names = names(src);
    if gate_netlist {
        if names.len() != 1 {
            return Err(format!(
                "error[S0910]: a gate-level netlist source is one file, not {}",
                names.len()
            ));
        }
        return Ok(SourceFormat::GateNetlist);
    }
    let formats: Vec<SourceFormat> = names.iter().map(|n| SourceFormat::of_name(n)).collect();
    let Some(&first) = formats.first() else {
        return Ok(SourceFormat::SystemVerilog);
    };
    if let Some(k) = formats.iter().position(|f| *f != first) {
        return Err(format!(
            "error[S0910]: sources of several formats: {} is {} and {} is {}; one run reads one \
             format",
            names[0],
            first.name(),
            names[k],
            formats[k].name()
        ));
    }
    if first == SourceFormat::Aiger && names.len() > 1 {
        return Err(format!(
            "error[S0910]: {} AIGER files; an AIGER file holds a whole design, so a run reads one",
            names.len()
        ));
    }
    Ok(first)
}

/// Rejects the options of SystemVerilog sources for another format (SYNTH-REQ-0901).
pub(crate) fn check_options(opts: &SynthOptions, format: SourceFormat) -> Result<(), String> {
    let used: Vec<&str> = [
        (!opts.defines.is_empty(), "macro definitions (-D)"),
        (!opts.params.is_empty(), "parameter overrides (-P)"),
        (opts.assume_full_case, "--assume-full-case"),
        (opts.assume_parallel_case, "--assume-parallel-case"),
    ]
    .into_iter()
    .filter_map(|(on, what)| on.then_some(what))
    .collect();
    if used.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "error[S0911]: {} apply to SystemVerilog sources only, not to a {} source",
            used.join(", "),
            format.name()
        ))
    }
}

/// A source read but not lowered yet.
#[derive(Debug)]
pub(crate) enum Parsed {
    Blif(Box<Network>),
    Aiger(Box<AigerGraph>, String),
    Gates(Box<GateNetlist>),
}

/// What the report records about a source other than RTL (SYNTH-REQ-0913).
#[derive(Debug, Clone, Default)]
pub(crate) struct Info {
    pub format: &'static str,
    pub initial_values: Vec<(String, bool)>,
    pub clock_port: Option<String>,
    pub init_port: Option<String>,
}

/// A lowered source: the design, its rendered warnings, their number and the report fields.
#[derive(Debug)]
pub(crate) struct Lowered {
    pub design: Design,
    pub messages: Vec<String>,
    pub warnings: usize,
    pub info: Info,
}

/// The texts of the sources: (name, bytes).
fn texts(src: &Sources) -> Result<Vec<(String, Vec<u8>)>, Vec<String>> {
    match src {
        Sources::Texts(t) => Ok(t
            .iter()
            .map(|(n, s)| (n.clone(), s.as_bytes().to_vec()))
            .collect()),
        Sources::Files(f) => f
            .iter()
            .map(|p| {
                std::fs::read(p)
                    .map(|b| (p.display().to_string(), b))
                    .map_err(|e| vec![format!("error[S0910]: cannot read {}: {e}", p.display())])
            })
            .collect(),
    }
}

/// Reads the sources of `format` with the shared readers (SYNTH-REQ-0914): warnings are returned
/// rendered, errors as the failure.
pub(crate) fn read(
    src: &Sources,
    format: SourceFormat,
    opts: &SynthOptions,
) -> Result<(Parsed, Vec<String>), Vec<String>> {
    let render = |d: &[rusteda_core::job::Diag]| d.iter().map(|x| x.render()).collect::<Vec<_>>();
    let texts = texts(src)?;
    match format {
        SourceFormat::Blif => {
            let mut owned = Vec::with_capacity(texts.len());
            for (n, b) in texts {
                match String::from_utf8(b) {
                    Ok(t) => owned.push((n, t)),
                    Err(e) => {
                        return Err(vec![format!(
                            "error[B0001]: {n}:1:1: the text is not UTF-8: {e}"
                        )]);
                    }
                }
            }
            let refs: Vec<(&str, &str)> = owned
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
            let o = rusteda_formats::blif::Options {
                top: opts.top.clone(),
                ..Default::default()
            };
            let r = rusteda_formats::blif::read_texts(&refs, &o).map_err(|e| render(&e))?;
            Ok((Parsed::Blif(Box::new(r.network)), render(&r.warnings)))
        }
        SourceFormat::Aiger => {
            let (name, bytes) = &texts[0];
            let g = rusteda_formats::aiger::read_bytes(bytes, name, &Default::default())
                .map_err(|e| render(&e))?;
            let module = opts.top.clone().unwrap_or_else(|| {
                Path::new(name)
                    .file_stem()
                    .map_or_else(|| name.clone(), |s| s.to_string_lossy().into_owned())
            });
            Ok((Parsed::Aiger(Box::new(g), module), Vec::new()))
        }
        SourceFormat::GateNetlist => {
            let (name, bytes) = &texts[0];
            let text = String::from_utf8_lossy(bytes);
            let pins = cells::pins(&opts.gate_libs);
            let g = rusteda_formats::verilog::netlist::read_structural(&text, name, &|c| {
                pins.get(c).cloned()
            })
            .map_err(|e| {
                e.diagnostics
                    .iter()
                    .map(|d| d.render(&e.sources))
                    .collect::<Vec<_>>()
            })?;
            Ok((Parsed::Gates(Box::new(g)), Vec::new()))
        }
        SourceFormat::SystemVerilog => Err(vec![
            "error[S0900]: internal: SystemVerilog sources take the RTL front end".into(),
        ]),
    }
}

/// Reads and lowers sources other than SystemVerilog into the design that `synthesize` then
/// optimizes, with the rendered warnings (SYNTH-REQ-0900 to SYNTH-REQ-0912).
pub fn lower_sources(
    src: &Sources,
    opts: &SynthOptions,
) -> Result<(Design, Vec<String>), Vec<String>> {
    let format = format_of(src, opts.gate_netlist).map_err(|m| vec![m])?;
    check_options(opts, format).map_err(|m| vec![m])?;
    let (p, mut warnings) = read(src, format, opts)?;
    let low = lower(&p, opts)?;
    warnings.extend(low.messages);
    Ok((low.design, warnings))
}

/// Lowers a source into the design (SYNTH-REQ-0903 to SYNTH-REQ-0912).
pub(crate) fn lower(parsed: &Parsed, opts: &SynthOptions) -> Result<Lowered, Vec<String>> {
    match parsed {
        Parsed::Blif(n) => blif::lower(n, &opts.gate_libs, opts.initial_values),
        Parsed::Aiger(g, module) => aiger::lower(g, module, opts.initial_values),
        Parsed::Gates(g) => gates::lower(g, &opts.gate_libs, opts.top.as_deref()),
    }
}

/// `true` for a port name of printable ASCII without blanks (SYNTH-REQ-0903).
fn printable(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// The first of `base`, `base_1`, `base_2`, ... that `taken` does not hold (SYNTH-REQ-0904,
/// SYNTH-REQ-0906).
pub(crate) fn fresh_name(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (1u64..)
        .map(|n| format!("{base}_{n}"))
        .find(|n| !taken(n))
        .unwrap_or_default()
}

/// Checks the port names of a source: printable and distinct (SYNTH-REQ-0903).
pub(crate) fn check_ports(ports: &[(String, bool)]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for (name, _) in ports {
        if !printable(name) {
            return Err(format!(
                "error[S0912]: port name {name:?} has a character outside printable ASCII or a \
                 blank"
            ));
        }
        if !seen.insert(name.as_str()) {
            return Err(format!(
                "error[S0912]: two ports are named {name}: a netlist module needs distinct port \
                 names"
            ));
        }
    }
    Ok(())
}

/// Checks state names against each other and against the ports (SYNTH-REQ-0907): a state may
/// share its name only with a one-bit output port whose function is the state itself.
pub(crate) fn check_states(d: &Design) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for s in &d.states {
        if !printable(&s.name) {
            return Err(format!(
                "error[S0914]: state name {:?} has a character outside printable ASCII or a blank",
                s.name
            ));
        }
        if !seen.insert(s.name.as_str()) {
            return Err(format!("error[S0914]: two states are named {}", s.name));
        }
        if let Some(p) = d.port(&s.name) {
            if p.input || p.bits != [s.q] {
                return Err(format!(
                    "error[S0914]: state {} has the name of {} port that is not the state itself",
                    s.name,
                    if p.input { "an input" } else { "an output" }
                ));
            }
        }
    }
    Ok(())
}

/// The AND of literals by a balanced tree (SYNTH-REQ-0908); 1 for none.
pub(crate) fn and_all(aig: &mut Aig, mut v: Vec<Lit>) -> Lit {
    if v.is_empty() {
        return Lit::TRUE;
    }
    while v.len() > 1 {
        let mut next = Vec::with_capacity(v.len().div_ceil(2));
        for pair in v.chunks(2) {
            next.push(match pair {
                [a, b] => aig.and(*a, *b),
                [a] => *a,
                _ => Lit::TRUE,
            });
        }
        v = next;
    }
    v[0]
}

/// The OR of literals by a balanced tree; 0 for none.
pub(crate) fn or_all(aig: &mut Aig, v: Vec<Lit>) -> Lit {
    !and_all(aig, v.into_iter().map(|l| !l).collect())
}

/// No driver.
pub(crate) const NONE: u32 = u32::MAX;
/// A signal whose value is not computed yet.
pub(crate) const UNSET: Lit = Lit(u32::MAX);

/// A combinational network over signals: the node that drives each signal and the signals each
/// node reads (compressed rows), for the lowering walk of SYNTH-REQ-0908.
#[derive(Debug, Default)]
pub(crate) struct Comb {
    pub driver: Vec<u32>,
    pub start: Vec<u32>,
    pub fanin: Vec<u32>,
}

impl Comb {
    pub fn new(signals: usize) -> Comb {
        Comb {
            driver: vec![NONE; signals],
            start: vec![0],
            fanin: Vec::new(),
        }
    }

    /// Adds a node reading `fanins`; returns its index.
    pub fn node(&mut self, fanins: impl IntoIterator<Item = u32>) -> u32 {
        self.fanin.extend(fanins);
        self.start.push(self.fanin.len() as u32);
        (self.start.len() - 2) as u32
    }

    fn fanins(&self, n: u32) -> &[u32] {
        &self.fanin[self.start[n as usize] as usize..self.start[n as usize + 1] as usize]
    }

    /// The value of signal `root`: every node is built after the nodes it reads, with an explicit
    /// stack; a signal that depends on itself is the error `S0915` (SYNTH-REQ-0908). `vals` holds
    /// the known values (inputs, states) and receives the computed ones; `build` makes node `n` of
    /// the values of its fanins.
    pub fn value(
        &self,
        root: u32,
        vals: &mut [Lit],
        open: &mut [bool],
        aig: &mut Aig,
        build: &mut dyn FnMut(&mut Aig, u32, &[Lit]) -> Result<Lit, String>,
        name: &dyn Fn(u32) -> String,
    ) -> Result<Lit, String> {
        let mut stack: Vec<(u32, bool)> = vec![(root, false)];
        let mut ins = Vec::new();
        while let Some((s, post)) = stack.pop() {
            if vals[s as usize] != UNSET {
                continue;
            }
            let n = self.driver[s as usize];
            if n == NONE {
                return Err(format!("error[S0918]: signal {} has no driver", name(s)));
            }
            if post {
                ins.clear();
                ins.extend(self.fanins(n).iter().map(|&f| vals[f as usize]));
                vals[s as usize] = build(aig, n, &ins)?;
                open[s as usize] = false;
                continue;
            }
            if open[s as usize] {
                let mut cycle: Vec<String> = vec![name(s)];
                for &(t, p) in stack.iter().rev() {
                    if t == s {
                        break;
                    }
                    if p && open[t as usize] && cycle.len() < 8 {
                        cycle.push(name(t));
                    }
                }
                return Err(format!(
                    "error[S0915]: combinational cycle through {}",
                    cycle.join(", ")
                ));
            }
            open[s as usize] = true;
            stack.push((s, true));
            for &f in self.fanins(n) {
                if vals[f as usize] == UNSET {
                    stack.push((f, false));
                }
            }
        }
        Ok(vals[root as usize])
    }
}
