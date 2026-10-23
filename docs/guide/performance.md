# Measure synthesis performance and CPU scaling

Performance comparisons need the same design, technology library, resource allocation, timing boundary, and quality checks. RustSys's benchmark measures complete CLI jobs: workspace initialization and import, RTL synthesis, memory-mapped tables, and exported gate-level Verilog. Source preparation, equivalence checking, and static timing analysis run separately.

Captured tool messages are part of those timed jobs. The raw invocations retain each tool's logging options, thread environment and CPU affinity alongside wall time, CPU seconds and memory receipts.

## Current RustSys CPU controls

The [Sky130/GF180 baseline](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md)
contains all forty configurations. At sixteen cores RustSys is **1.50×–42.11× faster**
than the pinned Yosys reference across this suite, with lower area and a higher setup estimate
in all ten design/PDK pairs. Maximum RustSys self-scaling is **4.02×**. All twenty distinct
netlists have complete arbitrary-state LEC and independent timing evidence. The physical timing
limits below remain part of the result.

These controls use the frozen compiler in the current campaign, with one warm-up and three timed repetitions per allocation. The linked README archive identifies the exact producer and reference campaigns. Every one-core and sixteen-core RustSys pair emits identical netlist bytes.

| Design · PDK | RustSys 16 cores before / after, s | 1→16 scaling before / after | Average active cores before / after |
|---|---:|---:|---:|
| VexRiscv · Sky130 HD | 2.83 / 2.46 | 3.57× / 4.02× | 6.14 / 6.67 |
| VexRiscv · GF180 7T | 2.59 / 2.23 | 2.90× / 3.27× | 4.75 / 5.26 |
| OpenC910 · Sky130 HD | 77.42 / 69.06 | 2.39× / 2.57× | 3.95 / 4.17 |
| OpenC910 · GF180 7T | 73.10 / 64.41 | 2.21× / 2.36× | 3.51 / 3.78 |

Default SERV cleanup also improves the independently measured setup estimate while preserving or reducing its starting area:

| PDK | Cell area before / after, µm² | OpenSTA setup estimate before / after, MHz |
|---|---:|---:|
| Sky130 HD | 51,460.6 / 51,460.6 | 17.99 / 60.28 |
| GF180 7T | 163,516.1 / 163,404.1 | 7.18 / 43.27 |

These are the ordinary area/medium pipeline results; no per-design timing target or reset assumption was supplied. Both resulting SERV netlists pass complete arbitrary-state RTL LEC. Electrical limits and the ideal-clock timing scope below still apply.

The [IHP SG13G2 / EZ130 8T extension](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-23-ez130/README.md)
adds ten 16-core configurations using the same binaries and RTL. RustSys is
3.14×–33.72× faster. Four designs use less area; SERV uses 6.6% more area than
Yosys (69,321 versus 65,052 µm²), with a higher setup estimate (121.65 versus
67.02 MHz). These are library-specific observations, not universal PPA claims.
EZ130 adds no one-core scaling measurement. Its functional proof uses the
[explicit vendor tie-cell model supplement](../verification/pdk.md#additional-ihp-sg13g2-benchmark-library).

## Compare equal host resources

At each allocation, both tools receive the same **1 or 16 physical host cores**, with one hardware thread per core. RustSys's `--threads` value follows that allocation; Yosys runs the published initial-state-preserving reference flow inside the same affinity set. Its library mapping uses the default ABC settings for every design. The process environment also follows the allocation where tools support it. An allocated CPU is a resource allowance, not evidence that a tool used it.

The reference completes ordinary X/Z **data** before optimizing flip-flops. A flip-flop whose next value is X must retain its arbitrary current value until its first clock under RustSys's two-valued contract. Folding it into an undriven X wire and then completing that wire to zero changes the initial behavior. Dual-PDK controls check this case alongside constant-next, independently initialized, enabled, latched and memory state. Later flip-flop enable extraction remains enabled.

Each selected design/library/tool/core configuration has one excluded warm-up and three measured repetitions. The baseline matrix contains five designs, two libraries and both tools at 1 and 16 physical cores: forty measured configurations. Earlier three-allocation campaigns retain their original scope and are labeled as historical evidence.

The RTL is unchanged between CPU allocations. OpenC910 remains the complete `ct_core` processor design with its default 64 KiB instruction and data caches. The 16 cores in a benchmark configuration refer to the **host computer**, not extra processors in the RTL. RAM models are lowered to standard cells inside the measured interval.

## Read two different speedups

| Measure | Calculation | Meaning |
|---|---|---|
| RustSys / Yosys speedup | Yosys time at 16 cores ÷ RustSys time at 16 cores | Comparative throughput for the same allocated host resources. |
| Within-tool CPU scaling | A tool's time at 1 core ÷ its time at 16 cores | Change in complete runtime under the larger CPU allowance. |
| Average active CPU cores | (user CPU seconds + system CPU seconds) ÷ wall seconds | Actual CPU work, including waited-for compiler children. |
| Average allocation utilization | Average active cores ÷ allocated cores | Fraction of available CPU capacity used over the entire job. |

The tables use three-run median wall time, worst observed process RSS, and the median average active-core measurement. They do not substitute peak CPU usage for whole-job utilization. A request for 16 threads is not a claim of 16× scaling.

These measurements run on a shared host with explicit affinity and resource guards. They are observations under the recorded conditions, not an isolated-machine performance guarantee. Keep the raw samples and their variation when interpreting a small difference.

## How CPU work is shared

Boolean rewriting and refactoring plan independent nodes on shared, immutable graph views.
Balancing uses bounded iterative tree collection and parallel planning, then rebuilds the graph
in the original order. Dense node images replace per-edge hash lookups. Tree collection has
explicit visit and leaf limits, including on long chains.
Resubstitution plans batches in parallel, records the state each decision used, and replays a
decision when an earlier committed change invalidates it. Results commit in a fixed order.
Worker caches, retained reads, cuts and proposals have explicit limits; workers do not each
receive a copy of the design. The report's `opt_work` records bounded windows, replays and
whole-pass budget fallbacks alongside `opt_phase_s`.

Exact mapping plans retain bounded reference-count observations and final count changes.
Ordered commit reuses a plan only when the relevant zero/one transitions and load-cost
boundaries remain valid; otherwise it recomputes the decision. Storage exhaustion also uses
ordered recomputation. Intermediate mapping passes retain a compact table of references;
the final gate list and its logic depth are generated once. Required times use monotonic
integer minimums, and an atomic update is needed only when a requirement becomes tighter.

`map_phase_s` includes diagnostic `exact_planning`, `exact_commit` and `exact_replay`
intervals. These are nested measurements: replay is part of commit, and both planning and
commit are part of `exact_passes`. Do not sum them into an end-to-end runtime.

Emission builds bounded batches of cell connections in parallel while retaining deterministic
names, tie-cell placement and accumulation order. Workspace synthesis writes canonical frozen
tables directly for explicit netlists; the general converter handles other layouts. State
observability and graph compaction share a single traversal, and state records are retained in
place. These changes reduce serial allocation and copying as well as CPU time.
Verilog formatting and streaming export from frozen tables use ordered parallel chunks,
with at most 32 MiB of temporary text. Oversized chunks use the serial writer. The output
bytes, including escaped identifiers and connection ordering, are unchanged by thread count.

The unconstrained default flow also attempts a bounded cleanup under the starting cover's
area. It can spend area recovered by equivalent cell substitutions on overloaded drivers,
then accepts only an area-compliant improvement in the load-aware estimate. A second compact
cover is considered only when severe load violations remain. These rules depend on the graph
and library, not design names. The report's `default_quality` gives the budget status, corner,
area and estimated path values. An exhausted budget preserves the input cover. Explicit
`--timing-period` runs the full electrical flow; default cleanup is not timing closure.

With electrical refinement enabled, timing uses the already checked topological layers of the
netlist. Workers calculate forward arrivals and backward requirements against shared vectors;
ordered commits preserve the serial calculation, including each net's capacitance summation
order. `Options.parallel_timing` bounds workers, batches, layers and proposal storage. If those
budgets cannot support a parallel pass, the serial evaluator remains available. Small designs
may benefit little from extra workers; the end-to-end measurements include that overhead.

## Preserve implementation quality

For each design/library, all three repetitions must complete. RustSys's 1-core and 16-core netlists must be **byte-identical**, with identical mapped cells and area. This binds the parallel speedup to the same implementation and preserves timing under the same constraints.

Comparative area and Fmax are reported independently for both tools. Identical RustSys netlists do not imply universal PPA superiority over Yosys. Include every selected case rather than reporting only favorable rows.

A prior LEC certificate can be reused only for the exact measured netlist, input RTL, and library bytes. Changed netlists need a new proof. The common campaign exports every output and state/event obligation from an independent RTL model and reimports the actual exported netlist. External ABC checks complete Boolean obligations. Inferred multiply/add blocks use exact operand/result observations: Arisca proves the extracted arithmetic, while ABC checks extraction, reconstruction, the complete Boolean context and transport to the actual measured netlist. Every stage must complete. The ordinary X/Z data and undriven values are completed to zero; this is the documented RustSys language model, not a four-state equivalence claim. No original state or output obligation is removed. Exact full-core certificates can be reused only for unchanged input, library, and netlist bytes. Independent positive, data-corruption, and clock-corruption controls cover both PDKs; arithmetic controls cover signed overflow and low/high-bit faults.

All functional comparisons retain arbitrary implementation initial states under checked correspondence, with every original RTL state quantified. No reset pulse or new input assumption is introduced. The [verification guide](checking.md) distinguishes the common model from historical undef-aware evidence.

## Interpret the timing estimate

An opaque OpenSTA executable analyzes the exact exported netlist using the same TT library, ideal root clock, 100 ps input/clock slew, 10 fF output load, and zero I/O delay budgets. No estimated or routed wire parasitics are added. Constraint-check output is retained.

The [OpenSTA command reference](https://opensta.readthedocs.io/en/latest/Commands/#report_clock_min_period) defines the reported minimum-period metric over same-clock, same-edge register setup paths. It excludes port paths, cross-edge paths, and level-sensitive latch paths. The table labels this **setup-Fmax estimate**; it is not post-layout Fmax, timing signoff, hold closure, or a physical DRC result.

Unclocked and otherwise unconstrained endpoints are also outside this estimate. For example, the common PicoRV32 analyses report 68 unclocked register/latch pins for each tool and library, with 104 unconstrained endpoints except for Yosys/Sky130, which reports 136. These warnings remain in the bound `check_setup` receipts; they are not counted as passing timing paths.

OpenSTA's structural reader rejects signed wire declarations retained for unused loop integers in some Yosys netlists. A bounded adapter erases that marker only after checking that each wire appears solely in its declaration and an optional constant driver. Any port, cell, alias or expression use rejects the adaptation. Every port, cell, bit and connection remains unchanged. Receipts bind the measured netlist, adapted analysis file and exact edit ledger; failed reader attempts stay in the record.

The 1000 ns reference clock period is a loose analysis reference. OpenSTA derives a minimum period from the constrained paths; the table does not report the reference clock's frequency as Fmax. Missing paths, model errors, resource limits, and absent metrics remain unverified.

The harness computes MHz as `1000 / reported minimum period in ns`. It checks that this period and OpenSTA's separately printed frequency agree within their printed rounding intervals. This avoids rejecting a valid low-frequency result merely because OpenSTA rounds the frequency to two decimal places. Both original values remain in the receipts; the derived frequency retains the precision limit of the reported period.

OpenC910 exceeds the default cleanup budget. Its current Sky130 critical-path
loads are 49.60 pF for RustSys and 55.98 pF for Yosys, against the respective
cells' Liberty limits of 0.052195 pF and 0.190440 pF. These loads are outside
the characterized range, so the raw setup estimates do not establish achievable
processor frequencies. On EZ130 8T, the selected critical-path INVX1 loads
are 52.44 pF (RustSys) and 35.84 pF (Yosys), against a 0.10 pF Liberty limit.
Those estimates also extrapolate beyond the characterized range. Usable Fmax requires electrical and physical closure,
followed by new timing and equivalence checks of the resulting netlists.

## Reproduce the campaign

The repository's [benchmark harness](https://github.com/lab-emi/RustSys/tree/main/verification/benchmarks/open-hardware) contains:

| Script | Responsibility |
|---|---|
| `scaling.py` | Serial sampling, fixed physical-core affinity, frozen producers, CPU/RSS receipts, and hash-checked resume. |
| `unified_join.py` | Require all forty actual tool/core allocations and retain source provenance for separately collected reference campaigns. |
| `scaling_verify.py` | Source/library/netlist binding, fresh RTL event-model proofs, external proof provenance, and timing orchestration. |
| `scaling_controls.py` | Independent dual-PDK data/clock corruption controls. |
| `observed_proof.py` | Complete arithmetic/context composition with externally checked reconstruction and measured-netlist transport. |
| `observed_controls.py` | Independent signed/overflow, low/high-bit and dual-PDK transport controls. |
| `sta_benchmark.py` | Guarded OpenSTA execution with explicit constraints and raw receipts. |
| `additional_verify.py` / `additional_report.py` | Validate the optional EZ130 campaign and combine its 16-core results with the baseline. |
| `scaling_report.py` | Publish the complete selected matrix and quality-preservation controls. |

First prepare the pinned hardware and libraries using the [reproduction instructions](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/README.md). The current campaign runner consumes the prepared input manifests and baseline configurations in that documented workspace layout. Freeze the producer binaries before starting; do not rebuild them during measurements.

Large children use a 32 GiB physical-memory guard, 1 GiB swap guard, separate address-space limit, and explicit deadline. Jobs estimated above 4 GiB RSS serialize behind the scale lock. The controller waits within a bounded budget when host RAM is temporarily unavailable. An incomplete repetition cannot become a published median or an equivalence pass.

The [README benchmark section](https://github.com/lab-emi/RustSys#benchmarks) links the current measured results and exact reproduction artifacts. The [benchmark interpretation guide](benchmarks.md) explains the design configurations and historical proof invalidations.
