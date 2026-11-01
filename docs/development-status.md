# Development status

Updated **2026-11-01**. This page describes the source published with the
RustEDA development update. Use the superproject’s exact gitlinks and the
repository lockfile when reproducing a run.

## SRAM-preserving synthesis

Opaque SRAM interfaces are now preserved through elaboration, mapping and
netlist export. Port widths, bus indices and instance/pin identities are checked;
unsupported or inconsistent interfaces produce diagnostics. The macro's behavior
continues to come from its original vendor model for independent checking.

The retained Ibex campaign uses two real IHP SRAMs and proves both synthesis and
final equivalence against the original RTL. The newer native flow records
472.2 s and 472.6 s with all 16 gates; those are whole-flow measurements, not
synthesis-only timings. Sky130 HD and GF180 7T/9T remain the baseline library
coverage; one successful IHP design does not qualify other SRAM configurations.

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
