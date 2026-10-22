// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// Logical latch models derived from the pinned Liberty pin functions.
// The LEC driver must prove each model against read_liberty before using it.
// This removes an unobservable complemented internal storage encoding; it does
// not alter the pins, gate polarity, or any observable latch behavior.
module gf180mcu_fd_sc_mcu7t5v0__latq_1(input D, E, output reg Q);
  always @* if (E) Q = D;
endmodule

module sky130_fd_sc_hd__dlxtp_1(input D, GATE, output reg Q);
  always @* if (GATE) Q = D;
endmodule
