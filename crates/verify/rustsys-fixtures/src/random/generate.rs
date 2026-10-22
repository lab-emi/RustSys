// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Generators of random combinational and sequential designs.

use super::*;

/// Generates a random combinational module. `max_input_bits` bounds the total input width.
pub fn random_comb(seed: u64, max_input_bits: u32) -> RandomComb {
    let mut r = Rng(seed);
    let mut vars = Vec::new();
    let mut bits = 0;
    let n_in = 2 + r.below(3) as usize;
    for i in 0..n_in {
        let rest = max_input_bits.saturating_sub(bits).max(1);
        let w = (1 + r.below(u64::from(rest.min(6)))) as u32;
        bits += w;
        vars.push(Var {
            name: format!("i{i}"),
            width: w,
            signed: r.coin(40),
        });
    }
    let n_loc = 2 + r.below(3) as usize;
    for i in 0..n_loc {
        let w = 1 + r.below(9) as u32;
        vars.push(Var {
            name: format!("o{i}"),
            width: w,
            signed: r.coin(40),
        });
    }
    let targets: Vec<usize> = (n_in..n_in + n_loc).collect();
    // Functions with conditional returns over two parameters.
    let mut funcs = Vec::new();
    let n_funcs = r.below(3) as usize;
    for k in 0..n_funcs {
        let pv = |r: &mut Rng, name: &str| Var {
            name: name.to_string(),
            width: 1 + r.below(6) as u32,
            signed: r.coin(40),
        };
        let params = [pv(&mut r, "a"), pv(&mut r, "b")];
        let ret = pv(&mut r, "ret");
        let mut fg = Gen {
            r: &mut r,
            vars: params.to_vec(),
            funcs: Vec::new(),
        };
        let cond = fg.expr(2, 2);
        let first = fg.expr(2, 2);
        let sel = fg.expr(1, 2);
        let mut items = Vec::new();
        for _ in 0..1 + fg.r.below(3) {
            let v = u128::from(fg.r.below(6));
            if !items.iter().any(|(x, _): &(u128, E)| *x == v) {
                items.push((v, fg.expr(2, 2)));
            }
        }
        let last = fg.expr(2, 2);
        funcs.push(Func {
            name: format!("f{k}"),
            params,
            ret,
            cond,
            first,
            sel,
            items,
            last,
        });
    }
    let sigs: Vec<[(u32, bool); 2]> = funcs
        .iter()
        .map(|f| {
            [
                (f.params[0].width, f.params[0].signed),
                (f.params[1].width, f.params[1].signed),
            ]
        })
        .collect();
    let mut g = Gen {
        r: &mut r,
        vars: vars.clone(),
        funcs: sigs,
    };
    // Every output starts with a defined value, then random statements may refine it.
    let mut body: Vec<Stmt> = targets
        .iter()
        .map(|t| Stmt::Assign(*t, g.expr(2, n_in)))
        .collect();
    let n_stmts = 3 + g.r.below(3) as usize;
    body.extend(g.stmts(2, &targets, n_in + n_loc, n_stmts));
    let mut m = RandomComb {
        name: format!("rnd_{seed}"),
        text: String::new(),
        vars,
        inputs: n_in,
        outputs: targets.clone(),
        body,
        funcs,
    };
    let mut t = String::new();
    let _ = writeln!(t, "module {} (", m.name);
    let mut ports = Vec::new();
    for v in &m.vars[..n_in] {
        ports.push(format!(
            "  input  logic{} [{}:0] {}",
            if v.signed { " signed" } else { "" },
            v.width - 1,
            v.name
        ));
    }
    for v in &m.vars[n_in..] {
        ports.push(format!(
            "  output logic{} [{}:0] {}",
            if v.signed { " signed" } else { "" },
            v.width - 1,
            v.name
        ));
    }
    let _ = writeln!(t, "{}\n);", ports.join(",\n"));
    for f in &m.funcs {
        t.push_str(&m.func_text(f));
    }
    let _ = writeln!(t, "  always_comb begin");
    let mut body_text = String::new();
    m.show_stmts(&m.body, 4, &mut body_text);
    t.push_str(&body_text);
    let _ = writeln!(t, "  end\nendmodule");
    m.text = t;
    m
}

/// A random sequential module (no evaluator; checked by equivalence only).
pub fn random_seq(seed: u64) -> String {
    let mut r = Rng(seed ^ 0xABCD);
    let comb = random_comb(seed, 12);
    let mut t = comb.text.clone();
    // Wrap: registers capture the combinational outputs, with random reset styles.
    let _ = writeln!(
        t,
        "module seq_{seed} (input logic clk, input logic rst_n, input logic set, input logic en,"
    );
    let ins: Vec<String> = comb.vars[..comb.inputs]
        .iter()
        .map(|v| {
            format!(
                "  input logic{} [{}:0] {}",
                if v.signed { " signed" } else { "" },
                v.width - 1,
                v.name
            )
        })
        .collect();
    let _ = writeln!(t, "{},", ins.join(",\n"));
    let outs: Vec<String> = comb.vars[comb.inputs..]
        .iter()
        .map(|v| format!("  output logic [{}:0] q_{}", v.width - 1, v.name))
        .collect();
    let _ = writeln!(t, "{}\n);", outs.join(",\n"));
    for v in &comb.vars[comb.inputs..] {
        let _ = writeln!(
            t,
            "  logic{} [{}:0] {};",
            if v.signed { " signed" } else { "" },
            v.width - 1,
            v.name
        );
    }
    let conns: Vec<String> = comb
        .vars
        .iter()
        .map(|v| format!(".{0}({0})", v.name))
        .collect();
    let _ = writeln!(t, "  {} u_comb ({});", comb.name, conns.join(", "));
    for v in &comb.vars[comb.inputs..] {
        let style = r.below(4);
        let rv = r.next() & ((1u64 << v.width) - 1);
        let q = format!("q_{}", v.name);
        match style {
            0 => {
                let _ = writeln!(
                    t,
                    "  always_ff @(posedge clk or negedge rst_n) if (!rst_n) {q} <= {}'h{rv:x}; else if (en) {q} <= {};",
                    v.width, v.name
                );
            }
            1 => {
                let _ = writeln!(
                    t,
                    "  always_ff @(posedge clk) if (!rst_n) {q} <= '0; else {q} <= {q} ^ {};",
                    v.name
                );
            }
            2 => {
                let _ = writeln!(
                    t,
                    "  always_ff @(negedge clk or posedge set) if (set) {q} <= '1; else {q} <= {} + {q};",
                    v.name
                );
            }
            _ => {
                let _ = writeln!(t, "  always_latch if (en) {q} = {};", v.name);
            }
        }
    }
    let _ = writeln!(t, "endmodule");
    t
}
