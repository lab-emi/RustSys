// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! VERILOG-REQ-0100, VERILOG-REQ-0103: parsing of typical RTL SystemVerilog.

use rusteda_formats::verilog::ast::{Item, ModuleItem, ParamKind, PortList, TypeKind};
use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};

const PKG: &str = r#"
package demo_pkg;
  localparam int unsigned Width = 8;
  typedef enum logic [1:0] { Idle, Busy = 2'd2, Done } state_e;
  typedef struct packed {
    logic [Width-1:0] data;
    logic             valid;
  } beat_t;
  function automatic logic [Width-1:0] rev(input logic [Width-1:0] x);
    for (int i = 0; i < Width; i++) rev[i] = x[Width-1-i];
  endfunction
endpackage
"#;

// VERILOG-REQ-0002, VERILOG-REQ-0100
#[test]
fn case_and_ternary_colons_allow_adjacent_comments() {
    let src = r#"
module comment_boundary(input [1:0] s, output reg [1:0] y, output z);
always @* casez (s)
  2'b0?:/* first branch */ y = 2'b01;
  2'b10:// second branch
    y = 2'b10;
  default: y = 2'b11;
endcase
assign z = s[0] ? 1'b1:// ternary else
  1'b0;
endmodule
"#;
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(
        &mut sm,
        &[("comment_boundary.sv", src)],
        PpOptions::default(),
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(units.len(), 1);
}

const TOP: &str = r#"
`define FF(q, d, rst_val) \
  always_ff @(posedge clk_i or negedge rst_ni) \
    if (!rst_ni) q <= rst_val; else q <= d;

module top import demo_pkg::*; #(
  parameter int unsigned N = 4,
  parameter type T = beat_t,
  localparam int L = $clog2(N)
) (
  input  logic          clk_i,
  input  logic          rst_ni,
  input  T              in_i,
  output T  [N-1:0]     out_o,
  output logic [L-1:0]  idx_o
);
  state_e state_q, state_d;
  T [N-1:0] regs_q;
  logic [L:0] cnt;

  `FF(state_q, state_d, Idle)

  always_comb begin
    state_d = state_q;
    unique case (state_q)
      Idle: if (in_i.valid) state_d = Busy;
      Busy: state_d = Done;
      default: state_d = Idle;
    endcase
  end

  for (genvar g = 0; g < N; g++) begin : gen_regs
    if (g == 0) begin : gen_first
      `FF(regs_q[g], in_i, '0)
    end else begin : gen_rest
      `FF(regs_q[g], regs_q[g-1], '{default: '0})
    end
  end

  counter #(.W(L+1)) u_cnt (.clk(clk_i), .rst_n(rst_ni), .q(cnt), .en());
  assign out_o = regs_q;
  assign idx_o = cnt[L-1:0] ^ demo_pkg::rev(8'(cnt))[L-1:0];

  // pragma translate_off
  assert property (@(posedge clk_i) disable iff (!rst_ni) state_q != Done |-> ##1 state_q == Idle)
    else $error("bad");
  // pragma translate_on
endmodule

module counter #(parameter W = 4) (input clk, rst_n, output reg [W-1:0] q, input en);
  always @(posedge clk or negedge rst_n)
    if (!rst_n) q <= {W{1'b0}};
    else if (en) q <= q + 1'b1;
endmodule

module oldstyle (a, b, y);
  input [3:0] a, b;
  output y;
  wire [3:0] t = a & b;
  nand g1 (y, t[0], t[1]);
endmodule
"#;

#[test]
fn parses_package_and_modules() {
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(
        &mut sm,
        &[("pkg.sv", PKG), ("top.sv", TOP)],
        PpOptions::default(),
    );
    let msgs: Vec<_> = diags.iter().map(|d| d.render(&sm)).collect();
    assert!(diags.is_empty(), "{}", msgs.join("\n"));
    assert_eq!(units.len(), 2);
    let Item::Package(p) = &units[0].items[0] else {
        panic!()
    };
    assert_eq!(p.items.len(), 4);
    let mods: Vec<_> = units[1]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Module(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(mods.len(), 3);
    let top = mods[0];
    assert_eq!(top.imports.len(), 1);
    assert_eq!(top.params.len(), 3);
    assert!(matches!(top.params[1].kind, ParamKind::Type { .. }));
    assert!(top.params[2].local);
    let PortList::Ansi(ports) = &top.ports else {
        panic!()
    };
    assert_eq!(ports.len(), 5);
    assert!(matches!(ports[2].ty.kind, TypeKind::Named(_)));
    assert!(ports[2].net.is_some(), "typed input port is a net");
    assert!(ports[3].net.is_none(), "typed output port is a variable");
    assert!(top.items.iter().any(|i| matches!(i, ModuleItem::GenFor(_))));
    assert!(
        top.items
            .iter()
            .any(|i| matches!(i, ModuleItem::Instance(_)))
    );
    assert!(
        top.items
            .iter()
            .any(|i| matches!(i, ModuleItem::Ignored { .. }))
    );
    let counter = mods[1];
    let PortList::Ansi(cports) = &counter.ports else {
        panic!()
    };
    // `rst_n` inherits `input` and the implicit type from `clk`.
    assert_eq!(cports[1].name.name, "rst_n");
    assert_eq!(
        cports[1].dir,
        rusteda_formats::verilog::ast::Direction::Input
    );
    assert!(matches!(mods[2].ports, PortList::NonAnsi(ref v) if v.len() == 3));
    assert!(
        mods[2]
            .items
            .iter()
            .any(|i| matches!(i, ModuleItem::Gate(_)))
    );
}

#[test]
fn reports_syntax_errors_with_location() {
    let mut sm = SourceMap::new();
    let (_, diags) = read_texts(
        &mut sm,
        &[("bad.sv", "module m;\n  assign = 1;\nendmodule\n")],
        PpOptions::default(),
    );
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0]
            .render(&sm)
            .starts_with("error[V0100]: bad.sv:2:10:"),
        "{}",
        diags[0].render(&sm)
    );
}

#[test]
fn never_panics_on_truncated_input() {
    let full = TOP;
    for cut in (0..full.len()).step_by(7) {
        if !full.is_char_boundary(cut) {
            continue;
        }
        let mut sm = SourceMap::new();
        let _ = read_texts(
            &mut sm,
            &[("pkg.sv", PKG), ("t.sv", &full[..cut])],
            PpOptions::default(),
        );
    }
}

/// VERILOG-REQ-0100, VERILOG-REQ-0103: interfaces with modports and interface ports.
#[test]
fn parses_interfaces_and_modports() {
    let src = r#"
interface bus_if #(parameter int W = 8) (input logic clk);
  logic [W-1:0] data;
  logic valid, ready;
  modport src (output data, output valid, input ready, input clk),
          dst (input data, valid, clk, output ready);
  modport mon (input .d(data), import check);
endinterface : bus_if

module producer (bus_if.src b, interface.dst g, bus_if plain);
endmodule
"#;
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[("i.sv", src)], PpOptions::default());
    assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    let Item::Module(intf) = &units[0].items[0] else {
        panic!("interface expected")
    };
    let mps: Vec<_> = intf
        .items
        .iter()
        .filter_map(|i| match i {
            ModuleItem::Modport(v) => Some(v),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(mps.len(), 3);
    assert_eq!(mps[0].name.name, "src");
    let names: Vec<(String, String)> = mps[1]
        .ports
        .iter()
        .map(|(d, n)| (format!("{d:?}"), n.name.clone()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("Input".into(), "data".into()),
            ("Input".into(), "valid".into()),
            ("Input".into(), "clk".into()),
            ("Output".into(), "ready".into())
        ]
    );
    assert!(mps[1].unsupported.is_none());
    assert!(mps[2].unsupported.is_some() && mps[2].ports.is_empty());
    let Item::Module(m) = &units[0].items[1] else {
        panic!("module expected")
    };
    let PortList::Ansi(ps) = &m.ports else {
        panic!("ANSI ports expected")
    };
    let intf_of = |k: usize| {
        ps[k]
            .interface
            .as_ref()
            .map(|(i, mp)| (i.name.clone(), mp.as_ref().map(|x| x.name.clone())))
    };
    assert_eq!(intf_of(0), Some(("bus_if".into(), Some("src".into()))));
    assert_eq!(intf_of(1), Some(("interface".into(), Some("dst".into()))));
    // `bus_if plain` is syntactically a port of a named type; elaboration resolves it.
    assert_eq!(intf_of(2), None);
}
