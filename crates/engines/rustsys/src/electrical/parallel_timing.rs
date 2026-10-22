// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded immutable timing proposals on Kahn layers. Shared vectors, no graph
//! copies per worker. Stable commits and per-net pin order preserve f64 bits.

use super::{
    Options,
    model::Model,
    network::Network,
    timing::{Timing, Value},
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParallelTiming {
    pub min_gates: usize,
    pub max_workers: usize,
    pub batch_gates: usize,
    pub max_levels: usize,
    pub max_plan_bytes: usize,
}

impl Default for ParallelTiming {
    fn default() -> Self {
        Self {
            min_gates: 4096,
            max_workers: 16,
            batch_gates: 16384,
            max_levels: 65536,
            max_plan_bytes: 16 * 1024 * 1024,
        }
    }
}

struct Backward {
    node: usize,
    slope: f64,
    inputs: SmallVec<[(usize, [f64; 2]); 6]>,
}

fn batch(opts: &Options) -> usize {
    opts.parallel_timing
        .batch_gates
        .min(65536)
        .min(opts.parallel_timing.max_plan_bytes / size_of::<Backward>())
}

pub(super) fn enabled(net: &Network, opts: &Options) -> bool {
    net.order.len() >= opts.parallel_timing.min_gates
        && rayon::current_num_threads() > 1
        && opts.parallel_timing.max_workers > 1
        && batch(opts) > 0
        && net
            .level_ends
            .as_ref()
            .is_some_and(|ends| ends.len().saturating_sub(1) <= opts.parallel_timing.max_levels)
}

fn grain(len: usize, opts: &Options) -> usize {
    len.div_ceil(
        rayon::current_num_threads()
            .min(opts.parallel_timing.max_workers)
            .min(16),
    )
    .max(1)
}

pub(super) fn initialize(t: &mut Timing, net: &Network, lib: &Model<'_>, opts: &Options) {
    // rebuild() appends sinks in gate/pin order, exactly the serial addition order.
    let node_grain = grain(t.load.len(), opts);
    t.load
        .par_iter_mut()
        .enumerate()
        .with_min_len(node_grain)
        .for_each(|(n, load)| {
            let node = &net.nodes[n];
            *load = opts.output_load_pf * node.outputs as f64;
            for &(g, i) in &node.sinks {
                let c = &lib.cells[net.gates[g].cell];
                *load += c.input_cap(c.inputs[i]);
            }
        });
    t.value
        .par_iter_mut()
        .enumerate()
        .with_min_len(node_grain)
        .for_each(|(n, value)| {
            let node = &net.nodes[n];
            if node.primary {
                *value = Value {
                    arrival: [opts.input_delay_ns; 2],
                    slew: [opts.input_slew_ns; 2],
                };
            }
            if let Some((g, pin)) = node.driver {
                let c = &lib.cells[net.gates[g].cell];
                if !c.comb && c.clock.is_some() {
                    *value =
                        Timing::launch(lib, net.gates[g].cell, pin, t.load[n], opts.clock_slew_ns);
                }
            }
        });
}

pub(super) fn forward(t: &mut Timing, net: &Network, lib: &Model<'_>, opts: &Options) {
    for layer in net.level_ends.as_ref().unwrap().windows(2) {
        for gates in net.order[layer[0]..layer[1]].chunks(batch(opts)) {
            if gates.len() < 32 {
                for &g in gates {
                    t.value[net.gates[g].outputs[0].1] =
                        t.candidate(net, lib, g, net.gates[g].cell, false);
                }
            } else {
                let values: Vec<_> = gates
                    .par_iter()
                    .with_min_len(grain(gates.len(), opts))
                    .map(|&g| t.candidate(net, lib, g, net.gates[g].cell, false))
                    .collect();
                for (&g, value) in gates.iter().zip(values) {
                    t.value[net.gates[g].outputs[0].1] = value;
                }
            }
        }
    }
}

fn propose(t: &Timing, net: &Network, lib: &Model<'_>, g: usize) -> Backward {
    let gate = &net.gates[g];
    let c = &lib.cells[gate.cell];
    let (op, n) = gate.outputs[0];
    let dc = (t.load[n] * 0.02).max(1e-5);
    let mut out = Backward {
        node: n,
        slope: 0.0,
        inputs: SmallVec::new(),
    };
    assert!(
        c.inputs.len() <= 6,
        "bounded combinational library interface"
    );
    for (&ip, &a) in c.inputs.iter().zip(&gate.inputs) {
        let slew = t.value[a].slew;
        let base = c.arc_delay(ip, op, slew, t.load[n]);
        let plus = c.arc_delay(ip, op, slew, t.load[n] + dc);
        let mut req = [f64::INFINITY; 2];
        for (&(ti, to, d), &(_, _, d_plus)) in base.iter().zip(&plus) {
            if d >= 0.0 {
                req[ti] = req[ti].min(t.required[n][to] - d);
                out.slope = out.slope.max((d_plus - d) / dc);
            }
        }
        out.inputs.push((a, req));
    }
    out
}

fn commit(t: &mut Timing, p: Backward) {
    t.slope[p.node] = t.slope[p.node].max(p.slope);
    for (a, req) in p.inputs {
        for (i, r) in req.into_iter().enumerate() {
            t.required[a][i] = t.required[a][i].min(r);
        }
    }
}

pub(super) fn backward(t: &mut Timing, net: &Network, lib: &Model<'_>, opts: &Options) {
    for layer in net.level_ends.as_ref().unwrap().windows(2).rev() {
        for gates in net.order[layer[0]..layer[1]].rchunks(batch(opts)) {
            if gates.len() < 32 {
                for &g in gates.iter().rev() {
                    let p = propose(t, net, lib, g);
                    commit(t, p);
                }
            } else {
                let proposals: Vec<_> = gates
                    .par_iter()
                    .rev()
                    .with_min_len(grain(gates.len(), opts))
                    .map(|&g| propose(t, net, lib, g))
                    .collect();
                for p in proposals {
                    commit(t, p);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::electrical::tests::{inst, libs, port};
    use crate::map::library::CellLib;
    use rusteda_formats::verilog::write::{NlDir, NlModule};

    fn bits(t: &Timing) -> Vec<u64> {
        t.load
            .iter()
            .copied()
            .chain(
                t.value
                    .iter()
                    .flat_map(|v| v.arrival.into_iter().chain(v.slew)),
            )
            .chain(t.required.iter().flat_map(|x| *x))
            .chain(t.slope.iter().copied())
            .chain([t.worst_slack, t.violation])
            .map(f64::to_bits)
            .collect()
    }

    // <500 nodes, <1MiB timing/plan state, shared immutable library. Exercise
    // all f64 bits on reconvergence, tied/repeated inputs, shared state controls,
    // multiple Kahn levels and batches; retain the serial evaluator as reference.
    #[test]
    fn parallel_timing_preserves_every_scalar_and_budget_fallback() {
        let ls = libs();
        let eligible = CellLib::build(&ls, &[]).unwrap();
        let lib = Model::new(&ls, &eligible);
        let mut module = NlModule {
            name: "reconvergent".into(),
            ports: vec![
                port("a", NlDir::Input),
                port("b", NlDir::Input),
                port("clk", NlDir::Input),
                port("q", NlDir::Output),
            ],
            ..Default::default()
        };
        for i in 0..96 {
            let n = format!("n{i}");
            let p = format!("p{i}");
            let z = format!("z{i}");
            module.instances.push(inst(
                &format!("g{i}"),
                "wt_nand2_x1",
                &[
                    ("A", "a"),
                    ("B", if i % 2 == 0 { "a" } else { "b" }),
                    ("Y", &n),
                ],
            ));
            module.instances.push(inst(
                &format!("h{i}"),
                "wt_nand2_x1",
                &[("A", &n), ("B", "b"), ("Y", &p)],
            ));
            module.instances.push(inst(
                &format!("j{i}"),
                "wt_nand2_x1",
                &[("A", &n), ("B", &p), ("Y", &z)],
            ));
            module.instances.push(inst(
                &format!("s{i}"),
                "wt_edff_x1",
                &[
                    ("D", &z),
                    ("E", "a"),
                    ("CK", "clk"),
                    ("Q", if i == 0 { "q" } else { "unused" }),
                ],
            ));
            if i != 0 {
                module
                    .instances
                    .last_mut()
                    .unwrap()
                    .conns
                    .last_mut()
                    .unwrap()
                    .1 = super::super::tests::r(&format!("q{i}"));
            }
        }
        let net = Network::new(&module, &lib).unwrap();
        let ends = net.level_ends.as_ref().unwrap();
        assert_eq!(ends.len(), 4);
        for period in [0.0, 0.3, 1000.0] {
            let mut opts = Options {
                period_ns: period,
                ..Default::default()
            };
            opts.parallel_timing.min_gates = 0;
            opts.parallel_timing.max_workers = 1;
            let reference = Timing::new(&net, &lib, &opts);
            let rules = Timing::rules(&net, &lib, &opts);
            for threads in [2, 16] {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                for batch in [17, 64, 16384] {
                    opts.parallel_timing.max_workers = 16;
                    opts.parallel_timing.batch_gates = batch;
                    let actual = pool.install(|| Timing::new(&net, &lib, &opts));
                    assert_eq!(bits(&reference), bits(&actual));
                    assert_eq!(reference.endpoints, actual.endpoints);
                    assert_eq!(
                        bits(&rules),
                        bits(&pool.install(|| Timing::rules(&net, &lib, &opts)))
                    );
                }
                for (levels, bytes) in [(0, 16 * 1024 * 1024), (65536, 0)] {
                    opts.parallel_timing.max_levels = levels;
                    opts.parallel_timing.max_plan_bytes = bytes;
                    assert_eq!(
                        bits(&reference),
                        bits(&pool.install(|| Timing::new(&net, &lib, &opts)))
                    );
                }
                opts.parallel_timing.max_levels = 65536;
                opts.parallel_timing.max_plan_bytes = 16 * 1024 * 1024;
            }
        }
    }
}
