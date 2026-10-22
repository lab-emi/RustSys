---
title: RustSys — Ultrafast Verilog/SystemVerilog synthesis
description: High-performance Verilog/SystemVerilog synthesis in Rust, built for terminals, CI and coding agents.
hide:
  - navigation
  - toc
---

<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSys contributors -->

<div class="rustsys-hero" markdown>

<p class="rustsys-eyebrow">RUSTEDA / LOGIC SYNTHESIS</p>

# Ultrafast RTL synthesis.<br>Ready for CLI and agents.

RustSys is a high-performance Verilog/SystemVerilog synthesizer written in Rust.
Turn RTL into standard cells with a CLI built for engineers, CI pipelines and coding
agents: explicit inputs, JSON reports, recorded runs and equivalence checks.

**1.50×–42.11× faster than Yosys** on the measured five-design, two-PDK suite at
16 physical host cores. [See the results and method](guide/benchmarks.md).

[Run your first synthesis](guide/quickstart.md){ .md-button .md-button--primary }
[Build an agent workflow](guide/agents.md){ .md-button }

<p class="rustsys-release">Source preview · 0.1.0 · Apache-2.0 · CPU reference backend</p>

</div>

<figure class="rustsys-banner">
  <picture>
    <source media="(prefers-reduced-motion: no-preference)" srcset="assets/rustsys-banner.gif" type="image/gif">
    <img src="assets/rustsys-banner.svg" alt="Verilog source passes through RustSys synthesis and emerges as connected logic gates and flip-flops in a gate-level netlist." width="1200" height="400" data-animation="assets/rustsys-banner.gif" data-poster="assets/rustsys-banner.svg">
  </picture>
  <figcaption>An illustration of RTL → synthesis → standard cells. <button type="button" class="rustsys-motion-control" aria-pressed="false" hidden>Pause animation</button></figcaption>
</figure>

## 16-core synthesis speed

**CPU: AMD Ryzen 9 9950X3D 16-Core Processor** · 16 physical host cores per tool, no SMT siblings.
One warm-up and three measured synthesis/export runs; median seconds shown.
**Speedup = Yosys time ÷ RustSys time.**

<p class="rustsys-comparison-hint">Swipe horizontally to compare all columns. Design and PDK stay visible.</p>

<div class="rustsys-comparison" markdown="1" role="region" aria-label="16-core RustSys and Yosys synthesis comparison" tabindex="0">

<!-- benchmark-16-core:start -->

| Design | PDK | RustSys<br>16 cores, s ↓ | Yosys<br>16 cores, s ↓ | RustSys<br>Speedup ↑ | RustSys<br>Peak mem, MiB ↓ | Yosys<br>Peak mem, MiB ↓ | RustSys<br>Area, µm² ↓ | Yosys<br>Area, µm² ↓ | RustSys<br>Setup Fmax†, MHz ↑ | Yosys<br>Setup Fmax†, MHz ↑ |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Ibex | Sky130 HD | 0.89 | 6.66 | **7.48×** | 91.9 | 273.1 | 117,405 | 123,670 | 42.92 | 33.47 |
| Ibex | GF180 7T | 0.98 | 6.69 | **6.83×** | 118.6 | 273.0 | 356,149 | 379,410 | 14.93 | 8.33 |
| PicoRV32 | Sky130 HD | 0.47 | 1.12 | **2.38×** | 66.0 | 81.0 | 76,169 | 83,603 | 107.53 | 67.75 |
| PicoRV32 | GF180 7T | 0.57 | 1.51 | **2.65×** | 117.3 | 100.2 | 233,097 | 255,798 | 26.65 | 15.98 |
| SERV | Sky130 HD | 0.35 | 0.87 | **2.49×** | 65.9 | 71.9 | 51,461 | 54,067 | 60.28 | 57.47 |
| SERV | GF180 7T | 0.76 | 1.14 | **1.50×** | 117.4 | 90.4 | 163,404 | 167,428 | 43.27 | 19.54 |
| VexRiscv | Sky130 HD | 2.46 | 63.53 | **25.83×** | 449.8 | 840.2 | 2,399,458 | 2,909,809 | 10.22 | 1.56 |
| VexRiscv | GF180 7T | 2.23 | 79.27 | **35.55×** | 481.3 | 864.1 | 7,490,709 | 8,856,262 | 4.47 | 0.689 |
| OpenC910 | Sky130 HD | 69.06 | 1,413.58 | **20.47×** | 7,492.7 | 16,339.8 | 53,453,688 | 59,995,158 | 0.69 | 0.0486 |
| OpenC910 | GF180 7T | 64.41 | 2,712.45 | **42.11×** | 7,543.2 | 17,511.3 | 165,273,357 | 184,901,294 | 0.0989 | 0.00397 |

<!-- benchmark-16-core:end -->

</div>

**Peak mem** = maximum GNU time process peak RSS across the three timed runs,
in MiB. Aggregate cgroup memory peaks remain in the complete receipts.

† **Setup-Fmax is an OpenSTA estimate** at the TT corner with ideal clocks and no wire
parasitics. OpenC910 loads exceed Liberty characterization limits; its raw Fmax
does not establish achievable frequency. [Timing scope](guide/performance.md#interpret-the-timing-estimate).

All **20 distinct netlists pass arbitrary-state RTL LEC**, with no reset or input
assumptions. [Method, full 1/16-core results, area and timing](guide/benchmarks.md).

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
