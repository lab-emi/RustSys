// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `known-answer` (SYNTH-REQ-0802): combinational modules with at most 12 input bits whose
//! netlists are compared, input by input, with the truth tables of the generator's own
//! evaluator.

mod common;

use std::collections::HashMap;

use common::{eval_netlist, lib, port_value, synth};
use rustsys::{Effort, Goal};
use rustsys_defects::read_netlist;
use rustsys_fixtures::liberty::Variant;
use rustsys_fixtures::random::random_comb;

#[test]
fn exhaustive_truth_tables() {
    let n = std::env::var("WT_KA_N")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(40u64);
    let l = lib(Variant::Rich);
    let mut checked = 0usize;
    for seed in 0..n {
        let m = random_comb(20_000 + seed, 12);
        let bits = m.input_bits();
        if bits > 12 {
            continue;
        }
        let files = vec![(format!("{}.sv", m.name), m.text.clone())];
        let out = synth(&files, &m.name, &l, Effort::High, Goal::Area)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}\n{}", m.text));
        let nl = read_netlist(&out.text).unwrap();
        for x in 0..(1u64 << bits) {
            let mut given = HashMap::new();
            let mut ins = Vec::new();
            let mut off = 0;
            for v in &m.vars[..m.inputs] {
                let val = (x >> off) & ((1 << v.width) - 1);
                off += v.width;
                ins.push(u128::from(val));
                for k in 0..v.width {
                    given.insert(format!("{}[{k}]", v.name), (val >> k) & 1 == 1);
                }
            }
            let expect = m.eval(&ins);
            let vals = eval_netlist(&nl, &l, &given, &HashMap::new());
            for (i, o) in m.outputs.iter().enumerate() {
                let got = port_value(&nl, &vals, &m.vars[*o].name);
                assert_eq!(
                    got, expect[i],
                    "seed {seed} input {x:#x} output {}\n{}",
                    m.vars[*o].name, m.text
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 1000, "only {checked} input vectors checked");
}
