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
| Equivalence | Independent RTL/event interpretation and external CEC in the published large-design campaign; budget exhaustion is not a proof |

The last engine/infrastructure revision before this documentation refresh is
[`b034e62`](https://github.com/lab-emi/RustSys/commit/b034e62207790a6939cfca606a6f5cd297f22328).
The [integration changelog](https://github.com/lab-emi/RustSys/blob/codex/unified-brand/CHANGELOG.md)
records feature details and compatibility limits.

## Retained measurement and proof evidence

Five complete RISC-V designs—Ibex, PicoRV32, SERV, VexRiscv and OpenC910—are
measured on Sky130 HD, GF180 7T and IHP SG13G2/EZ130 8T. All **30 distinct
tool/design/library netlists** pass arbitrary-state RTL equivalence, without
added reset or input assumptions.

The 16-host-core timings include initialization, import, synthesis, mmap and
full netlist export. Proof, STA and physical implementation are excluded.
Setup-Fmax is an ideal-clock, no-parasitics TT estimate; SERV/EZ130 and OpenC910
include loads beyond Liberty characterization. It is not achievable frequency.

[Measurements, paired runtime audit and certificates](guide/benchmarks.md) ·
[Timing interpretation](guide/performance.md#interpret-the-timing-estimate) ·
[Equivalence scope](guide/checking.md)

## Active SRAM development

A separate IHP Ibex checkout is developing opaque SRAM preservation: validated
macro interfaces, bounded input/output cuts and retention of the original macro
instances through mapping. That work is **not part of this integration revision**.
Do not infer SRAM support in this build, or whole-chip DRC/LVS/timing closure,
from the standard-cell benchmarks.

[RustEDA integration progress](https://lab-emi.github.io/RustEDA/engine-status/)
