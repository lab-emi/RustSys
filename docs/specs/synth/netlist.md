# RustSys output netlist contract

The output netlist is consumed by later tools and by the independent synthesis checker
(`docs/specs/check/synth.md`). Its naming rules are therefore normative.

**SYNTH-REQ-0601** The output is one Verilog module (IEEE 1364-2005 structural subset) named as the
top module. It uses only: ANSI port declarations, `wire` declarations, cell instances with named
port connections, and `assign` statements whose right-hand side is a net, a bit of a net, or the
constant `1'b0` or `1'b1`.

**SYNTH-REQ-0602** Ports appear in RTL order with RTL names and directions. A port of packed width 1
without packed dimensions is a scalar; any other port of packed width W is declared `[W-1:0]`,
and bit k of the netlist port is bit k (counted from the least significant bit, starting at 0) of
the RTL port's packed value. Ports of unpacked array type are rejected. `inout` ports are rejected.

**SYNTH-REQ-0603** State naming. Each retained state bit (SYNTH-REQ-0308) has a state name:
the hierarchical path of the variable (SYNTH-REQ-0140), followed by `[i]` for each unpacked
dimension with the SystemVerilog index value, followed by `[k]` where k is the bit position
counted from the least significant bit of the packed value. When the packed width of the
variable (of one element, for unpacked arrays) is 1, the `[k]` suffix is omitted. Examples: `count_q[3]`,
`u_fifo.mem[2][7]`, `u_ctrl.state_q[0]`, `busy_q`.

**SYNTH-REQ-0604** For each retained state bit the netlist declares a scalar wire whose name is the
state name, written as an escaped identifier when it is not a simple identifier. That wire carries
the RTL value of the state bit: it is connected to the true state output of the sequential cell, or
driven by an inverter from the complemented output when SYNTH-REQ-0520 uses one. When the state
name equals the name of a one-bit output port (the state variable is the port itself), the port
is that wire and no separate wire is declared.

**SYNTH-REQ-0605** Every other net and every instance name is generated and does not collide with a
port or state name. Generated names carry no meaning for consumers.

**SYNTH-REQ-0606** The netlist contains no combinational cycle, no undriven cell input, no net with
more than one driver, and no cell that is not defined in the given libraries.
