// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
module fpga_probe(input clk, rst_n, set_n, gate, enable, input [5:0] a,
                  input [7:0] data, input rs_n1, output [15:0] y);
  reg q, p, r, l, s;
  always @(posedge clk or negedge rst_n) if (!rst_n) q <= 0; else q <= data[0];
  always @(negedge clk or negedge set_n) if (!set_n) p <= 1; else p <= data[1];
  always @(posedge clk) if (enable) r <= data[2];
  always @* if (!rst_n) l = 0; else if (gate) l = data[3];
  always @* if (!set_n) s = 1; else if (gate) s = data[4];
  assign y = {q,p,r,l,s, ((a[0]^a[1]) & (a[2]|a[3])) ^ (a[4]&a[5]),
              ~rs_n1, 1'b1, data + 8'd37};
endmodule
