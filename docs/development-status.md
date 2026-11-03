# Development status

Updated **2026-11-02**. This page describes the source published with the
RustEDA development update. Use the superproject’s exact gitlinks and the
repository lockfile when reproducing a run.

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
(1.4052 ps against a 1 ps tolerance), which does not involve synthesis, so the
receipt's status is `comparison_incomplete` and its end-to-end ratios are null.
The run is one timed sample with all 4 vCPUs and the recipe's 8 GiB memory cap.
The EZ130 SG13G2 technology LEF and cell GDS were reconstructed from public
sources, and the reference tools are nixpkgs builds, including Yosys 0.70
instead of LibreLane's pinned Yosys 0.62.

The campaign's synthesis head-to-head maps the same prepared Ibex RTL (sv2v
0.0.13.1 output) onto the same EZ130 TT and SRAM Liberty. RustSys runs as the
native flow's workspace sequence (init, import, synth, export at 2 threads) and
in single-process direct mode; Yosys 0.70 + ABC runs LibreLane 3.0.17's
synthesis script. Each contender had one warm-up and three measured runs,
interleaved. RustSys had the shorter median wall time in both modes and the
lower peak RSS (largest process, GNU time). Its netlist is larger in
standard-cell area and deeper, with slightly fewer cells. Standard-cell area
comes from `yosys stat -liberty`; logic depth is the longest combinational cell
path between registers, macros and ports, computed by the harness on Yosys's
JSON netlist, both counting every cell and excluding buffer, inverter and delay
cells.

The equivalence verdicts come from the flows' synthesis gates, not from the
head-to-head netlists. The native flow's RustSys netlist is `proven`; the
head-to-head RustSys netlists have the same cells, differ from it only in SRAM
port-connection syntax and were not checked separately. LibreLane's synthesis
netlist, byte-identical to the head-to-head Yosys netlist, is `unverified`
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

The published benchmark is the Ibex RV32 core with two real IHP SRAMs on IHP
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

The final isolated workspace passes 475 Rust tests (6 ignored), formatting,
all-target/all-feature Clippy and rustdoc. Layout, 37 shared-file hashes and
fixture regeneration checks pass; coverage is 621 requirements with zero gaps.
All 120 existing benchmark/oracle harness controls pass. The negative SDC test
uses an explicitly unsupported cell-check derate qualifier now that global
derates are supported. Macro interface validation moved into a private module
with its validation body byte-identical, preserving the public API and behavior.
