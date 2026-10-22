// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// Liberty stores !D in IQ1 and exposes !IQ1 on Q: this is a positive data latch.

// Resettable latches: load when reset or enable is active, with reset priority.
module \$_DLATCH_PP0_ (input E, R, D, output Q);
  wire enabled = E;
  wire reset_active = R;
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(enabled | reset_active),
    .D(reset_active ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_PP1_ (input E, R, D, output Q);
  wire enabled = E;
  wire reset_active = R;
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(enabled | reset_active),
    .D(reset_active ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_PN0_ (input E, R, D, output Q);
  wire enabled = E;
  wire reset_active = ~R;
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(enabled | reset_active),
    .D(reset_active ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_PN1_ (input E, R, D, output Q);
  wire enabled = E;
  wire reset_active = ~R;
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(enabled | reset_active),
    .D(reset_active ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_NP0_ (input E, R, D, output Q);
  wire enabled = ~E;
  wire reset_active = R;
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(enabled | reset_active),
    .D(reset_active ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_NP1_ (input E, R, D, output Q);
  wire enabled = ~E;
  wire reset_active = R;
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(enabled | reset_active),
    .D(reset_active ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_NN0_ (input E, R, D, output Q);
  wire enabled = ~E;
  wire reset_active = ~R;
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(enabled | reset_active),
    .D(reset_active ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_NN1_ (input E, R, D, output Q);
  wire enabled = ~E;
  wire reset_active = ~R;
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(enabled | reset_active),
    .D(reset_active ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_P_ (input E, D, output Q);
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(E), .D(D), .Q(Q));
endmodule
module \$_DLATCH_N_ (input E, D, output Q);
  gf180mcu_fd_sc_mcu7t5v0__latq_1 latch_cell (.E(~E), .D(D), .Q(Q));
endmodule
