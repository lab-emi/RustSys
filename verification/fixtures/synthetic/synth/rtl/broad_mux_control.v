// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// SYNTH-REQ-0414: synthetic shared selectors with arbitrary stored state.
module broad_mux_control(input clk, en, s, t,
                         input [31:0] a, b, c,
                         output [31:0] direct, negated,
                         output reg [31:0] q);
  wire [31:0] f = s ? (t ? a : b) : c;
  assign direct = f;
  assign negated = ~f;
  always @(posedge clk) if (en) q <= f;
endmodule
