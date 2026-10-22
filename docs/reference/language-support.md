# Language and library support

RustSys implements a documented synthesizable subset of SystemVerilog. This page is a practical boundary; the [front-end](../specs/synth/frontend.md), [semantics](../specs/synth/semantics.md) and [sequential](../specs/synth/sequential.md) specifications are authoritative.

## RTL constructs

| Area | Supported examples |
|---|---|
| Design structure | Modules, packages, value/type parameters, hierarchy, generate conditions and loops. |
| Types | Packed/unpacked arrays, `typedef`, enumerations, packed structures and unions. |
| Logic | Continuous assignments, expressions, gate primitives, casts, assignment patterns and common constant system functions. |
| Procedures | `always_comb`, `always_ff`, `always_latch`, `always @*`, ordinary clocked processes, functions and tasks. |
| Control | Constant-bounded loops, `case`, `casez`, `casex`, `case inside`, `unique`, `unique0`, and `priority`. |
| Preprocessing | Includes, conditional compilation, object/function-like macros and explicit definitions. |
| State | Flip-flops and latches, with supported clock/enable behavior and optional asynchronous clear or preset. |

Examples of constructs outside this version's synthesis boundary include interfaces/modports, arrays of instances, `defparam`, tri-state logic, `inout`, asynchronous load, registers with two asynchronous controls, real numbers and dynamic data types. Unsupported semantic constructs produce diagnostics; do not expect a general event-driven SystemVerilog simulator.

## Two-valued interpretation

Ordinary `x` and `z` data bits become zero. Undriven RTL bits also read as zero and produce diagnostics where specified. `casez`, `casex`, wildcard equality and `inside` retain wildcard matching semantics; concatenation or replication does not erase a wildcard mask.

Registers have arbitrary initial values by default. A declared reset describes circuitry, and does not imply that reset occurred before observation. Optional reset and uninitialized-state flags change that contract and are recorded explicitly.

`unique`, `priority`, `full_case` and `parallel_case` annotations do not change the default logic. `--assume-full-case` and `--assume-parallel-case` opt into their specified synthesis assumptions. See [checking scopes](../guide/checking.md#state-and-language-scope).

## Standard-cell libraries

Mapping uses Liberty descriptions of single-output combinational cells with up to six inputs, flip-flops/latches with the supported asynchronous controls, and tie cells. The library's Boolean functions define logical behavior. Unsupported cells remain listed in `unused_cells` with reasons; malformed or semantically unsupported library constructs produce explicit diagnostics.

The checker has a stricter structural model boundary. It rejects unknown netlist literals, resolved multiple-driver nets, tri-state/electrical drive behavior, unsupported sequential groups and unsupported Liberty state descriptions. It assumes cells are powered and reports excluded supply ports. Read the [checker model](../specs/check/synth.md#netlist-model) before importing arbitrary third-party cell models.

Baseline technology coverage uses both `sky130_fd_sc_hd` and `gf180mcu_fd_sc_mcu7t5v0`/`gf180mcu_fd_sc_mcu9t5v0`. This is coverage of specific tested libraries, not a claim that every Liberty feature or every library works. See the [PDK verification matrix](../verification/pdk.md).

## Other formats and constraints

BLIF, AIGER and structural gate-netlist inputs are supported through explicit format contracts. See [source formats](../specs/synth/inputs.md), [BLIF](../specs/blif/README.md), [AIGER](../specs/aiger/README.md) and the [structural Verilog subset](../specs/verilog/netlist.md).

The workspace can import SDC constraints. The reader has an explicit supported subset and diagnostics for unsupported constraints; importing an SDC file does not imply full timing analysis. Standalone synthesis timing knobs use uniform arrivals/loads and ideal-clock estimates. See the [SDC specification](../specs/sdc/sdc.md) and [timing guide](../guide/gpu-and-timing.md).
