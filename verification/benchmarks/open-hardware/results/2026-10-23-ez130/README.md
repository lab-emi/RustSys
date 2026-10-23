# IHP SG13G2 / EZ130 8T synthesis benchmark

**CPU: AMD Ryzen 9 9950X3D 16-Core Processor.** Both tools use 16 physical host cores, without SMT siblings.
One warm-up and three measured runs per tool/design; reported time is the median.
The exact same RustSys and Yosys executables and RTL as the baseline campaign are used.
Time includes workspace init/import, synthesis, mmap and full exported Verilog.
Preparation, LEC and OpenSTA are outside the timer. Timed cases run serially on a shared host.
Peak mem is the maximum GNU-time process RSS across the three measurements; aggregate cgroup peaks remain in the receipts.

| Design | PDK | RustSys<br>16 cores, s ↓ | Yosys<br>16 cores, s ↓ | RustSys<br>Speedup ↑ | RustSys<br>Peak mem, MiB ↓ | Yosys<br>Peak mem, MiB ↓ | RustSys<br>Area, µm² ↓ | Yosys<br>Area, µm² ↓ | RustSys<br>Setup Fmax†, MHz ↑ | Yosys<br>Setup Fmax†, MHz ↑ |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Ibex | EZ130 8T | 0.84 | 6.80 | **8.10×** | 95.7 | 272.4 | 145,667 | 151,762 | 55.13 | 43.31 |
| PicoRV32 | EZ130 8T | 0.46 | 1.46 | **3.17×** | 69.0 | 76.4 | 97,920 | 99,559 | 157.73 | 73.15 |
| SERV | EZ130 8T | 0.35 | 1.10 | **3.14×** | 67.1 | 67.0 | 69,321 | 65,052 | 121.65 | 67.02 |
| VexRiscv | EZ130 8T | 3.09 | 79.06 | **25.59×** | 863.1 | 853.5 | 3,225,087 | 3,429,484 | 4.20 | 2.12 |
| OpenC910 | EZ130 8T | 83.37 | 2,811.45 | **33.72×** | 8,641.9 | 16,979.2 | 71,069,987 | 72,190,882 | 0.0171 | 0.0159 |

All ten new netlists pass fresh arbitrary-state RTL LEC with no reset or input assumptions.
Every implementation initial state is covered by checked correspondence; ordinary X/Z data
and undriven wires use the same explicit zero completion as the baseline, without initializing registers.
Independent RTL event obligations are checked by external ABC; inferred arithmetic also uses Arisca.
Anonymous states use bounded one-to-one proposals, with every original state still free;
only a complete external event/output proof accepts the relation.

**Cell-model scope:** EZ130 v1.1 marks TIEHI/TIELO with `open_drain`/`open_source`
driver types that contradict the same release's constant-output Verilog models.
Functional LEC uses those pinned vendor Verilog constants, independently checked
against an explicit two-attribute functional Liberty supplement with positive
and corrupted-constant ABC controls. The original synthesis/timing library and
all measured netlists are retained unchanged. Electrical drive strengths and
high-impedance behavior are outside this functional proof; the conflict and
exact byte edits are included in the receipts. General tri-state/strength cells
remain rejected by the native checker.

OpenC910 retains the complete ct_core with default 64 KiB instruction/data caches lowered to cells.

† OpenSTA setup-Fmax at TT 1.20 V / 25°C, ideal clocks, 100 ps slew, 10 fF port loads,
zero I/O delays and no wire parasitics. Same-clock/same-edge register paths only;
other and unconstrained paths are outside this estimate. Large lowered memories
can exceed Liberty load characterization; this does not establish achievable or signoff Fmax.

Library: [EZ130 8T v1.1](https://iip.ethz.ch/ez-library/ihp130-8t.html), Apache-2.0, IHP SG13G2 130 nm.
Liberty SHA-256: `847cb30b38f15ef0b9c4a735cfaf96c1782c013b1952261b25e5e38cd3ae072c`. Download/archive/member checksums are pinned
in [additional-pdks.json](../../additional-pdks.json).
Please cite Castañeda et al., [An Open-Source Standard-Cell Library for IHP 130 nm Developed by Students](https://arxiv.org/abs/2609.29965).

[Measurements](measurements.json.gz) · [Proofs](proofs.json.gz) · [Timing](timing.json.gz) · [Controls](controls.json.gz) · [Raw receipts and measured harness](evidence.tar.gz.index.json) · [Large artifact inventory](large-artifacts.json.gz.index.json) · [Provenance](provenance.json).
Large models remain in the local workspace with streamed SHA-256 inventory.
Archive parts are at most 1 MiB; concatenate index-listed parts before decompressing.
