// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The timing target of `opt-timing` derived from the `constraints` table of the base revision
//! (CLI-REQ-0209): the compiled SDC (`CONSTR` v1) gives the period, the input and output delays,
//! the output load, the input and clock transitions and the setup uncertainty of the uniform
//! ideal-clock estimate; command-line flags override them. Constraint content that the uniform
//! estimate does not model is reported, never dropped silently (Rule 7.1).

use std::collections::BTreeMap;

use rusteda_core::liberty::Library;
use rusteda_formats::sdc::{CmdKind, ConstrSection, MinMaxScope, Obj, Sdc, SetupHoldScope};
use rusteda_workspace::{Corruption, DiagCode, Diagnostic, Error, Revision, Severity};

use crate::args::OptTimingArgs;

/// The table name of compiled constraints.
pub(crate) const TABLE_NAME: &str = "constraints";

/// Constraint content that the estimate does not model, or reads approximately.
const UNMODELLED: DiagCode = DiagCode::of("S0535");
/// The timing target is not derived from constraints.
const NOT_DERIVED: DiagCode = DiagCode::of("S0536");

/// The default values of the estimate (the defaults of the electrical options).
pub(crate) mod default {
    pub(crate) const PERIOD_NS: f64 = 10.0;
    pub(crate) const INPUT_DELAY_NS: f64 = 0.0;
    pub(crate) const OUTPUT_DELAY_NS: f64 = 0.0;
    pub(crate) const INPUT_SLEW_NS: f64 = 0.1;
    pub(crate) const CLOCK_SLEW_NS: f64 = 0.1;
    pub(crate) const OUTPUT_LOAD_PF: f64 = 0.01;
    pub(crate) const MARGIN_NS: f64 = 0.05;
}

/// The values of the uniform estimate and where they came from (CLI-REQ-0209).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct Target {
    pub period_ns: f64,
    pub input_delay_ns: f64,
    pub output_delay_ns: f64,
    pub input_slew_ns: f64,
    pub clock_slew_ns: f64,
    pub output_load_pf: f64,
    /// The setup margin: `--timing-margin` (default 0.05 ns) plus the setup uncertainty.
    pub margin_ns: f64,
    /// The largest setup clock uncertainty of the constraints, included in `margin_ns`.
    pub setup_uncertainty_ns: f64,
}

/// The derived target with its source and the values that flags set.
#[derive(Debug, Clone)]
pub(crate) struct Derived {
    pub target: Target,
    /// `sdc`, `cli` or `defaults`.
    pub source: &'static str,
    /// The values that command-line flags set, by their names in [`Target`].
    pub overrides: Vec<&'static str>,
    /// Clocks of the constraints.
    pub clocks: usize,
}

/// The SDC of the `constraints` table of `rev`, content hash verified.
fn read_constraints(rev: &Revision) -> Result<Sdc, Error> {
    let rbin = rev.open_rbin(TABLE_NAME, "CONSTR", 1..=1)?;
    rbin.verify_hash()?;
    let corrupt = |reason: String| {
        Error::corrupt(
            rbin.path(),
            Corruption::Json {
                reason: format!("CONSTR v1 table: {reason}"),
            },
        )
    };
    let mut sections = Vec::with_capacity(rbin.sections().len());
    for s in rbin.sections() {
        let id = u16::try_from(s.id).map_err(|_| corrupt(format!("section id {}", s.id)))?;
        sections.push(ConstrSection {
            id,
            elem_size: s.elem_size,
            count: s.count(),
            bytes: rbin.section_bytes(s.id)?.to_vec(),
        });
    }
    Sdc::from_sections(&sections).map_err(|e| corrupt(e.to_string()))
}

/// Whether `obj` names ports only (CLI-REQ-0209: `set_load` on ports).
fn ports_only(obj: &Obj) -> bool {
    match obj {
        Obj::Ports(_) | Obj::AllInputs { .. } | Obj::AllOutputs | Obj::Names(_) => true,
        Obj::Union(list) => list.iter().all(ports_only),
        Obj::Diff(a, _) => ports_only(a),
        _ => false,
    }
}

/// Unmodelled content, by command name: count and first location.
#[derive(Default)]
struct Notes {
    by_kind: BTreeMap<String, (usize, Option<(String, u32, u32)>)>,
}

impl Notes {
    fn add(&mut self, what: impl Into<String>, at: Option<(String, u32, u32)>) {
        let e = self.by_kind.entry(what.into()).or_insert((0, None));
        e.0 += 1;
        if e.1.is_none() {
            e.1 = at;
        }
    }
}

/// Derives the target of `rev` with the flags of `args` (CLI-REQ-0209); `lib` gives the units
/// of constraints written without `set_units`.
pub(crate) fn derive(
    rev: &Revision,
    args: &OptTimingArgs,
    lib: &Library,
    diags: &mut Vec<Diagnostic>,
) -> Result<Derived, Error> {
    let flags: [(&'static str, Option<f64>); 7] = [
        ("period_ns", args.timing_period),
        ("input_delay_ns", args.input_delay),
        ("output_delay_ns", args.output_delay),
        ("input_slew_ns", args.input_slew),
        ("clock_slew_ns", args.clock_slew),
        ("output_load_pf", args.output_load),
        ("margin_ns", args.timing_margin),
    ];
    let overrides: Vec<&'static str> = flags
        .iter()
        .filter(|(_, v)| v.is_some())
        .map(|(k, _)| *k)
        .collect();
    let base_margin = args.timing_margin.unwrap_or(default::MARGIN_NS);
    if !rev.has_table(TABLE_NAME) {
        diags.push(
            Diagnostic::new(
                Severity::Warning,
                NOT_DERIVED,
                format!(
                    "{} has no constraints table: the timing target is not derived from \
                     constraints but taken from the command line and the defaults",
                    rev.rev()
                ),
            )
            .with_help("import the SDC constraints with `rusteda import --sdc` for a target that the timing analysis shares"),
        );
        let target = Target {
            period_ns: args.timing_period.unwrap_or(default::PERIOD_NS),
            input_delay_ns: args.input_delay.unwrap_or(default::INPUT_DELAY_NS),
            output_delay_ns: args.output_delay.unwrap_or(default::OUTPUT_DELAY_NS),
            input_slew_ns: args.input_slew.unwrap_or(default::INPUT_SLEW_NS),
            clock_slew_ns: args.clock_slew.unwrap_or(default::CLOCK_SLEW_NS),
            output_load_pf: args.output_load.unwrap_or(default::OUTPUT_LOAD_PF),
            margin_ns: base_margin,
            setup_uncertainty_ns: 0.0,
        };
        let source = if overrides.is_empty() {
            "defaults"
        } else {
            "cli"
        };
        return Ok(Derived {
            target,
            source,
            overrides,
            clocks: 0,
        });
    }
    let sdc = read_constraints(rev)?;
    let time = sdc.units.time_to_ns.unwrap_or(lib.time_unit_ns);
    let cap = sdc.units.cap_to_pf.unwrap_or(lib.cap_unit_pf);
    let mut notes = Notes::default();
    let mut periods: Vec<f64> = Vec::new();
    let max = |slot: &mut Option<f64>, v: f64| *slot = Some(slot.map_or(v, |s: f64| s.max(v)));
    let (mut in_delay, mut out_delay, mut in_slew, mut clk_slew, mut load, mut unc) =
        (None, None, None, None, None, None);
    let max_side = |m: MinMaxScope| matches!(m, MinMaxScope::Both | MinMaxScope::Max);
    for c in &sdc.cmds {
        let at = sdc
            .sources
            .get(usize::from(c.source))
            .map(|s| (s.path.clone(), c.line, c.col));
        match &c.kind {
            CmdKind::CreateClock { period, .. } => periods.push(period * time),
            CmdKind::SetClockUncertainty {
                uncertainty,
                setup_hold,
                ..
            } => {
                if matches!(setup_hold, SetupHoldScope::Both | SetupHoldScope::Setup) {
                    max(&mut unc, uncertainty * time);
                }
            }
            CmdKind::SetClockTransition {
                transition,
                min_max,
                ..
            } => {
                if max_side(*min_max) {
                    max(&mut clk_slew, transition * time);
                }
            }
            CmdKind::SetInputDelay {
                delay,
                min_max,
                clock_fall,
                level_sensitive,
                reference_pin,
                network_latency_included,
                source_latency_included,
                ..
            }
            | CmdKind::SetOutputDelay {
                delay,
                min_max,
                clock_fall,
                level_sensitive,
                reference_pin,
                network_latency_included,
                source_latency_included,
                ..
            } => {
                if !max_side(*min_max) {
                    continue;
                }
                let input = matches!(c.kind, CmdKind::SetInputDelay { .. });
                max(
                    if input { &mut in_delay } else { &mut out_delay },
                    delay * time,
                );
                if *clock_fall
                    || *level_sensitive
                    || reference_pin.is_some()
                    || *network_latency_included
                    || *source_latency_included
                {
                    notes.add(
                        format!(
                            "{} with -clock_fall, -level_sensitive, -reference_pin or latency \
                             options (read as a delay from the rising ideal clock edge)",
                            c.name()
                        ),
                        at,
                    );
                }
            }
            CmdKind::SetInputTransition {
                transition,
                min_max,
                ..
            } => {
                if max_side(*min_max) {
                    max(&mut in_slew, transition * time);
                }
            }
            CmdKind::SetLoad {
                value,
                min_max,
                objects,
                subtract_pin_load,
                ..
            } => {
                if !max_side(*min_max) {
                    continue;
                }
                if ports_only(objects) {
                    max(&mut load, value * cap);
                    if *subtract_pin_load {
                        notes.add("set_load -subtract_pin_load (read as a port load)", at);
                    }
                } else {
                    notes.add("set_load on nets, pins or cells", at);
                }
            }
            CmdKind::SetUnits { .. }
            | CmdKind::CurrentDesign { .. }
            | CmdKind::GroupPath { .. }
            | CmdKind::SetMinPulseWidth { .. }
            | CmdKind::SetMinDelay { .. }
            | CmdKind::SetMinCapacitance { .. }
            | CmdKind::SetMaxDynamicPower { .. }
            | CmdKind::SetMaxLeakagePower { .. }
            | CmdKind::UnsetPropagatedClock { .. } => {}
            _ => notes.add(c.name(), at),
        }
    }
    let mut distinct = periods.clone();
    distinct.sort_by(f64::total_cmp);
    distinct.dedup();
    let period = distinct.first().copied();
    if distinct.len() > 1 {
        diags.push(Diagnostic::new(
            Severity::Warning,
            UNMODELLED,
            format!(
                "the constraints define {} clocks with periods {distinct:?} ns; the estimate uses \
                 one ideal clock with the smallest period",
                periods.len()
            ),
        ));
    }
    if period.is_none() && args.timing_period.is_none() {
        diags.push(Diagnostic::new(
            Severity::Warning,
            NOT_DERIVED,
            format!(
                "the constraints define no clock: the period is the default {} ns",
                default::PERIOD_NS
            ),
        ));
    }
    let mut nonnegative = |name: &str, v: Option<f64>| -> Option<f64> {
        match v {
            Some(x) if x < 0.0 => {
                diags.push(Diagnostic::new(
                    Severity::Warning,
                    UNMODELLED,
                    format!(
                        "the largest {name} of the constraints is negative ({x} ns); the \
                         estimate uses 0"
                    ),
                ));
                Some(0.0)
            }
            other => other,
        }
    };
    let in_delay = nonnegative("input delay", in_delay);
    let out_delay = nonnegative("output delay", out_delay);
    let setup_uncertainty_ns = unc.unwrap_or(0.0).max(0.0);
    for (what, (count, at)) in notes.by_kind {
        let mut d = Diagnostic::new(
            Severity::Warning,
            UNMODELLED,
            format!(
                "{what} ({count} command{}) is not modelled by the opt-timing estimate (ideal \
                 clock, no wires, no path exceptions); the target ignores it",
                if count == 1 { "" } else { "s" }
            ),
        )
        .with_help(
            "check the result with the static timing analysis of the suite, which applies it",
        );
        if let Some((file, line, col)) = at {
            d = d.at(file, line.max(1), col.max(1));
        }
        diags.push(d);
    }
    let target = Target {
        period_ns: args.timing_period.or(period).unwrap_or(default::PERIOD_NS),
        input_delay_ns: args
            .input_delay
            .or(in_delay)
            .unwrap_or(default::INPUT_DELAY_NS),
        output_delay_ns: args
            .output_delay
            .or(out_delay)
            .unwrap_or(default::OUTPUT_DELAY_NS),
        input_slew_ns: args
            .input_slew
            .or(in_slew)
            .unwrap_or(default::INPUT_SLEW_NS),
        clock_slew_ns: args
            .clock_slew
            .or(clk_slew)
            .unwrap_or(default::CLOCK_SLEW_NS),
        output_load_pf: args.output_load.or(load).unwrap_or(default::OUTPUT_LOAD_PF),
        margin_ns: base_margin + setup_uncertainty_ns,
        setup_uncertainty_ns,
    };
    Ok(Derived {
        target,
        source: "sdc",
        overrides,
        clocks: periods.len(),
    })
}
