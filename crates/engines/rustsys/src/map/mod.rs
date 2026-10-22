// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Standard-cell mapping (`docs/specs/synth/mapping.md`).

mod area;
mod compute;
pub mod cuts;
pub mod library;
pub mod mapper;
mod seq_modes;
mod seq_select;
#[cfg(test)]
mod tests;

use rustc_hash::FxHashSet;

use crate::aig::{Aig, Lit};
use crate::elab::Design;
use library::CellLib;
pub use mapper::{
    MapBackend, MapOptions, Mapped, MappedGate, TimingBoundary, map, map_with, map_with_boundary,
};

/// Prepares the GPU backend of `opts` (SYNTH-REQ-0518): selects and self-tests the runtime,
/// compiles the kernels and warms the device allocator by mapping one two-input AND with the
/// library of `libs`. Meant to run on a background thread while the design is read and
/// optimized; everything it prepares is kept for the rest of the process. Errors are ignored:
/// the real mapping reports them.
pub fn prewarm(libs: &[rusteda_formats::liberty::Library], dont_use: &[String], opts: MapOptions) {
    if opts.backend == MapBackend::Cpu {
        return;
    }
    let Ok(lib) = CellLib::build(libs, dont_use) else {
        return;
    };
    let mut aig = Aig::new();
    let (a, b) = (aig.input(), aig.input());
    let y = aig.and(a, b);
    let _ = map(&aig, &lib, &[y], &FxHashSet::default(), opts);
}

/// Where the true state value of a sequential cell comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QSource {
    /// The cell's `q` pin.
    Q,
    /// The cell's `qn` pin.
    Qn,
    /// An inverter driven by the `q` pin.
    InvQ,
    /// An inverter driven by the `qn` pin.
    InvQn,
}

/// Cell selection for one state element (SYNTH-REQ-0520, SYNTH-REQ-0521).
#[derive(Debug, Clone)]
pub struct SeqChoice {
    pub cell: usize,
    /// Literal driving the data pin.
    pub data: Lit,
    /// Physical data-load control, when using an enabled flip-flop.
    pub enable: Option<Lit>,
    /// Physical inactive-arm input, bound to this cell's stored state.
    pub hold: Option<Lit>,
    /// Literal driving the clock or enable pin.
    pub clock: Lit,
    /// Literal driving the clear pin, if the cell has one.
    pub clear: Option<Lit>,
    /// Literal driving the preset pin, if the cell has one.
    pub preset: Option<Lit>,
    pub q_true: QSource,
    /// The complement of the state is available on a pin without an inverter.
    pub q_free_neg: bool,
}

pub use seq_select::SeqLimits;

/// Selects state cells, including library-defined enable selectors. The clock
/// function is preserved; enable control only selects data versus stored state.
pub fn select_seq(d: &Design, lib: &CellLib) -> Result<Vec<SeqChoice>, String> {
    select_seq_with_limits(d, lib, SeqLimits::default())
}

pub fn select_seq_with_limits(
    d: &Design,
    lib: &CellLib,
    limits: SeqLimits,
) -> Result<Vec<SeqChoice>, String> {
    seq_select::select(d, lib, limits)
}

/// Roots of the combinational mapping: output bits and sequential pin literals.
pub fn roots(d: &Design, seq: &[SeqChoice]) -> Vec<Lit> {
    let mut r = Vec::new();
    for p in d.ports.iter().filter(|p| !p.input) {
        r.extend(p.bits.iter().copied());
    }
    for c in seq {
        r.push(c.data);
        r.push(c.clock);
        r.extend(c.clear);
        r.extend(c.preset);
        r.extend(c.enable);
        r.extend(c.hold);
    }
    r
}

/// State outputs whose complement is free.
pub fn free_neg(d: &Design, seq: &[SeqChoice]) -> FxHashSet<u32> {
    d.states
        .iter()
        .zip(seq)
        .filter(|(_, c)| c.q_free_neg)
        .map(|(s, _)| s.q.node())
        .collect()
}

/// Requirements of primary outputs and FF data, with control roots left unconstrained.
/// This is the same uniform ideal-clock estimate used by electrical refinement.
pub fn timing_boundary(
    d: &Design,
    seq: &[SeqChoice],
    eligible: &CellLib,
    libs: &[rusteda_formats::liberty::Library],
    opts: &crate::electrical::Options,
) -> TimingBoundary {
    let quant = |ns: f64| {
        (ns / eligible.time_unit_ns * library::DELAY_SCALE)
            .floor()
            .clamp(
                -f64::from(library::ARR_INF - 1),
                f64::from(library::ARR_INF - 1),
            ) as i32
    };
    let mut launches = Vec::new();
    let mut required = Vec::new();
    for p in &d.ports {
        for &l in &p.bits {
            if p.input {
                launches.push((l.node(), quant(opts.input_delay_ns)));
            } else {
                required.push(quant(
                    opts.period_ns - opts.output_delay_ns - opts.margin_ns,
                ));
            }
        }
    }
    let mut setup = vec![0.0_f64; eligible.seq.len()];
    for (i, c) in eligible.seq.iter().enumerate() {
        if let Some((raw, lib)) = libs
            .iter()
            .find_map(|l| l.cells.iter().find(|r| r.name == c.name).map(|r| (r, l)))
        {
            for name in std::iter::once(c.data.as_str())
                .chain(c.enable.iter().map(|p| p.pin.as_str()))
                .chain(c.hold.iter().map(|p| p.pin.as_str()))
            {
                let pin = raw.pin(name).expect("selected sequential input pin");
                for arc in &pin.arcs {
                    if arc.timing_type.starts_with("setup_") {
                        for t in [&arc.rise_constraint, &arc.fall_constraint]
                            .into_iter()
                            .flatten()
                            .filter(|t| t.is_consistent())
                        {
                            let data = opts.input_slew_ns / lib.time_unit_ns;
                            let clock = opts.clock_slew_ns / lib.time_unit_ns;
                            let v = if t
                                .variable_1
                                .as_deref()
                                .is_some_and(|s| s.contains("related_pin"))
                            {
                                t.lookup(clock, data)
                            } else {
                                t.lookup(data, clock)
                            };
                            setup[i] = setup[i].max(v * lib.time_unit_ns);
                        }
                    }
                }
            }
        }
    }
    for (state, c) in d.states.iter().zip(seq) {
        launches.push((
            state.q.node(),
            quant(eligible.seq[c.cell].clk_to_q * eligible.time_unit_ns),
        ));
        required.push(if eligible.seq[c.cell].latch {
            library::ARR_INF
        } else {
            quant(opts.period_ns - opts.margin_ns - setup[c.cell])
        });
        required.push(library::ARR_INF);
        required.extend(c.clear.map(|_| library::ARR_INF));
        required.extend(c.preset.map(|_| library::ARR_INF));
        let data_required = quant(opts.period_ns - opts.margin_ns - setup[c.cell]);
        required.extend(c.enable.map(|_| data_required));
        required.extend(c.hold.map(|_| data_required));
    }
    TimingBoundary { launches, required }
}
