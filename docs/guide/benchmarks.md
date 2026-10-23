# Understand the benchmarks

The [README comparison](https://github.com/lab-emi/RustSys#benchmarks) uses one common protocol for **Ibex, PicoRV32, SERV, VexRiscv and OpenC910**, each mapped to Sky130 HD, GF180 7T, and **EZ130 8T v1.1 on IHP SG13G2 (TT 1.20 V / 25°C)**. The [complete unified archive](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md) contains the baseline 1/16-core samples, commands, input pins, limits, and proof receipts. The [IHP/EZ130 extension](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-23-ez130/README.md) adds the same five designs and both tools at 16 cores, using the exact same compiler binaries and RTL. The [current runtime repair and 16-core refresh](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-23-gf180-runtime-audit/README.md) remeasures all fifteen RustSys design/PDK combinations; unchanged Yosys measurements retain their original provenance. All fifteen repaired RustSys netlists match the previous checked bytes; all thirty tool/design/PDK LEC and OpenSTA receipts are rebound explicitly, without claiming a fresh solver run.

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
these raw estimates do not establish achievable frequency. [Timing scope](performance.md#interpret-the-timing-estimate).

Speedups use the recorded medians before display rounding. All **30 distinct
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

<!-- benchmark-full-matrix:end -->

In this historical baseline, each tool emits byte-identical netlists at both allocations, so area, timing and
proof results apply to both. Setup-Fmax is an ideal-clock, zero-wire OpenSTA
estimate. OpenC910 loads exceed Liberty characterization limits; these raw
values do not establish achievable or signoff Fmax. See the
[timing scope](performance.md#interpret-the-timing-estimate) and
[complete receipts](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md).

</details>

## GF180 runtime audit

The high-resolution audit compares the frozen pre-SERV-area producer with the
runtime repair at the same 16 physical cores. The complete hardware inputs and
all synthesis options are unchanged. Primary cohorts fix counts, seed and balanced
A/B–B/A order before running: 40 pairs each for Ibex, PicoRV32 and SERV, 64 for
VexRiscv and 12 for OpenC910, with two excluded paired warm-ups. A/A controls run
the same repaired executable on both sides: eight pairs per smaller design and
two for OpenC910. Earlier cohorts are retained separately.

<div class="rustsys-audit" markdown="1" role="region" aria-label="GF180 before and current RustSys paired runtime audit" tabindex="0">

| Design · GF180 7T | A/B pairs | Paired median Δ, ms | 99% interval, ms |
|---|---:|---:|---:|
| Ibex | 40 | -2.898 | [-9.946, +1.393] |
| PicoRV32 | 40 | -5.063 | [-9.458, -2.269] |
| SERV | 40 | -6.113 | [-12.138, -2.229] |
| VexRiscv | 64 | -0.463 | [-48.791, +37.252] |
| OpenC910 | 12 | -221.553 | [-1352.275, +880.824] |

</div>

On small screens, swipe horizontally. Δ is current minus before: positive means
slower. It is the median of paired differences. Intervals are exact 99% binomial
median intervals. The [full evidence](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-23-gf180-runtime-audit/README.md) also records paired means, 10,000 seeded
bootstrap replicates, log ratios, ranges and exact sign tests with Holm correction
across five designs. A/A variability is reported separately and never subtracted.
Two OpenC910 A/A pairs are descriptive; no 99% control interval is claimed.

Additional Sky130/EZ130 controls compare the previous `9afbf2b` producer with
this repair: 20 pairs per smaller design/PDK, two paired warm-ups and four A/A
controls. Their eight intervals remain separate in the full evidence, with a
fifteen-comparison Holm family. They address short-run changes in the other
libraries without assuming that every rounded 0.01 s change is noise.

The original SERV/Ibex 0.01 s increase is not reproduced as a fixed slowdown.
The repaired VexRiscv cohort is centered near zero, following positive shifts in
three earlier cohorts. The repair removes redundant candidate/launch/sink work;
it preserves candidate order, area ceilings and every selected netlist.
No confirmed regression was detected under the stated criterion. Intervals
crossing zero remain inconclusive: VexRiscv and OpenC910 do not resolve a 0.01 s
change at 99% confidence. Clock precision is not measurement accuracy.

The native monotonic clock includes Python startup/shutdown, all four CLI
startups, initialization/import, mmap, synthesis and full Verilog export.
Resource-guard startup, source preparation, LEC and STA are outside that clock;
no overhead is subtracted. Campaign-wide scans run outside primary timing;
OpenC910's bounded periodic scan visits only its current case. This boundary
is separate from the original GNU-time comparison above. The host remains shared.

Every timed GF180 netlist, including warm-ups and A/A controls, matches the
checked bytes, cell inventory and area. All fifteen three-PDK implementations
preserve their netlist bytes, so the raw OpenSTA estimates are unchanged.
All thirty tool/design/PDK arbitrary-state RTL LEC receipts are revalidated and
rebound by netlist, RTL, library and top. This is exact-byte reuse, not a fresh
solver/STA invocation. No reset, input or initial-state restriction is added.

## One protocol for all five designs

The Sky130/GF180 baseline measures both tools on **1 and 16 physical host cores**, with one hardware thread per core. Every design/library/tool/core configuration has one excluded warm-up and three measured repetitions. Compare median wall time and actual CPU accounting; the allowed CPU count is not a utilization measurement. One-core reference times are measured, not inferred from the sixteen-core process's CPU use.

The timer starts with prepared RTL and includes process startup, workspace initialization/import, synthesis, mmap I/O, and complete gate-level Verilog export. Download, source preparation, LEC and static timing analysis run outside the synthesis timer. Each large child has an explicit memory, swap, file-size and wall-clock guard; high-memory jobs run serially. The host is shared, not an isolated benchmark appliance.

RustSys uses the frozen CPU producer with `--goal area --effort medium`, a 4 GiB cut-storage budget and the default bounded area-capped cleanup. The report records requested and effective cut counts and cleanup status. Yosys uses one published initial-state-preserving structural-memory flow and its **default ABC library mapping** for all five designs. No fast mapping or reduced-cut OpenC910 profile is mixed into the common medians. The command scripts are retained in the archive.

The initial-state requirement matters. Standard synthesis flows can exploit undefined state or data in ways that differ from RustSys's two-valued contract. The common v3 reference flow completes ordinary X/Z data before flip-flop optimization, preserving arbitrary current state through the first clock. Later enable extraction remains enabled. Independent dual-PDK controls require an external counterexample for the earlier order and a complete proof for v3. Functional checking adds no reset sequence or input assumption.

Default cleanup uses the original cover's area as its ceiling and applies the same graph/library
rules to every design. It adds no per-design timing target or reset assumption. Explicit timed
profiles remain separate experiments. Each published sample retains its original source campaign;
reused reference measurements are labeled as reuse.

The IHP/EZ130 extension applies the same protocol at **16 physical cores**: ten
additional tool/design configurations and ten new, independently checked netlists.
Those historical campaigns contain fifty measured configurations and thirty distinct
netlists. The current refresh adds fifteen RustSys configurations at 16 cores. EZ130 has no one-core measurements or within-tool scaling claim.
The current SERV/EZ130 result uses 0.77% less area than Yosys and synthesizes
3.55× faster. All refreshed rows meet the previous RustSys area and raw setup-Fmax
results; timing limitations and per-run variability remain visible in the receipts.

The EZ130 release’s tie-cell Liberty driver-type tags conflict with its own
constant-output Verilog models. Functional LEC uses the pinned vendor constants
through an externally checked, verification-only supplement. Synthesis, area and
STA use the original Liberty bytes. This nominal functional scope excludes
electrical strengths and high impedance; it adds no initial-state or reset
assumptions. See [the exact model scope](../verification/pdk.md#additional-ihp-sg13g2-benchmark-library).

## What each design includes

| Design | Configuration |
|---|---|
| Ibex | Upstream configured RV32IMC core, flip-flop register file, no instruction cache. |
| PicoRV32 | Upstream default RV32I configuration with its internal register file. |
| SERV | Upstream `serv_rf_top`, W=1, WITH_CSR=1 and internal register file. |
| VexRiscv | LiteX default RV32IM core, 4 KiB instruction and data caches. |
| OpenC910 | Complete `ct_core` processor with default 64 KiB instruction and data caches. |

Memory models are lowered to standard cells inside the timed synthesis region. There are no SRAM black boxes or reduced cache sizes. OpenC910's L2, second processor core and cluster peripherals are outside `ct_core`. The benchmark's 16 cores describe the **host CPU allocation**, not a larger or smaller RTL configuration.

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

The current campaign is the only performance comparison in the README. Two earlier
result sets remain because the verification harness uses their exact input bindings
and complete certificates:

- [Four-core RTL proofs](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-20-lec-complete/README.md): three-run medians and an undef-aware refinement scope.
- [Calibrated OpenC910 proofs](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-20-openc910-verified/README.md): corrected RTL interpretation, reference calibration and complete projected obligations.
- [Proof invalidation registry](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/proof-invalidations.json): rejected producer hashes remain recorded after obsolete campaigns are removed.

Do not mix those baseline profiles or single samples into the current campaign's
three-run medians. The [reproduction guide](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/README.md)
retains the pinned upstream commits, libraries, executable hashes and commands.
