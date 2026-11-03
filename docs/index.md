# RustSys

RustSys turns RTL into mapped netlists with bounded synthesis, technology mapping and independently checked equivalence.

[All RustEDA modules](https://lab-emi.github.io/RustEDA/)

<picture>
  <source media="(prefers-reduced-motion: reduce)" srcset="assets/rustsys-banner.svg">
  <img src="assets/rustsys-banner.gif" alt="Synthesis for the next design iteration." width="1200">
</picture>

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

The published benchmark is the Ibex RV32 core with two real IHP SRAMs on IHP
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
