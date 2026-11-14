# Development status

Updated **2026-11-13**. The 2026-11-13 section reports the RustEDA host campaign
of that date and the dated sections under it the earlier campaigns and
development updates; the rest of this page describes the source published with
the RustEDA development update. Use the superproject’s exact gitlinks and the
repository lockfile when reproducing a run.

## Ibex host campaign · 2026-11-13

The RustEDA Ibex host campaign of 2026-11-13 reran the IHP SG13G2 / EZ130
design with two real SRAMs on a 4-vCPU cloud host (a Xeon reporting 2.10 GHz
with 15.7 GiB RAM, again a different machine from 2026-11-05's), with RustSys
built from commit [`86187e6`](https://github.com/lab-emi/RustSys/commit/86187e69c3111ad79ae4b651ad53d652b7609867)
(executable SHA-256 `d84e0810…`) and every engine built with Rust 1.100.0.
RustSys's changes since `86e59d8` are toolchain and maintenance commits (Rust
1.100.0 instead of 1.97.1); the flow's synthesis netlist is byte-identical to
2026-11-05's and the head-to-head QoR is unchanged. The native step graph ran
for the first time under the Rust executor `rusteda flow exec` instead of the
Python `producer.py`. RustSys synthesized
the design in the native flow, the flow's synthesis and final equivalence gates
are `proven`, and the native layout passes every shared gate that gives a
pass/fail verdict; the producer completed all 114 manifest steps. The run is
`unqualified` (the component-qualification evidence is not published), and
LibreLane's layout fails the timing gate, so the receipt keeps its ratio fields
null; divided, the recorded times give RustEDA a 4.4× shorter end-to-end time.

In the synthesis head-to-head on the same prepared RTL and Liberty, with Yosys +
ABC running LibreLane's synthesis script, **RustSys was faster, used less
memory and produced the smaller and shallower netlist** (one warm-up and three
measured runs per contender, interleaved, on this task and host):

| | RustSys | Yosys 0.70 + ABC, LibreLane script |
| --- | ---: | ---: |
| Median wall, workspace sequence (fastest) | 3.07 s (3.06 s) | 103.91 s (102.47 s) |
| Median wall, direct mode (fastest) | 2.19 s (2.05 s) | — |
| Quotient, Yosys ÷ RustSys | 33.8× (sequence), 47.4× (direct) | — |
| Worst peak RSS, sequence / direct | 116,264 / 112,660 KiB | 765,624 KiB |
| Standard-cell area (`yosys stat -liberty`) | 145,226.592 µm² (-1.79 %) | 147,866.947 µm² |
| Area including both SRAM macros | 425,591.9808 µm² | 428,232.336 µm² |
| Logic depth in cells (excluding buffers) | 36 (36) | 40 (40) |
| Cells: total / sequential / buffer and inverter / other / macro | 11,433 / 1,998 / 950 / 8,483 / 2 | 12,605 / 1,905 / 186 / 10,512 / 2 |
| Equivalence of each flow's synthesis netlist to RTL (shared gate) | `proven` | `unverified` |

Inside the two flows, RustSys's synthesis stage took 3.10 s against 124.93 s for
LibreLane's Yosys + ABC stage (40.3×). The equivalence verdicts come from the
flows' shared synthesis gates: the native flow's synthesis netlist is `proven`,
and LibreLane's synthesis netlist, byte-identical to the head-to-head Yosys
netlist, is `unverified` (433 renamed registers the checker cannot
name-match), which is not evidence of non-equivalence. The shared synthesis +
final equivalence gate took 19.41 s on RustEDA's layout against 15.39 s on
LibreLane's, but on LibreLane's layout neither check completes, so the two
times are not the same work and the campaign records no valid comparison.

Against 2026-11-05, as history and not as a speedup: this host is a different
virtual machine, on which every LibreLane stage took less time (floorplan
level) with byte-identical DEF and netlists. The workspace sequence took
3.07 s against 3.81 s then and Yosys 103.91 s against 135.04 s, so the sequence
multiple is 33.8× against 35.4×; in the flows, synthesis took 3.10 s against
3.48 s on the RustEDA side and 124.93 s against 132.63 s on LibreLane's.

The method is that of 2026-11-05 below, with the same task and tools: RustSys
runs the native flow's workspace sequence (init, import, synth, export at 2
threads) and its direct mode (`--threads 0`, all 4 CPUs); each contender had one
warm-up and three measured runs, interleaved; area comes from
`yosys stat -liberty` and logic depth from the harness's longest combinational
cell path.

[Synthesis head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys-2026-11-13) ·
[Host campaign 2026-11-13](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-13) ·
[Shared gates](https://lab-emi.github.io/RustEDA/benchmarks/#shared-gates-2026-11-13) ·
[Losses and parity](https://lab-emi.github.io/RustEDA/benchmarks/#where-rusteda-still-loses-or-is-at-parity-2026-11-13) ·
[Method and deviations](https://lab-emi.github.io/RustEDA/benchmarks/#host-campaign-method-and-deviations-2026-11-13)

## Elaboration update · 2026-11-06

Named generate blocks are now hierarchy scopes. Elaboration resolves
`block.signal`, `block[i].signal`, nested `block.inner.signal`,
`block.instance.port`, top-qualified `top.block.signal` and current-module
absolute names such as `leaf.signal` inside `leaf`. Generate-loop containers
retain their iteration scopes, so a constant genvar selects the right branch.
`RUSTSYS_ELAB_TRACE=1` emits per-instance and per-generate timings for
elaboration performance work. This covers the shape emitted by sv2v 0.0.13 when it inlines an
interface-bearing module into a generate block. The specification is
`docs/specs/synth/frontend.md` (SYNTH-REQ-0123) and the tests are in
`crates/engines/rustsys/tests/elab_semantics.rs`.

## PULPissimo IHP full-SoC synthesis · 2026-11-06

A synthesis-only full-PULPissimo test maps the L2 banks to 80 official IHP
`RM_IHPSG13_1P_1024x32_c2_bm_bist` macros and uses EZ130 8T TT cells. One
16-thread run completes in **426.343 s** at 1,316,868 KiB peak RSS, producing
161,034 standard cells plus 80 macros, 2,045,003.79 µm² of logic and
11,214,615.55 µm² of macro area (13,259,619.34 µm² total) at logic depth 78.
The merge pass is bounded; there is no equivalence, timing-closure, placement,
routing or GDS result. The behavioral-memory SKY130 full SoC still exceeds 2 h.
See the [IHP full-SoC test](https://lab-emi.github.io/RustEDA/benchmarks/pulpissimo-ihp/).

## Ibex host campaign · 2026-11-05

The RustEDA Ibex host campaign of 2026-11-05 reran the IHP SG13G2 / EZ130
design with two real SRAMs on a 4-vCPU cloud host (a different and slower
machine than on 2026-11-02), with RustSys built from commit [`86e59d8`](https://github.com/lab-emi/RustSys/commit/86e59d816e6ded82333ccc502bb37961c3bb1a9f) (executable SHA-256 `49afd8c8…`).
RustSys synthesized the design in the native flow, the flow's synthesis and
final equivalence gates are `proven`, and the native layout passes every shared
gate that gives a pass/fail verdict; the producer completed every step. The run is `unqualified` (the
component-qualification evidence is not published), and LibreLane's layout
fails the timing gate, so the receipt keeps its ratio fields null; divided, the
recorded times give RustEDA a 4.2× shorter end-to-end time.

In the synthesis head-to-head on the same prepared RTL and Liberty, with Yosys +
ABC running LibreLane's synthesis script, **RustSys was faster, used less
memory and produced the smaller and shallower netlist** (three measured runs per
contender on this task and host):

| | RustSys | Yosys 0.70 + ABC, LibreLane script |
| --- | ---: | ---: |
| Median wall, workspace sequence (fastest) | 3.81 s (3.62 s) | 135.04 s (126.82 s) |
| Median wall, direct mode (fastest) | 3.04 s (2.75 s) | — |
| Worst peak RSS, sequence / direct | 116,200 / 115,052 KiB | 766,036 KiB |
| Standard-cell area (`yosys stat -liberty`) | 145,226.592 µm² | 147,866.947 µm² |
| Logic depth in cells, excluding buffers | 36 (36) | 40 (40) |
| Cells: total / sequential / buffer and inverter | 11,433 / 1,998 / 950 | 12,605 / 1,905 / 186 |

Yosys took 35.4× RustSys's median wall time for the workspace sequence and
44.4× for the direct mode. RustSys's netlist has 1.79 % less standard-cell area
than Yosys's and 36 against 40 logic levels; on 2026-11-02 it was 2.18 % larger
and 55 levels deep. RustSys itself is slower than on 2026-11-02 (3.81 s against
2.12 s for the sequence), more than the host change explains (Yosys: 135.04 s
against 102.83 s): its quality changes cost work (the changelog records 29.0 G
instead of 22.0 G instructions for a single-thread Ibex synthesis). The
equivalence verdicts come from the flows' synthesis gates: the native flow's
RustSys netlist is `proven`, and LibreLane's synthesis netlist, the same bytes
as the head-to-head Yosys netlist, is `unverified` (433 registers lack a unique
named RTL counterpart for the checker), which is not evidence of
non-equivalence. The head-to-head RustSys netlist differs in bytes from the
flow's and was not checked separately.

The method is that of 2026-11-02 below: RustSys runs the native flow's
workspace sequence (init, import, synth, export at 2 threads) and its direct
mode (`--threads 0`, all 4 CPUs); each contender had one warm-up and three
measured runs, interleaved; area comes from `yosys stat -liberty` and logic
depth from the harness's longest combinational cell path.

[Synthesis head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys-2026-11-05) ·
[Host campaign 2026-11-05](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-05)

## Ibex host campaign · 2026-11-02

The RustEDA host campaign reran Ibex with two real IHP SRAMs on IHP SG13G2 /
EZ130 8T on a 4-vCPU Intel Xeon cloud host (15.7 GiB RAM, no swap), with
RustSys built from commit
[`0f8a2d8`](https://github.com/lab-emi/RustSys/commit/0f8a2d80654def4d91d08d6f3791c12726339a53)
(executable SHA-256 `a6507ecc…`), the SRAM-preserving source described below.
RustSys synthesized the design in the native flow, and the shared gate scripts
report synthesis and final equivalence `proven`. On those scripts the native
layout passes every design gate. The run is `unqualified`: the
component-qualification evidence that the 16-gate composer needs is not
published. Its producer stopped at the RustSTA/OpenSTA `timing_accuracy` gate
(1.4052 ps against a 1 ps tolerance), which does not involve synthesis.
LibreLane's layout fails the timing gate and its equivalence is `unverified`,
so neither flow passes every gate: the receipt's status is
`comparison_incomplete` and its end-to-end ratios are null.
The run is one timed sample with all 4 vCPUs and the recipe's 8 GiB memory cap.
The EZ130 SG13G2 technology LEF and cell GDS were reconstructed from public
sources, and the reference tools are nixpkgs builds, including Yosys 0.70
instead of LibreLane's pinned Yosys 0.62.

The campaign's synthesis head-to-head maps the same prepared Ibex RTL (sv2v
0.0.13.1 output) onto the same EZ130 TT and SRAM Liberty. RustSys runs as the
native flow's workspace sequence (init, import, synth, export at 2 threads) and
in single-process direct mode (`--threads 0`, all 4 CPUs); Yosys 0.70 + ABC
runs LibreLane 3.0.17's synthesis script on the same unpinned 4-vCPU host. Each contender had one warm-up and three measured runs,
interleaved. RustSys had the shorter median wall time in both modes and the
lower peak RSS (largest process, GNU time). Its netlist is larger in
standard-cell area and deeper, with slightly fewer cells. Standard-cell area
comes from `yosys stat -liberty`; logic depth is the longest combinational cell
path between registers, macros and ports, computed by the harness on Yosys's
JSON netlist, both counting every cell and excluding buffer, inverter and delay
cells.

The equivalence verdicts come from the flows' synthesis gates, not from the
head-to-head netlists. The native flow's RustSys netlist is `proven`; the
head-to-head RustSys netlists differ in bytes from it and were not checked
separately. LibreLane's synthesis netlist is `unverified`
because 433 of its registers lack a unique named RTL counterpart for the
checker; that is not evidence of non-equivalence. The measured values, their scope and the host's
deviations are on the RustEDA benchmarks page; they come from one host and
apply to this task only.

[Synthesis head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys) ·
[Host campaign, method and deviations](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-02) ·
[RustSys in the whole-flow benchmark](guide/benchmarks.md)

## SRAM-preserving synthesis

Opaque SRAM interfaces are now preserved through elaboration, mapping and
netlist export. Port widths, bus indices and instance/pin identities are checked;
unsupported or inconsistent interfaces produce diagnostics. The macro's behavior
continues to come from its original vendor model for independent checking.

The retained R20/R21 development runs of 2026-11-01 use two real IHP SRAMs and
prove both synthesis and final equivalence against the original RTL. They
record 472.2 s and 472.6 s with all 16 gates; those are whole-flow
measurements, not synthesis-only timings. Their recipe inputs are not in the
repository, so the 2026-11-02 host campaign ran the published run16-class recipe
instead. Sky130 HD and GF180 7T/9T remain the baseline library coverage; one
successful IHP design does not qualify other SRAM configurations.

The bounded native equivalence checker and external CEC retain arbitrary initial
state and explicit work limits. A budget exhaustion is not an equivalence proof.

[Whole-flow evidence and measurement scope](https://lab-emi.github.io/RustEDA/benchmarks/) ·
[RustEDA engine status](https://lab-emi.github.io/RustEDA/engine-status/)

## Earlier qualification record

The following measurements describe the earlier source revisions named here.

## Integrated capabilities

| Area | Available in the integration |
| --- | --- |
| Inputs | Supported Verilog/SystemVerilog, BLIF, AIGER and mapped gate-level netlists |
| Synthesis | Bounded AIG optimization and cut-based technology mapping; explicit diagnostics and work/memory budgets |
| Agent workflow | Recorded commands, hashes, reports and events; revision-guarded workspace synthesis, resize and timing optimization |
| Shared foundation | RustEDA Common owns the shared IR, parsers, workspace tables and compute interfaces |
| Equivalence | Independent RTL/event interpretation and external CEC in the published benchmark campaign; budget exhaustion is not a proof |

The last engine/infrastructure revision before this documentation refresh is
[`b034e62`](https://github.com/lab-emi/RustSys/commit/b034e62207790a6939cfca606a6f5cd297f22328).
The [integration changelog](https://github.com/lab-emi/RustSys/blob/codex/unified-brand/CHANGELOG.md)
records feature details and compatibility limits.

## Benchmark and proof evidence

The primary RTL-to-GDS benchmark is the Ibex RV32 core with two real IHP SRAMs on IHP
SG13G2 with the ETH EZ130 8T library, run end to end through the native RustEDA
flow and compared with LibreLane on the
[RustEDA benchmarks page](https://lab-emi.github.io/RustEDA/benchmarks/).
RustSys synthesizes the Ibex + SRAM design in that flow; the flow's synthesis
and final CEC gates prove the synthesized and final netlists equivalent to the
original RTL.

The earlier synthesis-only campaign proved all **six RustSys/Yosys Ibex
netlists** on Sky130 HD, GF180 7T and IHP SG13G2/EZ130 8T equivalent to their
RTL with arbitrary initial state, without added reset or input assumptions. Its
timing and quality receipts are historical archives, not current benchmark results.

[RustSys in the whole-flow benchmark](guide/benchmarks.md) ·
[Equivalence scope](guide/checking.md)

## Publication checks

For the 2026-11-01 publication, the final isolated workspace passed 475 Rust
tests (6 ignored), formatting, all-target/all-feature Clippy and rustdoc.
Layout, 37 shared-file hashes and fixture regeneration checks passed; coverage
was 621 requirements with zero gaps. All 120 existing benchmark/oracle harness
controls passed. The negative SDC test uses an explicitly unsupported
cell-check derate qualifier now that global derates are supported. Macro
interface validation moved into a private module with its validation body
byte-identical, preserving the public API and behavior. These checks were not
rerun for the 2026-11-02 update, whose host campaign section is measurement
evidence from the RustEDA host receipt.
