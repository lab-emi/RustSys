# Common multicore synthesis benchmark, 2026-10-21

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

‡ OpenSTA 3.1.0 setup-Fmax estimate at each library's TT corner: ideal clocks,
100 ps input/clock slew, 10 fF output load, zero I/O delays and no wire parasitics.
MHz = 1000 / reported minimum period in ns, within the report's printed precision.
Same-clock, same-edge register paths only; port paths, cross-edge paths and
level-sensitive latch paths are excluded by `report_clock_min_period`.
Unclocked or otherwise unconstrained endpoints are outside this estimate;
constraint-check receipts are retained. OpenC910 exceeds the default cleanup budget. OpenC910/Sky130 critical-path loads exceed the Liberty characterization range (RustSys/Yosys: 49.60/55.98 pF; respective cell limits 0.052195/0.190440 pF). These raw estimates do not establish achievable or signoff Fmax.

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

## CPU scaling and quality preservation


Average active cores = (GNU time user+system CPU seconds)/wall seconds,
median across three runs. This measures actual use, not requested workers or
peak utilization. RustSys 1→16-core scaling and RustSys/Yosys speedup are distinct.
Byte-identical 1-core and 16-core netlists from each tool preserve cell area and timing under the
same constraints. This does not claim universal PPA superiority over Yosys.

Speedup spans 1.50×–42.11× across all ten measured pairs.
RustSys self-scaling spans 1.30×–4.02×. No row is dropped.

All timing and proof records bind exact measured netlist/library bytes. Fresh
small-design proofs export every original output/state/event obligation and
require complete external CEC or arithmetic/context composition. Model export
alone is undecided. Exact-byte
full-core certificates can be reused only with their measured inputs unchanged.
Data and clock corruptions are rejected on both PDKs; signed/overflow arithmetic
calibration checks both low-bit and high-bit faults. Resource-limited/missing
results cannot be published as passes.
Frozen producer hashes identify the implementation measured in this campaign.
Source-campaign records identify separately collected reference measurements;
exact-byte proof or timing reuse is explicitly marked in each certificate and
does not represent a fresh solver or STA invocation.

[Measurements](measurements.json.gz) · [LEC](proofs.json.gz.index.json) · [OpenSTA](timing.json.gz) ·
[Controls](controls.json.gz) · [Raw receipts and harness](evidence.tar.gz.index.json) ·
[Large artifact hashes](large-artifacts.json.gz) · [Provenance](provenance.json).

Files above 1 MiB are split into ordered parts. Their index records the combined
SHA-256; concatenate the listed parts in order before decompressing. Raw data,
failed probes and larger models remain in the hash-listed local workspace.
Reproducible model payloads above the recorded copy limit are hash-listed;
small models and complete solver receipts are included in the streamed archive.

[Source snapshot, acceptance and optimization controls](supplement.md).

[Documentation and publication checks](publication-checks.json).
