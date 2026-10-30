# Understand the benchmarks

The [README comparison](https://github.com/lab-emi/RustSys#benchmarks) uses one common protocol for the **Ibex** RISC-V core, mapped to Sky130 HD, GF180 7T, and **EZ130 8T v1.1 on IHP SG13G2 (TT 1.20 V / 25°C)**. The [complete unified archive](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md) contains the baseline 1/16-core samples, commands, input pins, limits, and proof receipts. The [IHP/EZ130 extension](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-23-ez130/README.md) adds both tools at 16 cores, using the exact same compiler binaries and RTL. The [current runtime repair and 16-core refresh](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-23-gf180-runtime-audit/README.md) remeasures RustSys on all three PDKs; unchanged Yosys measurements retain their original provenance. The repaired RustSys netlists match the previous checked bytes; their LEC and OpenSTA receipts are rebound explicitly, without claiming a fresh solver run.

This page covers synthesis only. The [Ibex RTL-to-GDS benchmark](https://lab-emi.github.io/RustEDA/benchmarks/) compares the complete RustEDA flow with LibreLane.

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

<!-- benchmark-16-core:end -->

</div>

**Peak mem** = maximum GNU time process peak RSS across the three timed runs,
in MiB. Aggregate cgroup memory peaks remain in the complete receipts.

† **Setup-Fmax is an OpenSTA estimate** at the TT corner with ideal clocks and no wire
parasitics. These raw estimates do not establish achievable frequency. [Timing scope](performance.md#interpret-the-timing-estimate).

Speedups use the recorded medians before display rounding. All **six Ibex
netlists pass arbitrary-state RTL LEC**, with no reset or input assumptions.

<details markdown="1">
<summary>Historical 2026-10-21 baseline: full 1/16-core results, CPU use, area, setup-Fmax and LEC</summary>

<!-- benchmark-full-matrix:start -->

| Design · PDK | Tool<br>RustSys vs Yosys | 1 core, s ↓ | 16 cores, s ↓ | 1→16 speedup | Active cores / 16 | Cell area, µm² ↓ | Setup Fmax‡, MHz ↑ | Arbitrary-state RTL LEC |
|---|---|---:|---:|---:|---:|---:|---:|---|
| Ibex · Sky130 HD | RustSys | 1.65 | 0.89 | 1.85× | 3.76 | 117,405 | 42.92 | Proven · 1 & 16 cores |
| Ibex · Sky130 HD | Yosys | 6.67 | 6.66 | 1.00× | 1.06 | 123,670 | 33.47 | Proven · 1 & 16 cores |
| Ibex · GF180 7T | RustSys | 1.59 | 0.98 | 1.62× | 3.18 | 356,149 | 14.93 | Proven · 1 & 16 cores |
| Ibex · GF180 7T | Yosys | 6.68 | 6.69 | 1.00× | 1.07 | 379,410 | 8.33 | Proven · 1 & 16 cores |

<!-- benchmark-full-matrix:end -->

In this historical baseline, each tool emits byte-identical netlists at both allocations, so area, timing and
proof results apply to both. Setup-Fmax is an ideal-clock, zero-wire OpenSTA
estimate; these raw values do not establish achievable or signoff Fmax. See the
[timing scope](performance.md#interpret-the-timing-estimate) and
[complete receipts](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md).

</details>

## GF180 runtime audit

The high-resolution audit compares the frozen pre-area-fix producer with the
runtime repair at the same 16 physical cores. The complete hardware inputs and
all synthesis options are unchanged. Primary cohorts fix counts, seed and balanced
A/B–B/A order before running: 40 Ibex pairs, with two excluded paired warm-ups.
A/A controls run the same repaired executable on both sides: eight pairs.
Earlier cohorts are retained separately.

<div class="rustsys-audit" markdown="1" role="region" aria-label="GF180 before and current RustSys paired runtime audit" tabindex="0">

| Design · GF180 7T | A/B pairs | Paired median Δ, ms | 99% interval, ms |
|---|---:|---:|---:|
| Ibex | 40 | -2.898 | [-9.946, +1.393] |

</div>

On small screens, swipe horizontally. Δ is current minus before: positive means
slower. It is the median of paired differences. Intervals are exact 99% binomial
median intervals. The [full evidence](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-23-gf180-runtime-audit/README.md) also records paired means, 10,000 seeded
bootstrap replicates, log ratios, ranges and exact sign tests with Holm correction.
A/A variability is reported separately and never subtracted.

Additional Sky130/EZ130 controls compare the previous `9afbf2b` producer with
this repair: 20 pairs per PDK, two paired warm-ups and four A/A controls. Their
intervals remain separate in the full evidence. They address short-run changes
in the other libraries without assuming that every rounded 0.01 s change is noise.

The original Ibex 0.01 s increase is not reproduced as a fixed slowdown.
The repair removes redundant candidate/launch/sink work; it preserves candidate
order, area ceilings and every selected netlist. No confirmed regression was
detected under the stated criterion. Intervals crossing zero remain inconclusive.
Clock precision is not measurement accuracy.

The native monotonic clock includes Python startup/shutdown, all four CLI
startups, initialization/import, mmap, synthesis and full Verilog export.
Resource-guard startup, source preparation, LEC and STA are outside that clock;
no overhead is subtracted. Campaign-wide scans run outside primary timing.
This boundary is separate from the original GNU-time comparison above. The host remains shared.

Every timed GF180 netlist, including warm-ups and A/A controls, matches the
checked bytes, cell inventory and area. The three-PDK implementations
preserve their netlist bytes, so the raw OpenSTA estimates are unchanged.
The arbitrary-state RTL LEC receipts for both tools are revalidated and
rebound by netlist, RTL, library and top. This is exact-byte reuse, not a fresh
solver/STA invocation. No reset, input or initial-state restriction is added.

## One common protocol

The Sky130/GF180 baseline measures both tools on **1 and 16 physical host cores**, with one hardware thread per core. Every design/library/tool/core configuration has one excluded warm-up and three measured repetitions. Compare median wall time and actual CPU accounting; the allowed CPU count is not a utilization measurement. One-core reference times are measured, not inferred from the sixteen-core process's CPU use.

The timer starts with prepared RTL and includes process startup, workspace initialization/import, synthesis, mmap I/O, and complete gate-level Verilog export. Download, source preparation, LEC and static timing analysis run outside the synthesis timer. Each large child has an explicit memory, swap, file-size and wall-clock guard; high-memory jobs run serially. The host is shared, not an isolated benchmark appliance.

RustSys uses the frozen CPU producer with `--goal area --effort medium`, a 4 GiB cut-storage budget and the default bounded area-capped cleanup. The report records requested and effective cut counts and cleanup status. Yosys uses one published initial-state-preserving structural-memory flow and its **default ABC library mapping**. No fast mapping or reduced-cut profile is mixed into the common medians. The command scripts are retained in the archive.

The initial-state requirement matters. Standard synthesis flows can exploit undefined state or data in ways that differ from RustSys's two-valued contract. The common v3 reference flow completes ordinary X/Z data before flip-flop optimization, preserving arbitrary current state through the first clock. Later enable extraction remains enabled. Independent dual-PDK controls require an external counterexample for the earlier order and a complete proof for v3. Functional checking adds no reset sequence or input assumption.

Default cleanup uses the original cover's area as its ceiling and applies the same graph/library
rules to every design. It adds no per-design timing target or reset assumption. Explicit timed
profiles remain separate experiments. Each published sample retains its original source campaign;
reused reference measurements are labeled as reuse.

The IHP/EZ130 extension applies the same protocol at **16 physical cores**, with
new, independently checked netlists for both tools. The current refresh remeasures
RustSys at 16 cores on all three PDKs. EZ130 has no one-core measurements or within-tool scaling claim.
All refreshed rows meet the previous RustSys area and raw setup-Fmax
results; timing limitations and per-run variability remain visible in the receipts.

The EZ130 release’s tie-cell Liberty driver-type tags conflict with its own
constant-output Verilog models. Functional LEC uses the pinned vendor constants
through an externally checked, verification-only supplement. Synthesis, area and
STA use the original Liberty bytes. This nominal functional scope excludes
electrical strengths and high impedance; it adds no initial-state or reset
assumptions. See [the exact model scope](../verification/pdk.md#additional-ihp-sg13g2-benchmark-library).

## What the design includes

| Design | Configuration |
|---|---|
| Ibex | Upstream configured RV32IMC core, flip-flop register file, no instruction cache. |

Memory models are lowered to standard cells inside the timed synthesis region. There are no SRAM black boxes. The benchmark's 16 cores describe the **host CPU allocation**, not a larger or smaller RTL configuration.

An Ibex runtime is a synthesis/export measurement for this prepared core. It is not source-download time, full-SoC compilation time, physical implementation time or proof time.

## Read speed, resource use and quality together

| Metric | Meaning |
|---|---|
| Wall time | Median of three complete measured synthesis/export jobs. |
| Peak process RSS | Worst GNU time peak across the three samples; aggregate cgroup usage is recorded separately. |
| Average active cores | Median of `(user CPU seconds + system CPU seconds) / wall seconds`. |
| Within-tool CPU scaling | That tool's one-core median divided by its 16-core median. |
| Comparative speedup | Yosys median divided by RustSys median at the same allocated core count. |
| Cell area | Sum of mapped standard-cell areas in the selected library. |
| Setup-Fmax estimate | Independent OpenSTA result for the exact measured netlist and stated timing model. |
| RTL LEC | A complete external certificate for that netlist, source, library and semantic scope. |

The unified table shares area, timing and proof across allocations only after both tools'
netlist bytes and cell counts match between 1 and 16 cores. Different outputs require separate
quality evidence. Read the actual pairs; the measured suite does not establish universal PPA
superiority on other designs or constraints.

## Proof and timing scope

The common functional proof uses the independent RTL event model with ordinary X/Z data and undriven values completed to zero, plus external ABC CEC and complete observed-arithmetic/context compositions where required. Arisca checks exact arithmetic components; ABC checks their extraction and reconstruction, all Boolean context and transport to the measured netlist. **Register initial states remain arbitrary**: data completion does not initialize registers. All original outputs and retained state data, clock/enable and asynchronous controls remain in the proof. Export alone is incomplete.

The [checking guide](checking.md) and [verification overview](../verification/index.md) describe state correspondence, controls, hash binding and incomplete results. A missing tool, timeout, exhausted work budget or partial proof is not a pass.

Setup-Fmax uses the same TT library and ideal-clock/zero-wire model for both tools. It is not placement/routing, timing signoff, hold closure or physical DRC. The [performance guide](performance.md#interpret-the-timing-estimate) gives the exact slew/load/I/O assumptions and the paths excluded from this metric. Constraint-check receipts stay with each result.

## Retained verification baselines

The Ibex comparison above is the only performance comparison in the README; its
linked archives retain each campaign's complete raw receipts. Two earlier
result sets remain because the verification harness uses their exact input bindings
and complete certificates:

- [Four-core RTL proofs](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-20-lec-complete/README.md): three-run medians and an undef-aware refinement scope.
- [Calibrated OpenC910 proofs](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-20-openc910-verified/README.md): corrected RTL interpretation, reference calibration and complete projected obligations.
- [Proof invalidation registry](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/proof-invalidations.json): rejected producer hashes remain recorded after obsolete campaigns are removed.

Do not mix those baseline profiles or single samples into the current campaign's
three-run medians. The [reproduction guide](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/README.md)
retains the pinned upstream commits, libraries, executable hashes and commands.
