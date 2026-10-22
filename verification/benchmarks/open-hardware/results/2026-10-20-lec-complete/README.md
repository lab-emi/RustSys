# Arbitrary-state RTL verification, 2026-10-20

Complete external proofs: **RustSys 8/8, Yosys 8/8**.

| Core | PDK | Time, s ↓<br>RustSys / Yosys | Peak RAM, MiB ↓<br>RustSys / Yosys | Cell area, µm² ↓<br>RustSys / Yosys | RTL LEC<br>RustSys / Yosys |
|---|---|---:|---:|---:|---|
| Ibex | Sky130 HD | 1.74 / 6.41 | 96 / 160 | 121,075 / 121,358 | Proven / Proven |
| Ibex | GF180 7T | 1.63 / 6.41 | 117 / 159 | 370,341 / 374,108 | Proven / Proven |
| PicoRV32 | Sky130 HD | 0.91 / 1.29 | 66 / 76 | 78,260 / 77,797 | Proven / Proven |
| PicoRV32 | GF180 7T | 0.87 / 1.55 | 117 / 96 | 242,576 / 237,336 | Proven / Proven |
| SERV | Sky130 HD | 0.67 / 0.91 | 66 / 72 | 54,418 / 51,832 | Proven / Proven |
| SERV | GF180 7T | 0.75 / 1.19 | 117 / 91 | 174,104 / 157,813 | Proven / Proven |
| VexRiscv | Sky130 HD | 24.03 / 63.25 | 962 / 702 | 2,582,666 / 2,897,729 | Proven / Proven |
| VexRiscv | GF180 7T | 16.65 / 86.74 | 957 / 834 | 8,131,699 / 8,882,980 | Proven / Proven |

Each certificate is checked against the exact netlist from all three timed repetitions. The proof covers arbitrary implementation initial states under checked state correspondence, all original outputs, and every retained state transition, clock and reset. No reset sequence or input assumption is added. Defined RTL bits must match exactly; undefined RTL bits permit Boolean refinement. Optional full_case/parallel_case promises are removed before parsing for both tools.

Proofs use complete EQY/SBY results, external ABC CEC, or undef-aware Yosys SAT on every inductive obligation after all temporal storage is removed. Arithmetic compositions use AMulet certificates independently checked by Pacheck, or an Arisca signed multiply-add theorem with exact operand binding and unchanged-context checks, plus both adjoining whole-design proofs. A timeout, counterexample, memory limit, or partial proof remains unverified. Native RustSys checker results are recorded separately.

[Measured data](results.json.gz) · [Selected certificates](proofs.json) · [Evidence](evidence.tar.gz.index.json) · [Hashes and omitted reproducible models](provenance.json.gz) · [Reproduction](../../README.md)

The evidence archive is split into ordered parts for Git hosting. Concatenate the
parts listed in `evidence.tar.gz.index.json` to reconstruct `evidence.tar.gz`;
its combined SHA-256 is unchanged. `SHA256SUMS` checks the published files.
