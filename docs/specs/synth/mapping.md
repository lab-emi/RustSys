# RustSys standard-cell mapping

## Library use

**SYNTH-REQ-0501** RustSys reads one or more Liberty libraries (LIBERTY-REQ). When several
libraries define a cell of the same name, the first definition wins and a warning is issued. A cell
is usable as:
- a combinational cell: exactly one output pin with a `function`, no `three_state`, only input
  pins otherwise, no `ff`, `latch`, `statetable` or `test_cell` group;
- a flip-flop: exactly one primary `ff` group whose `next_state` is one data input or a
  supported data/enable/hold selector (SYNTH-REQ-0523), whose
  `clocked_on` is a single pin or its complement, with optional `clear` and `preset` that are a
  single pin or its complement; output pins whose functions are the state variable or its
  complement;
- a latch: exactly one `latch` group with `data_in` a single pin, `enable` a single pin or its
  complement, optional `clear`/`preset` as above;
- a tie cell: a cell with no inputs and no sequential groups whose output pins each have the
  constant `function` `0` or `1` (supporting both single-output and multi-output tie cells, each
  constant chosen by smallest cell area).
Cells with `dont_use : true`, cells matching a `--dont-use` glob, isolation cells
(`is_isolation_cell`, `isolation_cell_enable_pin`, `isolation_cell_data_pin`), level-shifter cells
(`is_level_shifter`, `level_shifter_enable_pin`, `level_shifter_data_pin`), always-on cells
(`always_on` on the cell or on a pin), cells with more than one output function (other than state
outputs or tie cells), cells with inout or bus pins, and cells without any function are not used.

**SYNTH-REQ-0502** Combinational cells with more than 6 inputs are not used. An inverter (a
one-input cell with function `!A`) is required; without one the run fails.

## Matching and mapping

**SYNTH-REQ-0510** A cut of the AIG is a set of at most k nodes such that every path from a
combinational input to the cut's root passes through the set; k is the largest number of inputs of
a usable combinational cell. For every node, cuts are enumerated bottom-up and the best C cuts
(priority cuts) are kept [@cong1999cut; @mishchenko2007combinational]. C and the number of
area-flow and exact-local-area passes (SYNTH-REQ-0512) depend on the effort: 12, 2 and 2 for
`low`; 40, 4 and 6 for `medium`; 100, 4 and 8 for `high`.

**SYNTH-REQ-0511** A cell matches a cut when the cut function equals the cell function under some
assignment of cut leaves to cell inputs, where each leaf may be used in true or complemented
polarity. Every AIG node is mapped in both polarities; a complemented leaf uses the leaf's
complemented implementation, and an inverter is added where a polarity is otherwise unavailable.
Matching is exact: the library is expanded into all input permutations and polarities of each
cell, keyed by truth table.

**SYNTH-REQ-0512** Mapping first minimizes the estimated arrival time of every combinational output,
then recovers area with area-flow and exact-local-area passes [@chen2004daomap;
@manohararajah2006heuristics] while keeping every output's arrival within the required time. The
required time is the best achievable arrival times `(1 + r)`, with `r` set by `--delay-relax`
(default 0.1); `--goal area` removes the delay bound.

**SYNTH-REQ-0513** The delay of a cell arc is estimated from the Liberty `cell_rise`/`cell_fall`
tables (the larger of the two) at the middle input-transition index and at a load of four times the
average input capacitance of the library's inverters. Arcs without tables use delay 1 in library
time units.

**SYNTH-REQ-0514** Mapping costs are integers: delays in units of `1e-5` library time units and
areas in units of `1e-2` library area units, each rounded to the nearest unit when the library is
classified; a delay is then clamped to 1 to `10^9` units (every cell adds a positive delay) and an
area to 0 to `2^30` units. Arrival times saturate below an "unmapped" value; area flows saturate at `2^30` units.
The area flow a node passes to one of its fanouts is `af * 2 / max(f, 2)`, rounded down, where `f`
counts half fanouts. The delay bound of SYNTH-REQ-0512 is `floor(target * (1 + r))`. Integer costs
make every comparison exact, so that the result does not depend on the order in which equal
costs are computed or on the backend.

**SYNTH-REQ-0515** The delay and area-flow passes run level by level: a node's level is one more
than the larger level of its fanins, and the nodes of a level depend only on lower levels, so they
are computed in parallel. A node's candidate cuts are the feasible merges of its fanins' cut pairs
in pair order, without constant cuts; of equal leaf sets the first is kept. Candidates are ranked by
the pass cost (arrival or area flow), then leaf count, then the other cost, then pair order, and
the best C are kept. In the area-flow passes, the cuts that the previous pass chose for the phases
of the node used by the current cover (known required time) follow the merged candidates in phase
order, unless a candidate has the same leaves, so that the previous cover remains available and
every used phase keeps a candidate that meets its required time [@mishchenko2007combinational].
Forced cuts always stay: the cut whose leaves are the two fanins, then the previous cuts of phase 0
and phase 1 (at most C of them, without repeats); the best other candidates fill the remaining
places, and the kept cuts are ordered by rank. The node's trivial cut follows the kept cuts.
Required times and the cover's (node, phase) pairs are propagated in reverse level order with
order-independent minimums. The result does not depend on the thread count.

**SYNTH-REQ-0516** GPU cut passes. With `--backend gpu` or `cuda`, the delay
and area-flow passes of SYNTH-REQ-0515 run on a GPU through `rusteda-compute` (COMPUTE-REQ-0101 to
0106) [@liu2024gpulut; @xie2025parallel]: one thread block of 128 threads per node of a level merges
the fanin cut pairs, removes duplicates, ranks the candidates, keeps the best C and chooses the best
match of both phases, with the same integer costs, orders and rules as the CPU passes, in 32-bit
unsigned integers only (arrival and required times biased by `2^31`, which keeps their order). The
cut table and choices stay on the device between passes; between passes only the chosen cut of every
(node, phase) and the choices are read back, and the whole cut table only after the last pass, for
the exact-local-area passes. The emitted netlist is byte-identical for every backend. The GPU passes
support 1 to 126 priority cuts per node and at most `2^22` library matches; a larger C, a larger
match table, a cut table larger than the device's buffer limit or 32-bit indices, and every runtime
failure of COMPUTE-REQ-0001 are input errors (`S0530`). Device scratch memory is sized by the widest
level, and pooled device memory is released after each mapping. `cuda` runs kernels written in CUDA C
under the conditions of COMPUTE-REQ-0002; there is no Metal runtime yet (Phase 4).

**SYNTH-REQ-0517** The exact-local-area passes visit the used (node, phase) pairs in level order
(increasing level, then increasing node number), phase 0 before phase 1. A phase read by the other
phase's inverter (chosen before or just now) must also meet the other phase's required time minus
the inverter delay, and an inverter candidate takes the other phase's arrival from the current
arrivals of its inputs, so that the passes keep every output within the delay bound
(SYNTH-REQ-0512). A node's reference-count walks reach only the node
itself and lower levels, so the nodes of a level are decided in parallel, each against the
reference counts at the start of a bounded batch within the level. The decisions are committed in level order; a decision
is recomputed on the current counts when an earlier node of the level changed a count it walked
through such that the count could have reached 0 or 1 differently (it changed and, in either case,
fell below 2 during the walk). The result equals that of visiting the nodes one by one in level
order, whatever the thread count.

`MapOptions::exact_limits` bounds batch size, live worker scratch, per-node overlay size and
retained observations. Defaults allow 65,536 nodes per batch, at most 16 workers, 32,768
overlay nodes and 256 MiB of scratch plus retained plans. A plan that exceeds its storage
budget is recomputed in order on the shared counts. Accepted plans apply their validated final
count changes directly. For load-priced mapping, a plan must also preserve every change in
the discrete buffer-cost function; crossing a price boundary triggers recomputation.

Intermediate mapping iterations keep a compact two-phase reference table. They count the
same pins and roots as a complete gate cover, including the separate treatment of constant
roots in fanout estimates. Only the final result materializes all mapped gates and computes
logic depth. This reduces repeated allocation without changing candidates, costs or tie order.
Atomic required-time updates occur only when the value becomes smaller; liveness flags only
change from false to true. Compare the compact view with complete gate covers and with the
one-by-one reference, then require independent equivalence of emitted implementations.

**SYNTH-REQ-0518** Preparation overlap (COMPUTE-REQ-0003): with a GPU backend, runtime selection, the
self-test, kernel compilation and device allocation run on a second thread (by mapping a two-input
AND) while the design is read, elaborated and optimized. Mapping waits for the preparation; a failed
preparation is reported by the mapping (SYNTH-REQ-0516).

## Sequential and special cells

**SYNTH-REQ-0520** Each state bit is implemented by the usable sequential cell of minimum area that
provides its clock edge, asynchronous clear and preset with their active levels, and at least one
state output. When no cell matches exactly, the following adaptations are applied in this order of
preference: an inverter on the clock pin (edge adaptation), an inverter on a clear or preset pin
(active-level adaptation), inverted storage (data and output inverted, exchanging clear and
preset), and use of the complemented state output. The comparison counts no inverter for a data
function computed by AND nodes, in either phase, because mapping implements both phases of every
node; a complemented input or state output on a data pin counts one inverter.

**SYNTH-REQ-0521** Latches follow SYNTH-REQ-0520 with enable level instead of clock edge.

**SYNTH-REQ-0522** Constant 0 and 1 signals are driven by tie cells when the library provides them;
otherwise the constants `1'b0` and `1'b1` appear in the netlist.

**SYNTH-REQ-0523** Recognize flip-flop next-state selectors by complete bounded Boolean
truth tables, without cell-name rules. A data-enable cell loads its data input while its
enable is active and otherwise retains its own stored state. A two-data selector, including
a scan flip-flop, may implement this behavior by wiring its inactive-arm input to that same
stored state, with input/output polarity compensated. Every physical input has an explicit
data, enable, feedback, clock, clear or preset role. Unknown variables, overlapping roles,
unsupported compound controls and other next-state functions are rejected explicitly.
The clock function is preserved; enable extraction does not introduce clock gating or a reset
assumption. The primary sequential model defines functional behavior even when a separate
`test_cell` view exists; no inactive scan-mode assumption is made. Bound role enumeration,
choice storage and total selection work. Selection and parallel emission must preserve
deterministic connection order. Setup requirements include enable and feedback inputs.

**SYNTH-REQ-0524** With electrical constraints and at least four area-flow passes, redirect
the last two ordinary passes into technology-priority passes, followed by the existing additional
technology pass. Keep the total pass count unchanged. Every such pass recomputes required times
and fanout estimates from its current cover, reserves realizable cuts of both phases, and runs
in common CPU code after any backend's base passes. This avoids increasing the search budget
while retaining useful cell implementations across refinements.

**SYNTH-REQ-0525** A CPU mapping pass may cache each node/phase's integer area-flow share.
Initialize the cache from the current choices and fixed per-pass fanout estimates, and refresh
both phases whenever a level commits. Cached division and direct division must produce identical
choices, including integer rounding, saturation, node order and tie breaks.
