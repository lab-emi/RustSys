// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Keep fast carries near critical paths, and spend structural slack elsewhere.
use crate::{
    aig::{Aig, Lit},
    elab::Design,
};
use rustc_hash::FxHashSet;

fn cost(g: &Aig, roots: &[Lit], boundary: &FxHashSet<u32>) -> usize {
    let mut seen = FxHashSet::default();
    let mut stack = roots.to_vec();
    while let Some(l) = stack.pop() {
        if l.is_const()
            || boundary.contains(&l.node())
            || !g.is_and(l.node())
            || !seen.insert(l.node())
        {
            continue;
        }
        let (a, b) = g.fanins(l.node());
        stack.extend([a, b]);
    }
    seen.len()
}

pub(crate) fn plan(d: &Design) -> Vec<usize> {
    let g = &d.aig;
    if g.add_sites.is_empty() {
        return Vec::new();
    }
    let roots: Vec<_> = d
        .ports
        .iter()
        .filter(|p| !p.input)
        .flat_map(|p| p.bits.iter().copied())
        .chain(
            d.states
                .iter()
                .flat_map(|s| [s.d, s.clk].into_iter().chain(s.arst.map(|x| x.0))),
        )
        .collect();
    let limit = roots.iter().map(|&r| g.level(r)).max().unwrap_or(0);
    let mut required = vec![u32::MAX; g.num_nodes()];
    for r in roots {
        required[r.node() as usize] = limit;
    }
    for n in (1..g.num_nodes() as u32).rev() {
        let r = required[n as usize];
        if r == u32::MAX || !g.is_and(n) {
            continue;
        }
        let (a, b) = g.fanins(n);
        for l in [a, b] {
            required[l.node() as usize] = required[l.node() as usize].min(r.saturating_sub(1));
        }
    }
    let mut candidate = g.clone();
    candidate.parallel_carry = false;
    candidate.carry_cost_weight = None;
    candidate.carry_plan.clear();
    g.add_sites
        .iter()
        .map(|site| {
            let Some(s) = site else { return 0 };
            let old: Vec<_> = s.sum.iter().copied().chain([s.carry]).collect();
            let boundary =
                s.a.iter()
                    .chain(&s.b)
                    .chain([&s.cin])
                    .map(|l| l.node())
                    .collect();
            let mut best = (cost(g, &old, &boundary), 0);
            let mut widths = vec![1, 2, 4, 8, 16, s.a.len()];
            widths.sort_unstable();
            widths.dedup();
            for width in widths {
                if width > s.a.len() {
                    continue;
                }
                let (sum, carry) = if width == s.a.len() {
                    super::ripple_add(&mut candidate, &s.a, &s.b, s.cin)
                } else {
                    super::sparse_add(&mut candidate, &s.a, &s.b, s.cin, width)
                };
                let new: Vec<_> = sum.into_iter().chain([carry]).collect();
                let live: Vec<_> = old
                    .iter()
                    .zip(&new)
                    .filter(|(l, _)| !l.is_const() && required[l.node() as usize] != u32::MAX)
                    .collect();
                if live.is_empty()
                    || !live.iter().all(|(a, b)| {
                        let slack = required[a.node() as usize].saturating_sub(g.level(**a));
                        candidate.level(**b) <= g.level(**a) + slack / 2
                    })
                {
                    continue;
                }
                let area = cost(&candidate, &new, &boundary);
                if area < best.0 {
                    best = (area, width);
                }
            }
            best.1
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bits, elab::Port};
    // SYNTH-REQ-0558: preserve critical carries while spending slack on another addition.
    #[test]
    fn local_chains_use_slack_without_touching_the_critical_addition() {
        for extra in [28, 128] {
            let mut g = Aig::new();
            g.parallel_carry = true;
            let a: Vec<_> = (0..32).map(|_| g.input()).collect();
            let b: Vec<_> = (0..32).map(|_| g.input()).collect();
            let c: Vec<_> = (0..32).map(|_| g.input()).collect();
            let d: Vec<_> = (0..32).map(|_| g.input()).collect();
            let controls: Vec<_> = (0..extra).map(|_| g.input()).collect();
            let (first, _) = bits::add(&mut g, &a, &b, Lit::FALSE);
            let (spare, _) = bits::add(&mut g, &c, &d, Lit::FALSE);
            let mut critical = first[31];
            for control in controls {
                critical = g.and(critical, control);
            }
            let design = Design {
                top: "slack".into(),
                aig: g,
                states: Vec::new(),
                ports: vec![
                    Port {
                        name: "critical".into(),
                        input: false,
                        width: 1,
                        scalar: true,
                        bits: vec![critical],
                    },
                    Port {
                        name: "spare".into(),
                        input: false,
                        width: 32,
                        scalar: false,
                        bits: spare,
                    },
                ],
            };
            let choices = plan(&design);
            assert_eq!(choices[0], 0);
            if extra == 128 {
                assert_eq!(choices[1], 32);
            } else {
                assert!(choices[1] > 4 && choices[1] < 32, "{choices:?}");
            }
        }
    }
    #[test]
    fn driver_resolution_keeps_sites_and_planned_additions_keep_the_function() {
        use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
        let text = "module m(input [31:0] a,b,c,d,input [127:0] ctl,output crit,output [31:0] spare); wire [31:0] first=a+b; wire [127:0] chain; assign chain[0]=first[31]&ctl[0]; for(genvar i=1;i<128;i=i+1) begin: level assign chain[i]=chain[i-1]&ctl[i]; end assign crit=chain[127]; assign spare=c+d; endmodule";
        let mut sm = SourceMap::new();
        let (units, diags) = read_texts(&mut sm, &[("m.sv", text)], PpOptions::default());
        assert!(!diags.iter().any(|d| d.is_error()));
        let options = crate::elab::ElabOptions {
            parallel_carry: true,
            ..Default::default()
        };
        let first = crate::elab::elaborate(&sm, &units, &options).0.unwrap();
        let choices = plan(&first);
        assert_eq!(choices, [0, 32]);
        let second = crate::elab::elaborate(
            &sm,
            &units,
            &crate::elab::ElabOptions {
                carry_plan: choices,
                ..options
            },
        )
        .0
        .unwrap();
        assert_ne!(first.aig.num_ands(), second.aig.num_ands());
        let mut seed = 0xaef3_12b0_3819_214bu64;
        for _ in 0..16 {
            let values: Vec<_> = (0..first.aig.num_inputs())
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed
                })
                .collect();
            let a = first.simulate(&values);
            let b = second.simulate(&values);
            for (x, y) in first
                .ports
                .iter()
                .zip(&second.ports)
                .filter(|(x, _)| !x.input)
            {
                for (&p, &q) in x.bits.iter().zip(&y.bits) {
                    let get = |v: &[u64], l: Lit| {
                        v[l.node() as usize] ^ if l.is_compl() { u64::MAX } else { 0 }
                    };
                    assert_eq!(get(&a, p), get(&b, q));
                }
            }
        }
    }
}
