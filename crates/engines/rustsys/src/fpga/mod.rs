// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Native, bounded LUT mapping for Zynq-7000. No external synthesizer is used.
//! Primitive semantics: AMD UG953; implementation/verification: UG904/UG900.

mod emit;
mod mapping;

use std::collections::BTreeMap;
use std::time::Instant;

use rusteda_formats::verilog::{Diagnostic, PpOptions, SourceMap, read_sources, read_texts};
use serde::{Deserialize, Serialize};

use crate::{Sources, SynthError, SynthOptions, diagnostics, elab};

/// Hard limits, checked before allocating mapper tables. Frontend allocations additionally
/// require an OS process memory limit, as in the tracked verification driver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Options {
    pub part: String,
    pub max_nodes: usize,
    /// Includes each node's trivial cut; at most 32.
    pub max_cuts_per_node: usize,
    pub max_cut_merges: u64,
    pub max_mapping_bytes: u64,
    pub max_output_bytes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            part: "xc7z020clg400-1".into(),
            max_nodes: 1_000_000,
            max_cuts_per_node: 8,
            max_cut_merges: 64_000_000,
            max_mapping_bytes: 512 << 20,
            max_output_bytes: 256 << 20,
        }
    }
}

impl Options {
    pub fn validate(&self) -> Result<(), String> {
        let devices = [
            "xc7z007s", "xc7z010", "xc7z012s", "xc7z014s", "xc7z015", "xc7z020", "xc7z030",
            "xc7z035", "xc7z045", "xc7z100",
        ];
        if !devices.iter().any(|d| self.part.starts_with(d))
            || !self
                .part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("unsupported FPGA part: only xc7z Zynq-7000 devices are supported; Vivado must validate package/speed grade".into());
        }
        if self.max_nodes == 0
            || !(2..=32).contains(&self.max_cuts_per_node)
            || self.max_cut_merges == 0
            || self.max_mapping_bytes == 0
            || self.max_output_bytes == 0
        {
            return Err(
                "invalid FPGA budget: positive limits and 2..=32 cuts per node required".into(),
            );
        }
        Ok(())
    }
}

/// Honest scope: resource counts are measured; timing, implementation and equivalence are
/// established only by external verification receipts, never by this mapper's success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub format: String,
    pub format_version: u32,
    pub top: String,
    pub family: String,
    pub part: String,
    pub cells: BTreeMap<String, usize>,
    pub lut_cells: usize,
    pub flip_flops: usize,
    pub latches: usize,
    pub aig_nodes: usize,
    pub lut_depth: u32,
    pub cut_merges: u64,
    pub mapping_bytes_bound: u64,
    pub limits: Options,
    pub runtime_s: f64,
    pub corner: Option<String>,
    pub wire_model: String,
    pub unconstrained_endpoints: Option<usize>,
    pub timing_status: String,
    pub implementation_status: String,
    pub equivalence_status: String,
    pub initial_state: String,
    pub drc_status: String,
    pub warnings: usize,
}

#[derive(Debug, Clone)]
pub struct Output {
    /// Structural Verilog, with parameterized vendor primitives and no behavioral logic.
    pub text: String,
    /// Little-endian records: root u32, leaf count u32, leaves [u32; 6], INIT u64.
    /// Kept in its own workspace side table (40 bytes per record).
    pub lut_table: Vec<u8>,
    pub report: Report,
    pub messages: Vec<String>,
}

fn error(message: impl Into<String>) -> SynthError {
    SynthError {
        messages: vec![format!("error[S0600]: {}", message.into())],
        internal: false,
    }
}

/// Elaboration shares RustSys's language frontend. FPGA covering is a separate target and
/// does not construct a synthetic Liberty library or substitute ASIC cost/timing units.
pub fn synthesize(
    src: &Sources,
    front: &SynthOptions,
    opts: &Options,
) -> Result<Output, SynthError> {
    let start = Instant::now();
    opts.validate().map_err(error)?;
    if front.gate_netlist
        || !front.gate_libs.is_empty()
        || front.electrical.is_some()
        || !front.electrical_corners.is_empty()
        || !front.after_reset.is_empty()
        || front.uninitialized_dont_care
        || !front.dont_use.is_empty()
        || front.assume_full_case
        || front.assume_parallel_case
        || front.backend != crate::map::MapBackend::Cpu
    {
        return Err(error(
            "FPGA synthesis requires RTL, cpu, arbitrary state and no ASIC/reset/case assumptions",
        ));
    }
    if crate::ingest::format_of(src, false).map_err(error)? != crate::SourceFormat::SystemVerilog {
        return Err(error(
            "FPGA input currently supports Verilog/SystemVerilog RTL only",
        ));
    }
    let mut sm = SourceMap::new();
    let mut defines = front.defines.clone();
    if !front.no_predefined_macros {
        defines.insert(0, ("SYNTHESIS".into(), String::new()));
    }
    let pp = PpOptions {
        defines,
        include_dirs: front.include_dirs.clone(),
    };
    let (units, diags) = match src {
        Sources::Files(paths) => read_sources(&mut sm, paths, pp),
        Sources::Texts(texts) => {
            let refs: Vec<_> = texts
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
            read_texts(&mut sm, &refs, pp)
        }
    };
    if diags.iter().any(Diagnostic::is_error) {
        return Err(SynthError {
            messages: diagnostics::render(&sm, &diags),
            internal: false,
        });
    }
    let mut messages = diagnostics::render(&sm, &diags);
    let mut params = Vec::new();
    for (name, text) in &front.params {
        let expr =
            rusteda_formats::verilog::parse_expression(&mut sm, &format!("<-P {name}>"), text)
                .map_err(|d| SynthError {
                    messages: diagnostics::render(&sm, &[d]),
                    internal: false,
                })?;
        params.push((name.clone(), expr));
    }
    let (design, mut diags) = elab::elaborate(
        &sm,
        &units,
        &elab::ElabOptions {
            top: front.top.clone(),
            params,
            ..Default::default()
        },
    );
    // Unlike an ASIC target, FPGA power-up initialization can affect hardware.
    // Unsupported synthesis constructs must remain explicit warnings here.
    for diagnostic in &mut diags {
        if diagnostic.code == "S0120" {
            diagnostic.severity = rusteda_formats::verilog::Severity::Warning;
        }
    }
    messages.extend(diagnostics::render(&sm, &diags));
    let design = design.ok_or_else(|| SynthError {
        messages: messages.clone(),
        internal: false,
    })?;
    // Do not sweep/merge/reset-prune states: every source state is retained. The mapper
    // covers only the live combinational roots, via one reverse topological scan.
    let mapped = mapping::map(&design, opts).map_err(error)?;
    let (text, cells) = emit::emit(&design, &mapped, opts).map_err(error)?;
    let mut lut_table = Vec::with_capacity(mapped.luts.len() * 40);
    for &(node, cut) in &mapped.luts {
        lut_table.extend_from_slice(&node.to_le_bytes());
        lut_table.extend_from_slice(&u32::from(cut.n).to_le_bytes());
        for leaf in cut.leaves {
            lut_table.extend_from_slice(&leaf.to_le_bytes());
        }
        lut_table.extend_from_slice(&cut.tt.to_le_bytes());
    }
    let warnings = messages.iter().filter(|m| m.starts_with("warning")).count();
    let flip_flops = design
        .states
        .iter()
        .filter(|s| s.kind == elab::StateKind::Flop)
        .count();
    let report = Report {
        format: "rustsys-fpga-report".into(), format_version: 1,
        top: design.top.clone(), family: "zynq7000".into(), part: opts.part.clone(),
        lut_cells: cells.iter().filter(|(n,_)| n.starts_with("LUT")).map(|(_,n)| n).sum(),
        cells, flip_flops, latches: design.states.len() - flip_flops,
        aig_nodes: design.aig.num_nodes(), lut_depth: mapped.depth, cut_merges: mapped.merges,
        mapping_bytes_bound: mapped.memory_bound, limits: opts.clone(), runtime_s: start.elapsed().as_secs_f64(),
        corner: None, wire_model: "unmodeled".into(), unconstrained_endpoints: None,
        timing_status: "unchecked".into(), implementation_status: "unchecked".into(),
        equivalence_status: "unchecked".into(), drc_status: "unchecked".into(),
        initial_state: "source states retained; FPGA power-up uses vendor primitive defaults; reset-qualified simulation required".into(),
        warnings,
    };
    Ok(Output {
        text,
        lut_table,
        report,
        messages,
    })
}
