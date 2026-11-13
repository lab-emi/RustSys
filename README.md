<p align="center">
  <img src="docs/assets/rustsys-wordmark.svg" alt="RustSys" width="360">
</p>

<h1 align="center">Synthesis for the next design iteration.</h1>

<p align="center">
  <a href="https://lab-emi.github.io/RustEDA/modules/rustsys/">Documentation</a> ·
  <a href="https://github.com/lab-emi/RustEDA">RustEDA</a> ·
  <a href="https://lab-emi.github.io/RustEDA/benchmarks/">Benchmarks</a> ·
  <a href="LICENSE">Apache-2.0</a>
</p>

<picture>
  <source media="(prefers-reduced-motion: reduce)" srcset="docs/assets/rustsys-banner.svg">
  <img src="docs/assets/rustsys-banner.gif" alt="RustSys turns RTL into mapped netlists with bounded synthesis, technology mapping and independently checked equivalence" width="1200">
</picture>

RustSys turns RTL into mapped netlists with bounded synthesis, technology mapping and independently checked equivalence.

Part of **[RustEDA](https://github.com/lab-emi/RustEDA)**, infrastructure for chip-design recursive self-improvement. Native tools shorten the work between inference steps; verified results guide the next iteration.

## RustSys vs Yosys at a glance

Ibex RTL-to-GDS on IHP SG13G2 with EZ130 cells and two SRAM macros, 2026-11-13: RustEDA and LibreLane 3.0.17 (OpenROAD, Yosys, KLayout, Magic, Netgen) ran one after the other on the same 4-vCPU host from the same prepared inputs, and the same signoff scripts checked both layouts. Every RustEDA engine was built with Rust 1.100.0, and RustEDA's step graph ran under its Rust executor, `rusteda flow exec`.

- **33.8× faster than Yosys + ABC** with LibreLane's synthesis script (3.07 s against 103.91 s, median of three interleaved runs; 47.4× in direct mode), with **6.6× less peak memory**.
- **1.8 % less standard-cell area** (145,226.592 µm² against 147,866.947 µm²) and **logic depth 36 against 40**; the SS critical path arrives at 50.5 ns against 71.7 ns with ideal wires.
- Equivalence of the mapped netlist to the RTL is **proven**; for the Yosys netlist it stays unverified.
- In the full flow, synthesis takes 3.10 s against 124.93 s (**40.3× faster**), and the whole RustEDA flow finishes **4.4× sooner end to end** (751.54 s against 3,283.05 s with both flows' foundry checks on the same schedule; 3.5× with every check run one after another) with a 5.2 % smaller layout.

LibreLane's layout fails the shared timing gate (112 maximum-capacitance and fanout violations at TT and SS) and its equivalence check does not complete, so the receipt keeps its own ratio fields null under its publication rule; the multiples here divide the recorded times. Stages are paired by phase, not as identical tasks. Against the three losses of 2026-11-05, in same-load runs: repair before global routing now takes 0.73× the time of LibreLane's two pre-route resizer steps (ranges do not overlap; 1.18× on 2026-11-05), OpenRCX on RustEDA's layout is level with its time on LibreLane's (1.00×, ranges overlap; 1.36× on 2026-11-05) and the latch-up check on RustEDA's layout takes 0.76× its time on LibreLane's (1.11× with the checker of that campaign). RustEDA is still not faster or not better everywhere: RustROAD's standalone extraction takes 1.22× OpenRCX's time at 4 threads, RustSTA's peak memory at 4 threads is 11.8 % above OpenSTA's, the timing gate is level (0.98×, ranges overlap) and its layout keeps less hold margin. RustSys has no loss in this campaign; the equivalence gate has no valid time comparison, because the check does not complete on LibreLane's layout. [All stages, gates and losses](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-13)

## Latest development · 2026-11-13

The RustEDA Ibex host campaign of 2026-11-13 reran the IHP SG13G2 / EZ130
design with two real SRAMs on a 4-vCPU cloud host (a Xeon reporting 2.10 GHz
with 15.7 GiB RAM, again a different machine from 2026-11-05's), with RustSys
built from commit [`86187e6`](https://github.com/lab-emi/RustSys/commit/86187e69c3111ad79ae4b651ad53d652b7609867)
and every engine built with Rust 1.100.0. RustSys's changes since `86e59d8` are
toolchain and maintenance commits (Rust 1.100.0 instead of 1.97.1); the flow's
synthesis netlist is byte-identical to 2026-11-05's and the head-to-head QoR is
unchanged. The native step graph ran for the first time under the Rust executor
`rusteda flow exec` instead of the Python `producer.py`. RustSys synthesized
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

[Synthesis head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys-2026-11-13) ·
[Host campaign 2026-11-13](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-13) ·
[Shared gates](https://lab-emi.github.io/RustEDA/benchmarks/#shared-gates-2026-11-13) ·
[Losses and parity](https://lab-emi.github.io/RustEDA/benchmarks/#where-rusteda-still-loses-or-is-at-parity-2026-11-13) ·
[Method and deviations](https://lab-emi.github.io/RustEDA/benchmarks/#host-campaign-method-and-deviations-2026-11-13)

## Ibex host campaign · 2026-11-05

The RustEDA Ibex host campaign of 2026-11-05 reran the IHP SG13G2 / EZ130
design with two real SRAMs on a 4-vCPU cloud host (a different and slower
machine than on 2026-11-02), with RustSys built from commit [`86e59d8`](https://github.com/lab-emi/RustSys/commit/86e59d816e6ded82333ccc502bb37961c3bb1a9f).
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

[Synthesis head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys-2026-11-05) ·
[Host campaign 2026-11-05](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-05)

## Ibex host campaign · 2026-11-02

The RustEDA Ibex host campaign reran the IHP SG13G2 / EZ130 design with two
real SRAMs on a 4-vCPU cloud host, with RustSys built from commit `0f8a2d8`.
RustSys synthesized the design in the native flow, and the flow's synthesis and
final equivalence gates are `proven`. The native run is `unqualified`, and its
producer stopped at the RustSTA/OpenSTA `timing_accuracy` gate, which does not
involve synthesis; the remaining gates ran separately with the recipe's scripts.

The campaign also includes a RustSys-vs-Yosys head-to-head on the same prepared
RTL and Liberty, with Yosys + ABC running LibreLane's synthesis script. RustSys
had the shorter median wall time and the lower peak memory (three measured runs
per contender on this task and host); the Yosys + ABC netlist has the smaller
standard-cell area and fewer logic levels. The equivalence verdicts come from
the flows' synthesis gates, not from the head-to-head netlists: RustSys's
native-flow netlist is `proven`, and LibreLane's synthesis netlist, the same
bytes as the head-to-head Yosys netlist, is `unverified`, which is not evidence
of non-equivalence. Results, scope and deviations are on the
[RustEDA benchmarks page](https://lab-emi.github.io/RustEDA/benchmarks/).

The 2026-11-01 update preserves opaque SRAM interfaces through elaboration,
mapping and netlist export. Port widths, bus indices and instance/pin identities
are checked; unsupported or inconsistent interfaces produce diagnostics. The
macro's behavior continues to come from its original vendor model for
independent checking.

[Current source and qualification](docs/development-status.md) ·
[Synthesis head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys) ·
[Host campaign evidence](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-02)

## Benchmarks

The published benchmark is the Ibex RV32 core with two real IHP SRAMs on IHP
SG13G2 with the ETH EZ130 8T library, run end to end through the native RustEDA
flow and compared with LibreLane. RustSys synthesizes the Ibex + SRAM design in
that flow; the flow's synthesis and final CEC gates prove the synthesized and
final netlists equivalent to the original RTL. The macro-preserving source is included in this development publication.
Run16 retains the frozen RustSys executable `e6cf0c77…` as its measurement identity.
The 2026-11-02 host campaign ran the run16-class recipe with RustSys built from
`0f8a2d8` (executable SHA-256 `a6507ecc…`), not run16's frozen executable; the
2026-11-05 campaign ran the current recipe with RustSys `86e59d8` (executable
SHA-256 `49afd8c8…`). The 2026-11-13 campaign ran the recipe at superproject
`820ffc8` with RustSys `86187e6` (executable SHA-256 `d84e0810…`).

[RustEDA benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/) ·
[RustSys in the whole-flow benchmark](https://lab-emi.github.io/RustEDA/modules/rustsys/guide/benchmarks/)

## Quick start

Install [Rust with rustup](https://rustup.rs) and Git. The repository pins Rust 1.100.0.
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
[JSON and report contracts](docs/reference/reports.md) · [Agent documentation index](https://lab-emi.github.io/RustEDA/modules/rustsys/llms.txt).

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
change the proof scope.

The synthesis semantics are two-valued: ordinary `x`/`z` data bits become zero, while language
wildcards retain their matching semantics. Unsupported constructs are diagnosed. Interfaces,
modports, tri-state logic, `inout`, and dynamic types are among the current limitations.

[Language and library support](docs/reference/language-support.md) ·
[Checking and proof scopes](docs/guide/checking.md) ·
[GPU and timing guide](docs/guide/gpu-and-timing.md) · [Troubleshooting](docs/guide/troubleshooting.md).

## Documentation

The [documentation site](https://lab-emi.github.io/RustEDA/modules/rustsys/) follows a task-based path from a first netlist to repeatable automation
and inspectable proof evidence. All pages also live in this repository.

| Start here | Go deeper |
|---|---|
| [Documentation home](https://lab-emi.github.io/RustEDA/modules/rustsys/) | [Architecture and crate boundaries](docs/architecture/index.md) |
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
