# RustSys

RustSys turns RTL into mapped netlists with bounded synthesis, technology mapping and independently checked equivalence.

[All RustEDA modules](https://lab-emi.github.io/RustEDA/)

<picture>
  <source media="(prefers-reduced-motion: reduce)" srcset="assets/rustsys-banner.svg">
  <img src="assets/rustsys-banner.gif" alt="Synthesis for the next design iteration." width="1200" height="360">
</picture>

## 16-core synthesis speed

**CPU: AMD Ryzen 9 9950X3D 16-Core Processor** · 16 physical host cores per tool, no SMT siblings.
One warm-up and three measured synthesis/export runs; median seconds shown.
**Speedup = Yosys time ÷ RustSys time.**

<p class="rustsys-comparison-hint">Swipe horizontally to compare all columns. Design and PDK stay visible.</p>

<div class="rustsys-comparison" markdown="1" role="region" aria-label="16-core RustSys and Yosys synthesis comparison" tabindex="0">

<!-- benchmark-16-core:start -->

| Design | PDK | RustSys<br>16 cores, s ↓ | Yosys<br>16 cores, s ↓ | RustSys<br>Speedup ↑ | RustSys<br>Peak mem, MiB ↓ | Yosys<br>Peak mem, MiB ↓ | RustSys<br>Area, µm² ↓ | Yosys<br>Area, µm² ↓ | RustSys<br>Setup Fmax†, MHz ↑ | Yosys<br>Setup Fmax†, MHz ↑ |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Ibex | Sky130 HD | 0.90 | 6.66 | **7.40×** | 92.1 | 273.1 | 117,405 | 123,670 | 42.92 | 33.47 |
| Ibex | GF180 7T | 1.00 | 6.69 | **6.69×** | 122.8 | 273.0 | 356,149 | 379,410 | 14.93 | 8.33 |
| Ibex | EZ130 8T | 0.79 | 6.80 | **8.61×** | 91.9 | 272.4 | 145,667 | 151,762 | 55.13 | 43.31 |
| PicoRV32 | Sky130 HD | 0.50 | 1.12 | **2.24×** | 67.4 | 81.0 | 76,169 | 83,603 | 107.53 | 67.75 |
| PicoRV32 | GF180 7T | 0.59 | 1.51 | **2.56×** | 117.4 | 100.2 | 233,097 | 255,798 | 26.65 | 15.98 |
| PicoRV32 | EZ130 8T | 0.44 | 1.46 | **3.32×** | 68.3 | 76.4 | 97,920 | 99,559 | 157.73 | 73.15 |
| SERV | Sky130 HD | 0.37 | 0.87 | **2.35×** | 65.8 | 71.9 | 51,461 | 54,067 | 60.28 | 57.47 |
| SERV | GF180 7T | 0.76 | 1.14 | **1.50×** | 117.4 | 90.4 | 163,404 | 167,428 | 43.27 | 19.54 |
| SERV | EZ130 8T | 0.31 | 1.10 | **3.55×** | 62.1 | 67.0 | 64,550 | 65,052 | 181.49 | 67.02 |
| VexRiscv | Sky130 HD | 2.47 | 63.53 | **25.72×** | 461.7 | 840.2 | 2,399,458 | 2,909,809 | 10.22 | 1.56 |
| VexRiscv | GF180 7T | 2.34 | 79.27 | **33.88×** | 496.0 | 864.1 | 7,490,709 | 8,856,262 | 4.47 | 0.689 |
| VexRiscv | EZ130 8T | 2.95 | 79.06 | **26.80×** | 879.8 | 853.5 | 3,225,087 | 3,429,484 | 4.20 | 2.12 |
| OpenC910 | Sky130 HD | 68.79 | 1,413.58 | **20.55×** | 7,702.1 | 16,339.8 | 53,453,688 | 59,995,158 | 0.69 | 0.0486 |
| OpenC910 | GF180 7T | 63.88 | 2,712.45 | **42.46×** | 7,731.3 | 17,511.3 | 165,273,357 | 184,901,294 | 0.0989 | 0.00397 |
| OpenC910 | EZ130 8T | 74.44 | 2,811.45 | **37.77×** | 8,559.3 | 16,979.2 | 71,069,802 | 72,190,882 | 0.0171 | 0.0159 |

<!-- benchmark-16-core:end -->

</div>

**Peak mem** = maximum GNU time process peak RSS across the three timed runs,
in MiB. Aggregate cgroup memory peaks remain in the complete receipts.

† **Setup-Fmax is an OpenSTA estimate** at the TT corner with ideal clocks and no wire
parasitics. SERV/EZ130 and OpenC910 loads exceed Liberty characterization limits;
these raw estimates do not establish achievable frequency. [Timing scope](guide/performance.md#interpret-the-timing-estimate).

All **30 distinct netlists pass arbitrary-state RTL LEC**, with no reset or input
assumptions. EZ130 8T means **IHP SG13G2, library v1.1, TT 1.20 V / 25°C**.
Its functional proof uses the vendor’s nominal tie-cell models through an
[explicitly checked supplement](verification/pdk.md#additional-ihp-sg13g2-benchmark-library).
SERV/EZ130 is 3.55× faster and uses 0.77% less cell area than Yosys.
[Current measurements and regression evidence](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-23-gf180-runtime-audit/README.md); pinned Yosys runs are retained with their original provenance.
[Method, complete results and timing scope](guide/benchmarks.md).

The [extended GF180 audit](guide/benchmarks.md#gf180-runtime-audit) adds 40–64 paired
runs for the smaller designs and 12 for OpenC910, with same-binary controls and
99% intervals. All three-PDK implementations preserve area, timing and arbitrary-state LEC.

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

- **Inspect the comparison.**

    ---

    Compare RustSys and Yosys on Ibex, PicoRV32, SERV, VexRiscv, and OpenC910.
    Every result states its workload, library, measurement method, and proof scope.

    [Benchmarks and proof status →](guide/benchmarks.md)

</div>

## Choose your path

| I want to… | Start here |
|---|---|
| Install the CLI and try a small checked design | [Installation](guide/installation.md) → [quickstart](guide/quickstart.md) |
| Synthesize my own RTL against a cell library | [Synthesis](guide/synthesis.md) → [language support](reference/language-support.md) |
| Drive the tools from an agent or CI system | [Agent workflow](guide/agents.md) → [commands](reference/cli.md) → [reports](reference/reports.md) |
| Understand what a proof establishes | [Equivalence checking](guide/checking.md) → [verification scope](verification/index.md) |
| Reproduce a comparison or assess a performance claim | [Benchmarks](guide/benchmarks.md) |
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
