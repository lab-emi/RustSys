# rusteda-compute

Compute tasks of the RustEDA tools (`docs/specs/compute/`, COMPUTE-REQ-0001 to COMPUTE-REQ-0029 and
COMPUTE-REQ-0101 to COMPUTE-REQ-0106; RustEDA shared-crate contract, section 1; development plan,
rule 5). Every task sits behind a coarse-grained trait with a pure Rust CPU backend (always compiled
and authoritative) and native GPU backends checked for consistency with it. All `unsafe` code in
this crate is confined to the GPU glue modules `cuda`, `timing::cuda` and `cut::cuda` (AGENTS
rules 8.4), each block with its `// SAFETY:` justification. There is no Metal backend yet
(development plan, Phase 4; `docs/specs/compute/decisions.md`).

- `cut` (`docs/specs/compute/README.md`, COMPUTE-REQ-0101 to COMPUTE-REQ-0106): priority-cut
  enumeration and selection (`cut::Session`, `CutPassProblem`) on the `cpu` and `cuda` backends
  with bit-exact 32-bit integer arithmetic across backends.
- `timing` (`docs/specs/compute/timing.md`, COMPUTE-REQ-0010 to COMPUTE-REQ-0029): levelized
  arrival, transition and predecessor propagation (`TimingPropagationBackend`, `TimingSession`,
  `timing_backend`, `prewarm`) on the `cpu` (`f64`) and `cuda` (`f64` bit-identical to `cpu`, and
  opt-in `f32`) backends.
- `BackendKind` (`cpu`, `cuda`, `gpu`), `Precision` (`f64`, `f32`) and `ComputeError` (mapped to
  `rusteda.v1` envelope codes and diagnostics `A0501` to `A0506`; COMPUTE-REQ-0005,
  COMPUTE-REQ-0025).
- `GpuRuntime`, `GpuTask`, `dispatch` and `prepare`: GPU runtime selection and per-process device
  self-test for engine kernels (COMPUTE-REQ-0001, COMPUTE-REQ-0003).
- `cuda` (feature `cuda`): CUDA device layer opening the default device once per process, compiling
  CUDA C kernels with NVRTC (cached per source and option list), allocating device and page-locked
  host buffers, and launching kernels through the CUDA driver API (COMPUTE-REQ-0002,
  COMPUTE-REQ-0004). The CUDA driver and NVRTC libraries are loaded at run time; builds need no
  CUDA toolkit.
