# ADR 0004: RustEDA crate map and shared foundation crates

- Status: accepted
- Date: 2026-10-16; amended 2026-10-17 (adopted `rusteda-workspace` and `rusteda-cli`; merged
  `rustsys-proto` into `rustsys-cli`)
- Note (2026-10-17): the Metal device layer (`metal`) and the `rusteda session` command named below
  do not exist; `rusteda-compute` has the `cpu` and `cuda` runtimes, and Metal and session export
  are planned (development plan, Phases 3 and 4).

## Context
RustSys originally named its foundation and format crates with a repository-specific prefix and split
the SystemVerilog and Liberty readers into separate workspace crates alongside a repository-local job
crate and GPU crate. To share foundational data models, workspace persistence, file format readers,
and compute kernels cleanly across the RustEDA family while keeping repository-specific synthesis,
verification, and CLI crates clearly scoped, the workspace needs a uniform crate map and a
deterministic integrity check for shared files.

## Decision
- Foundation, workspace, format, and shared CLI crates use the `rusteda-*` prefix and live at fixed
  workspace paths:
  - `rusteda-core` (`crates/core/rusteda-core`): engine run contexts, cancellation tokens, progress
    events and coded messages (`job`, JOB-REQ), source diagnostics (`diag`, CORE-REQ), the Liberty
    library model (`liberty`, LIBERTY-REQ), interned symbols (`symbol`), typed index arenas
    (`arena`), hierarchical design names (`names`), and the flat gate-level netlist (`netlist`,
    CORE-REQ-0100 to 0138).
  - `rusteda-compute` (`crates/core/rusteda-compute`): GPU runtime selection (`cpu`, `cuda`,
    `metal`), the per-process device self-test, the CUDA driver / NVRTC layer (`cuda`), the Metal
    device layer (`metal`), batched timing propagation (`timing`, COMPUTE-REQ-0010 to 0016), and
    priority-cut enumeration and selection (`cut`, COMPUTE-REQ-0001 to 0004, COMPUTE-REQ-0101 to
    0106).
  - `rusteda-workspace` (`crates/core/rusteda-workspace`): workspace layout (`manifest.json`,
    `workspace.lock`, `history.jsonl`, `ui_state.json`), immutable revisions (`rev<N>`), the `.rbin`
    binary container (`RBIN`, `NETLIST v1`, `RLIB v1`, `CONSTR v1`), handle validation, and the
    `rusteda.v1` JSON envelope (WS-REQ).
  - `rusteda-formats` (`crates/formats/rusteda-formats`): the SystemVerilog preprocessor, parser,
    structural netlist reader and writer (`verilog`, VERILOG-REQ), the Liberty syntax tree and model
    builder (`liberty`, LIBERTY-REQ), and the Synopsys Design Constraints reader (`sdc`, SDC-REQ).
  - `rusteda-cli` (`crates/apps/rusteda-cli`): the shared `rusteda` workspace command (`init`,
    `import`, `log`, `show`, `diff`, `revert`, `export`, `session`, RCLI-REQ).
- Tool-specific crates use the `rustsys` prefix and match their directory names:
  - `rustsys` (`crates/engines/rustsys`): logic synthesis and standard-cell mapping engine.
  - `rustsys-check` (`crates/verify/rustsys-check`): independent synthesis equivalence checker.
  - `rustsys-fixtures` (`crates/verify/rustsys-fixtures`): synthetic library and RTL generators.
  - `rustsys-defects` (`crates/verify/rustsys-defects`): fault injection for checker verification.
  - `rustsys-cli` (`crates/apps/rustsys-cli`): the `rustsys` command-line library and binary,
    including the job protocol (`src/proto/`) and workspace-first commands (`src/ws/`).
- `cargo xtask layout` checks that every workspace package name and directory path matches this map
  and that crate dependencies obey the layer DAG (`core` <- `formats` <- `engines`/`verify` <-
  `gui` <- `apps`).
- `cargo xtask shared check` (and `cargo xtask shared update`) records SHA-256 digests in
  `verification/shared.sha256` so shared automation and foundation files stay verified.

## Consequences
- `crates/engines/rustsys` and `crates/verify/rustsys-check` both have `#![forbid(unsafe_code)]`;
  all GPU FFI and kernel launches live in `rusteda-compute`, and `mmap` lives in
  `rusteda-workspace`.
- Format readers are imported as `rusteda_formats::{liberty, sdc, verilog}` and shared core types as
  `rusteda_core::{arena, diag, job, liberty, names, netlist, symbol}`.
