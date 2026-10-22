# ADR 0002: GPU runtimes of the cut passes and native CUDA / Metal

- Status: superseded by ADR 0004 and `docs/specs/compute/README.md`
- Date: 2026-09-28; superseded 2026-10-16 (migrated to `rusteda-compute` and native `cuda`/`metal` runtimes)
- Note (2026-10-17): the `metal` runtime that this record describes was never implemented; only
  `cpu` and `cuda` exist, and Metal is planned for Phase 4 (`docs/specs/compute/decisions.md`).

## Context
RustSys offloads the delay and area-flow cut passes of its standard-cell mapper to GPUs
(SYNTH-REQ-0516). Its first version had three GPU runtimes in a repository-local GPU crate: two
portable runtimes and a third implementation written directly in CUDA C, which made it possible to
measure what the portable path cost and to use the faster one where it mattered. The RustEDA
architecture (ADR 0004) then consolidated GPU runtime selection and compute-heavy kernels behind
coarse-grained Rust traits in the shared crate `rusteda-compute`, with native backends: `cpu` (pure
Rust, always built), `cuda` on Linux and `metal` on Apple Silicon.

## Decision
- `rusteda-compute` (`crates/core/rusteda-compute`), shared across the RustEDA workspace layout,
  owns runtime selection, the per-process device self-test, the CUDA and Metal device layers and
  the priority-cut task (`rusteda_compute::cut`, COMPUTE-REQ-0001 to 0004, COMPUTE-REQ-0101 to 0106).
- The GPU runtimes are `cuda` (Cargo feature `cuda`, kernels written in CUDA C, compiled at run
  time by NVRTC for the device's architecture and launched through the CUDA driver API,
  COMPUTE-REQ-0002) and `metal` (Cargo feature `metal`, kernels written in Metal Shading Language,
  compiled at run time by `MTLDevice` and dispatched on `MTLCommandQueue`, COMPUTE-REQ-0004). Host
  CUDA code uses the `cudarc` crate (MIT OR Apache-2.0), pinned to an exact version, which loads
  the CUDA driver and NVRTC libraries at run time. Kernels stay stable-Rust-buildable: no nightly
  compiler, no `nvcc` at build time and no device-code crates.
- The cut passes use 32-bit unsigned integers only, so every runtime (`cpu`, `cuda`, `metal`) emits
  the netlist of the CPU passes byte for byte (SYNTH-REQ-0516, COMPUTE-REQ-0103). Tests run every
  compiled runtime against the CPU results.
- Unsafe code is confined to backend glue in `rusteda-compute` (`src/cuda.rs`, `src/metal.rs`,
  `src/cut/cuda.rs`, `src/cut/metal.rs`), each block with a `// SAFETY:` comment (AGENTS rules 8.4);
  `crates/engines/rustsys` has `#![forbid(unsafe_code)]`.
- With a GPU backend, synthesis prepares the runtime and compiles the kernels on a second thread
  while the design is read and optimized (COMPUTE-REQ-0003, SYNTH-REQ-0518).

## Consequences
- Priority-cut enumeration and selection live in `rusteda_compute::cut::Session` behind a pure-Rust
  host interface; `rustsys` contains no GPU kernel sources or `unsafe` blocks.
- The `cuda` runtime needs the CUDA driver and the NVRTC library of a CUDA toolkit at run time;
  without them the runtime fails its preparation and reports an input error (`S0530`).
- Earlier run records still load: the former native CUDA runtime alias deserializes as `cuda`, and
  the removed portable runtime variant is retained in `rustsys_cli::proto` for historical run
  records (PROTO-REQ-0062).
