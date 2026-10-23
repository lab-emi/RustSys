# GF180 alternating runtime audit

**CPU: AMD Ryzen 9 9950X3D 16-Core Processor.** Fixed 16 physical cores, no SMT siblings; shared host.
Before = frozen pre-SERV-area producer; current = frozen runtime-repair producer.
All producer executable hashes are pinned in the complete cohort measurements.
Each cohort fixes its counts, seed and balanced A/B versus B/A order before running.
Two paired warm-ups are excluded. A/A controls execute the current binary on both sides.

The initial 9afbf2b producer showed positive VexRiscv paired mean shifts of 16.740, 38.467 and 21.022 ms in three cohorts. Their individual intervals were inconclusive. The persistent tendency and redundant candidate work motivated the runtime repair; non-significance was not treated as evidence of equality. A separate 32-pair 9afbf2b/repair VexRiscv control and all initial cohorts remain in all-cohorts. Different monitor profiles are not pooled into the primary result.


| Design · GF180 7T | A/B pairs | Before, s | Current, s | Paired median Δ, ms | 99% median interval, ms |
|---|---:|---:|---:|---:|---:|
| Ibex | 40 | 0.9578 | 0.9514 | -2.898 | [-9.946, +1.393] |
| PicoRV32 | 40 | 0.5640 | 0.5588 | -5.063 | [-9.458, -2.269] |
| SERV | 40 | 0.7464 | 0.7401 | -6.113 | [-12.138, -2.229] |
| VexRiscv | 64 | 2.3100 | 2.3104 | -0.463 | [-48.791, +37.252] |
| OpenC910 | 12 | 64.4327 | 64.4488 | -221.553 | [-1352.275, +880.824] |

Same-binary controls (reported separately; never subtracted from A/B):

| Design · GF180 7T | A/A pairs | Same-binary median Δ, ms | 99% median interval, ms |
|---|---:|---:|---:|
| Ibex | 8 | -1.277 | [-14.566, +12.512] |
| PicoRV32 | 8 | +1.970 | [-4.959, +18.132] |
| SERV | 8 | -1.550 | [-15.664, +2.923] |
| VexRiscv | 8 | -23.417 | [-209.590, +19.157] |
| OpenC910 | 2 | -348.013 | Not estimable with two pairs |

Δ = current minus before; positive means slower. Before/current columns are
separate medians; Δ is the median of paired differences, so they need not subtract.
The 99% median intervals use exact binomial/order statistics. The machine-readable
results also retain paired means, seeded 10,000-replicate 99% bootstrap intervals,
log ratios, ranges, exact one-sided sign tests and Holm correction across five designs.
No confirmed regression was detected by the stated criterion. An interval containing
zero is inconclusive; it does not prove exact equality or exclude smaller regressions.
Read the absolute interval widths before interpreting a 0.01 s difference, particularly
for OpenC910. All exploratory and confirmation cohorts are retained separately.

The native monotonic timer wraps the unchanged complete CLI pipeline, including
Python startup/shutdown, all four CLI startups, initialization, RTL/Liberty import,
mmap, synthesis and full Verilog export. Resource-guard startup, source preparation,
LEC and OpenSTA are outside the clock; no overhead is subtracted. GNU time retains
CPU accounting and peak RSS, but its centisecond wall-time field is not used here.
Primary cohorts move campaign-wide disk scans outside the timer; OpenC910's bounded
periodic scanner checks only its current case. Earlier cohorts used a growing
campaign scan during timing and are labeled supplemental, never pooled silently.

Every timed netlist, including warm-ups and controls, is byte-identical to its
GF180 baseline/current implementation and has identical cell inventory and area.
Complete arbitrary-state RTL LEC and OpenSTA receipts are rebound by exact netlist,
RTL, library and top; all 30 retained tool/design/PDK certificates are revalidated.
This is explicit exact-byte reuse, not a fresh solver or STA run. No reset sequence,
input assumption or initial-state restriction is added. The runtime repair skips
unaffordable drive alternatives, reuses state-output pin roles and sink limits,
and avoids duplicate launch calculations. A balanced-truth precheck rejects
impossible mux functions. Candidate order, area ceilings and selected netlists
are preserved; the established three-PDK area/Fmax results remain unchanged.
The original two-valued data/X/Z completion, certified EZ130 nominal tie-cell scope,
and ideal-clock/zero-wire timing limitations remain in force. SERV/EZ130 and
OpenC910 raw setup-Fmax extrapolations do not establish achievable frequency.

## Sky130/EZ130 paired controls

Separate 9afbf2b versus repair comparisons: 20 pairs per smaller design/PDK,
two excluded paired warm-ups and four same-binary controls; Holm family size 15.
All selected netlists, cell inventories, areas and raw setup estimates are unchanged.

| Design | PDK | A/B pairs | Paired median Δ, ms | 99% median interval, ms |
|---|---|---:|---:|---:|
| Ibex | EZ130 8T | 20 | -1.290 | [-4.169, +3.628] |
| Ibex | Sky130 HD | 20 | +4.228 | [-2.243, +22.788] |
| PicoRV32 | EZ130 8T | 20 | +1.689 | [-7.479, +8.082] |
| PicoRV32 | Sky130 HD | 20 | +1.821 | [-5.363, +7.323] |
| SERV | EZ130 8T | 20 | -3.187 | [-5.587, +3.289] |
| SERV | Sky130 HD | 20 | -2.898 | [-7.478, +5.484] |
| VexRiscv | EZ130 8T | 20 | +11.785 | [-298.764, +246.978] |
| VexRiscv | Sky130 HD | 20 | +2.603 | [-187.276, +248.467] |

## Three-PDK RustSys/Yosys refresh

One warm-up and three measured repetitions at 16 physical cores, original GNU-time
boundary. RustSys is freshly remeasured with the repaired producer; Yosys runs
retain their pinned original provenance. These times are not the paired audit
times above. Peak RSS is the maximum measured process peak across three runs.
Setup-Fmax retains the original ideal-clock, zero-wire, TT, same-edge register
scope and characterization/excluded-path limitations.

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

[statistics](statistics.json.gz) · [all-cohorts](all-cohorts.json.gz.index.json) · [quality-and-lec](quality-and-lec.json.gz) · [checked-file-hashes](checked-file-hashes.json.gz) · [measurements](measurements.json.gz) · [proofs](proofs.json.gz.index.json) · [timing](timing.json.gz) · [quality](quality.json.gz) ·
[Raw clocks, commands, reports, frozen harnesses and resource receipts](evidence.tar.gz.index.json) ·
[Large artifact SHA-256 inventory](large-artifacts.json.gz) · [Provenance](provenance.json).
Original proof payloads remain in the
[SERV-area archive](../2026-10-23-serv-area/README.md),
[Sky130/GF180 archive](../2026-10-21-default-multicore/README.md) and
[EZ130 archive](../2026-10-23-ez130/README.md); reused receipts retain those source paths.
The RustSys/Yosys overview retains the original three-run protocol and timer
boundary; the alternating producer audit is a separate experiment.
