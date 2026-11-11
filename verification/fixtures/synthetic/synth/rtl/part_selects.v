// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// SYNTH-REQ-0206: dynamic indexed part-selects, read and written: element windows, a field
// within an element, windows that start below the vector or above it, an ascending range,
// windows that overlap, an offset of two operands, and a select wider than its vector.
module part_selects(
    input  [15:0] d,
    input  [2:0]  i,
    input  [3:0]  w,
    output [3:0]  r_up,
    output [3:0]  r_down,
    output [1:0]  r_field,
    output [3:0]  r_low,
    output [3:0]  r_asc,
    output [4:0]  r_bits,
    output [3:0]  r_sum,
    output [63:0] r_wide,
    output [15:0] w_up,
    output [15:0] w_field,
    output [15:0] w_low,
    output [15:0] w_bit,
    output [15:0] w_bits,
    output [15:0] w_asc,
    output [15:0] w_two
);
    wire [0:15] a = d;
    wire [3:0]  n = d[3:0];
    wire signed [3:0] s = {1'b0, i};
    assign r_up    = d[i * 4 +: 4];
    assign r_down  = d[i * 4 + 3 -: 4];
    assign r_field = d[1 + i * 4 +: 2];
    assign r_low   = d[s * 4 - 2 +: 4];
    assign r_asc   = a[i * 4 +: 4];
    assign r_bits  = d[i +: 5];
    assign r_sum   = d[(i[1:0] + w[1:0]) * 4 +: 4];
    assign r_wide  = n[i[0] +: 64];

    reg [15:0] t_up, t_field, t_low, t_bit, t_bits, t_two;
    reg [0:15] t_asc;
    always @* begin
        t_up = d;
        t_up[i * 4 +: 4] = w;
        t_field = d;
        t_field[1 + i * 4 +: 2] = w[1:0];
        t_low = d;
        t_low[s * 4 - 2 +: 4] = w;
        t_bit = d;
        t_bit[{i, w[0]}] = w[1];
        t_bits = d;
        t_bits[i +: 4] = w;
        t_asc = d;
        t_asc[i * 4 +: 4] = w;
        t_two = d;
        t_two[i[1:0] * 4 +: 4] = w;
        t_two[i[2] * 8 +: 8] = {w, ~w};
    end
    assign w_up    = t_up;
    assign w_field = t_field;
    assign w_low   = t_low;
    assign w_bit   = t_bit;
    assign w_bits  = t_bits;
    assign w_asc   = t_asc;
    assign w_two   = t_two;
endmodule
