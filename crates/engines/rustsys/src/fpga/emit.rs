// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use super::{Options, mapping::Mapping};
use crate::aig::Lit;
use crate::elab::{Design, StateKind};
use rusteda_formats::verilog::write::ident;

struct BoundedText {
    text: String,
    limit: usize,
}
impl Write for BoundedText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        if value.len() > self.limit.saturating_sub(self.text.len()) {
            return Err(std::fmt::Error);
        }
        self.text.push_str(value);
        Ok(())
    }
}
impl BoundedText {
    fn len(&self) -> usize {
        self.text.len()
    }
}

pub(super) fn emit(
    d: &Design,
    m: &Mapping,
    opts: &Options,
) -> Result<(String, BTreeMap<String, usize>), String> {
    let mut prefix = "rs_".to_string();
    while d.ports.iter().any(|p| p.name.starts_with(&prefix)) {
        prefix.push('_');
    }
    let net = |n| format!("{prefix}n{n}");
    let inv = |n| format!("{prefix}i{n}");
    let reference = |l: Lit| {
        if l.is_const() {
            return format!("{prefix}{}", if l == Lit::TRUE { "vcc" } else { "gnd" });
        }
        if l.is_compl() {
            inv(l.node())
        } else {
            net(l.node())
        }
    };
    let mut negatives = vec![false; d.aig.num_nodes()];
    for p in d.ports.iter().filter(|p| !p.input) {
        for l in &p.bits {
            if !l.is_const() && l.is_compl() {
                negatives[l.node() as usize] = true;
            }
        }
    }
    for s in &d.states {
        // Clock/reset polarity of flops uses the dedicated programmable inversion.
        for l in [
            Some(s.d),
            (s.kind == StateKind::Latch).then_some(s.clk),
            s.arst
                .filter(|_| s.kind == StateKind::Latch)
                .map(|(l, _)| l),
        ]
        .into_iter()
        .flatten()
        {
            if !l.is_const() && l.is_compl() {
                negatives[l.node() as usize] = true;
            }
        }
    }
    let mut out = BoundedText {
        text: String::new(),
        limit: opts.max_output_bytes,
    };
    let mut cells = BTreeMap::new();
    let mut count = |c: &str| {
        *cells.entry(c.into()).or_insert(0) += 1;
    };
    writeln!(
        out,
        "// RustSys Zynq-7000 structural netlist; AMD UG953 primitives.\nmodule {} ({});",
        ident(&d.top),
        d.ports
            .iter()
            .map(|p| ident(&p.name))
            .collect::<Vec<_>>()
            .join(", ")
    )
    .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
    for p in &d.ports {
        let width = if p.scalar {
            String::new()
        } else {
            format!("[{}:0] ", p.width - 1)
        };
        writeln!(
            out,
            "  {} {width}{};",
            if p.input { "input" } else { "output" },
            ident(&p.name)
        )
        .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
    }
    writeln!(out,"  wire {prefix}gnd, {prefix}vcc;\n  GND {prefix}g0 (.G({prefix}gnd));\n  VCC {prefix}g1 (.P({prefix}vcc));").map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
    count("GND");
    count("VCC");
    for &node in d.aig.inputs() {
        writeln!(out, "  wire {};", net(node))
            .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
    }
    for &(node, _) in &m.luts {
        writeln!(out, "  wire {};", net(node))
            .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
    }
    for (node, &needed) in negatives.iter().enumerate() {
        let node = node as u32;
        if needed {
            writeln!(
                out,
                "  wire {};\n  LUT1 #(.INIT(2'h1)) {prefix}inv{node} (.I0({}), .O({}));",
                inv(node),
                net(node),
                inv(node)
            )
            .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
            count("LUT1");
        }
    }
    for p in &d.ports {
        for (bit, &l) in p.bits.iter().enumerate() {
            let port = if p.scalar {
                ident(&p.name)
            } else {
                format!("{}[{bit}]", ident(&p.name))
            };
            if p.input {
                writeln!(out, "  assign {} = {port};", net(l.node()))
                    .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
            } else {
                writeln!(out, "  assign {port} = {};", reference(l))
                    .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
            }
        }
    }
    for &(node, cut) in &m.luts {
        if cut.n == 0 {
            writeln!(
                out,
                "  assign {} = {prefix}{};",
                net(node),
                if cut.tt & 1 != 0 { "vcc" } else { "gnd" }
            )
            .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
        } else {
            let k = usize::from(cut.n);
            let width = 1usize << k;
            let mask = if width == 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            write!(
                out,
                "  LUT{k} #(.INIT({width}'h{:x})) {prefix}lut{node} (.O({})",
                cut.tt & mask,
                net(node)
            )
            .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
            for (i, leaf) in cut.leaves().iter().enumerate() {
                write!(out, ", .I{i}({})", net(*leaf))
                    .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
            }
            writeln!(out, ");").map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
            count(&format!("LUT{k}"));
        }
        if out.len() > opts.max_output_bytes {
            return Err("FPGA_OUTPUT_BUDGET: netlist byte limit reached".into());
        }
    }
    let clocks: BTreeSet<_> = d
        .states
        .iter()
        .filter(|s| s.kind == StateKind::Flop && !s.clk.is_const())
        .map(|s| s.clk.node())
        .collect();
    for node in clocks {
        writeln!(out,"  wire {prefix}clk{node};\n  BUFG {prefix}clock{node} (.I({}), .O({prefix}clk{node}));",net(node)).map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
        count("BUFG");
    }
    for (i, s) in d.states.iter().enumerate() {
        let (cell, control) = match (s.kind, s.arst.map(|(_, v)| v)) {
            (StateKind::Flop, None) => ("FDRE", "R"),
            (StateKind::Flop, Some(false)) => ("FDCE", "CLR"),
            (StateKind::Flop, Some(true)) => ("FDPE", "PRE"),
            (StateKind::Latch, None | Some(false)) => ("LDCE", "CLR"),
            (StateKind::Latch, Some(true)) => ("LDPE", "PRE"),
        };
        if s.q.is_compl() || !d.aig.is_input(s.q.node()) {
            return Err("FPGA_STATE: expected positive state input".into());
        }
        if s.kind == StateKind::Flop {
            write!(
                out,
                "  {cell} #(.IS_C_INVERTED(1'b{})",
                u8::from(s.clk.is_compl() && !s.clk.is_const())
            )
            .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
            if let Some((a, _)) = s.arst {
                write!(
                    out,
                    ", .IS_{control}_INVERTED(1'b{})",
                    u8::from(a.is_compl() && !a.is_const())
                )
                .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
            }
            write!(
                out,
                ") {prefix}reg{i} (.C({}), .CE({prefix}vcc)",
                if s.clk.is_const() {
                    reference(s.clk)
                } else {
                    format!("{prefix}clk{}", s.clk.node())
                }
            )
            .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
        } else {
            write!(
                out,
                "  {cell} {prefix}reg{i} (.G({}), .GE({prefix}vcc)",
                reference(s.clk)
            )
            .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
        }
        let a = s
            .arst
            .map(|(a, _)| {
                if s.kind == StateKind::Flop && !a.is_const() {
                    a.regular()
                } else {
                    a
                }
            })
            .unwrap_or(Lit::FALSE);
        writeln!(
            out,
            ", .{control}({}), .D({}), .Q({}));",
            reference(a),
            reference(s.d),
            net(s.q.node())
        )
        .map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
        count(cell);
        if out.len() > opts.max_output_bytes {
            return Err("FPGA_OUTPUT_BUDGET: netlist byte limit reached".into());
        }
    }
    writeln!(out, "endmodule").map_err(|_| "FPGA_OUTPUT_BUDGET: netlist byte limit reached")?;
    if out.len() > opts.max_output_bytes {
        return Err("FPGA_OUTPUT_BUDGET: netlist byte limit reached".into());
    }
    Ok((out.text, cells))
}
