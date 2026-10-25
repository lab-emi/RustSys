# Contributing to RustSys

RustSys is one of the tools of RustEDA, a suite of electronic design automation tools in Rust that
share their core crates and a disk workspace. This file holds the rules that every change follows:
the eight engineering rules of RustEDA, the rules of this repository, ownership and hand-over
between work items, and the checks to run before sending a change. The phases of the suite and
their work items are in the [scope](https://lab-emi.github.io/RustEDA/verification/).

Instruction files for coding agents, such as `AGENTS.md`, stay local and are never committed; this
file is the tracked statement of the rules.

## Engineering rules

The rules are numbered so that sources, specifications and reviews can cite them, for example as
Rule 7.1.

### Rule 1: Clean room and independent core

1. Never copy or translate source code under the GPL or another restrictive license, and never
   study the internals of a program whose license does not allow it. Algorithms from the
   literature are re-expressed in our own code and cited with a key from
   [`docs/references.bib`](docs/references.bib).
2. Other programs are used only as black boxes over standard file formats (Verilog, BLIF, AIGER,
   Liberty, SDC, SPEF, DEF). There is no code-level differential comparison: their source code is
   never read, diffed against ours, ported, or used to explain or close a difference. Their
   released binaries may be run and compared directly (Rule 8.4).
3. The core crates and the engine and checker crates never depend on Python, machine-learning
   frameworks or user-interface crates. User-interface crates are used only by `gui` and `apps`
   crates; `cargo xtask layout` enforces it.

### Rule 2: Daemonless disk workspace

1. No background daemons, resident servers or blocking prompts on standard input. Every command
   runs to completion or to a deterministic budget, commits or discards its work, and exits with a
   fixed exit status.
2. Every workspace command works on a workspace directory (`-w/--workspace DIR`): it takes a shared
   lock on `workspace.lock` to read and an exclusive lock to commit, and reads binary tables
   through memory maps. Committed revisions are immutable.
3. Every mutating workspace command takes `--base-rev REV` and fails with `REVISION_CONFLICT` when
   the workspace's `HEAD` is another revision. `HEAD` itself is never accepted as a base.

The file-based commands (`rustsys synth`, `rustsys check synth`, `rustsys show`) operate on files
and run directories; work item `WP1` adds workspace revisions alongside them.

### Rule 3: Memory model and data structures

1. No `Rc<RefCell<T>>` graphs: mutable designs use generational typed indices (`InstId`, `NetId`,
   `PinId`, `CellId`) into arenas, so that a stale id fails with an error instead of naming another
   object.
2. Names are interned per design database, never in a process-global interner.
3. Mutable representations stay separate from the dense compute views (structures of arrays and
   compressed sparse rows) that CPU and GPU kernels read. The And-Inverter Graph, the gate-level
   netlist and the other domain graphs are distinct structures, linked by id tables.

### Rule 4: Shell-safe handles and honest status

1. Handles (`rev42`, `path17`, `sel8`, `inst102`) match `[A-Za-z0-9_-]+`: never `#`, `$`, spaces
   or other characters that a shell interprets.
2. A property that was not checked is never reported as passing, and a metric that was not
   computed is never reported as zero: an equivalence proof that stops at its conflict budget is
   reported as `undecided`, never `equivalent` or `error`. Every result states its scope and
   assumptions (`after_reset`, `uninitialized_dont_care`, `assume_full_case`,
   `assume_parallel_case`, electrical corners).

### Rule 5: CPU first, then native GPU backends

1. Every task of `rusteda-compute` first has a deterministic pure-Rust CPU backend. The CPU
   backend is authoritative and it is the default backend.
2. GPU backends are native: CUDA C kernels compiled at run time on Linux (`cuda`), and Metal
   Shading Language kernels compiled at run time on macOS (`metal`). Only `rusteda-compute` uses
   GPU runtime crates (`cudarc`, `objc2-metal`); portable GPU kernel languages are not used. A GPU
   backend that cannot be used is an input error (`S0530`), never a silent fallback to the CPU.
3. Every GPU kernel has a consistency test against the CPU backend, with an explicit bound that
   the compute specification ([`docs/specs/compute/`](docs/specs/compute/)) states. The
   priority-cut task (`rusteda_compute::cut`) uses 32-bit unsigned integer costs and orders,
   producing byte-identical cut tables, choices and emitted netlists across `cpu`, `cuda` and
   `metal` (`SYNTH-REQ-0516`, `COMPUTE-REQ-0103`).
4. `unsafe` code exists only in the GPU glue of `rusteda-compute` (and, once adopted, the memory
   maps of `rusteda-workspace`), under a module-scoped `allow`, with a `// SAFETY:` comment on
   every block. Both the synthesis engine (`rustsys`) and the independent checker (`rustsys-check`)
   forbid unsafe code (`#![forbid(unsafe_code)]`).

### Rule 6: Precision by algorithm, and determinism

1. Layout geometry and design-rule coordinates use integer database units (`i64`), never floating
   point.
2. Cut-based technology mapping ranks cuts by integer arrival times, integer area-flow estimates,
   leaf counts and deterministic topological tie-breaking. Results depend neither on the number of
   threads nor on the compute backend (`cpu`, `cuda`, `metal`), and the same inputs give
   byte-identical netlists.
3. Parallel reductions on a GPU offer a deterministic mode with fixed-point integer atomics or
   reductions in a fixed order.
4. Electrical refinement and delay calculations evaluate NLDM tables deterministically in `f64`.

### Rule 7: No silent drops, and two default PDKs

1. Nothing in an input (SystemVerilog, Liberty, SDC) is dropped silently. A construct is either
   used, or listed in the specification of its reader as having no effect on results (`SYNTH-REQ-0121`),
   or reported: an unsupported or ignored construct produces a diagnostic with its code and source
   location (`SYNTH-REQ-0141`, `CHECK-REQ-0023`).
2. Liberty, technology-mapping and electrical features are tested on the standard cells of both
   default PDKs, `sky130_fd_sc_hd` and `gf180mcu_fd_sc_mcu7t5v0`/`gf180mcu_fd_sc_mcu9t5v0`, so
   that no feature is fitted to one library's naming or table conventions.
3. PDK libraries are fetched at pinned versions, with their SHA-256 sums and sizes recorded, into
   the directory that `RUSTEDA_PDK_DIR` names (default `target/pdk`); they are never committed.
   Tracked design data is synthetic and lives only below a `fixtures/synthetic/` directory.

### Rule 8: Compilation is only the entry ticket

1. Passing `cargo check` and self-written unit tests is not evidence of correctness.
2. No algorithm is validated only by checks written under its own assumptions. The specification
   comes first; the synthesis engine (`rustsys`) and the independent checker (`rustsys-check`) share
   no engine code (`cargo xtask layout` enforces that `verify` crates never depend on `engines`
   crates). Synthesis changes must keep the corpus, random, known-answer, metamorphic, defect and
   dual-PDK suites passing with formal equivalence proven by `rustsys-check`.
3. A change made for performance reports end-to-end wall-clock time, including lowering into
   compute buffers and host-device synchronization, together with its effect on synthesis quality
   (cell count, area, logic depth, estimated delay); isolated kernel times are not enough.
4. Comparisons with other programs run their released binaries as black boxes over standard file
   formats and compare what can be observed from outside: run time, peak memory and the outputs
   (quality of results, timing values, equivalence verdicts). Their harnesses, the pinned program
   versions, inputs fetched by checksum, results and thresholds may be tracked, may name the open
   programs, and may serve as acceptance gates. A program that is not installed leaves its
   comparison `unverified`, which fails under `--require` and is never reported as a pass. The
   specification, not another program, defines correct behavior: a difference is resolved against
   the specification, and a tracked tolerance names the program and its version.

## Repository rules

### Layout and files

- Crates live at `crates/<group>/<name>/`, with the groups `core`, `formats`, `engines`, `verify`,
  `gui` and `apps` and kebab-case names; the top level holds only the entries of an allowlist.
  `cargo xtask layout` checks the crate map (`xtask/src/layout/packages.rs`), top-level paths,
  SPDX headers, file sizes, and the dependency directions between crate groups.
- Files are at most 1 MiB, and Rust sources at most 1,200 lines: split a longer module.
- Every Rust source starts with an SPDX header:

  ```rust
  // SPDX-License-Identifier: Apache-2.0
  // SPDX-FileCopyrightText: 2026 RustSys contributors
  ```

  Files of the shared crates (`rusteda-core`, `rusteda-compute`, `rusteda-formats`) and shared
  automation modules name `2026 RustEDA contributors` instead. Other files take their license and
  copyright from [`REUSE.toml`](REUSE.toml).
- Design data (`.lib`, `.v`, `.sv`, `.sdc` and the like) is tracked only below
  `verification/fixtures/synthetic/`. Synthetic libraries and RTL fixtures are produced by
  `rustsys-fixtures` and regenerated with `cargo xtask fixtures`.
- Scratch output stays in directories that git ignores: `cargo xtask layout` also checks untracked
  files that git does not ignore.

### Lab policy for tracked text

The lab that develops RustEDA applies these rules to every tracked file, and a pre-commit hook in
the lab checks them:

- Names of open third-party EDA programs appear only where a black-box comparison runs them
  (Rule 8.4): its harness, its manifest of pinned versions, its results and their documentation,
  and `docs/references.bib`.
- No names of commercial programs or foundries anywhere, the bibliography included: their licenses
  commonly forbid publishing comparisons, so a comparison with a commercial binary stays local.
- Another program's output is reported as a measured comparison (program, version, difference),
  never as the definition of correctness, and never explained by that program's source code
  (Rules 1.2 and 8.4).
- No absolute paths of a local machine (home directories, mount points) and no paths of local
  scratch directories.
- No name of the repository of another RustEDA tool: tracked text refers to such a tool by its
  role, for example the static timing analysis tool of the suite.

### Specifications and requirements

- Every behavior starts as a specification in `docs/specs/<area>/` with requirement IDs of the
  form `<AREA>-REQ-<nnnn>` (for example `SYNTH-REQ-0516`). Tests and sources cite the IDs they
  implement or verify, and `cargo xtask coverage` rewrites `docs/verification/requirements.md`.
- Shared code and shared specifications use neutral single-token prefixes (`CORE`, `COMPUTE`,
  `VERILOG`, `LIBERTY`, `SDC`, `JOB`), never the prefix of one tool.
- Specifications cite standards and papers with keys from [`docs/references.bib`](docs/references.bib);
  manuals of commercial programs are not cited.
- Architecture decisions are recorded in [`docs/adr/`](docs/adr/).

### Shared files

- `xtask/src/shared.rs` lists the shared crates, specifications and automation files whose SHA-256
  sums are recorded in `verification/shared.sha256`.
- After changing a shared file, run `cargo xtask shared update` and verify with
  `cargo xtask shared check`.

### Dependencies

- Dependencies come from crates.io only, are pinned in the root `[workspace.dependencies]`, and
  must pass `cargo deny check`.

## Ownership and hand-over

Work is divided into work items (see the [scope](https://lab-emi.github.io/RustEDA/verification/)), each with its specification,
its acceptance criteria and the paths it owns.

- Every path has one owner at a time. A work item edits only the paths it owns, plus minimal
  call-site or manifest updates where required.
- The synthesis engine (`rustsys`) and the independent checker (`rustsys-check`) are maintained
  independently and share no synthesis or checking code (Rule 8.2).

## Checks before sending a change

Run from the repository root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo xtask layout
cargo xtask coverage                    # rewrites docs/verification/requirements.md
cargo xtask shared update               # only after changing a shared file
cargo xtask shared check                # verifies verification/shared.sha256
cargo xtask fixtures
git status --porcelain -- verification/fixtures   # no output: the committed fixtures are fresh
cargo xtask verify synth
cargo deny check
reuse lint
```

On a machine with an NVIDIA GPU and CUDA toolkit (including NVRTC), also run the GPU verification
suites, and when the open PDK libraries are installed in `RUSTEDA_PDK_DIR` (or `target/pdk`), run
the dual-PDK suite:

```bash
cargo xtask verify synth --gpu
cargo xtask verify pdk --require
```

## Commits

- Conventional Commits, for example `feat(synth): map async-reset registers (SYNTH-REQ-0310)`.
- The body explains what changed and why, in terms of specifications and tests.
- Contributions are accepted under the Developer Certificate of Origin: sign off every commit
  (`git commit -s`).


### Canonical first-party foundation (2026-10-25)

The four shared library implementations and the workspace CLI implementation are owned by
lab-emi/RustEDA-Common, the public first-party foundation. Workspace dependencies and Cargo.lock
pin one exact revision over HTTPS; no GitHub credentials are required. The local rusteda-cli package is a
product adapter and retains its package version in command output and revision history.

This supersedes the legacy requirement to copy shared implementation files between tool
repositories, and is the sole first-party exception to the crates.io-only source policy.
Third-party package versions, licenses and source restrictions are unchanged. Shared specification
and automation manifests still require deliberate updates. Canonical core tests run in the
foundation repository; product adapters and engines retain their local tests.

Cross-repository validation uses the pinned foundation source. The historical public main
branch retains its existing build policy until a reviewed migration is merged.
