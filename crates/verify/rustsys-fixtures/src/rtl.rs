// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Hand-written RTL modules typical of digital designs (SYNTH-REQ-0802, suite `corpus`).

/// A corpus entry: name, top module, source files (name, text).
#[derive(Debug, Clone, Copy)]
pub struct Design {
    pub name: &'static str,
    pub top: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

const PKG: &str = r#"
package wt_pkg;
  typedef enum logic [2:0] { OpAdd, OpSub, OpAnd, OpOr, OpXor, OpSll, OpSrl, OpSlt } alu_op_e;
  typedef struct packed {
    logic [7:0] data;
    logic [1:0] tag;
    logic       last;
  } beat_t;
  localparam int unsigned DataWidth = 8;
  function automatic logic [DataWidth-1:0] reverse(input logic [DataWidth-1:0] v);
    for (int i = 0; i < DataWidth; i++) reverse[i] = v[DataWidth-1-i];
  endfunction
endpackage
"#;

const COUNTER: &str = r#"
module counter #(parameter int unsigned W = 8, parameter logic [W-1:0] MAX = '1) (
  input  logic         clk_i,
  input  logic         rst_ni,
  input  logic         en_i,
  input  logic         clr_i,
  output logic [W-1:0] cnt_o,
  output logic         wrap_o
);
  logic [W-1:0] cnt_q, cnt_d;
  always_comb begin
    cnt_d = cnt_q;
    wrap_o = 1'b0;
    if (clr_i) begin
      cnt_d = '0;
    end else if (en_i) begin
      if (cnt_q == MAX) begin
        cnt_d = '0;
        wrap_o = 1'b1;
      end else begin
        cnt_d = cnt_q + 1'b1;
      end
    end
  end
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) cnt_q <= '0;
    else         cnt_q <= cnt_d;
  end
  assign cnt_o = cnt_q;
endmodule
"#;

const SHIFT_REG: &str = r#"
module shift_reg #(parameter int Depth = 4, parameter int Width = 3) (
  input  logic             clk,
  input  logic             rst_n,
  input  logic [Width-1:0] d,
  output logic [Width-1:0] q
);
  logic [Width-1:0] stage [Depth+1];
  assign stage[0] = d;
  for (genvar i = 0; i < Depth; i++) begin : g_stage
    always_ff @(posedge clk or negedge rst_n)
      if (!rst_n) stage[i+1] <= '0;
      else        stage[i+1] <= stage[i];
  end
  assign q = stage[Depth];
endmodule
"#;

const SEQ_DET: &str = r#"
module seq_det (input logic clk, input logic rst_n, input logic din, output logic hit);
  typedef enum logic [1:0] { S0, S1, S10, S101 } state_e;
  state_e state_q, state_d;
  always_comb begin
    state_d = state_q;
    unique case (state_q)
      S0:   state_d = din ? S1   : S0;
      S1:   state_d = din ? S1   : S10;
      S10:  state_d = din ? S101 : S0;
      S101: state_d = din ? S1   : S10;
    endcase
  end
  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n) state_q <= S0;
    else        state_q <= state_d;
  assign hit = (state_q == S101);
endmodule
"#;

const FIFO: &str = r#"
module fifo #(parameter int unsigned Depth = 4, parameter int unsigned Width = 8) (
  input  logic             clk_i,
  input  logic             rst_ni,
  input  logic             push_i,
  input  logic [Width-1:0] data_i,
  input  logic             pop_i,
  output logic [Width-1:0] data_o,
  output logic             full_o,
  output logic             empty_o,
  output logic [$clog2(Depth):0] count_o
);
  localparam int unsigned PtrW = $clog2(Depth);
  logic [Width-1:0] mem_q [Depth];
  logic [PtrW-1:0] wptr_q, rptr_q;
  logic [PtrW:0]   cnt_q;
  logic push, pop;
  assign full_o  = (cnt_q == Depth[PtrW:0]);
  assign empty_o = (cnt_q == '0);
  assign push = push_i && !full_o;
  assign pop  = pop_i && !empty_o;
  assign data_o = mem_q[rptr_q];
  assign count_o = cnt_q;
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) begin
      wptr_q <= '0;
      rptr_q <= '0;
      cnt_q  <= '0;
    end else begin
      if (push) wptr_q <= (wptr_q == PtrW'(Depth - 1)) ? '0 : wptr_q + 1'b1;
      if (pop)  rptr_q <= (rptr_q == PtrW'(Depth - 1)) ? '0 : rptr_q + 1'b1;
      unique case ({push, pop})
        2'b10: cnt_q <= cnt_q + 1'b1;
        2'b01: cnt_q <= cnt_q - 1'b1;
        default: ;
      endcase
    end
  end
  always_ff @(posedge clk_i) begin
    if (push) mem_q[wptr_q] <= data_i;
  end
endmodule
"#;

const ALU: &str = r#"
module alu import wt_pkg::*; (
  input  alu_op_e         op_i,
  input  logic [7:0]      a_i,
  input  logic [7:0]      b_i,
  output logic [7:0]      y_o,
  output logic            zero_o,
  output logic            carry_o
);
  logic [8:0] sum;
  always_comb begin
    sum = '0;
    y_o = '0;
    unique case (op_i)
      OpAdd: begin sum = a_i + b_i; y_o = sum[7:0]; end
      OpSub: begin sum = {1'b0, a_i} - {1'b0, b_i}; y_o = sum[7:0]; end
      OpAnd: y_o = a_i & b_i;
      OpOr:  y_o = a_i | b_i;
      OpXor: y_o = a_i ^ b_i;
      OpSll: y_o = a_i << b_i[2:0];
      OpSrl: y_o = a_i >> b_i[2:0];
      OpSlt: y_o = {7'd0, $signed(a_i) < $signed(b_i)};
      default: y_o = reverse(a_i);
    endcase
  end
  assign zero_o = (y_o == 8'd0);
  assign carry_o = sum[8];
endmodule
"#;

const PRIO_ENC: &str = r#"
module prio_enc #(parameter int N = 12) (
  input  logic [N-1:0]         req_i,
  output logic [$clog2(N)-1:0] idx_o,
  output logic                 valid_o,
  output logic [$clog2(N):0]   lzc_o
);
  always_comb begin
    idx_o = '0;
    valid_o = 1'b0;
    for (int i = N - 1; i >= 0; i--) begin
      if (req_i[i]) begin
        idx_o = i[$clog2(N)-1:0];
        valid_o = 1'b1;
        break;
      end
    end
  end
  always_comb begin
    lzc_o = N;
    for (int i = 0; i < N; i++) begin
      if (req_i[N-1-i]) begin
        lzc_o = i;
        break;
      end
    end
  end
endmodule
"#;

const RR_ARB: &str = r#"
module rr_arb #(parameter int N = 4) (
  input  logic         clk,
  input  logic         rst_n,
  input  logic [N-1:0] req,
  output logic [N-1:0] gnt
);
  logic [$clog2(N)-1:0] last_q;
  always_comb begin
    gnt = '0;
    for (int k = 1; k <= N; k++) begin
      automatic int idx = (last_q + k) % N;
      if (req[idx] && gnt == '0) gnt[idx] = 1'b1;
    end
  end
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) last_q <= N - 1;
    else begin
      for (int i = 0; i < N; i++) if (gnt[i]) last_q <= i;
    end
  end
endmodule
"#;

const CRC8: &str = r#"
module crc8 (
  input  logic       clk,
  input  logic       rst_n,
  input  logic       valid,
  input  logic [7:0] data,
  output logic [7:0] crc
);
  // Polynomial x^8 + x^2 + x + 1, processed one byte per cycle.
  function automatic logic [7:0] crc_step(input logic [7:0] c, input logic [7:0] d);
    logic [7:0] r;
    r = c;
    for (int i = 7; i >= 0; i--) begin
      logic fb;
      fb = r[7] ^ d[i];
      r = {r[6:0], 1'b0};
      if (fb) r = r ^ 8'h07;
    end
    return r;
  endfunction
  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n)     crc <= 8'hFF;
    else if (valid) crc <= crc_step(crc, data);
endmodule
"#;

const LFSR: &str = r#"
module lfsr #(parameter int W = 16, parameter logic [W-1:0] Taps = 16'hB400, parameter logic [W-1:0] Seed = 16'hACE1) (
  input  logic         clk_i,
  input  logic         rst_ni,
  input  logic         en_i,
  output logic [W-1:0] state_o
);
  logic [W-1:0] s_q;
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) s_q <= Seed;
    else if (en_i) s_q <= s_q[0] ? ((s_q >> 1) ^ Taps) : (s_q >> 1);
  end
  assign state_o = s_q;
endmodule
"#;

const GRAY: &str = r#"
module gray #(parameter int W = 5) (
  input  logic         clk,
  input  logic         rst_n,
  input  logic [W-1:0] bin_i,
  output logic [W-1:0] gray_o,
  output logic [W-1:0] bin_o,
  output logic [W-1:0] gcnt_o
);
  assign gray_o = bin_i ^ (bin_i >> 1);
  always_comb begin
    bin_o[W-1] = bin_i[W-1];
    for (int i = W - 2; i >= 0; i--) bin_o[i] = bin_o[i+1] ^ bin_i[i];
  end
  logic [W-1:0] b_q;
  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n) b_q <= '0; else b_q <= b_q + 1'b1;
  assign gcnt_o = b_q ^ (b_q >> 1);
endmodule
"#;

const SYNC_EDGE: &str = r#"
module sync_edge (input logic clk, input logic rst_n, input logic async_i, output logic rise_o, output logic fall_o, output logic level_o);
  logic [2:0] sync_q;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) sync_q <= 3'b000;
    else        sync_q <= {sync_q[1:0], async_i};
  end
  assign level_o = sync_q[1];
  assign rise_o  = sync_q[1] & ~sync_q[2];
  assign fall_o  = ~sync_q[1] & sync_q[2];
endmodule
"#;

const MULT: &str = r#"
module mult (
  input  logic               clk,
  input  logic signed [5:0]  a,
  input  logic signed [5:0]  b,
  input  logic        [5:0]  ua,
  output logic signed [11:0] p_q,
  output logic        [11:0] up
);
  always_ff @(posedge clk) p_q <= a * b;
  assign up = ua * $unsigned(b);
endmodule
"#;

const BARREL: &str = r#"
module barrel (
  input  logic [15:0] d,
  input  logic [3:0]  sh,
  input  logic [1:0]  mode,
  output logic [15:0] y
);
  always_comb begin
    case (mode)
      2'd0: y = d << sh;
      2'd1: y = d >> sh;
      2'd2: y = $unsigned($signed(d) >>> sh);
      default: y = (d << sh) | (d >> (5'd16 - {1'b0, sh}));
    endcase
  end
endmodule
"#;

const REGFILE: &str = r#"
module regfile #(parameter int Regs = 8, parameter int W = 8) (
  input  logic                    clk,
  input  logic                    rst_n,
  input  logic                    we,
  input  logic [$clog2(Regs)-1:0] waddr,
  input  logic [W-1:0]            wdata,
  input  logic [$clog2(Regs)-1:0] raddr_a,
  input  logic [$clog2(Regs)-1:0] raddr_b,
  output logic [W-1:0]            rdata_a,
  output logic [W-1:0]            rdata_b
);
  logic [W-1:0] regs_q [Regs];
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      for (int i = 0; i < Regs; i++) regs_q[i] <= '0;
    end else if (we && waddr != '0) begin
      regs_q[waddr] <= wdata;
    end
  end
  assign rdata_a = regs_q[raddr_a];
  assign rdata_b = (raddr_b == '0) ? '0 : regs_q[raddr_b];
endmodule
"#;

const UART_TX: &str = r#"
module uart_tx #(parameter int unsigned ClksPerBit = 4) (
  input  logic       clk_i,
  input  logic       rst_ni,
  input  logic       valid_i,
  input  logic [7:0] data_i,
  output logic       ready_o,
  output logic       tx_o
);
  typedef enum logic [1:0] { Idle, Start, Data, Stop } state_e;
  state_e state_q;
  logic [$clog2(ClksPerBit)-1:0] baud_q;
  logic [2:0] bit_q;
  logic [7:0] shift_q;
  logic tick;
  assign tick = (baud_q == ClksPerBit - 1);
  assign ready_o = (state_q == Idle);
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) begin
      state_q <= Idle;
      baud_q <= '0;
      bit_q <= '0;
      shift_q <= '0;
      tx_o <= 1'b1;
    end else begin
      baud_q <= (state_q == Idle || tick) ? '0 : baud_q + 1'b1;
      case (state_q)
        Idle: if (valid_i) begin
          state_q <= Start;
          shift_q <= data_i;
          tx_o <= 1'b0;
        end
        Start: if (tick) begin
          state_q <= Data;
          bit_q <= '0;
          tx_o <= shift_q[0];
        end
        Data: if (tick) begin
          shift_q <= shift_q >> 1;
          if (bit_q == 3'd7) begin
            state_q <= Stop;
            tx_o <= 1'b1;
          end else begin
            bit_q <= bit_q + 1'b1;
            tx_o <= shift_q[1];
          end
        end
        Stop: if (tick) state_q <= Idle;
        default: state_q <= Idle;
      endcase
    end
  end
endmodule
"#;

const PIPELINE: &str = r#"
module pipeline import wt_pkg::*; (
  input  logic  clk,
  input  logic  rst_n,
  input  logic  in_valid,
  input  beat_t in_beat,
  input  logic  out_ready,
  output logic  in_ready,
  output logic  out_valid,
  output beat_t out_beat
);
  beat_t stage_q [2];
  logic [1:0] valid_q;
  logic advance;
  assign advance = out_ready || !valid_q[1];
  assign in_ready = advance;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      valid_q <= '0;
      stage_q <= '{default: '0};
    end else if (advance) begin
      valid_q <= {valid_q[0], in_valid};
      stage_q[0] <= '{data: in_beat.data + 8'd1, tag: in_beat.tag, last: in_beat.last};
      stage_q[1] <= stage_q[0];
    end
  end
  assign out_valid = valid_q[1];
  always_comb begin
    out_beat = stage_q[1];
    out_beat.data = reverse(stage_q[1].data);
  end
endmodule
"#;

const BCD: &str = r#"
module bin2bcd (input logic [7:0] bin, output logic [11:0] bcd, output logic [3:0] ones_o);
  always_comb begin
    bcd = '0;
    for (int i = 7; i >= 0; i--) begin
      for (int d = 0; d < 3; d++) begin
        if (bcd[4*d +: 4] >= 4'd5) bcd[4*d +: 4] = bcd[4*d +: 4] + 4'd3;
      end
      bcd = {bcd[10:0], bin[i]};
    end
  end
  assign ones_o = bin % 8'd10;
endmodule
"#;

const DEBOUNCE: &str = r#"
module debounce #(parameter int Stable = 5) (input logic clk, input logic rst_n, input logic btn, output logic level, output logic pressed);
  logic [$clog2(Stable+1)-1:0] cnt;
  logic last;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      cnt <= '0;
      level <= 1'b0;
      last <= 1'b0;
      pressed <= 1'b0;
    end else begin
      pressed <= 1'b0;
      if (btn == last) begin
        if (cnt != Stable) cnt <= cnt + 1'b1;
        else if (level != btn) begin
          level <= btn;
          pressed <= btn;
        end
      end else begin
        cnt <= '0;
      end
      last <= btn;
    end
  end
endmodule
"#;

const PWM: &str = r#"
module pwm (input logic clk, input logic rst_n, input logic [3:0] duty, output logic out);
  logic [3:0] phase;
  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n) phase <= 4'd0; else phase <= phase + 4'd1;
  always_comb out = (phase < duty);
endmodule
"#;

use crate::rtl_misc::{
    DECODER, DIV_CONST, LATCHES, MEM2D, OLD_STYLE, SAT_ADD, SET_NEG, SKID, SUBROUTINES, TOP_HIER,
};
use crate::rtl_more::{RV_CORE, RV_PKG, ZERO_REPL};

pub const CORPUS: &[Design] = &[
    Design {
        name: "counter",
        top: "counter",
        files: &[("counter.sv", COUNTER)],
    },
    Design {
        name: "shift_reg",
        top: "shift_reg",
        files: &[("shift_reg.sv", SHIFT_REG)],
    },
    Design {
        name: "seq_det",
        top: "seq_det",
        files: &[("seq_det.sv", SEQ_DET)],
    },
    Design {
        name: "fifo",
        top: "fifo",
        files: &[("fifo.sv", FIFO)],
    },
    Design {
        name: "alu",
        top: "alu",
        files: &[("wt_pkg.sv", PKG), ("alu.sv", ALU)],
    },
    Design {
        name: "prio_enc",
        top: "prio_enc",
        files: &[("prio_enc.sv", PRIO_ENC)],
    },
    Design {
        name: "rr_arb",
        top: "rr_arb",
        files: &[("rr_arb.sv", RR_ARB)],
    },
    Design {
        name: "crc8",
        top: "crc8",
        files: &[("crc8.sv", CRC8)],
    },
    Design {
        name: "lfsr",
        top: "lfsr",
        files: &[("lfsr.sv", LFSR)],
    },
    Design {
        name: "gray",
        top: "gray",
        files: &[("gray.sv", GRAY)],
    },
    Design {
        name: "sync_edge",
        top: "sync_edge",
        files: &[("sync_edge.sv", SYNC_EDGE)],
    },
    Design {
        name: "mult",
        top: "mult",
        files: &[("mult.sv", MULT)],
    },
    Design {
        name: "barrel",
        top: "barrel",
        files: &[("barrel.sv", BARREL)],
    },
    Design {
        name: "regfile",
        top: "regfile",
        files: &[("regfile.sv", REGFILE)],
    },
    Design {
        name: "uart_tx",
        top: "uart_tx",
        files: &[("uart_tx.sv", UART_TX)],
    },
    Design {
        name: "pipeline",
        top: "pipeline",
        files: &[("wt_pkg.sv", PKG), ("pipeline.sv", PIPELINE)],
    },
    Design {
        name: "bin2bcd",
        top: "bin2bcd",
        files: &[("bin2bcd.sv", BCD)],
    },
    Design {
        name: "debounce",
        top: "debounce",
        files: &[("debounce.sv", DEBOUNCE)],
    },
    Design {
        name: "pwm",
        top: "pwm",
        files: &[("pwm.sv", PWM)],
    },
    Design {
        name: "latches",
        top: "latches",
        files: &[("latches.sv", LATCHES)],
    },
    Design {
        name: "set_neg",
        top: "set_neg",
        files: &[("set_neg.sv", SET_NEG)],
    },
    Design {
        name: "div_const",
        top: "div_const",
        files: &[("div_const.sv", DIV_CONST)],
    },
    Design {
        name: "sat_add",
        top: "sat_add",
        files: &[("sat_add.sv", SAT_ADD)],
    },
    Design {
        name: "decoder",
        top: "decoder",
        files: &[("decoder.sv", DECODER)],
    },
    Design {
        name: "subroutines",
        top: "subroutines",
        files: &[("subroutines.sv", SUBROUTINES)],
    },
    Design {
        name: "mem2d",
        top: "mem2d",
        files: &[("mem2d.sv", MEM2D)],
    },
    Design {
        name: "skid",
        top: "skid",
        files: &[("skid.sv", SKID)],
    },
    Design {
        name: "old_style",
        top: "old_style",
        files: &[("old_style.sv", OLD_STYLE)],
    },
    Design {
        name: "top_hier",
        top: "top_hier",
        files: &[
            ("counter.sv", COUNTER),
            ("crc8.sv", CRC8),
            ("top_hier.sv", TOP_HIER),
        ],
    },
    Design {
        name: "rv_core",
        top: "rv_core",
        files: &[("rv_pkg.sv", RV_PKG), ("rv_core.sv", RV_CORE)],
    },
    Design {
        name: "zero_repl",
        top: "zero_repl",
        files: &[("zero_repl.sv", ZERO_REPL)],
    },
];
