// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
// Deterministic RV32IM program with independently checked store signatures,
// load-use dependency, branch, multiply/divide/remainder and bus backpressure.
// Both designs see identical reference-driven responses. Comparison starts after
// reset/GSR. Inactive bus payloads and unknown diagnostic bits are not observations.
`timescale 1ns/1ps
`ifndef REFERENCE
`define REFERENCE ibex_reference
`endif
`define OUTPUTS(P) \
  wire P``_instr_req, P``_data_req, P``_data_we; \
  wire [31:0] P``_instr_addr, P``_data_addr, P``_data_wdata; \
  wire [3:0] P``_data_be; wire [6:0] P``_data_intg; \
  wire P``_scramble_req, P``_double_fault, P``_minor, P``_major_internal, P``_major_bus, P``_sleep; \
  wire [159:0] P``_crash;
`define PORTS(P) \
  .clk_i(clk), .rst_ni(rst_n), .test_en_i(1'b0), .ram_cfg_i(10'b0), \
  .hart_id_i(32'b0), .boot_addr_i(32'b0), \
  .instr_req_o(P``_instr_req), .instr_gnt_i(instr_gnt), .instr_rvalid_i(instr_valid), \
  .instr_addr_o(P``_instr_addr), .instr_rdata_i(instr_data), .instr_rdata_intg_i(7'b0), .instr_err_i(1'b0), \
  .data_req_o(P``_data_req), .data_gnt_i(data_gnt), .data_rvalid_i(data_valid), \
  .data_we_o(P``_data_we), .data_be_o(P``_data_be), .data_addr_o(P``_data_addr), \
  .data_wdata_o(P``_data_wdata), .data_wdata_intg_o(P``_data_intg), \
  .data_rdata_i(data_read), .data_rdata_intg_i(7'b0), .data_err_i(1'b0), \
  .irq_software_i(1'b0), .irq_timer_i(1'b0), .irq_external_i(1'b0), .irq_fast_i(15'b0), .irq_nm_i(1'b0), \
  .scramble_key_valid_i(1'b0), .scramble_key_i(128'b0), .scramble_nonce_i(64'b0), \
  .scramble_req_o(P``_scramble_req), .debug_req_i(1'b0), .crash_dump_o(P``_crash), \
  .double_fault_seen_o(P``_double_fault), .fetch_enable_i(4'b0101), \
  .alert_minor_o(P``_minor), .alert_major_internal_o(P``_major_internal), .alert_major_bus_o(P``_major_bus), \
  .core_sleep_o(P``_sleep), .scan_rst_ni(rst_n)

module ibex_tb;
  reg clk=0, rst_n=0;
  reg instr_valid=0, data_valid=0;
  reg [31:0] instr_data=32'h00000013, data_read=0;
  integer cycle=0, checks=0, stores=0, loads=0, fetches=0, stalls=0, byte_index;
  reg [31:0] memory [0:63];
  reg [31:0] random_state=32'h1729abcd;
  integer seed=1, diagnostic_index, diagnostic_defined_checks=0, diagnostic_unknown_checks=0;
  `OUTPUTS(gold)
  `OUTPUTS(dut)
  wire #2 instr_gnt=gold_instr_req && random_state[0];
  wire #2 data_gnt=gold_data_req && random_state[1];
  `REFERENCE reference_dut (`PORTS(gold));
  ibex_top dut (`PORTS(dut));
  always #50 clk=~clk;
  // Encodings are hand specified; known architectural results are asserted below.
  function [31:0] rom(input [31:0] address);
    case (address)
      'h80: rom='h10000093; // addi x1,x0,256
      'h84: rom='h00700113; // addi x2,x0,7
      'h88: rom='h00300193; // addi x3,x0,3
      'h8c: rom='h00310233; // add x4,x2,x3 = 10
      'h90: rom='h403102b3; // sub x5,x2,x3 = 4
      'h94: rom='h00524333; // xor x6,x4,x5 = 14
      'h98: rom='h0040a023; // sw x4,0(x1)
      'h9c: rom='h0000a383; // lw x7,0(x1)
      'ha0: rom='h00439463; // bne x7,x4,+8 (must not take)
      'ha4: rom='h0050a223; // sw x5,4(x1)
      'ha8: rom='h0060a423; // sw x6,8(x1)
      'hac: rom='h02520433; // mul x8,x4,x5 = 40
      'hb0: rom='h0080a623; // sw x8,12(x1)
      'hb4: rom='h023444b3; // div x9,x8,x3 = 13
      'hb8: rom='h0090a823; // sw x9,16(x1)
      'hbc: rom='h02346533; // rem x10,x8,x3 = 1
      'hc0: rom='h00a0aa23; // sw x10,20(x1)
      'hc4: rom='h000065b7; // lui x11,6
      'hc8: rom='h00d58593; // addi x11,x11,13 = 0x600d
      'hcc: rom='h00b0ac23; // sw x11,24(x1)
      'hd0: rom='hfb1ff06f; // jal x0,-80 (repeat from 0x80)
      default: rom='h00000013;
    endcase
  endfunction

  task equal32(input [31:0] a, b, input [255:0] name);
    if (a !== b) $fatal(1,"RUSTSYS_MISMATCH cycle=%0d %0s dut=%h gold=%h",cycle,name,b,a);
  endtask

  always @(posedge clk) begin
    cycle <= cycle+1;
    instr_valid <= #2 rst_n && gold_instr_req && instr_gnt;
    data_valid <= #2 rst_n && gold_data_req && data_gnt;
    if (rst_n && gold_instr_req && instr_gnt) begin instr_data <= #2 rom(gold_instr_addr); fetches=fetches+1; end
    if (rst_n && gold_instr_req && !instr_gnt) stalls=stalls+1;
    if (rst_n && gold_data_req && data_gnt) begin
      if (gold_data_addr < 'h100 || gold_data_addr >= 'h200) $fatal(1,"unexpected data address");
      data_read <= #2 memory[gold_data_addr[7:2]];
      if (gold_data_we) begin
        for (byte_index=0;byte_index<4;byte_index=byte_index+1)
          if (gold_data_be[byte_index]) memory[gold_data_addr[7:2]][8*byte_index+:8] <= gold_data_wdata[8*byte_index+:8];
        stores=stores+1;
      end else loads=loads+1;
    end
  end
  always @(posedge clk) begin
    // Observe near the next active edge, after the 98 ns output timing budget.
    #98.5;
    if (rst_n) begin
      equal32({30'b0,gold_instr_req,gold_data_req},{30'b0,dut_instr_req,dut_data_req},"requests");
      equal32({26'b0,gold_scramble_req,gold_double_fault,gold_minor,gold_major_internal,gold_major_bus,gold_sleep},
              {26'b0,dut_scramble_req,dut_double_fault,dut_minor,dut_major_internal,dut_major_bus,dut_sleep},"status");
      if (gold_instr_req) equal32(gold_instr_addr,dut_instr_addr,"instruction address");
      if (gold_data_req) begin
        equal32(gold_data_addr,dut_data_addr,"data address");
        equal32({27'b0,gold_data_we,gold_data_be},{27'b0,dut_data_we,dut_data_be},"write enables");
        if (gold_data_we) begin
          equal32(gold_data_wdata,dut_data_wdata,"write data");
          equal32({25'b0,gold_data_intg},{25'b0,dut_data_intg},"write integrity");
        end
      end
      for (diagnostic_index=0;diagnostic_index<160;diagnostic_index=diagnostic_index+1) begin
        if (gold_crash[diagnostic_index] === 1'b0 || gold_crash[diagnostic_index] === 1'b1) begin
          if (dut_crash[diagnostic_index] !== gold_crash[diagnostic_index])
            $fatal(1,"RUSTSYS_MISMATCH crash dump bit=%0d cycle=%0d",diagnostic_index,cycle);
          diagnostic_defined_checks=diagnostic_defined_checks+1;
        end else diagnostic_unknown_checks=diagnostic_unknown_checks+1;
      end
      checks=checks+1;
    end
  end
  // Launch the ready decision at the source edge. Delayed continuous grants
  // and intra-assignment response delays place every dynamic DUT input at +2 ns.
  // Response RHS values are sampled at the edge before requests/state change.
  always @(posedge clk) begin
    random_state <= {random_state[30:0],random_state[31]^random_state[21]^random_state[1]^random_state[0]};
  end
  initial begin
    if ($value$plusargs("SEED=%d",seed)) random_state=32'h1729abcd ^ seed;
    for (byte_index=0;byte_index<64;byte_index=byte_index+1) memory[byte_index]=0;
    #425; rst_n=1;
    // A second asynchronous reset checks recovery after running state exists.
    #75000; rst_n=0; #500; rst_n=1;
    #75000;
    if (checks<1400 || stores<14 || loads<2 || fetches<50 || stalls<10) $fatal(1,"insufficient execution coverage");
    if (memory[0]!==10 || memory[1]!==4 || memory[2]!==14 || memory[3]!==40 ||
        memory[4]!==13 || memory[5]!==1 || memory[6]!==32'h600d) $fatal(1,"architectural store signature failed");
    $display("RUSTSYS_PASS ibex checks=%0d stores=%0d loads=%0d fetches=%0d stalls=%0d seed=%0d diagnostic_defined=%0d diagnostic_unknown=%0d",checks,stores,loads,fetches,stalls,seed,diagnostic_defined_checks,diagnostic_unknown_checks);
    $finish;
  end
  initial begin #200000; $fatal(1,"RUSTSYS_TIMEOUT ibex"); end
endmodule
