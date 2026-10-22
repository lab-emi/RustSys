<p align="center">
  <img src="docs/assets/rustsys-wordmark.svg" alt="RustSys — a RustEDA tool" width="360">
</p>

# RustSys

**Verilog in. Standard cells out. A CLI your scripts and agents can drive.**

[![CI](https://github.com/lab-emi/RustSys/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/lab-emi/RustSys/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust 1.97.1](https://img.shields.io/badge/rust-1.97.1-orange.svg?logo=rust)](rust-toolchain.toml)
[![Documentation](https://img.shields.io/badge/docs-online-57D6BE)](https://lab-emi.github.io/RustSys/)

RustSys turns synthesizable SystemVerilog and Liberty libraries into a gate-level Verilog
netlist and a JSON report. Synthesize, check equivalence, inspect results, and repeat from your
terminal, CI job, or coding agent. It is the logic synthesis tool in the **RustEDA** family.

<picture>
  <source media="(prefers-reduced-motion: reduce)" srcset="docs/assets/rustsys-banner.svg">
  <img src="docs/assets/rustsys-banner.gif" alt="Verilog source flows through RustSys synthesis into connected standard-cell gates." width="1200">
</picture>

[Static banner](docs/assets/rustsys-banner.svg) · [Get started](#quick-start) ·
[Agent guide](docs/guide/agents.md) · [Documentation](https://lab-emi.github.io/RustSys/) · [Benchmarks](#benchmarks)

<!-- measured-performance-pitch -->

- **Measured performance:** 1.50×–42.11× synthesis speedup over pinned Yosys 0.67 with the same 16-core allowance; up to 4.02× RustSys 1→16-core scaling with byte-identical netlists.
- **Easy to script:** result-only stdout, coded diagnostics on stderr, JSON output, fixed exit statuses.
- **Easy to resume:** recorded commands, input hashes, artifacts, and event logs; query reports as JSON or TSV.
- **Safe to coordinate:** daemonless disk workspaces, immutable revisions, and explicit `--base-rev` conflict checks.
- **Built to check:** an independent synthesis checker, bounded proof work, and external open-source LEC evidence.

> **Development preview:** version 0.1.0 is unreleased and is not on crates.io. Build from this
> repository. Linux is tested in CI; other platforms are not tested.

## Benchmarks

Five complete RISC-V designs × two PDKs, with **both tools measured at 1 and 16 physical host cores**.
Each allocation uses one warm-up and three timed runs; the table shows medians.
RustSys uses area/medium mapping and its default area-capped quality cleanup;
Yosys 0.67 uses the same arbitrary-initial-state-preserving/default-ABC flow for every design.
RustSys worker count follows the CPU allowance. Both tools use CPU 0 at one core and CPU 0–15 at 16 cores,
with no SMT siblings. Host: AMD Ryzen 9 9950X3D; child limits: 32 GiB RAM + 1 GiB swap.

Time covers prepared RTL through workspace init/import, synthesis, mmap and full Verilog export.
Preparation, LEC and OpenSTA are outside the timer. Timed cases run serially on a shared host.
Average active cores = (user+system CPU seconds)/wall seconds, median across three runs.
Each tool's 1-core and 16-core netlists are byte-identical, so each displayed area/Fmax/LEC result applies to both.
Peak process RSS and aggregate cgroup peaks remain in the complete measurement receipts.
New Yosys 1-core measurements and unchanged earlier Yosys 16-core samples retain their separate provenance.

<!-- open-hardware-benchmark:start -->

| Design · PDK | Tool<br>RustSys vs Yosys | 1 core, s ↓ | 16 cores, s ↓ | 1→16 speedup | Active cores / 16 | Cell area, µm² ↓ | Setup Fmax‡, MHz ↑ | Arbitrary-state RTL LEC |
|---|---|---:|---:|---:|---:|---:|---:|---|
| Ibex · Sky130 HD | RustSys | 1.65 | 0.89 | 1.85× | 3.76 | 117,405 | 42.92 | Proven · 1 & 16 cores |
| Ibex · Sky130 HD | Yosys | 6.67 | 6.66 | 1.00× | 1.06 | 123,670 | 33.47 | Proven · 1 & 16 cores |
| Ibex · GF180 7T | RustSys | 1.59 | 0.98 | 1.62× | 3.18 | 356,149 | 14.93 | Proven · 1 & 16 cores |
| Ibex · GF180 7T | Yosys | 6.68 | 6.69 | 1.00× | 1.07 | 379,410 | 8.33 | Proven · 1 & 16 cores |
| PicoRV32 · Sky130 HD | RustSys | 0.70 | 0.47 | 1.49× | 3.15 | 76,169 | 107.53 | Proven · 1 & 16 cores |
| PicoRV32 · Sky130 HD | Yosys | 1.10 | 1.12 | 0.98× | 1.00 | 83,603 | 67.75 | Proven · 1 & 16 cores |
| PicoRV32 · GF180 7T | RustSys | 0.74 | 0.57 | 1.30× | 2.35 | 233,097 | 26.65 | Proven · 1 & 16 cores |
| PicoRV32 · GF180 7T | Yosys | 1.43 | 1.51 | 0.95× | 1.01 | 255,798 | 15.98 | Proven · 1 & 16 cores |
| SERV · Sky130 HD | RustSys | 0.51 | 0.35 | 1.46× | 2.31 | 51,461 | 60.28 | Proven · 1 & 16 cores |
| SERV · Sky130 HD | Yosys | 0.84 | 0.87 | 0.97× | 1.00 | 54,067 | 57.47 | Proven · 1 & 16 cores |
| SERV · GF180 7T | RustSys | 1.05 | 0.76 | 1.38× | 1.95 | 163,404 | 43.27 | Proven · 1 & 16 cores |
| SERV · GF180 7T | Yosys | 1.10 | 1.14 | 0.96× | 1.00 | 167,428 | 19.54 | Proven · 1 & 16 cores |
| VexRiscv · Sky130 HD | RustSys | 9.89 | 2.46 | 4.02× | 6.67 | 2,399,458 | 10.22 | Proven · 1 & 16 cores |
| VexRiscv · Sky130 HD | Yosys | 64.62 | 63.53 | 1.02× | 1.09 | 2,909,809 | 1.56 | Proven · 1 & 16 cores |
| VexRiscv · GF180 7T | RustSys | 7.30 | 2.23 | 3.27× | 5.26 | 7,490,709 | 4.47 | Proven · 1 & 16 cores |
| VexRiscv · GF180 7T | Yosys | 75.41 | 79.27 | 0.95× | 1.08 | 8,856,262 | 0.689 | Proven · 1 & 16 cores |
| OpenC910 · Sky130 HD | RustSys | 177.21 | 69.06 | 2.57× | 4.17 | 53,453,688 | 0.69 | Proven · 1 & 16 cores |
| OpenC910 · Sky130 HD | Yosys | 1489.95 | 1413.58 | 1.05× | 1.26 | 59,995,158 | 0.0486 | Proven · 1 & 16 cores |
| OpenC910 · GF180 7T | RustSys | 152.26 | 64.41 | 2.36× | 3.78 | 165,273,357 | 0.0989 | Proven · 1 & 16 cores |
| OpenC910 · GF180 7T | Yosys | 2754.81 | 2712.45 | 1.02× | 1.13 | 184,901,294 | 0.00397 | Proven · 1 & 16 cores |

<!-- open-hardware-benchmark:end -->

The default area/medium flow raises SERV’s setup estimate from **17.99 to 60.28 MHz**
(Sky130) and **7.18 to 43.27 MHz** (GF180), with area unchanged or lower than the previous
measured RustSys build. All **20 distinct netlists pass arbitrary-state RTL LEC**, covering
both core allocations; no reset or input assumption is added.

‡ OpenSTA 3.1.0 setup-Fmax estimate at each library's TT corner: ideal clocks,
100 ps input/clock slew, 10 fF output load, zero I/O delays and no wire parasitics.
MHz = 1000 / reported minimum period in ns, within the report's printed precision.
Same-clock, same-edge register paths only; port paths, cross-edge paths and
level-sensitive latch paths are excluded by `report_clock_min_period`.
Unclocked or otherwise unconstrained endpoints are outside this estimate;
constraint-check receipts are retained. OpenC910 exceeds the default cleanup budget. OpenC910/Sky130 critical-path loads exceed the Liberty characterization range (RustSys/Yosys: 49.60/55.98 pF; respective cell limits 0.052195/0.190440 pF). These raw estimates do not establish achievable or signoff Fmax.

[Complete measurements, scope and certificates](verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md).

<!-- large-hardware-benchmark:start -->

### OpenC910 configuration and proof scope

The OpenC910 RTL top is `ct_core`: one complete processor core with default 64 KiB
instruction and 64 KiB data caches, lowered to standard cells. The 16 cores above
are **host CPU resources**, not a change to the hardware configuration. L2 and
other processor cores are outside this RTL top. The source/caches are unchanged.

OpenC910 uses the same one-warm-up/three-sample protocol as every other design.
The common Yosys flow preserves arbitrary initial state and uses default ABC
library mapping for all five designs. RustSys uses area/medium mapping with
its 4 GiB cut-storage budget, retaining the effective cut counts in every report.

All netlists use the independent RTL event model with
ordinary X/Z data and undriven wires completed to zero, plus external ABC CEC.
Inferred multiply/add blocks use exact arithmetic components checked by Arisca,
with complete ABC checks of their Boolean context, component/context
reconstruction, and transport to the actual measured netlist.
The same semantic contract applies to every tool/design pair. Every result
keeps arbitrary implementation initial state under checked correspondence;
**no reset sequence or input assumption is added**.


<!-- large-hardware-benchmark:end -->

<!-- parallel-hardware-scaling:start -->

Both tools’ single-core and 16-core comparisons are consolidated in the table above. Thread count changes neither tool’s measured netlist bytes, area or timing under the same constraints.

<!-- parallel-hardware-scaling:end -->

## Quick start

Install [Rust with rustup](https://rustup.rs) and Git. The repository pins Rust 1.97.1.
The CPU flow needs no GPU or CUDA toolkit. Run these commands in one POSIX shell:

```sh
git clone https://github.com/lab-emi/RustSys.git
cd RustSys
cargo install --locked --path crates/apps/rustsys-cli

FIX=verification/fixtures/synthetic/synth
mkdir -p .work
rustsys synth --top counter --lib "$FIX/lib/wt_rich.lib" \
  --out .work/counter_mapped.v --report .work/counter.json \
  "$FIX/rtl/counter/counter.sv"

rustsys check synth --top counter --lib "$FIX/lib/wt_rich.lib" \
  --netlist .work/counter_mapped.v "$FIX/rtl/counter/counter.sv"

rustsys show .work/counter.json --section cells --sort count --reverse --limit 5
```

The included synthetic counter maps to **45 cells**, with **8 retained state bits**.
The checker proves **9 output bits and 8 state pairs** equivalent. This uses a tiny demonstration
library; its area and delay are not PDK measurements.

Bring your own RTL with `--top`, repeatable `--lib`, `-I`, `-D`, and `-P`.
[Installation options](docs/guide/installation.md) · [Walkthrough](docs/guide/quickstart.md) ·
[Synthesis guide](docs/guide/synthesis.md).

## Built for CLI and agent workflows

### Synthesize → check → query

Record a run once, then reuse its exact inputs and options for checking:

```sh
export RUSTSYS_RUNS_ROOT="$PWD/.work/runs"
rustsys synth --top counter --lib "$FIX/lib/wt_rich.lib" --record \
  --print json "$FIX/rtl/counter/counter.sv" > .work/synthesis.json
rustsys check synth --from-run @synth --record --print json > .work/check.json
rustsys show @synth --section cells --print tsv > .work/cells.tsv
rustsys show @synth --section command
```

A recorded run stores the canonical command, input hashes, netlist, reports, and `events.jsonl`.
`@synth` selects the latest synthesis run under that runs root; use an explicit run directory
when independent jobs share a root. `--print json` emits the direct command's report.

### Read → change → commit

For work that spans tools or multiple steps, install the shared workspace CLI too:

```sh
cargo install --locked --path crates/apps/rusteda-cli

WS=.work/counter-workspace
rusteda -w "$WS" --format json init
rusteda -w "$WS" --format json import --base-rev rev0 \
  --lib "$FIX/lib/wt_rich.lib" --rtl "$FIX/rtl/counter/counter.sv" --top counter
rustsys -w "$WS" --format json synth --base-rev rev1 --check
rusteda -w "$WS" --format json export --rev rev2 --netlist .work/counter_exported.v
```

Use a fresh workspace directory for this example. Workspace commands return one **`rusteda.v1`
JSON envelope** with status, diagnostics, artifacts, and suggested next actions. Every mutation
of an existing workspace names the expected revision; a stale base fails with
`REVISION_CONFLICT` (exit 4). Commands acquire disk locks, finish, and exit.

| Contract | What a script or agent can rely on |
|---|---|
| Machine output | Direct file mode: `--print json`; workspace mode: `--format json`. These have different schemas. |
| Diagnostics | Stable message codes, locations when available, and progress on stderr. |
| Queryable results | `show` filters, sorts, and selects sections; exports text, JSON, or TSV. |
| Reproducibility | Canonical commands and input hashes in recorded runs; deterministic netlists across thread counts and mapping backends. |
| Explicit outcomes | 0 success; 1 rejected input; 2 internal failure; 3 inequivalent; 4 revision conflict in workspace mode; 5 proof undecided. |
| Bounded work | Proof/search budgets report exhaustion as undecided; `unverified` evidence stays unverified. |

[Agent integration recipes](docs/guide/agents.md) · [CLI reference](docs/reference/cli.md) ·
[JSON and report contracts](docs/reference/reports.md) · [Agent documentation index](https://lab-emi.github.io/RustSys/llms.txt).

## Synthesis and verification

- **SystemVerilog front end:** packages, parameters, packed structures, arrays, generate blocks,
  functions, tasks, registers, and latches. BLIF, AIGER, and mapped-netlist inputs are also supported.
- **Standard-cell mapping:** AIG optimization and Boolean matching to your Liberty libraries,
  area or delay goals, three effort levels, and cell exclusion filters.
- **Timing refinement:** optional buffering, cell sizing, and electrical-limit repair using Liberty
  NLDM tables. Timing estimates assume ideal clocks and no interconnect.
- **CPU first, optional CUDA:** CPU synthesis needs no GPU dependencies at run time. CUDA
  accelerates mapping cut passes; a requested unavailable backend returns an error. Metal is planned.
- **Independent checking:** `rustsys check synth` interprets RTL, evaluates library cells, and
  proves output and state-transition obligations with its own checker and SAT implementation.
  It shares format readers and core tables with synthesis; external LEC provides another oracle.

By default, retained register and latch initial states remain arbitrary; state is not merged,
retimed, or re-encoded. Reset assumptions and initial-state refinement are explicit opt-ins and
change the proof scope. The benchmark proofs above add no reset sequence.

The synthesis semantics are two-valued: ordinary `x`/`z` data bits become zero, while language
wildcards retain their matching semantics. Unsupported constructs are diagnosed. Interfaces,
modports, tri-state logic, `inout`, and dynamic types are among the current limitations.

[Language and library support](docs/reference/language-support.md) ·
[Checking and proof scopes](docs/guide/checking.md) ·
[GPU and timing guide](docs/guide/gpu-and-timing.md) · [Troubleshooting](docs/guide/troubleshooting.md).

## Documentation

The [documentation site](https://lab-emi.github.io/RustSys/) follows a task-based path from a first netlist to repeatable automation
and inspectable proof evidence. All pages also live in this repository.

| Start here | Go deeper |
|---|---|
| [Documentation home](https://lab-emi.github.io/RustSys/) | [Architecture and crate boundaries](docs/architecture/index.md) |
| [Install and run your first design](docs/guide/quickstart.md) | [Synthesis specifications](docs/specs/synth/README.md) |
| [Automate with scripts and agents](docs/guide/agents.md) | [Workspace and JSON protocol](docs/specs/workspace/README.md) |
| [Check equivalence](docs/guide/checking.md) | [Verification evidence and limitations](docs/verification/index.md) |
| [Understand the benchmarks](docs/guide/benchmarks.md) | [Architecture decisions](docs/adr/) |

Build the site locally (documentation-only dependencies):

```sh
python3 -m venv .work/docs-venv
.work/docs-venv/bin/pip install -r docs/requirements.txt
.work/docs-venv/bin/mkdocs build --strict -f docs/mkdocs.yml
```

See [documentation maintenance](docs/about/maintaining-docs.md) for previewing, checking links,
and publishing through the included GitHub Pages workflow. The [brand guide](docs/brand/README.md)
explains the RustEDA family marks and how to regenerate the animation.

## Development and contributing

RustSys is clean-room Rust. Reference tools interoperate as external executables over standard
formats. Engine and checker crates remain independent; `cargo xtask layout` enforces crate boundaries.

[Contributor guide](docs/contributing.md) · [Repository rules](CONTRIBUTING.md) ·
[Requirement coverage](docs/verification/requirements.md) · [Changelog](CHANGELOG.md).
The contributor guide lists the required formatting, lint, verification, and memory-bounded checks.
Report vulnerabilities using [SECURITY.md](SECURITY.md); participants follow the
[code of conduct](CODE_OF_CONDUCT.md).

Licensed under [Apache-2.0](LICENSE). See [NOTICE](NOTICE).
