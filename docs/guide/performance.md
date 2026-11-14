# Measure synthesis performance and CPU scaling

Performance comparisons need the same design, technology library, resource allocation, timing boundary, and quality checks. The synthesis harness in `verification/benchmarks/open-hardware/` measures complete CLI jobs: workspace initialization and import, RTL synthesis, memory-mapped tables, and exported gate-level Verilog. Source preparation, equivalence checking, and static timing analysis run separately.

Captured tool messages are part of those timed jobs. The raw invocations retain each tool's logging options, thread environment and CPU affinity alongside wall time, CPU seconds and memory receipts.

This page describes that harness and the parallel design of the synthesis engine. It
publishes no measured results; the primary RTL-to-GDS benchmark is the RustEDA
whole-flow Ibex result and its module head-to-heads, and the separate
[PULPissimo synthesis benchmark](benchmarks.md)
covers the PULPissimo Ibex core, both described on the benchmark page.

## Elaboration tracing

Set `RUSTSYS_ELAB_TRACE=1` to emit per-module-instance and per-generate-block
elaboration times, signal counts and AIG node counts on standard error. The
trace is off by default and does not change synthesis results. It is intended
for locating a hot module or a behavioral memory that should have been an
opaque macro; it is not a benchmark receipt.

## Mapping choices that affect quality and runtime

The mapper uses bounded Liberty truth/cost checks to preserve efficient mux trees
for one- and two-bit variable-index reads. Clocked output drivers may use a
compatible stronger cell when an affordable buffering plan is unavailable.
Pin roles and state equations must match; the existing area ceiling remains.
A feasible buffering plan keeps priority. No design names, PDK names, reset
sequences or per-design timing targets select this behavior.

Candidate evaluation avoids duplicate drive, launch and sink calculations and
skips work on unaffordable alternatives. Sequential pin compatibility and the
original area ceiling remain. A balanced-truth precheck rejects impossible mux
functions before the bounded cofactor search.

## Compare equal host resources

At each allocation, both tools receive the same **1 or 16 physical host cores**, with one hardware thread per core. RustSys's `--threads` value follows that allocation; Yosys runs the published initial-state-preserving reference flow inside the same affinity set. Its library mapping uses the default ABC settings for every design. The process environment also follows the allocation where tools support it. An allocated CPU is a resource allowance, not evidence that a tool used it.

The reference completes ordinary X/Z **data** before optimizing flip-flops. A flip-flop whose next value is X must retain its arbitrary current value until its first clock under RustSys's two-valued contract. Folding it into an undriven X wire and then completing that wire to zero changes the initial behavior. Dual-PDK controls check this case alongside constant-next, independently initialized, enabled, latched and memory state. Later flip-flop enable extraction remains enabled.

Each selected design/library/tool/core configuration has one excluded warm-up and three measured repetitions.

The RTL is unchanged between CPU allocations. The 16 cores in a benchmark configuration refer to the **host computer**, not extra processors in the RTL. RAM models are lowered to standard cells inside the measured interval.

## Read two different speedups

| Measure | Calculation | Meaning |
|---|---|---|
| RustSys / Yosys speedup | Yosys time at 16 cores ÷ RustSys time at 16 cores | Comparative throughput for the same allocated host resources. |
| Within-tool CPU scaling | A tool's time at 1 core ÷ its time at 16 cores | Change in complete runtime under the larger CPU allowance. |
| Average active CPU cores | (user CPU seconds + system CPU seconds) ÷ wall seconds | Actual CPU work, including waited-for compiler children. |
| Average allocation utilization | Average active cores ÷ allocated cores | Fraction of available CPU capacity used over the entire job. |

Harness reports use three-run median wall time, worst observed process RSS, and the median average active-core measurement. They do not substitute peak CPU usage for whole-job utilization. A request for 16 threads is not a claim of 16× scaling.

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

The [OpenSTA command reference](https://opensta.readthedocs.io/en/latest/Commands/#report_clock_min_period) defines the reported minimum-period metric over same-clock, same-edge register setup paths. It excludes port paths, cross-edge paths, and level-sensitive latch paths. Harness reports label this **setup-Fmax estimate**; it is not post-layout Fmax, timing signoff, hold closure, or a physical DRC result.

Unclocked and otherwise unconstrained endpoints are also outside this estimate. These warnings remain in the bound `check_setup` receipts; they are not counted as passing timing paths.

OpenSTA's structural reader rejects signed wire declarations retained for unused loop integers in some Yosys netlists. A bounded adapter erases that marker only after checking that each wire appears solely in its declaration and an optional constant driver. Any port, cell, alias or expression use rejects the adaptation. Every port, cell, bit and connection remains unchanged. Receipts bind the measured netlist, adapted analysis file and exact edit ledger; failed reader attempts stay in the record.

The 1000 ns reference clock period is a loose analysis reference. OpenSTA derives a minimum period from the constrained paths; the reports do not give the reference clock's frequency as Fmax. Missing paths, model errors, resource limits, and absent metrics remain unverified.

The harness computes MHz as `1000 / reported minimum period in ns`. It checks that this period and OpenSTA's separately printed frequency agree within their printed rounding intervals. This avoids rejecting a valid low-frequency result merely because OpenSTA rounds the frequency to two decimal places. Both original values remain in the receipts; the derived frequency retains the precision limit of the reported period.

A critical-path load beyond the Liberty characterization range makes the raw
setup estimate an extrapolation; it does not establish achievable frequency.
Usable Fmax requires electrical and physical closure, followed by new timing and
equivalence checks of the resulting netlists.

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

The dated archives under `verification/benchmarks/open-hardware/results/` are historical synthesis-only records, not current benchmark results. The [benchmark page](benchmarks.md) describes the published whole-flow benchmark and RustSys's part in it.
