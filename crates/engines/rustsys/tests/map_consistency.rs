// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Internal consistency of mapping and emission: the emitted netlist evaluated cell by cell
//! computes the same outputs as the optimized graph (SYNTH-REQ-0511, SYNTH-REQ-0601 to -0606).

use std::collections::HashMap;

use rusteda_formats::liberty::Library;
use rusteda_formats::verilog::write::NlRef;
use rustsys::{Goal, Sources, SynthOptions, synthesize};
use rustsys_fixtures::liberty::{Variant, library};

fn lib(v: Variant) -> Library {
    rusteda_formats::liberty::read_text(&library(v), v.name()).unwrap()
}

/// Evaluates a netlist combinationally: port inputs and state wires are given.
fn eval_netlist(
    nl: &rusteda_formats::verilog::write::NlModule,
    l: &Library,
    given: &HashMap<String, bool>,
) -> HashMap<String, bool> {
    let key = |r: &NlRef| match r {
        NlRef::Net(n) => n.clone(),
        NlRef::Bit(n, k) => format!("{n}[{k}]"),
        NlRef::Const(v) => format!("#{}", u8::from(*v)),
    };
    let mut val: HashMap<String, bool> = given.clone();
    val.insert("#0".into(), false);
    val.insert("#1".into(), true);
    let mut changed = true;
    let mut rounds = 0;
    while changed {
        changed = false;
        rounds += 1;
        assert!(rounds < 10_000, "no fixpoint");
        for inst in &nl.instances {
            let cell = l.cells.iter().find(|c| c.name == inst.cell).unwrap();
            if !cell.sequentials.is_empty() {
                continue;
            }
            for o in cell.outputs() {
                let Some((_, r)) = inst.conns.iter().find(|(p, _)| *p == o.name) else {
                    continue;
                };
                let k = key(r);
                if val.contains_key(&k) {
                    continue;
                }
                let f = o.function.as_ref().unwrap();
                let ins: Option<HashMap<String, bool>> = inst
                    .conns
                    .iter()
                    .filter(|(p, _)| *p != o.name)
                    .map(|(p, r)| val.get(&key(r)).map(|v| (p.clone(), *v)))
                    .collect();
                if let Some(ins) = ins {
                    let v = f.eval(&|n| ins.get(n).copied()).unwrap();
                    val.insert(k, v);
                    changed = true;
                }
            }
        }
        for (lhs, rhs) in &nl.assigns {
            let k = key(lhs);
            if !val.contains_key(&k) {
                if let Some(v) = val.get(&key(rhs)).copied() {
                    val.insert(k, v);
                    changed = true;
                }
            }
        }
    }
    val
}

const DESIGNS: &[(&str, &str)] = &[
    (
        "alu",
        r#"
module alu (input logic [3:0] a, b, input logic [1:0] op, output logic [3:0] y, output logic z);
  always_comb begin
    unique case (op)
      2'd0: y = a + b;
      2'd1: y = a - b;
      2'd2: y = a & b;
      default: y = a ^ ~b;
    endcase
  end
  assign z = (y == 4'd0);
endmodule"#,
    ),
    (
        "mux4",
        r#"
module mux4 (input logic [3:0] d, input logic [1:0] s, input logic en, output logic y, output logic c1, output logic c0);
  assign y = en ? d[s] : 1'b0;
  assign c1 = 1'b1;
  assign c0 = 1'b0;
endmodule"#,
    ),
    (
        "cmp",
        r#"
module cmp (input logic signed [4:0] a, b, output logic lt, ge, eq, output logic [4:0] mx);
  assign lt = a < b;
  assign ge = a >= b;
  assign eq = a == b;
  assign mx = (a > b) ? a : b;
endmodule"#,
    ),
];

// SYNTH-REQ-0511: every library variant yields a netlist equal to the graph on all inputs.
#[test]
fn netlists_match_graph_on_all_inputs() {
    for v in Variant::ALL {
        let l = lib(v);
        for goal in [Goal::Delay, Goal::Area] {
            for (name, src) in DESIGNS {
                let opts = SynthOptions {
                    goal,
                    ..SynthOptions::default()
                };
                let out = synthesize(
                    &Sources::Texts(vec![(format!("{name}.sv"), src.to_string())]),
                    std::slice::from_ref(&l),
                    &opts,
                )
                .unwrap_or_else(|e| panic!("{name} {v:?}: {:?}", e.messages));
                // Enumerate all inputs of the (small) designs.
                let ports: Vec<_> = out
                    .netlist
                    .ports
                    .iter()
                    .filter(|p| p.dir == rusteda_formats::verilog::write::NlDir::Input)
                    .cloned()
                    .collect();
                let bits: usize = ports.iter().map(|p| p.width.unwrap_or(1) as usize).sum();
                assert!(bits <= 12);
                // Evaluate the RTL semantics independently with Rust closures for each design.
                for m in 0..(1u64 << bits) {
                    let mut given = HashMap::new();
                    let mut off = 0;
                    let mut vals = HashMap::new();
                    for p in &ports {
                        let w = p.width.unwrap_or(1) as usize;
                        let x = (m >> off) & ((1 << w) - 1);
                        vals.insert(p.name.clone(), x);
                        for k in 0..w {
                            let key = if p.width.is_none() {
                                p.name.clone()
                            } else {
                                format!("{}[{k}]", p.name)
                            };
                            given.insert(key, (x >> k) & 1 == 1);
                        }
                        off += w;
                    }
                    let r = eval_netlist(&out.netlist, &l, &given);
                    let get = |n: &str, w: usize| -> u64 {
                        (0..w).fold(0, |a, k| {
                            let key = if w == 1 && !r.contains_key(&format!("{n}[0]")) {
                                n.to_string()
                            } else {
                                format!("{n}[{k}]")
                            };
                            a | (u64::from(r[&key]) << k)
                        })
                    };
                    match *name {
                        "alu" => {
                            let (a, b, op) = (vals["a"], vals["b"], vals["op"]);
                            let y = match op {
                                0 => (a + b) & 15,
                                1 => a.wrapping_sub(b) & 15,
                                2 => a & b,
                                _ => (a ^ !b) & 15,
                            };
                            assert_eq!(get("y", 4), y, "{v:?} alu a={a} b={b} op={op}");
                            assert_eq!(get("z", 1), u64::from(y == 0));
                        }
                        "mux4" => {
                            let (d, s, en) = (vals["d"], vals["s"], vals["en"]);
                            assert_eq!(get("y", 1), if en == 1 { (d >> s) & 1 } else { 0 });
                            assert_eq!(get("c1", 1), 1);
                            assert_eq!(get("c0", 1), 0);
                        }
                        "cmp" => {
                            let sx = |x: u64| if x & 16 != 0 { x as i64 - 32 } else { x as i64 };
                            let (a, b) = (sx(vals["a"]), sx(vals["b"]));
                            assert_eq!(get("lt", 1), u64::from(a < b));
                            assert_eq!(get("ge", 1), u64::from(a >= b));
                            assert_eq!(get("eq", 1), u64::from(a == b));
                            assert_eq!(get("mx", 5), (a.max(b) as u64) & 31);
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
    }
}

// SYNTH-REQ-0501: dont_use/multi-output cells and explicitly excluded sequential
// cells are never used, even when their Boolean model is supported.
#[test]
fn excluded_cells_are_not_used() {
    let l = lib(Variant::Rich);
    let src = "module m(input logic clk, a, b, c, output logic s, co, output logic q); assign s = a ^ b ^ c; assign co = (a & b) | (c & (a ^ b)); always_ff @(posedge clk) q <= s; endmodule";
    let out = synthesize(
        &Sources::Texts(vec![("m.sv".into(), src.into())]),
        &[l],
        &SynthOptions {
            dont_use: vec!["wt_sdff_x1".into(), "wt_edff_x1".into()],
            ..Default::default()
        },
    )
    .unwrap();
    for bad in [
        "wt_nand2_x0",
        "wt_fa_x1",
        "wt_sdff_x1",
        "wt_edff_x1",
        "wt_fill_x1",
    ] {
        assert!(!out.report.cells.contains_key(bad), "{bad} used");
    }
    assert_eq!(out.report.sequential_cells, 1);
}
