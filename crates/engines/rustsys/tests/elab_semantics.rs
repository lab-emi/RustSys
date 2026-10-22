// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Elaboration semantics checked by exhaustive or seeded evaluation (SYNTH-REQ-01xx to -03xx).

mod common;

use common::{Cycle, Eval, design, elab};

fn sx(v: u64, w: u32) -> i64 {
    let m = 1u64 << (w - 1);
    ((v ^ m).wrapping_sub(m)) as i64
}

// SYNTH-REQ-0105: body parameters participate in legacy ordered overrides;
// localparams do not consume a position. <100 nodes, <2 MiB with cut budgets.
#[test]
fn ordered_legacy_body_parameters_skip_localparams() {
    let d = design(
        r#"
module leaf(a, y);
  parameter W = 1;
  localparam NOT_OVERRIDABLE = 7;
  parameter MASK = 0;
  input [W-1:0] a;
  output [W-1:0] y;
  assign y = a ^ MASK;
endmodule
module top(input [3:0] a, output [3:0] ordered, named);
  leaf #(4, 9) u_ordered(a, ordered);
  leaf #(.W(4), .MASK(6)) u_named(a, named);
endmodule
"#,
    );
    for a in 0..16 {
        let e = Eval::new(&d, &[("a", a)], &[]);
        assert_eq!(e.out("ordered"), a ^ 9);
        assert_eq!(e.out("named"), a ^ 6);
    }
}

// SYNTH-REQ-0202, SYNTH-REQ-0203, SYNTH-REQ-0204
#[test]
fn operators_and_sizing() {
    let d = design(
        r#"
module ops (input logic [3:0] a, b, input logic signed [3:0] sa, sb,
            output logic [4:0] sum, output logic [7:0] prod, output logic lt_u, lt_s,
            output logic [3:0] shr, sra_u, output logic signed [3:0] sra_s,
            output logic [4:0] mixed, output logic [3:0] quo, rem, output logic red, output logic [5:0] cat);
  assign sum = a + b;                 // context width 5 keeps the carry
  assign prod = a * b;
  assign lt_u = a < b;
  assign lt_s = sa < sb;
  assign shr = a >> b[1:0];
  assign sra_u = a >>> 1;             // unsigned operand: logical
  assign sra_s = sa >>> b[1:0];
  assign mixed = sa + b;              // signed and unsigned: unsigned, sa zero-extended
  assign quo = a / b;
  assign rem = a % b;
  assign red = ^a;
  assign cat = {a[1:0], 1'b1, b[2:0]};
endmodule"#,
    );
    for a in 0..16u64 {
        for b in 0..16u64 {
            let e = Eval::new(&d, &[("a", a), ("b", b), ("sa", a), ("sb", b)], &[]);
            assert_eq!(e.out("sum"), a + b);
            assert_eq!(e.out("prod"), a * b);
            assert_eq!(e.out("lt_u"), u64::from(a < b));
            assert_eq!(e.out("lt_s"), u64::from(sx(a, 4) < sx(b, 4)));
            assert_eq!(e.out("shr"), a >> (b & 3));
            assert_eq!(e.out("sra_u"), a >> 1);
            assert_eq!(e.out("sra_s"), ((sx(a, 4) >> (b & 3)) as u64) & 15);
            assert_eq!(e.out("mixed"), (a + b) & 31);
            assert_eq!(e.out("quo"), a.checked_div(b).unwrap_or(0));
            assert_eq!(e.out("rem"), a.checked_rem(b).unwrap_or(0));
            assert_eq!(e.out("red"), u64::from(a.count_ones() % 2));
            assert_eq!(e.out("cat"), ((a & 3) << 4) | 8 | (b & 7));
        }
    }
}

// SYNTH-REQ-0202: sign extension of signed operands in signed contexts.
#[test]
fn signed_extension_and_casts() {
    let d = design(
        r#"
module ext (input logic signed [3:0] s, input logic [3:0] u,
            output logic [7:0] y1, y2, y3, y4, output logic [7:0] y5);
  assign y1 = s;                 // sign-extended
  assign y2 = u;                 // zero-extended
  assign y3 = $unsigned(s);      // zero-extended
  assign y4 = $signed(u);        // sign-extended
  assign y5 = 8'(s) + 8'd0;      // size cast keeps signedness, then unsigned context
endmodule"#,
    );
    for v in 0..16u64 {
        let e = Eval::new(&d, &[("s", v), ("u", v)], &[]);
        let se = (sx(v, 4) as u64) & 255;
        assert_eq!(e.out("y1"), se);
        assert_eq!(e.out("y2"), v);
        assert_eq!(e.out("y3"), v);
        assert_eq!(e.out("y4"), se);
        assert_eq!(e.out("y5"), se);
    }
}

// SYNTH-REQ-0215, SYNTH-REQ-0201: case priority and casez wildcards.
#[test]
fn case_statements() {
    let d = design(
        r#"
module dec (input logic [3:0] x, output logic [2:0] y, output logic [1:0] z);
  always_comb begin
    y = 3'd7;
    casez (x)
      4'b1???: y = 3'd3;
      4'b01??: y = 3'd2;
      4'b001?: y = 3'd1;
      4'b0001: y = 3'd0;
    endcase
  end
  always_comb begin
    unique case (x[1:0])
      2'd0, 2'd3: z = 2'd1;
      2'd1: z = 2'd2;
      default: z = 2'd0;
    endcase
  end
endmodule"#,
    );
    for x in 0..16u64 {
        let e = Eval::new(&d, &[("x", x)], &[]);
        let want = if x == 0 {
            7
        } else {
            3 - u64::from(x.leading_zeros() - 60)
        };
        assert_eq!(e.out("y"), want, "x={x}");
        let zw = match x & 3 {
            0 | 3 => 1,
            1 => 2,
            _ => 0,
        };
        assert_eq!(e.out("z"), zw);
    }
    assert!(d.states.is_empty(), "no latches");
}

// SYNTH-REQ-0216: loops with a data-dependent break keep the loop variable exact.
#[test]
fn loop_with_break_priority_encoder() {
    let d = design(
        r#"
module penc (input logic [7:0] req, output logic [2:0] idx, output logic found, output logic [3:0] ones);
  always_comb begin
    idx = '0;
    found = 1'b0;
    for (int i = 0; i < 8; i++) begin
      if (req[i]) begin
        idx = i[2:0];
        found = 1'b1;
        break;
      end
    end
  end
  always_comb begin
    ones = '0;
    foreach (req[k]) if (req[k]) ones += 1;
  end
endmodule"#,
    );
    for r in 0..256u64 {
        let e = Eval::new(&d, &[("req", r)], &[]);
        assert_eq!(e.out("found"), u64::from(r != 0));
        assert_eq!(
            e.out("idx"),
            if r == 0 {
                0
            } else {
                u64::from(r.trailing_zeros())
            }
        );
        assert_eq!(e.out("ones"), u64::from(r.count_ones()));
    }
}

// SYNTH-REQ-0301 to SYNTH-REQ-0303, SYNTH-REQ-0603
#[test]
fn counter_with_async_reset_and_enable() {
    let d = design(
        r#"
module cnt #(parameter int W = 4) (input logic clk_i, rst_ni, en_i, output logic [W-1:0] q_o);
  logic [W-1:0] q_q;
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) q_q <= '0;
    else if (en_i) q_q <= q_q + 1'b1;
  end
  assign q_o = q_q;
endmodule"#,
    );
    assert_eq!(d.states.len(), 4);
    for k in 0..4 {
        let s = d.state(&format!("q_q[{k}]")).unwrap();
        let (_, v) = s.arst.expect("asynchronous reset");
        assert!(!v);
    }
    let mut c = Cycle::new(&d);
    c.step(&[("rst_ni", 0)]);
    assert_eq!(c.read("q_q", 4), 0);
    for i in 0..20u64 {
        let en = u64::from(i % 3 != 0);
        let before = c.read("q_q", 4);
        let o = c.step(&[("rst_ni", 1), ("en_i", en)]);
        assert_eq!(o["q_o"], before);
        assert_eq!(c.read("q_q", 4), (before + en) & 15);
    }
}

// SYNTH-REQ-0110, SYNTH-REQ-0207, SYNTH-REQ-0208: packages, enums, structs.
#[test]
fn packages_structs_and_enums() {
    let pkg = r#"
package p;
  typedef enum logic [1:0] { A = 2'd1, B, C = 2'd0 } e_t;
  typedef struct packed { logic [3:0] hi; e_t kind; logic lo; } s_t;
  localparam int K = 3;
  function automatic logic [3:0] inc(logic [3:0] v); return v + K[3:0]; endfunction
endpackage"#;
    let top = r#"
module m import p::*; (input s_t s, output logic [3:0] h, output e_t k, output logic isb, output s_t t);
  assign h = inc(s.hi);
  assign k = s.kind;
  assign isb = (s.kind == B);
  always_comb begin
    t = '{hi: s.hi ^ 4'hF, kind: C, lo: ~s.lo};
  end
endmodule"#;
    let d = elab(&[pkg, top], None).unwrap();
    for v in 0..128u64 {
        let e = Eval::new(&d, &[("s", v)], &[]);
        let hi = (v >> 3) & 15;
        let kind = (v >> 1) & 3;
        let lo = v & 1;
        assert_eq!(e.out("h"), (hi + 3) & 15);
        assert_eq!(e.out("k"), kind);
        assert_eq!(e.out("isb"), u64::from(kind == 2));
        assert_eq!(e.out("t"), ((hi ^ 15) << 3) | (1 - lo));
    }
}

// SYNTH-REQ-0123, SYNTH-REQ-0140: generate loops, instances and hierarchical state names.
#[test]
fn generate_and_hierarchy() {
    let d = design(
        r#"
module dff (input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module shreg #(parameter int N = 3) (input logic clk, input logic din, output logic dout);
  logic [N:0] c;
  assign c[0] = din;
  for (genvar i = 0; i < N; i++) begin : g_stage
    dff u_ff (.clk, .d(c[i]), .q(c[i+1]));
  end
  assign dout = c[N];
endmodule"#,
    );
    let mut names: Vec<_> = d.states.iter().map(|s| s.name.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "g_stage[0].u_ff.q",
            "g_stage[1].u_ff.q",
            "g_stage[2].u_ff.q"
        ]
    );
    let mut c = Cycle::new(&d);
    let seq = [1u64, 0, 1, 1, 0, 0, 1];
    let mut outs = Vec::new();
    for s in seq {
        outs.push(c.step(&[("din", s)])["dout"]);
    }
    assert_eq!(outs[3..], seq[..4]);
}

// SYNTH-REQ-0305
#[test]
fn latch_inference() {
    let d = design(
        r#"
module lat (input logic en, input logic [1:0] d, output logic [1:0] q);
  always_latch if (en) q = d;
endmodule"#,
    );
    assert_eq!(d.states.len(), 2);
    assert!(
        d.states
            .iter()
            .all(|s| s.kind == rustsys::elab::StateKind::Latch)
    );
    let e = Eval::new(&d, &[("en", 1), ("d", 2)], &[]);
    assert!(e.lit(d.states[0].clk));
}

// SYNTH-REQ-0210, SYNTH-REQ-0214, SYNTH-REQ-0302, SYNTH-REQ-0303: rejected designs.
#[test]
fn rejected_designs() {
    let two_drivers = "module m(input a, b, output y); assign y = a; assign y = b; endmodule";
    assert!(elab(&[two_drivers], None).unwrap_err().contains("S0201"));
    let comb_loop =
        "module m(input a, output y); logic t; assign t = a & y; assign y = t; endmodule";
    assert!(elab(&[comb_loop], None).unwrap_err().contains("S0214"));
    let two_clocks = "module m(input c1, c2, d, output logic q); always_ff @(posedge c1 or posedge c2) q <= d; endmodule";
    assert!(elab(&[two_clocks], None).unwrap_err().contains("S0301"));
    let async_load = "module m(input c, r, d, e, output logic q); always_ff @(posedge c or posedge r) if (r) q <= e; else q <= d; endmodule";
    assert!(elab(&[async_load], None).unwrap_err().contains("S0303"));
    let nonconst_loop = "module m(input [3:0] n, output logic [3:0] y); always_comb begin y = 0; for (int i = 0; i < n; i++) y++; end endmodule";
    assert!(elab(&[nonconst_loop], None).unwrap_err().contains("S0228"));
}

// SYNTH-REQ-0132, SYNTH-REQ-0216: `return` and `break` inside branches take effect only on the
// paths that execute them.
#[test]
fn conditional_returns_and_breaks() {
    let d = design(
        r#"
module ret (input logic [1:0] op, input logic [3:0] a, b, input logic [3:0] m, output logic [3:0] y, output logic [2:0] first, output logic [3:0] z);
  function automatic logic [3:0] f(logic [1:0] o, logic [3:0] x, logic [3:0] w);
    case (o)
      2'd0: return x + w;
      2'd1: return x - w;
      2'd2: begin
        if (x > w) return x;
        return w;
      end
      default: return x ^ w;
    endcase
  endfunction
  function automatic logic [2:0] find(logic [3:0] v, logic [3:0] mask);
    for (int i = 0; i < 4; i++) begin
      if (mask[i]) begin
        if (v[i]) return i[2:0];
      end
    end
    return 3'd7;
  endfunction
  assign y = f(op, a, b);
  assign first = find(a, m);
  always_comb begin
    z = 4'd0;
    for (int i = 0; i < 4; i++) begin
      if (m[i]) begin
        if (b[i]) begin
          z = 4'(i + 8);
          break;
        end
        z = z + 1'b1;
      end
    end
  end
endmodule"#,
    );
    for op in 0..4u64 {
        for a in 0..16u64 {
            for b in 0..16u64 {
                for m in [0u64, 5, 10, 15, 6] {
                    let e = Eval::new(&d, &[("op", op), ("a", a), ("b", b), ("m", m)], &[]);
                    let want = match op {
                        0 => (a + b) & 15,
                        1 => a.wrapping_sub(b) & 15,
                        2 => a.max(b),
                        _ => a ^ b,
                    };
                    assert_eq!(e.out("y"), want, "op {op} a {a} b {b}");
                    let f = (0..4)
                        .find(|i| (m >> i) & 1 == 1 && (a >> i) & 1 == 1)
                        .map_or(7, |i| i);
                    assert_eq!(e.out("first"), f);
                    let mut z = 0u64;
                    for i in 0..4 {
                        if (m >> i) & 1 == 1 {
                            if (b >> i) & 1 == 1 {
                                z = i + 8;
                                break;
                            }
                            z += 1;
                        }
                    }
                    assert_eq!(e.out("z"), z & 15, "m {m} b {b}");
                }
            }
        }
    }
}

// SYNTH-REQ-0206: dynamic indexing, part-selects and writes.
#[test]
fn dynamic_selects_and_writes() {
    let d = design(
        r#"
module sel (input logic [7:0] v, input logic [2:0] i, input logic [3:0] j, input logic b,
            output logic bit_o, output logic [3:0] up_o, output logic [7:0] w_o, output logic [3:0] rd_o);
  logic [3:0] mem [0:5];
  assign bit_o = v[i];
  assign up_o = v[i +: 4];
  always_comb begin
    w_o = v;
    w_o[i] = b;
  end
  always_comb begin
    for (int k = 0; k < 6; k++) mem[k] = k[3:0] + v[3:0];
  end
  assign rd_o = mem[j];
endmodule"#,
    );
    for v in (0..256u64).step_by(7) {
        for i in 0..8u64 {
            for j in 0..8u64 {
                let b = (v ^ i ^ j) & 1;
                let e = Eval::new(&d, &[("v", v), ("i", i), ("j", j), ("b", b)], &[]);
                assert_eq!(e.out("bit_o"), (v >> i) & 1);
                let up = (0..4).fold(0u64, |a, k| {
                    if i + k < 8 {
                        a | (((v >> (i + k)) & 1) << k)
                    } else {
                        a
                    }
                });
                assert_eq!(e.out("up_o"), up);
                assert_eq!(e.out("w_o"), (v & !(1 << i)) | (b << i));
                assert_eq!(e.out("rd_o"), if j < 6 { (j + (v & 15)) & 15 } else { 0 });
            }
        }
    }
}

// SYNTH-REQ-0223: a `default` key descends into structures and arrays of non-bit elements, and
// sets an element whose type is the default value's own type as a whole.
#[test]
fn pattern_default_descends_into_aggregates() {
    let pkg = r#"
package q;
  typedef logic [0:2][7:0] row_t;
  typedef row_t [0:1] tab_t;
  typedef struct packed { logic [3:0] a; logic [1:0][1:0] b; } s_t;
  typedef struct packed { s_t s; logic [2:0] c; } outer_t;
  localparam tab_t T = '{default: 1};
  localparam s_t S = '{a: 4'h3, default: '0};
endpackage"#;
    let top = r#"
module m import q::*; (input logic [1:0] i, output tab_t t, output outer_t o,
                       output s_t [1:0] arr, output logic [1:0][7:0] w, output logic [3:0] z);
  logic [3:0] un [2][2];
  assign t = T;
  assign o = '{default: 1};
  assign arr = '{default: S};
  assign w = '{default: 1};
  assign un = '{default: 2};
  assign z = un[i[1]][i[0]] + 4'd1;
endmodule"#;
    let d = elab(&[pkg, top], None).unwrap();
    for i in 0..4u64 {
        let e = Eval::new(&d, &[("i", i)], &[]);
        assert_eq!(e.out("t"), 0x0101_0101_0101);
        assert_eq!(e.out("o"), (1 << 7) | (0b0101 << 3) | 1);
        assert_eq!(e.out("arr"), 0x3030);
        assert_eq!(e.out("w"), 0x0101);
        assert_eq!(e.out("z"), 3);
    }
}
