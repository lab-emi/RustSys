// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// SYNTH-REQ-0105, VERILOG-REQ-0002. Synthetic black-box differential fixture.
module legacy_leaf(a, y);
  parameter W = 1;
  localparam SKIP_ORDERED = 7;
  parameter MASK = 0;
  input [W-1:0] a;
  output [W-1:0] y;
  assign y = a ^ MASK;
endmodule

module legacy_params_comments(input [3:0] a, input [1:0] s,
                              output [3:0] ordered, named,
                              output reg [1:0] branch, output ternary);
  legacy_leaf #(4, 9) u_ordered(a, ordered);
  legacy_leaf #(.W(4), .MASK(6)) u_named(a, named);
  always @* casez (s)
    2'b0?:/* adjacent block comment */ branch = 2'b01;
    2'b10:// adjacent line comment
      branch = 2'b10;
    default: branch = 2'b11;
  endcase
  assign ternary = s[0] ? 1'b1:// ternary else
    1'b0;
endmodule
