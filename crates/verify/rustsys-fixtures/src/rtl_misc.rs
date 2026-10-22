// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Corpus designs (continued): latches, signed and saturating arithmetic, decoders,
//! subroutines, memories, handshakes, old-style ports and hierarchy.

pub(crate) const LATCHES: &str = r#"
module latches (input logic en, input logic en_n, input logic [3:0] d, input logic sel, output logic [3:0] q, output logic [1:0] r);
  always_latch begin
    if (en) q <= d;
  end
  always @* begin
    if (!en_n) r = d[1:0];
  end
endmodule
"#;

pub(crate) const SET_NEG: &str = r#"
module set_neg (input logic clk, input logic set, input logic rst_n, input logic [1:0] d, output logic [1:0] q, output logic [1:0] qn);
  always_ff @(posedge clk or posedge set)
    if (set) q <= 2'b11;
    else     q <= d;
  always @(negedge clk or negedge rst_n)
    if (~rst_n) qn <= 2'b01;
    else        qn <= d ^ q;
endmodule
"#;

pub(crate) const DIV_CONST: &str = r#"
module div_const (input logic [9:0] x, input logic signed [7:0] s, output logic [9:0] q3, output logic [2:0] r5, output logic signed [7:0] sq, output logic signed [7:0] sr);
  assign q3 = x / 10'd3;
  assign r5 = x % 10'd5;
  assign sq = s / 8'sd3;
  assign sr = s % 8'sd3;
endmodule
"#;

pub(crate) const SAT_ADD: &str = r#"
module sat_add (input logic signed [7:0] a, input logic signed [7:0] b, output logic signed [7:0] y, output logic ovf);
  logic signed [8:0] s;
  always_comb begin
    s = 9'(a) + 9'(b);
    ovf = (s > 9'sd127) || (s < -9'sd128);
    if (s > 9'sd127) y = 8'sd127;
    else if (s < -9'sd128) y = -8'sd128;
    else y = s[7:0];
  end
endmodule
"#;

pub(crate) const DECODER: &str = r#"
module decoder (input logic [7:0] opc, output logic [2:0] cls, output logic is_mem, output logic [7:0] onehot);
  always_comb begin
    cls = 3'd0;
    case (opc) inside
      [8'h00:8'h0F]: cls = 3'd1;
      8'h1?:         cls = 3'd2;
      8'b001?_??01:  cls = 3'd3;
      [8'h80:8'hBF], 8'hFF: cls = 3'd4;
      default: cls = 3'd0;
    endcase
  end
  always_comb begin
    casex (opc[7:5])
      3'b1x1: is_mem = 1'b1;
      3'b01x: is_mem = 1'b1;
      default: is_mem = 1'b0;
    endcase
  end
  assign onehot = 8'b1 << opc[2:0];
endmodule
"#;

pub(crate) const SUBROUTINES: &str = r#"
module subroutines (input logic [7:0] a, input logic [7:0] b, output logic [7:0] mx, output logic [7:0] mn, output logic [3:0] ones);
  task automatic minmax(input logic [7:0] x, input logic [7:0] y, output logic [7:0] hi, output logic [7:0] lo);
    if (x > y) begin hi = x; lo = y; end
    else begin hi = y; lo = x; end
  endtask
  function automatic int count_ones(logic [7:0] v);
    int n;
    n = 0;
    foreach (v[i]) n += v[i];
    return n;
  endfunction
  always_comb begin
    minmax(a, b, mx, mn);
    ones = count_ones(a ^ b);
  end
endmodule
"#;

pub(crate) const MEM2D: &str = r#"
module mem2d (
  input  logic       clk,
  input  logic       we,
  input  logic [1:0] row,
  input  logic [1:0] col,
  input  logic [3:0] wdata,
  output logic [3:0] rdata,
  output logic [15:0] row_bits
);
  logic [3:0] m [4][4];
  always_ff @(posedge clk) if (we) m[row][col] <= wdata;
  assign rdata = m[row][col];
  assign row_bits = {m[row][3], m[row][2], m[row][1], m[row][0]};
endmodule
"#;

pub(crate) const SKID: &str = r#"
module skid #(parameter int W = 4) (
  input  logic         clk,
  input  logic         rst_n,
  input  logic         in_valid,
  input  logic [W-1:0] in_data,
  output logic         in_ready,
  output logic         out_valid,
  input  logic         out_ready,
  output logic [W-1:0] out_data
);
  logic full_q;
  logic [W-1:0] buf_q, data_q;
  logic valid_q;
  assign in_ready = !full_q;
  assign out_valid = valid_q;
  assign out_data = data_q;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      full_q <= 1'b0;
      valid_q <= 1'b0;
    end else begin
      if (out_ready || !valid_q) begin
        valid_q <= in_valid || full_q;
        full_q <= 1'b0;
      end else if (in_valid && !full_q) begin
        full_q <= 1'b1;
      end
    end
  end
  always_ff @(posedge clk) begin
    if (out_ready || !valid_q) data_q <= full_q ? buf_q : in_data;
    if (in_valid && !full_q && !(out_ready || !valid_q)) buf_q <= in_data;
  end
endmodule
"#;

pub(crate) const OLD_STYLE: &str = r#"
module old_style (clk, reset, a, b, sel, y, q);
  input clk, reset;
  input [3:0] a, b;
  input sel;
  output [3:0] y;
  output reg [3:0] q;
  wire [3:0] t = sel ? a : b;
  assign y = t;
  and g0 (w0, a[0], b[0]);
  xor g1 (w1, w0, sel);
  always @(posedge clk)
    if (reset) q <= 4'b0000;
    else q <= {q[2:0], w1};
endmodule
"#;

pub(crate) const TOP_HIER: &str = r#"
module top_hier (
  input  logic       clk_i,
  input  logic       rst_ni,
  input  logic       en_i,
  input  logic [7:0] data_i,
  output logic [7:0] cnt_o,
  output logic [7:0] crc_o,
  output logic       any_o
);
  counter #(.W(8)) u_cnt (.clk_i, .rst_ni, .en_i, .clr_i(1'b0), .cnt_o, .wrap_o());
  crc8 u_crc (.clk(clk_i), .rst_n(rst_ni), .valid(en_i), .data(data_i), .crc(crc_o));
  if (1) begin : g_or
    assign any_o = |cnt_o;
  end
endmodule
"#;
