// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Incremental timing optimization of a mapped and refined netlist (SYNTH-REQ-0561 to
//! SYNTH-REQ-0564).
//!
//! Unlike the complete refinement of synthesis, the incremental mode keeps every cell of the
//! netlist as it is and makes one change per iteration, only to the editable instances (the
//! focus) and the nets they drive: a resize to another cell of the same family (same function
//! and pins), or the move of the least critical sinks of a driven net behind a new
//! non-inverting buffer. Every candidate change is applied, the whole network is timed again
//! with the estimator, and the change with the best estimated (worst slack, total negative
//! slack) wins when it improves both the current estimate and keeps the electrical-rule
//! violation counts of every corner and the area limit. The loop is bounded by the iteration
//! limit and by the number of candidates per iteration that the effort sets.

use rustc_hash::FxHashMap;
use rusteda_core::job::{Ctx, Event, JobError};
use rusteda_formats::liberty::Library;
use rusteda_formats::verilog::write::{NlModule, NlRef};

use super::model::Model;
use super::network::{Gate, Network, Node};
use super::timing::Timing;
use super::{
    Incremental, IncrementalReport, IterationRecord, Options, Report, SearchEffort, StopReason,
    StopRule, rules,
};
use crate::map::library::CellLib;

/// The most iterations one invocation may ask for (Rule 9 work budget).
pub(super) const MAX_ITERATIONS: usize = 10_000;

/// Timing differences below this are ties (ns).
const EPS: f64 = 1e-9;

/// The smallest gain of the worst or total negative slack that a change must bring (ns):
/// changes with a smaller gain are not worth their area (SYNTH-REQ-0562).
const GAIN: f64 = 1e-5;

/// The search breadth of one iteration (SYNTH-REQ-0563).
struct Breadth {
    /// The most critical editable gates examined.
    gates: usize,
    /// Every other cell of the family, or only the next smaller and the next larger one.
    all_variants: bool,
    /// Buffer cells tried per net (smallest first); 0 disables buffering.
    buffer_cells: usize,
    /// Kept sink counts tried per net: the most critical one, or also half of the sinks.
    keep_half: bool,
    /// The most candidate changes timed per iteration.
    evaluations: usize,
}

fn breadth(e: SearchEffort) -> Breadth {
    match e {
        SearchEffort::Low => Breadth {
            gates: 4,
            all_variants: false,
            buffer_cells: 0,
            keep_half: false,
            evaluations: 64,
        },
        SearchEffort::Medium => Breadth {
            gates: 16,
            all_variants: true,
            buffer_cells: 1,
            keep_half: false,
            evaluations: 512,
        },
        SearchEffort::High => Breadth {
            gates: 64,
            all_variants: true,
            buffer_cells: usize::MAX,
            keep_half: true,
            evaluations: 4096,
        },
    }
}

/// The estimate of a network: timing of the primary library, rule violations of every corner.
struct Measure {
    timing: Timing,
    wns: f64,
    tns: f64,
    area: f64,
    /// Per corner (primary first): capacitance, transition and fanout violations.
    rules: Vec<[usize; 3]>,
}

fn measure(net: &Network, model: &Model<'_>, corners: &[Model<'_>], opts: &Options) -> Measure {
    let timing = Timing::new(net, model, opts);
    let count = |r: rules::RuleReport| [r.capacitance_nets, r.transition_nets, r.fanout_nets];
    let mut rule_counts = vec![count(rules::report(net, model, &timing))];
    for m in corners {
        rule_counts.push(count(rules::report(net, m, &Timing::rules(net, m, opts))));
    }
    let tns = if timing.violation > 0.0 {
        -timing.violation
    } else {
        0.0
    };
    Measure {
        wns: timing.worst_slack,
        tns,
        area: net.area(model),
        timing,
        rules: rule_counts,
    }
}

/// Whether `a` improves on `current`: a worst slack larger by more than [`GAIN`], or a worst
/// slack not smaller and a total negative slack larger by more than [`GAIN`].
fn improves(a: &Measure, current: &Measure) -> bool {
    a.wns > current.wns + GAIN || (a.wns >= current.wns - EPS && a.tns > current.tns + GAIN)
}

/// Whether candidate `a` ranks before candidate `b`: the larger worst slack, then the larger
/// total negative slack, then the smaller area; on a full tie the earlier candidate stays.
fn ranks_before(a: &Measure, b: &Measure) -> bool {
    if (a.wns - b.wns).abs() > EPS {
        return a.wns > b.wns;
    }
    if (a.tns - b.tns).abs() > EPS {
        return a.tns > b.tns;
    }
    a.area < b.area - 1e-12
}

fn met(rule: StopRule, m: &Measure) -> bool {
    match rule {
        StopRule::WnsAtLeast(x) => m.wns >= x,
        StopRule::TnsAtLeast(x) => m.tns >= x,
        StopRule::NoImprovement => false,
    }
}

/// One candidate change.
#[derive(Debug, Clone, Copy)]
enum Change {
    Resize {
        gate: usize,
        cell: usize,
    },
    /// Moves the last `moved` sinks of `order` (least critical last) of `node` behind `buffer`.
    Buffer {
        node: usize,
        buffer: usize,
        moved: usize,
    },
}

/// Inserts `buffer` between `node` and `sinks`; the new gate is the last one.
fn split(net: &mut Network, node: usize, sinks: &[(usize, usize)], buffer: usize, m: &Model<'_>) {
    let out = net.nodes.len();
    net.nodes.push(Node::default());
    net.refs.push(NlRef::Net(String::new()));
    for &(g, p) in sinks {
        net.gates[g].inputs[p] = out;
    }
    net.gates.push(Gate {
        active: true,
        cell: buffer,
        inputs: vec![node],
        outputs: vec![(m.cells[buffer].outputs[0], out)],
    });
}

/// The sinks of `node`, most critical (smallest required time at the sink) first.
fn sinks_by_criticality(
    net: &Network,
    m: &Model<'_>,
    t: &Timing,
    node: usize,
    opts: &Options,
) -> Vec<(usize, usize)> {
    let mut sinks: Vec<(f64, (usize, usize))> = net.nodes[node]
        .sinks
        .iter()
        .map(|&(g, p)| (t.sink_required(net, m, g, p, node, opts), (g, p)))
        .collect();
    sinks.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    sinks.into_iter().map(|(_, s)| s).collect()
}

fn ref_name(r: &NlRef) -> String {
    match r {
        NlRef::Net(n) => n.clone(),
        NlRef::Bit(n, i) => format!("{n}[{i}]"),
        NlRef::Const(v) => format!("1'b{}", u8::from(*v)),
    }
}

/// Applies `change` to `net`, which stays a DAG.
fn apply(
    net: &mut Network,
    change: Change,
    order: &[(usize, usize)],
    m: &Model<'_>,
) -> Result<(), String> {
    match change {
        Change::Resize { gate, cell } => {
            net.change_cell(gate, cell, m);
            Ok(())
        }
        Change::Buffer {
            node,
            buffer,
            moved,
        } => {
            let from = order.len() - moved;
            split(net, node, &order[from..], buffer, m);
            net.rebuild(m)
        }
    }
}

/// The candidate gates of an iteration: editable, active, timed, unprotected gates with a
/// family, most critical (smallest output slack) first (SYNTH-REQ-0563).
fn candidates(net: &Network, m: &Model<'_>, t: &Timing, editable: &[bool]) -> Vec<usize> {
    let mut out: Vec<(f64, usize)> = Vec::new();
    for (g, gate) in net.gates.iter().enumerate() {
        let c = &m.cells[gate.cell];
        if !gate.active
            || !editable[g]
            || gate.outputs.is_empty()
            || c.family >= m.families.len()
            || (!c.comb && c.clock.is_none())
            || gate
                .outputs
                .iter()
                .any(|&(_, n)| net.nodes[n].protected || net.nodes[n].clock)
        {
            continue;
        }
        let slack = gate
            .outputs
            .iter()
            .map(|&(_, n)| t.slack(n))
            .fold(f64::INFINITY, f64::min);
        if slack.is_finite() {
            out.push((slack, g));
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    out.into_iter().map(|(_, g)| g).collect()
}

/// The best improving change of one iteration, if any, and whether a change was rejected only
/// for the area limit (SYNTH-REQ-0562, SYNTH-REQ-0563).
fn search(
    net: &mut Network,
    model: &Model<'_>,
    corners: &[Model<'_>],
    opts: &Options,
    editable: &[bool],
    current: &Measure,
    b: &Breadth,
) -> (Option<(Change, Vec<(usize, usize)>, Measure)>, bool) {
    let t = &current.timing;
    let mut best: Option<(Change, Vec<(usize, usize)>, Measure)> = None;
    let mut area_blocked = false;
    let mut evaluations = 0usize;
    let gates: Vec<usize> = candidates(net, model, t, editable)
        .into_iter()
        .take(b.gates)
        .collect();
    let mut consider =
        |change: Change,
         order: Vec<(usize, usize)>,
         m: Measure,
         best: &mut Option<(Change, Vec<(usize, usize)>, Measure)>| {
            let legal = m
                .rules
                .iter()
                .zip(&current.rules)
                .all(|(a, c)| a.iter().zip(c).all(|(x, y)| x <= y));
            if !legal || !improves(&m, current) {
                return;
            }
            if opts
                .area_limit
                .is_some_and(|limit| m.area > limit + 1e-9 && m.area > current.area + 1e-12)
            {
                area_blocked = true;
                return;
            }
            let wins = best.as_ref().is_none_or(|(_, _, x)| ranks_before(&m, x));
            if wins {
                *best = Some((change, order, m));
            }
        };
    'gates: for g in gates {
        let cell = net.gates[g].cell;
        let family = &model.families[model.cells[cell].family];
        let at = family.iter().position(|&c| c == cell).unwrap_or(0);
        let variants: Vec<usize> = if b.all_variants {
            family.iter().copied().filter(|&c| c != cell).collect()
        } else {
            [at.checked_sub(1), at.checked_add(1)]
                .into_iter()
                .flatten()
                .filter_map(|i| family.get(i).copied())
                .filter(|&c| c != cell)
                .collect()
        };
        for v in variants {
            if evaluations >= b.evaluations {
                break 'gates;
            }
            evaluations += 1;
            net.change_cell(g, v, model);
            let m = measure(net, model, corners, opts);
            net.change_cell(g, cell, model);
            consider(
                Change::Resize { gate: g, cell: v },
                Vec::new(),
                m,
                &mut best,
            );
        }
        if b.buffer_cells == 0 {
            continue;
        }
        let outputs: Vec<usize> = net.gates[g].outputs.iter().map(|&(_, n)| n).collect();
        for node in outputs {
            let n = &net.nodes[node];
            if n.sinks.len() < 2 || n.primary || n.protected || n.clock {
                continue;
            }
            let order = sinks_by_criticality(net, model, t, node, opts);
            let mut keeps = vec![1usize];
            if b.keep_half && order.len() / 2 > 1 {
                keeps.push(order.len() / 2);
            }
            for keep in keeps {
                let moved = (order.len() - keep).min(opts.buffer_fanout);
                for &buffer in model.buffers.iter().take(b.buffer_cells) {
                    if evaluations >= b.evaluations {
                        break 'gates;
                    }
                    evaluations += 1;
                    let change = Change::Buffer {
                        node,
                        buffer,
                        moved,
                    };
                    let mut trial = net.clone();
                    if apply(&mut trial, change, &order, model).is_err() {
                        continue;
                    }
                    let m = measure(&trial, model, corners, opts);
                    consider(change, order.clone(), m, &mut best);
                }
            }
        }
    }
    (best, area_blocked)
}

/// Runs the incremental mode on `netlist` (SYNTH-REQ-0561 to SYNTH-REQ-0564).
pub(super) fn run(
    netlist: &mut NlModule,
    libs: &[Library],
    eligible: &CellLib,
    opts: &Options,
    inc: &Incremental,
    corners: &[Library],
    ctx: Ctx<'_>,
) -> Result<Report, JobError<String>> {
    let model = Model::new(libs, eligible);
    let corner_models: Vec<_> = corners
        .iter()
        .map(|l| Model::corner(&model, l, eligible))
        .collect::<Result<_, _>>()
        .map_err(JobError::Failed)?;
    let mut net = Network::with_state_aliases(netlist, &model, None).map_err(JobError::Failed)?;
    let index: FxHashMap<&str, usize> = netlist
        .instances
        .iter()
        .enumerate()
        .map(|(i, x)| (x.name.as_str(), i))
        .collect();
    let mut editable = vec![inc.focus.is_none(); net.gates.len()];
    for name in inc.focus.iter().flatten() {
        let g = *index.get(name.as_str()).ok_or_else(|| {
            JobError::Failed(format!(
                "focus instance '{name}' is not an instance of the netlist"
            ))
        })?;
        editable[g] = true;
    }
    let editable_instances = editable.iter().filter(|&&e| e).count();
    let names: Vec<String> = netlist.instances.iter().map(|x| x.name.clone()).collect();
    let start: Vec<usize> = net.gates.iter().map(|g| g.cell).collect();
    let original = net.gates.len();
    let b = breadth(inc.effort);
    let mut current = measure(&net, &model, &corner_models, opts);
    let (wns_before, tns_before) = (current.wns, current.tns);
    let mut report = Report {
        area_before: current.area,
        ..Report::default()
    };
    let mut history = Vec::new();
    let stop_reason = loop {
        if met(inc.stop, &current) {
            break StopReason::ConditionMet;
        }
        if history.len() >= inc.max_iterations {
            break StopReason::MaxIters;
        }
        ctx.checkpoint()?;
        let (best, area_blocked) = search(
            &mut net,
            &model,
            &corner_models,
            opts,
            &editable,
            &current,
            &b,
        );
        let Some((change, order, measured)) = best else {
            break if area_blocked {
                StopReason::AreaLimit
            } else {
                StopReason::NoImprovement
            };
        };
        let name_of = |g: usize| (g < original).then(|| names[g].clone());
        let record = match change {
            Change::Resize { gate, cell } => IterationRecord {
                iteration: history.len() + 1,
                change: "resize".into(),
                instance: name_of(gate),
                from_cell: model.cells[net.gates[gate].cell].raw.name.clone(),
                to_cell: model.cells[cell].raw.name.clone(),
                net: None,
                moved_sinks: 0,
                estimated_wns_ns: measured.wns,
                estimated_tns_ns: measured.tns,
                area: measured.area,
            },
            Change::Buffer {
                node,
                buffer,
                moved,
            } => {
                let (driver, _) = net.nodes[node].driver.expect("a buffered net has a driver");
                report.buffers_added += 1;
                IterationRecord {
                    iteration: history.len() + 1,
                    change: "buffer".into(),
                    instance: name_of(driver),
                    from_cell: model.cells[net.gates[driver].cell].raw.name.clone(),
                    to_cell: model.cells[buffer].raw.name.clone(),
                    net: Some(ref_name(&net.refs[node])).filter(|n| !n.is_empty()),
                    moved_sinks: moved,
                    estimated_wns_ns: measured.wns,
                    estimated_tns_ns: measured.tns,
                    area: measured.area,
                }
            }
        };
        apply(&mut net, change, &order, &model).map_err(JobError::Failed)?;
        if let Change::Buffer { .. } = change {
            editable.push(true);
        }
        current = measured;
        ctx.emit(|| Event::Metric {
            name: "electrical_slack_ns",
            value: current.wns,
        });
        history.push(record);
    };
    let t = &current.timing;
    report.estimated_slack_ns = t.worst_slack;
    report.estimated_timing_met = t.worst_slack >= -1e-8;
    report.estimated_violating_endpoints = t.endpoints;
    report.electrical_rules = rules::report(&net, &model, t);
    report.corner_rules = corners
        .iter()
        .zip(&corner_models)
        .map(|(raw, m)| {
            (
                raw.name.clone(),
                rules::report(&net, m, &Timing::rules(&net, m, opts)),
            )
        })
        .collect();
    let clean = |r: &rules::RuleReport| {
        r.capacitance_nets == 0 && r.transition_nets == 0 && r.fanout_nets == 0
    };
    report.estimated_data_rules_met =
        clean(&report.electrical_rules) && report.corner_rules.iter().all(|(_, r)| clean(r));
    let mut depth = vec![0u32; net.nodes.len()];
    for &g in &net.order {
        let gate = &net.gates[g];
        let d = 1 + gate.inputs.iter().map(|&n| depth[n]).max().unwrap_or(0);
        for &(_, n) in &gate.outputs {
            depth[n] = d;
        }
        report.logic_depth = report.logic_depth.max(d);
    }
    report.resized_cells = (0..original)
        .filter(|&g| net.gates[g].active && net.gates[g].cell != start[g])
        .count();
    report.iterations = history.len();
    report.incremental = Some(IncrementalReport {
        iterations_run: history.len(),
        stop_reason,
        editable_instances,
        wns_before_ns: wns_before,
        tns_before_ns: tns_before,
        wns_after_ns: current.wns,
        tns_after_ns: current.tns,
        history,
    });
    net.write(netlist, &model);
    report.area_after = net.area(&model);
    report.area_limit_met = opts
        .area_limit
        .is_none_or(|a| report.area_after <= a + 1e-8);
    Ok(report)
}
