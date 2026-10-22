// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// CHECK-REQ-0010, SYNTH-REQ-0217. Public loop counters keep their exit value.
module module_loop_counters(input en, input [3:0] a, output reg [7:0] y);
  integer i;
  always @* begin
    i = 9;
    y = 0;
    if (en) begin
      for (i = 0; i < 4; i = i + 1) y[i] = a[i];
    end
    y[7:4] = i;
  end
endmodule
