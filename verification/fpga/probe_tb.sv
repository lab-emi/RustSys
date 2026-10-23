// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
`timescale 1ns/1ps
module fpga_probe_tb;
  reg clk=0, rst_n=0, set_n=0, gate=1, enable=1, rs_n1=0;
  reg [5:0] a=0;
  reg [7:0] data=0;
  wire [15:0] y, gold;
  fpga_probe dut(clk,rst_n,set_n,gate,enable,a,data,rs_n1,y);
  fpga_probe_reference reference_dut(clk,rst_n,set_n,gate,enable,a,data,rs_n1,gold);
  always #50 clk=~clk;
  reg [31:0] random_state=32'h13579bdf;
  integer i;
  task compare;
    if (y !== gold) $fatal(1,"RUSTSYS_MISMATCH probe case=%0d dut=%h gold=%h",i,y,gold);
  endtask
  initial begin
    #425; rst_n=1; set_n=1;
    for (i=0;i<512;i=i+1) begin
      @(negedge clk); #5;
      random_state = {random_state[30:0],random_state[31]^random_state[21]^random_state[1]^random_state[0]};
      a=i; data=random_state[7:0]; rs_n1=random_state[8];
      gate=random_state[9]; enable=random_state[10];
      if (i%23==0) rst_n=0;
      if (i%29==0) set_n=0;
      #20; compare();
      rst_n=1; set_n=1;
      @(posedge clk); #20; compare();
    end
    $display("RUSTSYS_PASS probe vectors=512"); $finish;
  end
  initial begin #100000; $fatal(1,"RUSTSYS_TIMEOUT probe"); end
endmodule
