// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// CHECK-REQ-0042: every Boolean circuit operator used by obligation export.
module aiger_ops(input a, b, s, output [5:0] y);
  assign y = {((a ^ b) & s) | ~(a | b), s ? a : b, a ^ b, a | b, a & b, ~a};
endmodule
