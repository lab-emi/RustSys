# Validated 16-core synthesis refresh

**CPU: AMD Ryzen 9 9950X3D 16-Core Processor.** 16 physical host cores per tool, without SMT siblings.
One warm-up and three measured runs; median end-to-end seconds, maximum process RSS.
Time includes initialization/import, synthesis, mmap and full Verilog export.
Source preparation, LEC and OpenSTA run outside the timer.

| Design | PDK | RustSys<br>16 cores, s ↓ | Yosys<br>16 cores, s ↓ | RustSys<br>Speedup ↑ | RustSys<br>Peak mem, MiB ↓ | Yosys<br>Peak mem, MiB ↓ | RustSys<br>Area, µm² ↓ | Yosys<br>Area, µm² ↓ | RustSys<br>Setup Fmax†, MHz ↑ | Yosys<br>Setup Fmax†, MHz ↑ |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Ibex | Sky130 HD | 0.90 | 6.66 | **7.40×** | 93.4 | 273.1 | 117,405 | 123,670 | 42.92 | 33.47 |
| Ibex | GF180 7T | 0.98 | 6.69 | **6.83×** | 118.9 | 273.0 | 356,149 | 379,410 | 14.93 | 8.33 |
| Ibex | EZ130 8T | 0.77 | 6.80 | **8.83×** | 92.1 | 272.4 | 145,667 | 151,762 | 55.13 | 43.31 |
| PicoRV32 | Sky130 HD | 0.47 | 1.12 | **2.38×** | 67.3 | 81.0 | 76,169 | 83,603 | 107.53 | 67.75 |
| PicoRV32 | GF180 7T | 0.58 | 1.51 | **2.60×** | 117.2 | 100.2 | 233,097 | 255,798 | 26.65 | 15.98 |
| PicoRV32 | EZ130 8T | 0.44 | 1.46 | **3.32×** | 68.4 | 76.4 | 97,920 | 99,559 | 157.73 | 73.15 |
| SERV | Sky130 HD | 0.36 | 0.87 | **2.42×** | 66.2 | 71.9 | 51,461 | 54,067 | 60.28 | 57.47 |
| SERV | GF180 7T | 0.77 | 1.14 | **1.48×** | 117.5 | 90.4 | 163,404 | 167,428 | 43.27 | 19.54 |
| SERV | EZ130 8T | 0.31 | 1.10 | **3.55×** | 65.9 | 67.0 | 64,550 | 65,052 | 181.49 | 67.02 |
| VexRiscv | Sky130 HD | 2.74 | 63.53 | **23.19×** | 472.9 | 840.2 | 2,399,458 | 2,909,809 | 10.22 | 1.56 |
| VexRiscv | GF180 7T | 2.28 | 79.27 | **34.77×** | 483.6 | 864.1 | 7,490,709 | 8,856,262 | 4.47 | 0.689 |
| VexRiscv | EZ130 8T | 3.24 | 79.06 | **24.40×** | 892.7 | 853.5 | 3,225,087 | 3,429,484 | 4.20 | 2.12 |
| OpenC910 | Sky130 HD | 70.15 | 1,413.58 | **20.15×** | 7,490.5 | 16,339.8 | 53,453,688 | 59,995,158 | 0.69 | 0.0486 |
| OpenC910 | GF180 7T | 66.92 | 2,712.45 | **40.53×** | 7,648.0 | 17,511.3 | 165,273,357 | 184,901,294 | 0.0989 | 0.00397 |
| OpenC910 | EZ130 8T | 75.75 | 2,811.45 | **37.11×** | 8,427.2 | 16,979.2 | 71,069,802 | 72,190,882 | 0.0171 | 0.0159 |

RustSys was remeasured for all fifteen design/PDK combinations. Yosys measurements
are retained from the pinned reference campaigns listed in provenance.json;
no new Yosys timing run is claimed. Changed netlists require fresh complete LEC
and OpenSTA; exact-byte reuse checks RTL, library, top, tools and original receipts.
All thirty tool/design/PDK entries have complete arbitrary-state RTL LEC.
No reset sequence or input assumptions are added. Ordinary X/Z data and undriven
wires use the existing explicit zero-completion contract, without initializing state.

EZ130 functional LEC retains the certified nominal vendor tie-cell interpretation.
The two conflicting Liberty driver-type attributes are recorded in the supplement;
the original synthesis/timing library is unchanged. Strength/high-impedance behavior
is outside this functional proof. OpenSTA Fmax is an ideal-clock, zero-wire,
same-edge register setup estimate. SERV/EZ130 and OpenC910 loads exceed characterization;
these estimates do not establish achievable or signoff frequency.

[Measurements](measurements.json.gz) · [Proofs](proofs.json.gz.index.json) · [Timing](timing.json.gz) · [Quality](quality.json.gz) · [Receipts](evidence.tar.gz.index.json) · [Large artifact hashes](large-artifacts.json.gz) · [Provenance](provenance.json).
Historical 1/16-core measurements remain in their original archives.

The evidence archive also contains alternating-version SERV/VexRiscv timing controls and the full-core projection receipts. The GF180 SERV comparison differs by one 0.01 s reporting unit with overlapping samples; the wall-time observations do not establish universal runtime equality. See the [timing and control scope](https://lab-emi.github.io/RustSys/guide/performance/).
