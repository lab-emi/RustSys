// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use std::mem::size_of;

use super::Options;
use crate::elab::Design;
use crate::map::cuts::Cut;

pub(super) struct Mapping {
    pub luts: Vec<(u32, Cut)>,
    pub depth: u32,
    pub merges: u64,
    pub memory_bound: u64,
}

/// O(nodes * max_cuts^2), bounded storage; AIG IDs must be topological. Ranking
/// uses LUT depth, area flow and canonical leaf IDs, so it is deterministic.
pub(super) fn map(d: &Design, opts: &Options) -> Result<Mapping, String> {
    let g = &d.aig;
    let n = g.num_nodes();
    if n > opts.max_nodes {
        return Err(format!("FPGA_NODE_BUDGET: {n} > {}", opts.max_nodes));
    }
    // Includes retained cuts, Vec headers, live/fanout/depth/flow vectors, emitted records,
    // allocator headroom and the bounded transient candidate table. No path enumeration.
    let per_node = opts
        .max_cuts_per_node
        .checked_mul(size_of::<Cut>())
        .and_then(|v| v.checked_add(size_of::<Vec<Cut>>() + 4 + 4 + 8 + 1 + 48 + 32))
        .ok_or("FPGA_MEMORY_BUDGET: size overflow")?;
    let memory_bound = (n as u64)
        .checked_mul(per_node as u64)
        .and_then(|v| v.checked_add((opts.max_cuts_per_node * size_of::<Cut>()) as u64))
        .ok_or("FPGA_MEMORY_BUDGET: size overflow")?;
    if memory_bound > opts.max_mapping_bytes {
        return Err(format!(
            "FPGA_MEMORY_BUDGET: {memory_bound} > {}",
            opts.max_mapping_bytes
        ));
    }
    let mut refs = vec![0u32; n];
    for node in 1..n as u32 {
        if !g.is_and(node) {
            continue;
        }
        let (a, b) = g.fanins(node);
        if a.node() >= node || b.node() >= node {
            return Err("FPGA_CYCLE: non-topological AIG".into());
        }
        refs[a.node() as usize] = refs[a.node() as usize].saturating_add(1);
        refs[b.node() as usize] = refs[b.node() as usize].saturating_add(1);
    }
    let mut live = vec![false; n];
    // Root collection is streamed; no stack or duplicated reconvergent paths.
    for p in d.ports.iter().filter(|p| !p.input) {
        for l in &p.bits {
            live[l.node() as usize] = true;
        }
    }
    for s in &d.states {
        for l in [Some(s.d), Some(s.clk), s.arst.map(|(l, _)| l)]
            .into_iter()
            .flatten()
        {
            live[l.node() as usize] = true;
        }
    }
    for (i, &used) in live.iter().enumerate() {
        if used {
            refs[i] = refs[i].saturating_add(1);
        }
    }
    let mut cuts: Vec<Vec<Cut>> = Vec::with_capacity(n);
    cuts.push(vec![Cut {
        leaves: [0; 6],
        n: 0,
        tt: 0,
        sign: 0,
    }]);
    let mut depth = vec![0u32; n];
    let mut flow = vec![0f64; n];
    let mut merges = 0u64;
    for node in 1..n as u32 {
        if !g.is_and(node) {
            cuts.push(vec![Cut::trivial(node)]);
            continue;
        }
        let (a, b) = g.fanins(node);
        let mut candidates = Vec::with_capacity(opts.max_cuts_per_node);
        let rank = |c: &Cut| {
            let dep = c
                .leaves()
                .iter()
                .map(|l| depth[*l as usize])
                .max()
                .unwrap_or(0)
                + u32::from(c.n != 0);
            let area = (1.0 + c.leaves().iter().map(|l| flow[*l as usize]).sum::<f64>())
                / f64::from(refs[node as usize].max(1));
            (dep, area)
        };
        let cmp = |x: &Cut, y: &Cut| {
            let (xd, xf) = rank(x);
            let (yd, yf) = rank(y);
            xd.cmp(&yd)
                .then_with(|| xf.total_cmp(&yf))
                .then_with(|| x.leaf_key().cmp(&y.leaf_key()))
        };
        for x in &cuts[a.node() as usize] {
            for y in &cuts[b.node() as usize] {
                if merges == opts.max_cut_merges {
                    return Err("FPGA_WORK_BUDGET: cut merge limit reached".into());
                }
                merges += 1;
                let Some(c) = Cut::merge(x, a, y, b, 6) else {
                    continue;
                };
                if candidates
                    .iter()
                    .any(|p: &Cut| p.leaf_key() == c.leaf_key())
                {
                    continue;
                }
                let pos = candidates.partition_point(|p| cmp(p, &c).is_le());
                if pos < opts.max_cuts_per_node - 1 {
                    // Pop before inserting, so capacity never grows beyond its hard limit.
                    if candidates.len() == opts.max_cuts_per_node - 1 {
                        candidates.pop();
                    }
                    candidates.insert(pos, c);
                }
            }
        }
        let best = candidates
            .first()
            .ok_or("FPGA_INTERNAL: no feasible two-input cut")?;
        (depth[node as usize], flow[node as usize]) = rank(best);
        candidates.push(Cut::trivial(node));
        cuts.push(candidates);
    }
    let mut max_depth = 0;
    // Reverse topological marking gives O(V+E) covering even on reconvergent DAGs.
    for node in (1..n as u32).rev() {
        if !live[node as usize] {
            continue;
        }
        max_depth = max_depth.max(depth[node as usize]);
        if g.is_and(node) {
            for leaf in cuts[node as usize][0].leaves() {
                live[*leaf as usize] = true;
            }
        }
    }
    // Explicit root inversions are LUT1 cells; internal edge inversions are
    // absorbed into INIT. Flop clock/reset inversions use primitive attributes.
    let mut endpoint = |lit: crate::aig::Lit, inverter: bool| {
        max_depth = max_depth.max(
            depth[lit.node() as usize] + u32::from(inverter && lit.is_compl() && !lit.is_const()),
        );
    };
    for port in d.ports.iter().filter(|p| !p.input) {
        for &lit in &port.bits {
            endpoint(lit, true);
        }
    }
    for state in &d.states {
        endpoint(state.d, true);
        endpoint(state.clk, state.kind == crate::elab::StateKind::Latch);
        if let Some((lit, _)) = state.arst {
            endpoint(lit, state.kind == crate::elab::StateKind::Latch);
        }
    }
    let mut luts = Vec::with_capacity(
        live.iter()
            .enumerate()
            .filter(|(i, b)| **b && g.is_and(*i as u32))
            .count(),
    );
    for node in 1..n as u32 {
        if live[node as usize] && g.is_and(node) {
            luts.push((node, cuts[node as usize][0]));
        }
    }
    Ok(Mapping {
        luts,
        depth: max_depth,
        merges,
        memory_bound,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aig::{Aig, Lit};
    use crate::elab::Port;

    // About 4,000 nodes, <4 MiB including cuts and work vectors. Each diamond
    // reconverges; enumerating paths would be exponential in the 1,000 levels.
    #[test]
    fn reconvergence_uses_bounded_topological_work() {
        let mut aig = Aig::new();
        let mut inputs = vec![aig.input(), aig.input()];
        let mut root = inputs[0];
        for _ in 0..1000 {
            let select = aig.input();
            inputs.push(select);
            let positive = aig.and(root, select);
            let negative = aig.and(root, !select);
            let alternate = aig.xor(negative, inputs[1]);
            root = aig.or(positive, alternate);
        }
        let d = Design {
            top: "diamonds".into(),
            aig,
            states: vec![],
            ports: vec![
                Port {
                    name: "i".into(),
                    input: true,
                    width: inputs.len() as u32,
                    scalar: false,
                    bits: inputs,
                },
                Port {
                    name: "o".into(),
                    input: false,
                    width: 1,
                    scalar: true,
                    bits: vec![root],
                },
            ],
        };
        let options = Options {
            max_nodes: 10000,
            max_cut_merges: 1_000_000,
            ..Default::default()
        };
        let output = map(&d, &options).unwrap();
        assert!(output.merges < options.max_cut_merges);
        assert!(output.memory_bound < 8 << 20);
        assert!(output.luts.len() <= d.aig.num_ands());
        let tiny = Options {
            max_cut_merges: 100,
            ..options
        };
        assert!(map(&d, &tiny).err().unwrap().contains("FPGA_WORK_BUDGET"));
        assert_ne!(root, Lit::FALSE);
    }
}
