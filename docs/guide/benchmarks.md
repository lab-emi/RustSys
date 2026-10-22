# Understand the benchmarks

The [README comparison](https://github.com/lab-emi/RustSys#benchmarks) uses one common protocol for **Ibex, PicoRV32, SERV, VexRiscv and OpenC910**, each mapped to Sky130 HD and GF180 7T. The [complete unified archive](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md) contains the raw samples, commands, input pins, limits, and proof receipts.

## One protocol for all five designs

The unified matrix measures both tools on **1 and 16 physical host cores**, with one hardware thread per core. Every design/library/tool/core configuration has one excluded warm-up and three measured repetitions. Compare median wall time and actual CPU accounting; the allowed CPU count is not a utilization measurement. One-core reference times are measured, not inferred from the sixteen-core process's CPU use.

The timer starts with prepared RTL and includes process startup, workspace initialization/import, synthesis, mmap I/O, and complete gate-level Verilog export. Download, source preparation, LEC and static timing analysis run outside the synthesis timer. Each large child has an explicit memory, swap, file-size and wall-clock guard; high-memory jobs run serially. The host is shared, not an isolated benchmark appliance.

RustSys uses the frozen CPU producer with `--goal area --effort medium`, a 4 GiB cut-storage budget and the default bounded area-capped cleanup. The report records requested and effective cut counts and cleanup status. Yosys uses one published initial-state-preserving structural-memory flow and its **default ABC library mapping** for all five designs. No fast mapping or reduced-cut OpenC910 profile is mixed into the common medians. The command scripts are retained in the archive.

The initial-state requirement matters. Standard synthesis flows can exploit undefined state or data in ways that differ from RustSys's two-valued contract. The common v3 reference flow completes ordinary X/Z data before flip-flop optimization, preserving arbitrary current state through the first clock. Later enable extraction remains enabled. Independent dual-PDK controls require an external counterexample for the earlier order and a complete proof for v3. Functional checking adds no reset sequence or input assumption.

Default cleanup uses the original cover's area as its ceiling and applies the same graph/library
rules to every design. It adds no per-design timing target or reset assumption. Explicit timed
profiles remain separate experiments. Each published sample retains its original source campaign;
reused reference measurements are labeled as reuse.

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
