# RustSys sources other than RTL: BLIF, AIGER and gate-level netlists

Besides SystemVerilog RTL (SYNTH-REQ-0101), `rustsys synth` reads logic networks in BLIF
(`docs/specs/blif/`), and-inverter graphs in AIGER (`docs/specs/aiger/`), and structural netlists of
the cells of other libraries. This document states how such a source becomes the design that the
optimization, mapping and emission of SYNTH-REQ-0401 to 0606 work on, and how its ports, clocks,
initial values and states appear in the netlist. The independent checker reads the same sources
with readers of its own (CHECK-REQ-0100 to 0107).

| Section | Requirements |
|---|---|
| [Formats and options](#formats-and-options) | SYNTH-REQ-0900 to 0902 |
| [Ports, clocks and initial values](#ports-clocks-and-initial-values) | SYNTH-REQ-0903 to 0907 |
| [Logic and cells](#logic-and-cells) | SYNTH-REQ-0908 to 0912 |
| [Report and messages](#report-and-messages) | SYNTH-REQ-0913 to 0914 |

## Formats and options

**SYNTH-REQ-0900** The format of the sources is chosen by the extension of their file names,
ignoring letter case: `.blif` is BLIF, `.aag` and `.aig` are AIGER, anything else is
SystemVerilog. All sources of a run have one format; sources of several formats are an error
`S0910`, and so are several AIGER files, as an AIGER file holds a whole design. A structural
gate-level netlist is not chosen by its extension: `SynthOptions::gate_netlist` (`--gate-netlist`)
marks the one source as a netlist of the cells of `SynthOptions::gate_libs` (`--gate-lib`,
SYNTH-REQ-0910).

**SYNTH-REQ-0901** Macro definitions, parameter overrides and the case assumptions (`-D`, `-P`,
`--assume-full-case`, `--assume-parallel-case`) apply to SystemVerilog only: with another format the
engine rejects them with an error `S0911`. Include directories and `--no-predefined-macros` have no
meaning for other formats and are not read; the command line rejects all six options with other
formats (CLI-REQ-0220). `--after-reset` and `--uninitialized-dont-care` apply to every format
(SYNTH-REQ-0309, 0311).

**SYNTH-REQ-0902** Module name. BLIF: the top model (`--top`, else the first model of the first
file, BLIF-REQ-0042) names the module. AIGER: `--top` names the module, by default the file name
without its directory and extension. Gate-level netlist: the module of the netlist; a `--top` that
names another module is an error `S0920`.

## Ports, clocks and initial values

**SYNTH-REQ-0903** Ports. BLIF and AIGER sources have scalar ports: the primary inputs (BLIF in the
order of BLIF-REQ-0003, AIGER in input order), then the generated clock and initial-value ports of
SYNTH-REQ-0904 and 0906 in that order, then the primary outputs. A BLIF port is named after its
signal; an AIGER port takes the symbol of its input or output (AIGER-REQ-0023), else `i<k>` or
`o<k>` with its position k from 0. Two ports of one name (a BLIF output that is also an input, two
AIGER ports with one symbol) are an error `S0912`, and so is a port name with a character outside
printable ASCII (`0x21` to `0x7E`). A gate-level netlist keeps the ports of its module, their
order, names, directions and widths, bit k of a vector counted from the least significant bit
(SYNTH-REQ-0602); an `inout` port is an error `S0916`.

**SYNTH-REQ-0904** Global clock. A BLIF `.latch` without type and control and every AIGER latch is a
rising-edge flip-flop on the global clock; a BLIF `.latch` with the control `NIL` takes the global
clock as its control (SYNTH-REQ-0905), and so does a BLIF `.mlatch` with the control `NIL`
(SYNTH-REQ-0909). The global clock is a generated scalar input port named `clk`, or `clk_<n>` with
the smallest n from 1 such that no port and no state of the source has the name; it exists only
when a state uses it.

**SYNTH-REQ-0905** Latch types. A BLIF `.latch` of type `re` is a rising-edge flip-flop on its
control signal and one of type `fe` a flip-flop on the complement of its control (SYNTH-REQ-0307);
type `ah` is a level-sensitive latch enabled while the control is 1 and type `al` one enabled while
it is 0. The data input is the `.latch` input signal; an AIGER latch takes its next-state literal.

**SYNTH-REQ-0906** Initial values. State whose source declares the initial value 0 or 1 (a BLIF
`.latch` or `.mlatch` with `init` 0 or 1, an AIGER latch with reset 0 or 1, the AIGER default being
0) has that initial value. BLIF `init` 2 (don't care) and 3 (unknown, the default) and AIGER
latches whose reset is their own literal declare none: such state starts in an arbitrary state,
like RTL state without reset (SYNTH-REQ-0306). `--initial-values <reset|arbitrary>` (default
`reset`, `SynthOptions::initial_values`) selects what declared initial values become:
- `reset`: each such state gets an asynchronous control that sets it to its initial value while a
  generated scalar input port is 1. The port is named `init`, or `init_<n>` with the smallest n from
  1 such that no port, no state and not the clock port of SYNTH-REQ-0904 has the name, and exists
  only when some state has an initial value. Asserting it establishes the source's initial state:
  after its release the netlist behaves as the source does from its initial state. With
  `--after-reset <port>=1` the reset-mode optimizations of SYNTH-REQ-0309 and 0310 apply.
- `arbitrary`: initial values are not implemented: there is no such port, every state starts in an
  arbitrary state, and a warning `S0913` gives the number of states whose declared initial value is
  not implemented. The netlist is equivalent to the source from every initial state, and so from the
  declared one once its state elements are started there.

**SYNTH-REQ-0907** State. Every BLIF `.latch`, every instance of a sequential cell (SYNTH-REQ-0911)
and every AIGER latch is one state element, preserved as SYNTH-REQ-0308 requires. State names
(SYNTH-REQ-0603): a BLIF latch is named after its output signal; a sequential cell instance of a
BLIF source after the signal at its output pin whose function is the state variable (an error
`S0917` when no such pin is connected); an AIGER latch after its symbol, else `l<k>`; an instance
of a gate-level netlist after the instance (SYNTH-REQ-0912). A state name that equals a port name
is an error `S0914`, unless the port is a one-bit output whose function is the state itself
(SYNTH-REQ-0604); so are two states of one name.

## Logic and cells

**SYNTH-REQ-0908** Combinational logic. BLIF covers become AND-inverter logic with the function of
BLIF-REQ-0030: the literals of a row are joined by a balanced tree of ANDs and the rows by a
balanced tree of ORs, complemented for an off-set cover. A BLIF signal without a driver
(BLIF-REQ-0022) reads as the constant 0, as undriven RTL bits do (SYNTH-REQ-0213); the reader's
warning `B0007` names it. AIGER AND gates become AND nodes with the same inversions. A signal whose function depends on itself without a state element in between is
an error `S0915` that names up to eight signals of the cycle. The lowering walks the network with
explicit stacks, so that the depth of the logic does not depend on the call stack.

**SYNTH-REQ-0909** BLIF cells. `.gate` and `.mlatch` instances (BLIF-REQ-0033) are resolved against
the cells of the `--gate-lib` libraries, the first library that defines a cell name giving its
description, and interpreted as in SYNTH-REQ-0911: every formal names a pin of the cell, output
pins drive their signals and input pins read theirs. Errors `S0917`: an instance without
`--gate-lib`, an unknown cell or pin, an unconnected input pin, and an `.mlatch` of a cell without
one `ff` or `latch` group whose clock or enable is a single input pin, possibly complemented. That
pin of an `.mlatch` is connected to the control (the global clock of SYNTH-REQ-0904 for `NIL`),
must not be among the formals, and its initial value follows SYNTH-REQ-0906. A signal that a cell
output pin drives and that has another driver is an error `S0918`; a signal that a cell input pin
reads and that nothing drives reads as 0 with a warning `S0921` (BLIF-REQ-0034, SYNTH-REQ-0908).

**SYNTH-REQ-0910** Gate-level netlists. `--gate-netlist FILE` names a structural netlist of cells of
the `--gate-lib` libraries, read with the structural reader (VERILOG-REQ-0310 to 0316): one flat
module of ports, wires, cell instances and assignments. Its logic is derived from the Liberty
descriptions of its cells (SYNTH-REQ-0911), so that a mapped design is synthesized again for the
`--lib` libraries. Every net that a cell input pin, an assignment or an output port reads has
exactly one driver, an input port, a cell output pin, an assignment or a constant: an undriven net
and a net with several drivers are errors `S0918`.

**SYNTH-REQ-0911** Cell interpretation. An output pin computes its Liberty `function`
(LIBERTY-REQ-0020) of the cell's input pins and, for a sequential cell, of the state variable of its
`ff` or `latch` group and that variable's complement. A sequential cell has one `ff` group, a
flip-flop with `next_state` and `clocked_on`, or one `latch` group, a latch with `data_in` and
`enable`, all functions of the input pins and of the state (an enable flip-flop's `next_state`
reads the state); its `clear` and `preset`, when present, are asynchronous controls that force the
state to 0 and to 1. An output pin depends only on the input pins its function names, so that a
state output feeding the cell's own data input through other cells is no combinational cycle. Errors `S0917`: an instance of a cell with a connected
output pin without a function, a three-state pin, a state table, more than one `ff` or `latch`
group, `clocked_on_also`, bus pins, a connected `inout` or internal pin, a function that reads
anything but input pins and the state, or reading problems in its description (LIBERTY-REQ-0021);
an unconnected input pin. A state element has at most one asynchronous control (SYNTH-REQ-0304): an
instance whose `clear` and `preset` are both other than the constant 0 is an error `S0919`, and a
control that is the constant 0 is no control.

**SYNTH-REQ-0912** The state element of a sequential instance of a gate-level netlist is named after
the instance, as the netlist writes its name without the backslash and the terminating blank of an
escaped identifier.

## Report and messages

**SYNTH-REQ-0913** The report (SYNTH-REQ-0703) of a run whose source is not SystemVerilog also
records `source_format` (`blif`, `aiger` or `gate-netlist`), `initial_values_mode` (`reset` or
`arbitrary`), `initial_values` (the declared initial values as `[state name, value]` in state
order), and `clock_port` and `init_port` (the generated ports, when they exist). `state_bits_rtl`
counts the state elements of the source. The phase `read` covers reading the sources and the
phase `elaborate` their lowering into the and-inverter graph.

**SYNTH-REQ-0914** Messages of the BLIF reader (`B` codes, BLIF-REQ-0062), the AIGER reader (`G`
codes, AIGER-REQ-0032) and the structural netlist reader (`V` codes, VERILOG-REQ-0313 to 0316)
pass through with their locations. Codes of this document: `S0910` (formats), `S0911` (options of
SystemVerilog), `S0912` (port names), `S0913` (warning: initial values not implemented), `S0914`
(state names), `S0915` (combinational cycle), `S0916` (unsupported port), `S0917` (cells),
`S0918` (net drivers), `S0919` (two asynchronous controls), `S0920` (module name) and `S0921`
(warning: an undriven signal of a BLIF cell input reads as 0).
