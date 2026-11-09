# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
# Third-party structural netlist flow, AMD UG904/UG835. No synth_design pass.
if {$argc != 3} {error "expected: mapped.v part output-directory"}
lassign $argv netlist part output
file mkdir $output
set_param general.maxThreads 2
create_project -in_memory -part $part
# UG835 link_design requires a gate-level source fileset for third-party netlists.
set_property DESIGN_MODE GateLvl [current_fileset]
read_verilog $netlist
link_design -top ibex_top -part $part -mode out_of_context
if {[llength [get_cells -hierarchical -filter {IS_BLACKBOX == 1}]] != 0} {
    error "unresolved primitive/black box in RustSys netlist"
}
config_timing_corners -corner Slow -delay_type min_max
config_timing_corners -corner Fast -delay_type min_max
create_clock -name clk -period 100.0 [get_ports clk_i]
set_input_delay 2.0 -clock clk [remove_from_collection [all_inputs] [get_ports clk_i]]
set_output_delay 2.0 -clock clk [all_outputs]
set clock_index 0
foreach buffer [get_cells -hierarchical -filter {REF_NAME == BUFG}] {
    set pin [get_pins $buffer/O]
    if {[llength [get_clocks -quiet -of_objects $pin]] == 0} {
        create_generated_clock -name gated_$clock_index -source [get_ports clk_i] -divide_by 1 $pin
        incr clock_index
    }
}
proc export_stage {stage output} {
    write_checkpoint -force [file join $output $stage.dcp]
    write_verilog -force -mode funcsim -rename_top ibex_top [file join $output ${stage}_functional.v]
    # The driver annotates explicitly with xelab -sdfmax at /ibex_tb/dut.
    write_verilog -force -mode timesim -rename_top ibex_top -sdf_anno false \
        [file join $output ${stage}_timing.v]
    write_sdf -force -process_corner slow [file join $output ${stage}.sdf]
    report_utilization -file [file join $output ${stage}_utilization.rpt]
    report_timing_summary -delay_type min_max -report_unconstrained -file [file join $output ${stage}_timing.rpt]
}
export_stage post_synthesis $output
opt_design
place_design
route_design
report_route_status -file [file join $output route_status.rpt]
if {![report_route_status -boolean_check ROUTED_FULLY]} {error "implementation is not fully routed"}
if {[report_route_status -boolean_check ERRORS_IN_ROUTES]} {error "routing errors remain"}
report_drc -file [file join $output drc.rpt]
set violations [get_drc_violations -quiet -filter {SEVERITY == "Error" || SEVERITY == "Critical Warning"}]
if {[llength $violations] != 0} {error "implementation has blocking DRC violations: $violations"}
export_stage post_implementation $output
set paths [get_timing_paths -delay_type max -max_paths 1 -nworst 1]
if {[llength $paths] == 0} {error "no constrained setup paths"}
set slack [get_property SLACK [lindex $paths 0]]
if {$slack < 0.0} {error "100 ns setup constraint failed: slack=$slack"}
set hold_paths [get_timing_paths -delay_type min -max_paths 1 -nworst 1]
if {[llength $hold_paths] == 0} {error "no constrained hold paths"}
set hold_slack [get_property SLACK [lindex $hold_paths 0]]
if {$hold_slack < 0.0} {error "hold constraint failed: slack=$hold_slack"}
set receipt [open [file join $output implementation.json] w]
puts $receipt [format {{"status":"passed","scope":"out_of_context","part":"%s","period_ns":100.0,"worst_setup_slack_ns":%s,"worst_hold_slack_ns":%s,"unconstrained_endpoints":null,"wire_model":"routed","corner":"slow/fast min_max; SDF slow/max","drc_status":"checked","blocking_drc_violations":0}} $part $slack $hold_slack]
close $receipt
puts "RUSTSYS_IMPLEMENTED out_of_context part=$part"
