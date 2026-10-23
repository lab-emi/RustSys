// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// Independent functional models from AMD UG953 public primitive specifications.
// Verification only. No vendor/reference source was read or translated.
// LUTs use Shannon mux expansion so unknown, unselected inputs do not poison known results.
// State begins unknown: these models do not simulate GSR or physical power-up INIT.
`timescale 1ns/1ps
module GND(output G); assign G = 1'b0; endmodule
module VCC(output P); assign P = 1'b1; endmodule
module BUFG(input I, output O); assign O = I; endmodule
module LUT1 #(parameter [1:0] INIT = 0) (input I0, output O);
  assign O = I0 ? INIT[1:1] : INIT[0:0];
endmodule
module LUT2 #(parameter [3:0] INIT = 0) (input I0, input I1, output O);
  wire [1:0] stage_1;
  assign stage_1 = I1 ? INIT[3:2] : INIT[1:0];
  assign O = I0 ? stage_1[1:1] : stage_1[0:0];
endmodule
module LUT3 #(parameter [7:0] INIT = 0) (input I0, input I1, input I2, output O);
  wire [3:0] stage_2;
  assign stage_2 = I2 ? INIT[7:4] : INIT[3:0];
  wire [1:0] stage_1;
  assign stage_1 = I1 ? stage_2[3:2] : stage_2[1:0];
  assign O = I0 ? stage_1[1:1] : stage_1[0:0];
endmodule
module LUT4 #(parameter [15:0] INIT = 0) (input I0, input I1, input I2, input I3, output O);
  wire [7:0] stage_3;
  assign stage_3 = I3 ? INIT[15:8] : INIT[7:0];
  wire [3:0] stage_2;
  assign stage_2 = I2 ? stage_3[7:4] : stage_3[3:0];
  wire [1:0] stage_1;
  assign stage_1 = I1 ? stage_2[3:2] : stage_2[1:0];
  assign O = I0 ? stage_1[1:1] : stage_1[0:0];
endmodule
module LUT5 #(parameter [31:0] INIT = 0) (input I0, input I1, input I2, input I3, input I4, output O);
  wire [15:0] stage_4;
  assign stage_4 = I4 ? INIT[31:16] : INIT[15:0];
  wire [7:0] stage_3;
  assign stage_3 = I3 ? stage_4[15:8] : stage_4[7:0];
  wire [3:0] stage_2;
  assign stage_2 = I2 ? stage_3[7:4] : stage_3[3:0];
  wire [1:0] stage_1;
  assign stage_1 = I1 ? stage_2[3:2] : stage_2[1:0];
  assign O = I0 ? stage_1[1:1] : stage_1[0:0];
endmodule
module LUT6 #(parameter [63:0] INIT = 0) (input I0, input I1, input I2, input I3, input I4, input I5, output O);
  wire [31:0] stage_5;
  assign stage_5 = I5 ? INIT[63:32] : INIT[31:0];
  wire [15:0] stage_4;
  assign stage_4 = I4 ? stage_5[31:16] : stage_5[15:0];
  wire [7:0] stage_3;
  assign stage_3 = I3 ? stage_4[15:8] : stage_4[7:0];
  wire [3:0] stage_2;
  assign stage_2 = I2 ? stage_3[7:4] : stage_3[3:0];
  wire [1:0] stage_1;
  assign stage_1 = I1 ? stage_2[3:2] : stage_2[1:0];
  assign O = I0 ? stage_1[1:1] : stage_1[0:0];
endmodule
module FDRE #(parameter INIT=1'b0, IS_C_INVERTED=1'b0)
  (input C, CE, D, R, output reg Q);
  wire clock_active = C ^ IS_C_INVERTED;
  always @(posedge clock_active) if (R) Q <= 0; else if (CE) Q <= D;
endmodule
module FDCE #(parameter INIT=1'b0, IS_C_INVERTED=1'b0, IS_CLR_INVERTED=1'b0)
  (input C, CE, D, CLR, output reg Q);
  wire clock_active = C ^ IS_C_INVERTED;
  wire clear_active = CLR ^ IS_CLR_INVERTED;
  always @(posedge clock_active or posedge clear_active)
    if (clear_active) Q <= 0; else if (CE) Q <= D;
endmodule
module FDPE #(parameter INIT=1'b1, IS_C_INVERTED=1'b0, IS_PRE_INVERTED=1'b0)
  (input C, CE, D, PRE, output reg Q);
  wire clock_active = C ^ IS_C_INVERTED;
  wire preset_active = PRE ^ IS_PRE_INVERTED;
  always @(posedge clock_active or posedge preset_active)
    if (preset_active) Q <= 1; else if (CE) Q <= D;
endmodule
module LDCE #(parameter INIT=1'b0) (input G, GE, D, CLR, output reg Q);
  always @* if (CLR) Q <= 0; else if (G && GE) Q <= D;
endmodule
module LDPE #(parameter INIT=1'b1) (input G, GE, D, PRE, output reg Q);
  always @* if (PRE) Q <= 1; else if (G && GE) Q <= D;
endmodule
