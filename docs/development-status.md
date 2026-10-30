# Development status

Updated **2026-10-28** for the RustEDA integration branch,
[`codex/unified-brand`](https://github.com/lab-emi/RustSys/tree/codex/unified-brand).
The standalone `main` branch can lag this snapshot. RustSys remains a source
preview; use RustEDA's recursive checkout to reproduce its exact module pin.

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

## Active SRAM development

A separate IHP Ibex checkout is developing opaque SRAM preservation: validated
macro interfaces, bounded input/output cuts and retention of the original macro
instances through mapping. That work is **not part of this integration revision**.
Do not infer SRAM support in this build from the whole-flow Ibex benchmark.

[RustEDA integration progress](https://lab-emi.github.io/RustEDA/engine-status/)
