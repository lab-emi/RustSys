// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// Direct Liberty bindings: LATQX1 stores D while CK is high; LATNQX1 while low.
// Reset wrappers assert the enable and give reset priority without fixing
// the initial latch state. Combinational wrappers are mapped by ordinary ABC.
module \$_DLATCH_PP0_ (input E, R, D, output Q);
  LATQX1 latch_cell (.CK(E | R), .D(R ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_PP1_ (input E, R, D, output Q);
  LATQX1 latch_cell (.CK(E | R), .D(R ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_PN0_ (input E, R, D, output Q);
  LATQX1 latch_cell (.CK(E | ~R), .D(~R ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_PN1_ (input E, R, D, output Q);
  LATQX1 latch_cell (.CK(E | ~R), .D(~R ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_NP0_ (input E, R, D, output Q);
  LATQX1 latch_cell (.CK(~E | R), .D(R ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_NP1_ (input E, R, D, output Q);
  LATQX1 latch_cell (.CK(~E | R), .D(R ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_NN0_ (input E, R, D, output Q);
  LATQX1 latch_cell (.CK(~E | ~R), .D(~R ? 1'b0 : D), .Q(Q));
endmodule
module \$_DLATCH_NN1_ (input E, R, D, output Q);
  LATQX1 latch_cell (.CK(~E | ~R), .D(~R ? 1'b1 : D), .Q(Q));
endmodule
module \$_DLATCH_P_ (input E, D, output Q);
  LATQX1 latch_cell (.CK(E), .D(D), .Q(Q));
endmodule
module \$_DLATCH_N_ (input E, D, output Q);
  LATNQX1 latch_cell (.CK(E), .D(D), .Q(Q));
endmodule
