// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// The caller must prove both directions against the pinned Liberty cell.
// No initial value or reset assumption is introduced.
module LATQX1(input D, CK, output reg Q);
  always @* if (CK) Q = D;
endmodule
