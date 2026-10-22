// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Typed jobs: every option that can change a result (PROTO-REQ-0001).

use std::path::PathBuf;

use rustsys::view as synth;
use serde::{Deserialize, Serialize};

use crate::proto::Product;

/// Optimization effort (SYNTH-REQ-0412).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effort {
    Low,
    #[default]
    Medium,
    High,
}

/// Mapping goal (SYNTH-REQ-0512).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Goal {
    #[default]
    Delay,
    Area,
}

/// Where parallel phases run (SYNTH-REQ-0516).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    #[default]
    Cpu,
    Gpu,
    #[serde(alias = "cuda-native")]
    Cuda,
    /// Kept only so that old run records load; cannot run.
    Wgpu,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Gpu => "gpu",
            Backend::Cuda => "cuda",
            Backend::Wgpu => "wgpu",
        }
    }

    pub fn map_backend(self) -> Option<synth::MapBackend> {
        match self {
            Backend::Cpu => Some(synth::MapBackend::Cpu),
            Backend::Gpu => Some(synth::MapBackend::Gpu),
            Backend::Cuda => Some(synth::MapBackend::Cuda),
            Backend::Wgpu => None,
        }
    }
}

impl Effort {
    pub fn name(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }

    pub fn engine(self) -> synth::Effort {
        match self {
            Effort::Low => synth::Effort::Low,
            Effort::Medium => synth::Effort::Medium,
            Effort::High => synth::Effort::High,
        }
    }
}

impl Goal {
    pub fn name(self) -> &'static str {
        match self {
            Goal::Delay => "delay",
            Goal::Area => "area",
        }
    }

    pub fn engine(self) -> synth::Goal {
        match self {
            Goal::Delay => synth::Goal::Delay,
            Goal::Area => synth::Goal::Area,
        }
    }
}

/// RTL front-end options of synthesis and of `check synth`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FrontEnd {
    pub top: Option<String>,
    /// `NAME` or `NAME=VALUE`.
    #[serde(default)]
    pub defines: Vec<String>,
    #[serde(default)]
    pub include: Vec<PathBuf>,
    /// `NAME=EXPR`.
    #[serde(default)]
    pub params: Vec<String>,
    #[serde(default)]
    pub no_predefined_macros: bool,
    /// Initial simultaneous reset pulse, each scalar input as NAME=0 or NAME=1.
    #[serde(default)]
    pub after_reset: Vec<String>,
    /// Explicit initial-state refinement; disabled in old and default jobs.
    #[serde(default)]
    pub uninitialized_dont_care: bool,
    /// Complete-case assumption (SYNTH-REQ-0224); disabled in old and default jobs.
    #[serde(default)]
    pub assume_full_case: bool,
    /// Parallel-case assumption (SYNTH-REQ-0225); disabled in old and default jobs.
    #[serde(default)]
    pub assume_parallel_case: bool,
    pub libs: Vec<PathBuf>,
    pub sources: Vec<PathBuf>,
    /// A gate-level netlist as the source or the reference (CLI-REQ-0221); absent in old jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_netlist: Option<PathBuf>,
    /// Libraries of the gate-level netlist and of BLIF cells (CLI-REQ-0221).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_libs: Vec<PathBuf>,
    /// What declared initial values become (CLI-REQ-0222); absent: `reset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_values: Option<InitialValues>,
}

/// What declared initial values of BLIF and AIGER state become (SYNTH-REQ-0906, CLI-REQ-0222).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InitialValues {
    #[default]
    Reset,
    Arbitrary,
}

impl InitialValues {
    pub fn name(self) -> &'static str {
        match self {
            InitialValues::Reset => "reset",
            InitialValues::Arbitrary => "arbitrary",
        }
    }

    pub fn engine(self) -> synth::InitialValues {
        match self {
            InitialValues::Reset => synth::InitialValues::Reset,
            InitialValues::Arbitrary => synth::InitialValues::Arbitrary,
        }
    }

    pub fn checker(self) -> rustsys_check::InitialValues {
        match self {
            InitialValues::Reset => rustsys_check::InitialValues::Reset,
            InitialValues::Arbitrary => rustsys_check::InitialValues::Arbitrary,
        }
    }
}

/// The format of the sources of a front end (CLI-REQ-0220).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    SystemVerilog,
    Blif,
    Aiger,
    GateNetlist,
}

impl FrontEnd {
    /// The format of the sources, or the code and message of a usage error (CLI-REQ-0220,
    /// CLI-REQ-0221).
    pub fn format(&self) -> Result<Format, (&'static str, String)> {
        if self.gate_netlist.is_some() {
            return Ok(Format::GateNetlist);
        }
        let of = |p: &PathBuf| match synth::SourceFormat::of_name(&p.to_string_lossy()) {
            synth::SourceFormat::Blif => Format::Blif,
            synth::SourceFormat::Aiger => Format::Aiger,
            _ => Format::SystemVerilog,
        };
        let Some(first) = self.sources.first().map(of) else {
            return Ok(Format::SystemVerilog);
        };
        if let Some(p) = self.sources.iter().find(|p| of(p) != first) {
            return Err((
                "A0300",
                format!(
                    "sources of several formats ({} and {}); one run reads SystemVerilog, BLIF \
                     or AIGER",
                    self.sources[0].display(),
                    p.display()
                ),
            ));
        }
        if first == Format::Aiger && self.sources.len() > 1 {
            return Err((
                "A0300",
                format!(
                    "{} AIGER files; an AIGER file holds a whole design",
                    self.sources.len()
                ),
            ));
        }
        Ok(first)
    }

    /// The usage errors of the sources and of the options that depend on their format
    /// (CLI-REQ-0220, CLI-REQ-0221).
    pub fn source_problems(&self) -> Vec<(&'static str, String)> {
        let format = match self.format() {
            Ok(f) => f,
            Err(e) => return vec![e],
        };
        let mut p = Vec::new();
        if self.gate_netlist.is_some() && !self.sources.is_empty() {
            p.push((
                "A0302",
                "--gate-netlist is the source; positional sources are not taken with it".into(),
            ));
        }
        if format == Format::GateNetlist && self.gate_libs.is_empty() {
            p.push((
                "A0303",
                "--gate-netlist needs the libraries of its cells (--gate-lib)".into(),
            ));
        }
        if matches!(format, Format::SystemVerilog | Format::Aiger) && !self.gate_libs.is_empty() {
            p.push((
                "A0303",
                "--gate-lib applies to --gate-netlist and to the cells of BLIF sources".into(),
            ));
        }
        if format != Format::SystemVerilog {
            let sv = [
                (!self.defines.is_empty(), "-D"),
                (!self.include.is_empty(), "-I"),
                (!self.params.is_empty(), "-P"),
                (self.no_predefined_macros, "--no-predefined-macros"),
                (self.assume_full_case, "--assume-full-case"),
                (self.assume_parallel_case, "--assume-parallel-case"),
            ];
            for (on, flag) in sv {
                if on {
                    p.push((
                        "A0301",
                        format!("{flag} applies to SystemVerilog sources only"),
                    ));
                }
            }
        }
        if self.initial_values.is_some()
            && matches!(format, Format::SystemVerilog | Format::GateNetlist)
        {
            p.push((
                "A0301",
                "--initial-values applies to BLIF and AIGER sources only".into(),
            ));
        }
        p
    }

    /// The sources that the engine and the checker read: the gate-level netlist or the
    /// positional sources.
    pub fn source_files(&self) -> Vec<PathBuf> {
        match &self.gate_netlist {
            Some(g) => vec![g.clone()],
            None => self.sources.clone(),
        }
    }
}

/// A synthesis job (SYNTH-REQ-0701).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SynthJob {
    #[serde(flatten)]
    pub fe: FrontEnd,
    #[serde(default)]
    pub dont_use: Vec<String>,
    pub effort: Effort,
    pub goal: Goal,
    pub delay_relax: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electrical: Option<synth::ElectricalOptions>,
    #[serde(default)]
    pub electrical_corners: Vec<PathBuf>,
    pub backend: Backend,
    pub out: Option<PathBuf>,
    pub report: Option<PathBuf>,
}

impl Default for SynthJob {
    fn default() -> SynthJob {
        SynthJob {
            fe: FrontEnd::default(),
            dont_use: Vec::new(),
            effort: Effort::Medium,
            goal: Goal::Delay,
            delay_relax: 0.1,
            electrical: None,
            electrical_corners: Vec::new(),
            backend: Backend::Cpu,
            out: None,
            report: None,
        }
    }
}

/// A synthesis check job (CHECK-REQ-0001).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckSynthJob {
    #[serde(flatten)]
    pub fe: FrontEnd,
    pub netlists: Vec<PathBuf>,
    pub seed: u64,
    pub cycles: usize,
    /// Netlist supply ports (CHECK-REQ-0022).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supply_ports: Vec<String>,
    /// SAT conflict budget of each proof obligation (CHECK-REQ-0002); jobs written before the
    /// field have the default.
    #[serde(default = "default_budget")]
    pub budget: u64,
    /// SAT conflict budget of each proof that the checker's RTL model needs (CHECK-REQ-0002).
    #[serde(default = "default_model_budget")]
    pub model_budget: u64,
    /// Optional sweeping work limit; old records use the bounded default.
    #[serde(default = "default_sweep_max_candidates")]
    pub sweep_max_candidates: usize,
    pub report: Option<PathBuf>,
}

fn default_budget() -> u64 {
    rustsys_check::DEFAULT_BUDGET
}

fn default_model_budget() -> u64 {
    rustsys_check::DEFAULT_MODEL_BUDGET
}

fn default_sweep_max_candidates() -> usize {
    rustsys_check::DEFAULT_SWEEP_MAX_CANDIDATES
}

impl Default for CheckSynthJob {
    fn default() -> CheckSynthJob {
        CheckSynthJob {
            fe: FrontEnd::default(),
            netlists: Vec::new(),
            seed: 1,
            cycles: 0,
            supply_ports: Vec::new(),
            budget: default_budget(),
            model_budget: default_model_budget(),
            sweep_max_candidates: default_sweep_max_candidates(),
            report: None,
        }
    }
}

/// A job of any product.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "product", rename_all = "kebab-case")]
pub enum Job {
    Synth(SynthJob),
    CheckSynth(CheckSynthJob),
}

impl Job {
    pub fn product(&self) -> Product {
        match self {
            Job::Synth(_) => Product::Synth,
            Job::CheckSynth(_) => Product::CheckSynth,
        }
    }

    /// Input files with their roles, for run records.
    pub fn inputs(&self) -> Vec<(&'static str, PathBuf)> {
        let mut v = Vec::new();
        let libs = |v: &mut Vec<_>, l: &[PathBuf]| v.extend(l.iter().map(|p| ("lib", p.clone())));
        match self {
            Job::Synth(j) => {
                libs(&mut v, &j.electrical_corners);
                libs(&mut v, &j.fe.libs);
                libs(&mut v, &j.fe.gate_libs);
                v.extend(j.fe.source_files().into_iter().map(|p| ("source", p)));
            }
            Job::CheckSynth(j) => {
                libs(&mut v, &j.fe.libs);
                libs(&mut v, &j.fe.gate_libs);
                v.extend(j.fe.source_files().into_iter().map(|p| ("source", p)));
                v.extend(j.netlists.iter().map(|p| ("netlist", p.clone())));
            }
        }
        v
    }

    /// The usage errors of the sources, with their codes (CLI-REQ-0220, CLI-REQ-0221).
    pub fn source_problems(&self) -> Vec<(&'static str, String)> {
        match self {
            Job::Synth(j) => j.fe.source_problems(),
            Job::CheckSynth(j) => j.fe.source_problems(),
        }
    }

    /// The problems that keep the job from running, in words (CLI-REQ-0005).
    pub fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        let need = |p: &mut Vec<String>, ok: bool, what: &str| {
            if !ok {
                p.push(format!("{what}: required"));
            }
        };
        match self {
            Job::Synth(j) => {
                need(&mut p, !j.fe.libs.is_empty(), "Liberty libraries (--lib)");
                need(&mut p, !j.fe.source_files().is_empty(), "Sources");
                if !(j.delay_relax.is_finite() && j.delay_relax >= 0.0) {
                    p.push("Delay relaxation (--delay-relax): finite and not negative".into());
                }
                if j.electrical.is_none() && !j.electrical_corners.is_empty() {
                    p.push("Electrical corners require a timing period".into());
                }
                if let Some(e) = &j.electrical {
                    if let Err(error) = e.validate() {
                        p.push(error);
                    }
                }
            }
            Job::CheckSynth(j) => {
                need(&mut p, !j.fe.libs.is_empty(), "Liberty libraries (--lib)");
                need(&mut p, !j.fe.source_files().is_empty(), "Sources");
                need(&mut p, !j.netlists.is_empty(), "Netlists (--netlist)");
            }
        }
        p
    }
}

/// `NAME` or `NAME=VALUE` split at the first `=`.
pub fn split_kv(v: &[String]) -> Vec<(String, String)> {
    v.iter()
        .map(|s| match s.split_once('=') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (s.clone(), String::new()),
        })
        .collect()
}
