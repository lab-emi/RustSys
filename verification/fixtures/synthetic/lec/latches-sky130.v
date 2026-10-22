// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// Direct bindings to the latch functions in the pinned Liberty library.

// Resettable latches: load when reset or enable is active, with reset priority.
module \$_DLATCH_PP0_ (input E, R, D, output Q);
  wire enabled = E;
  wire reset_active = R;
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(enabled | reset_active),
    .D(reset_active ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_PP1_ (input E, R, D, output Q);
  wire enabled = E;
  wire reset_active = R;
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(enabled | reset_active),
    .D(reset_active ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_PN0_ (input E, R, D, output Q);
  wire enabled = E;
  wire reset_active = ~R;
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(enabled | reset_active),
    .D(reset_active ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_PN1_ (input E, R, D, output Q);
  wire enabled = E;
  wire reset_active = ~R;
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(enabled | reset_active),
    .D(reset_active ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_NP0_ (input E, R, D, output Q);
  wire enabled = ~E;
  wire reset_active = R;
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(enabled | reset_active),
    .D(reset_active ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_NP1_ (input E, R, D, output Q);
  wire enabled = ~E;
  wire reset_active = R;
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(enabled | reset_active),
    .D(reset_active ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_NN0_ (input E, R, D, output Q);
  wire enabled = ~E;
  wire reset_active = ~R;
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(enabled | reset_active),
    .D(reset_active ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_NN1_ (input E, R, D, output Q);
  wire enabled = ~E;
  wire reset_active = ~R;
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(enabled | reset_active),
    .D(reset_active ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_P_ (input E, D, output Q);
  sky130_fd_sc_hd__dlxtp_1 latch_cell (.GATE(E), .D(D), .Q(Q));
endmodule
module \$_DLATCH_N_ (input E, D, output Q);
  sky130_fd_sc_hd__dlxtn_1 latch_cell (.GATE_N(E), .D(D), .Q(Q));
endmodule
