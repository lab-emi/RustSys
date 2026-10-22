// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// SYNTH-REQ-0201: wildcard bits survive concatenation and replication.
module wildcard_structures(input [7:0] s, output reg [3:0] y, output eq);
  always @* begin
    y = 0;
    casez (s[5:0])
      {1'b0, 1'b1, 2'b00, 2'b??}: y[0] = 1;
      default: y[0] = 0;
    endcase
    casez (s[3:0])
      {2{2'b?1}}: y[1] = 1;
      default: y[1] = 0;
    endcase
    casex (s[3:0])
      {2'b1x, 2'bz0}: y[2] = 1;
      default: y[2] = 0;
    endcase
    casez (s[5:0])
      {{2'b0?, 2'b?1}, 2'b?0}: y[3] = 1;
      default: y[3] = 0;
    endcase
  end
  assign eq = s[3:0] ==? {2'b??, 2'b01};
endmodule
