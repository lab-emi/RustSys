# OpenC910 large-design stress measurement

Pinned [OpenC910](https://github.com/XUANTIE-RV/openc910/tree/b91c90914c19f114d35c8f6b73408eb241ed847c), Apache-2.0,
top `ct_core`: **complete single core**, default 64 KiB instruction and 64 KiB data caches.
Every upstream functional FPGA RAM model is lowered to standard cells inside the timer.
L2, the second core and cluster peripherals are outside this top. No SRAM black boxes.

| PDK | Time, s ↓<br>RustSys / Yosys | Peak process RSS, MiB ↓<br>RustSys / Yosys | Standard cells<br>RustSys / Yosys | Cell area, µm² ↓<br>RustSys / Yosys | RTL LEC<br>RustSys / Yosys |
|---|---:|---:|---:|---:|---|
| Sky130 HD | 369.09 / 663.91 | 12,271 / 17,327 | 5,956,465 / 5,541,421 | 60,039,439 / 63,120,252 | Proven† / Proven† |
| GF180 7T | 273.57 / 685.93 | 12,313 / 18,451 | 5,688,532 / 5,900,714 | 178,830,078 / 196,370,477 | Proven† / Proven† |

Recorded synthesis failures: none.

One measured run per tool and library, no warm-up; this is a stress measurement, not
the three-run median of the four-core table. One pinned logical CPU (0),
32 GiB physical RAM per tool, plus 1 GiB swap per child,
10800 seconds per tool. Time includes init/import, synthesis,
mmap and full Verilog export. Preparation and proof attempts are outside the timer.
No repeat-determinism claim is made for a single sample.
Yosys samples and netlist bytes are retained unchanged from the bounded-mapping reference run. RustSys was freshly measured after the wildcard correction; both tools use the 32 GiB guard. Proof transport requires identical RTL, library and netlist hashes.

Yosys uses the arbitrary-initial-state flow in `yosys_initial.py`, structural memory lowering, `opt_merge -keepdc` and `opt_dff -keepdc`. Mapping uses the public ABC `strash; &get -n; &nf -C 4 -F 1; &put` script: at most four priority cuts and one area-flow round. No register initial value or reset assumption is introduced. The earlier default-profile probe failed at its 16 GiB child
memory limit; its failure is preserved in `provenance/baseline-failure.json`.
RustSys uses area/medium mapping with a 4 GiB cut-storage budget; consult each
`report.json` for requested and effective priority cuts. Resource-limited cut selection
changes the area/search tradeoff; it never shrinks the RTL or caches.

`Proven` requires a complete external certificate against this exact measured netlist.
`Proven†` uses the independent RTL interpreter with ordinary X/Z data literals set to zero,
undriven wires set to zero, and external ABC CEC of every output, state update,
clock/enable and asynchronous clear/preset obligation. Exact shared-structure
identities are discharged directly. Each remaining output cone is projected
without changing its Boolean function, with one common input map for both sides;
the interval ledger must cover every original obligation once. Uninitialized
registers remain arbitrary. Wildcard labels retain their `casez`/`casex` matching
semantics through concatenation and replication. Its named state relation covers every implementation
initial state. The native export itself is recorded as undecided; only ABC can
complete this certificate. The four-core table has its separately stated proof scope.
An em dash means the tool did not complete that metric. `Unverified` is not a pass.
Successful RSS is GNU time's maximum process RSS, including waited-for children;
cgroup aggregate memory/swap peaks are recorded separately. Failed-scope reporter
RSS is retained only as collector telemetry and is unavailable as compiler RSS.
All proof attempts retain arbitrary initial states and add no reset or input assumptions.

Corner is per library; wire model: none; all endpoints unconstrained; STA and DRC unchecked.

`results.json` retains raw samples, corners, PDK hashes and executable pins.
`large-artifacts.json` hashes the large generated files kept in the local benchmark
workspace; those multi-gigabyte artifacts are not copied into this bounded archive.
Raw tool logs above 4 MiB also remain in that hash inventory; commands, exit
statuses, timing reports and complete QoR JSON remain in the archive.
The producer source snapshot and exact build flags are in `provenance/`.
This corrected run uses `producer-wildcard-fixed-sources.json` and
`checker-wildcard-fixed-sources.json`. Earlier OpenC910 native certificates are
invalidated in `provenance/proof-invalidations.json`. The independent Icarus
truth table, exhaustive before/after results, Yosys formal comparisons and
negative old-netlist controls are in `controls/`; both complete Yosys baseline
proofs calibrated the corrected RTL interpreter before accepting RustSys.
Reproduction is documented in the [benchmark guide](../../README.md#large-design-stress-run).

## Archive layout

The complete original receipts, source snapshots, controls and original checksum
ledger are retained in [the evidence archive](evidence.tar.gz.index.json). Concatenate the
ordered parts listed in its index, verify the combined SHA-256, and extract the
tar archive to recover the original paths described above.

The benchmark verifier reads [the baseline results](results.json.gz.index.json) directly without
extracting design data. Compression and splitting preserve the decoded JSON bytes.
`SHA256SUMS` validates the published files. These baseline measurements are
separate from [the current unified comparison](../2026-10-21-default-multicore/README.md).
