// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// CHECK-REQ-0040, CHECK-REQ-0041: all event/control/data obligations are necessary.
module event_cec(input clk, rn, wr, input [3:0] a, output reg [3:0] q);
  always @(posedge clk or negedge rn)
    if (!rn) q <= 4'b1010;
    else if (wr) q <= a;
endmodule
