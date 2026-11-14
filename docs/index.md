# RustSys

RustSys turns RTL into mapped netlists with bounded synthesis, technology mapping and independently checked equivalence.

[All RustEDA modules](https://lab-emi.github.io/RustEDA/)

<picture>
  <source media="(prefers-reduced-motion: reduce)" srcset="assets/rustsys-banner.svg">
  <img src="assets/rustsys-banner.gif" alt="Synthesis for the next design iteration." width="1200">
</picture>

## RustSys vs Yosys at a glance

Ibex RTL-to-GDS on IHP SG13G2 with EZ130 cells and two SRAM macros, 2026-11-13: RustEDA and LibreLane 3.0.17 (OpenROAD, Yosys, KLayout, Magic, Netgen) ran one after the other on the same 4-vCPU host from the same prepared inputs, and the same signoff scripts checked both layouts. Every RustEDA engine was built with Rust 1.100.0, and RustEDA's step graph ran under its Rust executor, `rusteda flow exec`.

- **33.8× faster than Yosys + ABC** with LibreLane's synthesis script (3.07 s against 103.91 s, median of three interleaved runs; 47.4× in direct mode), with **6.6× less peak memory**.
- **1.8 % less standard-cell area** (145,226.592 µm² against 147,866.947 µm²) and **logic depth 36 against 40**; the SS critical path arrives at 50.5 ns against 71.7 ns with ideal wires.
- Equivalence of the mapped netlist to the RTL is **proven**; for the Yosys netlist it stays unverified.
- In the full flow, synthesis takes 3.10 s against 124.93 s (**40.3× faster**), and the whole RustEDA flow finishes **4.4× sooner end to end** (751.54 s against 3,283.05 s with both flows' foundry checks on the same schedule; 3.5× with every check run one after another) with a 5.2 % smaller layout.

LibreLane's layout fails the shared timing gate (112 maximum-capacitance and fanout violations at TT and SS) and its equivalence check does not complete, so the receipt keeps its own ratio fields null under its publication rule; the multiples here divide the recorded times. Stages are paired by phase, not as identical tasks. Against the three losses of 2026-11-05, in same-load runs: repair before global routing now takes 0.73× the time of LibreLane's two pre-route resizer steps (ranges do not overlap; 1.18× on 2026-11-05), OpenRCX on RustEDA's layout is level with its time on LibreLane's (1.00×, ranges overlap; 1.36× on 2026-11-05) and the latch-up check on RustEDA's layout takes 0.76× its time on LibreLane's (1.11× with the checker of that campaign). RustEDA is still not faster or not better everywhere: RustROAD's standalone extraction took 1.22× OpenRCX's time at 4 threads in the campaign and, after three changes since, is narrowly faster at 4 threads (0.85×, faster in all 30 runs of two A/Bs; the ranges are apart in one and overlap by 47 ms in the other) and still 1.52× slower at 2 ([third round](https://lab-emi.github.io/RustEDA/benchmarks/#rustroad-extraction-third-round-2026-11-14)); the timing gate is level (0.98×, ranges overlap) and its layout keeps less hold margin. RustSTA's peak memory at 4 threads, 11.8 % above OpenSTA's in the campaign, is 12.6 % below it since a change after the campaign ([details](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-peak-memory-and-rustroad-extraction-2026-11-13)). RustSys has no loss in this campaign; the equivalence gate has no valid time comparison, because the check does not complete on LibreLane's layout. [All stages, gates and losses](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-13)

## Development update · 2026-11-13

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

[Source, evidence and remaining limits](development-status.md) ·
[Synthesis in the benchmark](guide/benchmarks.md) ·
[Synthesis head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys-2026-11-13) ·
[Host campaign evidence](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-13) ·
[Shared gates](https://lab-emi.github.io/RustEDA/benchmarks/#shared-gates-2026-11-13) ·
[Losses and parity](https://lab-emi.github.io/RustEDA/benchmarks/#where-rusteda-still-loses-or-is-at-parity-2026-11-13) ·
[Method and deviations](https://lab-emi.github.io/RustEDA/benchmarks/#host-campaign-method-and-deviations-2026-11-13)

## PULPissimo synthesis · 2026-11-06

A second design benchmark compares the PULPissimo Ibex core with a bounded Yosys 0.70/ABC reference on SKY130 HD. On the same prepared Verilog, RustSys is **1.07× slower**, uses **1.58× more peak memory**, and produces an **8.96% smaller-area, 32.35% smaller-cell-count** netlist (15,041 cells and 146,711.96 µm² against 22,235 cells and 161,157.06 µm²). The full PULPissimo SoC passes hierarchy elaboration. On SKY130 with the behavioral L2 memory it still exceeds a 2 h budget; an IHP SG13G2 / EZ130 synthesis-only test with 80 official SRAM macros completes in 426 s. Neither is a physical or equivalence result. This is synthesis-only and does not extend the Ibex RTL-to-GDS qualification.

[PULPissimo benchmark](https://lab-emi.github.io/RustEDA/benchmarks/pulpissimo/) · [Receipt](https://lab-emi.github.io/RustEDA/benchmarks/pulpissimo-soc-20261106.json)

## Development update · 2026-11-05

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
[Host campaign evidence](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-05)

## Development update · 2026-11-02

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

[Source, evidence and remaining limits](development-status.md) ·
[Synthesis in the benchmark](guide/benchmarks.md) ·
[Synthesis head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys) ·
[Host campaign evidence](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-02)


## Development status · 2026-10-28

Superseded by the updates above; kept as the record of that date.
The RustEDA integration combines bounded synthesis, BLIF/AIGER and mapped-netlist
inputs, revision-bound timing optimization and a shared RustEDA foundation.
The earlier Ibex three-library synthesis campaign proves all six RustSys/Yosys
netlists equivalent with arbitrary initial state. Real-SRAM preservation for
IHP Ibex remains experimental; physical closure is a separate open gate.

[Progress, source revisions and verification limits →](development-status.md)

## Benchmark

The primary RTL-to-GDS benchmark is the Ibex RV32 core with two real IHP SRAMs on IHP
SG13G2 with the ETH EZ130 8T library, run end to end through the native RustEDA
flow and compared with LibreLane. RustSys synthesizes the Ibex + SRAM design in
that flow; the flow's synthesis and final CEC gates prove the synthesized and
final netlists equivalent to the original RTL.

[RustEDA benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/) ·
[RustSys in the whole-flow benchmark](guide/benchmarks.md)

## A CLI you can compose

<div class="grid cards" markdown>

- **Start with files. Finish with files.**

    ---

    Read RTL and Liberty. Export gate-level Verilog and a JSON report.
    Run in a shell, a CI job, or an agent loop.

    [Synthesis workflow →](guide/synthesis.md)

- **Give agents a contract.**

    ---

    Keep results on standard output and diagnostics on standard error.
    Use structured errors, bounded tables, revision guards, and explicit exit statuses.

    [Agent integration →](guide/agents.md)

- **Keep the evidence.**

    ---

    Record resolved commands, input hashes, events, and reports.
    Check the exported netlist with a separate RTL interpreter and SAT solver.

    [Equivalence workflow →](guide/checking.md)

- **Follow the benchmark.**

    ---

    RustSys synthesizes the Ibex + SRAM design in the RustEDA whole-flow
    benchmark on IHP SG13G2. The flow proves its netlists equivalent to the RTL.

    [Benchmark and proof scope →](guide/benchmarks.md)

</div>

## Choose your path

| I want to… | Start here |
|---|---|
| Install the CLI and try a small checked design | [Installation](guide/installation.md) → [quickstart](guide/quickstart.md) |
| Synthesize my own RTL against a cell library | [Synthesis](guide/synthesis.md) → [language support](reference/language-support.md) |
| Drive the tools from an agent or CI system | [Agent workflow](guide/agents.md) → [commands](reference/cli.md) → [reports](reference/reports.md) |
| Understand what a proof establishes | [Equivalence checking](guide/checking.md) → [verification scope](verification/index.md) |
| Find the published benchmark or assess a performance claim | [Benchmarks](guide/benchmarks.md) |
| Scale CPU synthesis and compare area or setup-Fmax | [CPU performance and scaling](guide/performance.md) |
| Extend RustSys or another RustEDA tool | [Architecture](architecture/index.md) → [contributing](contributing.md) |

## A small first run

From the repository root, after [building RustSys](guide/installation.md):

```bash
FIX=verification/fixtures/synthetic/synth
rustsys synth --top counter --lib "$FIX/lib/wt_rich.lib" \
  --out .work/counter_mapped.v --report .work/counter.json \
  "$FIX/rtl/counter/counter.sv"

rustsys check synth --top counter --lib "$FIX/lib/wt_rich.lib" \
  --netlist .work/counter_mapped.v "$FIX/rtl/counter/counter.sv"
```

The bundled library is a small verification fixture. Use a characterized
technology library for your own standard-cell targets. The
[quickstart](guide/quickstart.md) explains the generated files and verdicts;
the [agent guide](guide/agents.md) adds recorded runs and machine-readable output.

!!! note "Preview status"
    RustSys 0.1.0 is built from source and is not published to crates.io.
    Supported constructs and analysis limits are documented explicitly.
    A synthesis success, a timing estimate, and an equivalence proof are separate results.
