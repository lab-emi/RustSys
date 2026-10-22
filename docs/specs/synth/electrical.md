# RustSys electrical refinement

**SYNTH-REQ-0530** An optional ideal-clock refinement pass accepts a positive target period in ns,
uniform input/output delays, input and clock transitions, output load in pF, a nonnegative margin,
and an optional positive cell-area limit. All numeric inputs are finite. A buffer fanout bound is
between 2 and 128. Absent these options, no electrical refinement is performed.

**SYNTH-REQ-0531** Refinement first compacts the mapped cover: bounded cones (up to five gates
and six boundary signals, at most 48 candidates per root, two sweeps) may be replaced by a
cheaper single cell with the exact same truth table, accounting for retained external fanout.
Input permutations and absorbed inversions must preserve the function. Logical state wires
and control cones are retained. The cover is then reduced to minimum-area drive variants of
each input-ordered function; larger variants remain eligible for sizing. No cell excluded by
the library or the user may be introduced. Sequential selection charges a small tie-break cost
for the phase of an internal data cone, since covering can absorb it, while charging an actual
inverter for a complemented primary input or a required state-output inversion.

**SYNTH-REQ-0532** The electrical estimate sums connected input-pin capacitances and specified
output loads. It propagates separate rising/falling arrival and transition values using Liberty
NLDM interpolation (LIBERTY-REQ-0030), including library-unit conversions. Sequential outputs use
clock-to-output arcs and every synchronous input uses its setup tables, including data,
load-enable and external hold-data pins. A cell with multiple mapper role assignments has one
physical electrical model. This is an ideal-clock setup estimate:
latch borrowing, multiple clock phases, timing exceptions, hold and physical interconnect need
the separate timing analysis.

Forward and backward propagation may use checked Kahn layers, with immutable planning and
stable commits. Net capacitances retain the serial gate/pin summation order; every timing
scalar must match the serial evaluator bit for bit. `Options.parallel_timing` defaults to at
most 16 workers, 16,384 gates per batch, 65,536 layers and 16 MiB of proposal storage, with a
4,096-gate threshold. Exhausted parallel budgets use the serial linear evaluator. No graph
copy per worker or path enumeration is permitted.

**SYNTH-REQ-0533** Refinement may insert non-inverting buffers and substitute cells with identical
input-ordered truth tables. It preserves output/state net names and sequential cell behavior.
Clock and asynchronous-control cones are protected from compaction, timing-directed buffering and
sizing; rule repair of control nets follows SYNTH-REQ-0552. Constants and primary inputs are not
buffered by this pass. Buffering splits existing fanout edges and must
preserve the combinational DAG. Loads are ranked by criticality and the product of driver
delay sensitivity and load. With an area limit, at most 65% of the available headroom is used
for buffering, reserving the remainder for sizing. Failure to meet the fanout target within
these budgets is reported explicitly and does not imply electrical or timing feasibility.

**SYNTH-REQ-0534** Sizing uses endpoint requirements, actual output loads, propagated slew and a
first-order estimate of changes in predecessor delay. Candidate order and generated names are
deterministic. At most 32 sizing sweeps per invocation bound work; checkpoints permit cancellation. The best
observed estimate is retained when a later sweep regresses.

**SYNTH-REQ-0535** The report contains final recounted area and cell statistics, inserted buffers,
collapsed gates, compacted cells, resized cells, estimated slack, estimated violating endpoints, and area/timing feasibility flags.
The final logic depth is recomputed after refinement. Unmet area and estimated timing targets
also produce diagnostics S0532 and S0533; unmet data-net rules produce S0534; an estimate is not a signoff result. Tests cover functional
preservation, aliases/state names, protected control nets, fanout splitting, units, area limits,
determinism, invalid options and cancellation.

**SYNTH-REQ-0536** The maximum characterized input capacitance range contributes to late-load
estimation. Electrical constraints include library/pin maximum capacitance, transition and
weighted fanout, with pin limits overriding defaults. Timing feasibility and electrical-rule
feasibility are reported separately. Clock nets (SYNTH-REQ-0552) are reported separately
from data-net repairs. A repair must preserve logic; it uses the available area budget first and
exceeds it only as SYNTH-REQ-0552 requires.

**SYNTH-REQ-0537** Unconstrained required-time annotations must distinguish a used phase from
an unused phase. Used phases retain their previous cut choices even in pure area covering.
CPU and GPU consume the same annotations.

**SYNTH-REQ-0538** Constrained covering retains a representative match for each complemented
leaf mask of a library function so naturally available negative phases can compete. All retained
matches must implement the same cut function. Parallel exclusive-case lowering groups equal data
values before gating them by the disjunction of their guards. Exact-area passes may stop at a
fixed point only when every selected implementation, arrival and area-flow annotation is unchanged.


**SYNTH-REQ-0539** Additional electrical corners. Repeatable `--electrical-corner LIB` inputs
provide alternate characterization of the same eligible cells. A target period is required.
Missing cells, different pin order/directions/functions, state behavior, area, or eligibility are
rejected before modifying the netlist. Units and capacitance ranges are interpreted separately
for each corner. Bounded repair checks cap/transition and then weighted fanout across all
corners, before and after primary-library setup sizing (up to three sweeps per mode). Sizing
reserves a fraction of remaining area for subsequent repair. A previously legal cover is retained
if subsequent refinement loses electrical feasibility. Report each corner's final rule violations. Setup optimization remains single-corner; only independent STA establishes Fmax at
other corners. Reordering compatible corners can change heuristics but not functional behavior.


**SYNTH-REQ-0540** Cover recovery considers both output phases together and prices whole-buffer
fanout repair using a bounded-fanout tree lower bound. Primary input driver area is outside the
cover. Since this cost depends on counts above one, parallel speculative decisions must be
recomputed when any reference count read by their cost calculation changes. A common CPU
technology pass follows CPU/GPU cut passes and reserves up to four realizable cuts per output
phase in addition to construction and previous-cover cuts. This is a cost proxy, not proof that
a mapped cover meets electrical constraints.

**SYNTH-REQ-0541** A fanout tree of uniform bound L requires at least
`ceil(max(0, F-L)/(L-1))` buffers for F equal-weight sinks. Use this minimum count rather than
buffering a partly occupied final chunk unnecessarily. Cap/slew may require additional buffers.
Timing-directed buffering may leave the most critical sinks directly connected and skips nets
where buffering does not improve sink slack. Inexpensive
combinational drivers may instead be replicated when the area is no greater than buffering, no
strictly cheaper legal drive variant resolves the violation, and
all characterized corners permit the added upstream pin load. Constants and control cones remain
protected; final rule checks remain mandatory. Report replicated cells separately from buffers.

**SYNTH-REQ-0542** Phase absorption may price a shared inverter and all affected consumer
substitutions jointly, including a change of the shared driver's physical phase. Exact truth
functions and connected output names are preserved. Only actual named state probes require
state-alias protection when that information is available; the public netlist-only entry point
retains conservative protection. Sequential drive variants must have identical state behavior,
input names/order and pin functions. Keep the connected Q/QN pin identity during sizing and
include launch and capture timing in its local cost.

**SYNTH-REQ-0543** Sizing and symmetric-pin reassignment check actual pin capacitance, weighted
fanout and transitions at all supplied corners, including changed input-driver loading. Pin
reassignment must leave the truth table unchanged and cannot increase area. Re-evaluate the
primary timing score and discard a regressing pin-assignment batch. Rule-only timing may omit
backward requirements and derivatives while retaining identical forward loads and transitions.

**SYNTH-REQ-0544** With electrical constraints, the mapping delay proxy uses the specified input
transition and FO4 of the smallest eligible inverter rather than an average over drive strengths.
Boundary estimates include primary-input delay, selected FF launch delay, output delay and FF
setup. Asynchronous controls, clocks and latch data remain unconstrained in this proxy and need
separate STA. Used unconstrained phases must remain distinguishable from unused phases. These
estimates do not model physical wires or replace STA.


**SYNTH-REQ-0545** Small combinational windows may be replaced by an exact two-cell decomposition,
and associative sum-of-products or enable-multiplexer cones may be repacked into denser library
cells when total area decreases.
For a disjoint partition of at most six boundary variables, the bound-set cofactors must all be
constants or one function and its complement. Both the inner and outer truth tables must match
eligible library cells. Price removed gates with retained external uses accounted for, accept only
strict area improvements, preserve names and controls, and check the DAG. Cache recipes only by the
complete truth table and variable count within one library model. Report two-cell rewrites separately.

**SYNTH-REQ-0546** Electrical models may pre-resolve pin limits, fanout weights, arc senses and table
orientation at library load. Preserve the exact interpolation, units, fallback arcs, transition
polarity and reduction order. These caches must not change netlists or numerical timing results.

**SYNTH-REQ-0547** Sequential output-phase absorption may replace a state cell and inverter (or
complement-preferring combinational consumers and enable multiplexers) by
a compatible cell providing both output phases at no greater total area. Compare parsed clock,
data, asynchronous and simultaneous-control behavior independently of the Liberty internal state
variable names; preserve all externally named state aliases and exclude protected control nets.
An independent arbitrary-initial-state check must validate the transformed state implementation.

**SYNTH-REQ-0548** Timing-driven load isolation may insert a buffer even for a low-fanout net,
including a single heavy sink. Estimate the gain using actual driver and buffer NLDM delays,
respect the area ceiling and all corner load limits, and keep a batch only when full primary
corner timing improves without increasing electrical-rule violations. A pin-count threshold
alone is not an electrical load estimate. Report the number of accepted load buffers.

**SYNTH-REQ-0549** Pin reassignment may exchange complete equal-sized symmetry groups of a
compound gate after exact truth-table validation. Individual cross-group pin swaps need not be
valid. Aggregate capacitance and fanout changes by electrical net, including repeated inputs;
check all corners before applying a change and retain the whole-pass primary timing rollback.
Candidates that cannot reduce area and cannot meet the resize criticality predicate may be
skipped using the original gate's candidate-independent slack.

**SYNTH-REQ-0550** Alternative cover under an area limit. When an area limit is given and the
refined netlist of the timing-bounded cover misses a data-net rule, the area limit or the
estimated period (the mapping estimate does not see loads, so the cover without the boundary can
also be the faster one after refinement), a second cover is mapped without the timing boundary (area-oriented, the same cuts, phases and fanout
recovery), emitted and refined with the same options. The result kept is the better of the
two by, in order: the data-net rules met, the area limit met, then the larger estimated setup
slack when both meet the limit or the smaller area when neither does; ties keep the first. Both
covers implement the same function; only the kept one is reported.

**SYNTH-REQ-0551** Best-effort drive repair. When no affordable drive variant and no affordable
buffer tree removes a capacitance or transition violation at the output of a combinational
driver, the affordable drive variant of the same function that most reduces the larger of the
load-to-limit and transition-to-limit ratios is used, provided it reduces that ratio by at least
a tenth. The violation stays reported until a later repair removes it.

**SYNTH-REQ-0552** Rule precedence and clock nets. Electrical design rules take precedence over
the area limit, which takes precedence over estimated setup timing. When the budgeted repairs of
SYNTH-REQ-0539 and SYNTH-REQ-0541 leave a data-net capacitance, transition or weighted-fanout
violation at any corner, the same replication and repair passes run again without the area limit,
cheapest repair first, and the result reports the limit as unmet (S0532). A net is a clock net
when it lies in the combinational fan-in of a sequential clock or enable pin and is either a
primary input or the output of a combinational cell with a clock-net input. Clock nets are left
to clock-tree construction and counted separately. Every other net, including asynchronous
set/reset nets and enables derived from state, follows the data-net rules; on control nets, rule
repair inserts non-inverting buffers or substitutes drive variants of the same function and never
restructures logic. Tests: a reset net and a state-derived latch enable are buffered beyond a
zero-headroom area limit while the clock net stays untouched and every control pin keeps its
function.

**SYNTH-REQ-0553** Delay-oriented alternative covers. With a timing period and the area goal, when
the refined netlist kept so far misses the estimated period, covers of the same function are
mapped with the delay goal's area recovery (the arrival bound of SYNTH-REQ-0512 relaxed by
`--delay-relax`), first with the timing boundary, then, if the period is still missed, without it;
when the initial cover already satisfies the data rules and area limit, the bounded delay-oriented
cover may precede the unbounded area-oriented cover and stop once all constraints are met;
each is emitted and refined with the same options and kept when it ranks better by the order of
SYNTH-REQ-0550. Only the kept cover is reported. The
area-oriented cover remains the first: a delay-oriented cover is often faster but larger, and it is
kept only where it meets the rules and the area limit at least as well. Tests: a design whose
area-oriented cover misses a period that a delay-oriented cover meets within the limit keeps the
delay-oriented cover; with ample period the area-oriented cover is kept.

**SYNTH-REQ-0554** Arithmetic alternatives. When the electrical result misses its area limit or
period, first try the high-effort optimization and cover when the requested effort is medium,
then repeat from elaboration with independently selected carry blocks and with ripple carries.
Stop when an alternative satisfies all three estimated constraints. Keep a completed alternative only if it ranks better under SYNTH-REQ-0550. Alternatives
have the same semantic assumptions and electrical constraints and report no separate phases.
The returned phase and elapsed times include all alternatives. A feasible result needs no
arithmetic repetition.

SYNTH-REQ-0555 (power-aware refinement) is withdrawn; the number is not reused.

**SYNTH-REQ-0556** Complete fanout trees. After electrical repair, when the estimated period or area limit is missed, use up to sixteen
passes of at most sixteen data fanouts in order of estimated slack and node identity. A candidate may replace a whole tree of
non-inverting buffers; stop at observed outputs, state aliases and protected controls. Unbuffered
critical fanouts are eligible too. Examine branch widths 2, 4, 8, 16, 32 and 64 and eligible buffer
cells or shared pairs of inverter stages with an even number of inversions on every path.
When recovering area, also collect unobserved inverter/buffer trees with signed sink polarities;
a shared inverted branch must preserve each original sink polarity and all observable boundaries,
assigning tighter required-time sinks nearer the root. Local boundary timing and area shortlist
at most four candidates per root for full-network timing and corner-rule checks. Check the
local tree rules at all corners before that shortlist. Stop once the requested constraints are met. Preserve all logical connections,
clock trees, observed nets and sequential cells. Keep a candidate only if it fits the area limit
(or does not grow an already over-limit result), does not increase any corner's electrical-rule
violation counts, and improves estimated slack, reduces total negative endpoint slack without degrading the worst
slack, or saves area without degrading either timing measure. Above the
area limit require area reduction while keeping setup feasible if it was feasible, or without
worsening worst slack if it was already infeasible.

When a legal cover still exceeds the area limit and has positive setup slack, a bounded
cleanup may try up to 512 smaller family-cell substitutions. Recompute whole-network
setup timing and every corner's data rules after each substitution; keep it only when
all still pass. Constant or unsupported cells without a replacement family are skipped.

**SYNTH-REQ-0557** Per-addition carry choice. The weighted arithmetic alternatives independently evaluate
local block widths 1, 2, 4, 8 and the word width, combining each block's carry transfer with a
prefix. Rank each candidate by its largest AIG arrival level plus an area weight times the number of new
reachable AND nodes. Completed designs first try weights 0, 0.01, 0.025 and 0.08: increasing order when
timing is missed within the area limit, decreasing order when the area limit is missed.
If still infeasible, try the more area-biased weights 0.2 and 0.5 before the all-ripple alternative. Ties retain the earlier width. Unselected logic is swept; the final mapped
and electrically refined result still competes with the original and ripple alternatives under
SYNTH-REQ-0550. Verify integer addition, subtraction and outgoing carry for partial blocks,
constant operands, arbitrary incoming carry and unequal input arrival levels.


**SYNTH-REQ-0558** Structural-slack carry candidate. Preserve wide-addition sites through driver
resolution. Before Boolean optimization, compute required AIG levels backward from outputs and
state data/control functions with the largest root level as the boundary. A planned alternative
chooses the cheapest of local block widths 1, 2, 4, 8, 16 and the word width whose added
levels fit within half the available structural slack; other additions retain their prefix form.
The word-width choice is a ripple chain. This is a candidate
heuristic, not a timing guarantee: the complete resulting design is mapped and refined under the
same constraints, and competes by SYNTH-REQ-0550. Tests exercise a critical and a noncritical
addition, site retention through driver resolution, and functional preservation of the plan.


**SYNTH-REQ-0559** Shared XOR carry candidate. Under an unmet timed area constraint,
the alternatives may reuse the sum propagate term `a XOR b` in both the carry-transfer
prefix and each full adder (`carry = (a AND b) OR ((a XOR b) AND carry_in)`).
Try both a fixed local block and an arrival-driven block selection. The default
OR-propagate implementation remains a separate candidate. Select only after
mapping and the same electrical refinement; test both propagate forms with arbitrary
incoming carries, partial blocks, integer sums and carry-out.


**SYNTH-REQ-0560** Individual carry recovery. If ordinary alternatives leave no jointly
feasible implementation, but a legal timing-feasible cover is within five percent of the
area limit, try at most twelve wide additions in reverse lowering order. Each trial starts
from XOR carry propagation and replaces only that addition with a local ripple chain;
other additions retain their original structure. Re-elaborate, map and electrically refine
every trial with the same constraints. Accept only a candidate meeting area, setup and all
configured data rules together. Skip this recovery above 64 addition sites and propagate
cancellation. The final result still requires external STA and formal checking.

## Default cleanup without timing constraints

When synthesis has no explicit electrical options, `SynthOptions::default_quality` enables
a bounded cleanup under the initial mapped cover's area. The default limits are 65,536 gates,
524,288 pins and 16 MiB of referenced names; ports and assignments count in the preflight.
An exhausted limit retains the input cover and reports `budget_reached`, with unchecked path
values absent. The engine API can lower these limits or disable the cleanup.

The attempt uses the primary library's load-aware model, ideal launches, 100 ps input/clock
slew, 10 fF output load and no wire parasitics. Its search period is one quarter of the
baseline estimated path, with a 1 ps floor. This is an internal search target, not a supplied
timing constraint. Exact state-phase, cell-collapse and polarity transformations may recover
area; cap/slew repair, sizing and pin permutation may spend that area. Retain the result only
when its estimated worst path improves and its area does not exceed the initial cover.

At medium or high effort, one alternate cover may be considered when the remaining path is
above 30% of baseline and a data-net capacitance or transition ratio remains above four.
The alternative is limited to 65,536 Boolean nodes. Its bounded optimization passes and
mapping use a loaded library profile and fanout pricing; `dont_use` still applies. It may
spend area saved relative to the original cover, but final acceptance uses that original
area ceiling. No design names, PDK-specific cell names or benchmark-specific area values
control this decision. State initial values, clock functions and reset assumptions stay in
the original functional contract.

`default_quality` reports the budget status, corner, wire model, unconstrained scope,
before/after area and estimated path, and the selected netlist's logic depth. Trial counters
remain diagnostic when an attempt is discarded. The mapper's `estimated_delay` remains its
mapping proxy; use the separately scoped load-aware estimate or independent STA for timing.
Default cleanup does not guarantee that every electrical rule is satisfied. Explicit
`--timing-period` continues to select the full constrained flow described above. Acceptance
requires external arbitrary-state LEC and independent STA on both baseline PDKs.

## Incremental mode for post-mapping commands

**SYNTH-REQ-0561** Incremental mode. With `Options::incremental` set, electrical refinement
optimizes a netlist that is already mapped and refined, for post-mapping commands
(CLI-REQ-0205): it starts from the cells of the netlist (no compaction, no reset to the smallest
variant of a family, no restructuring pass of SYNTH-REQ-0531 to SYNTH-REQ-0560) and makes at
most one change per iteration, only to the editable gates: the instances of the focus (every
instance when the focus is absent, none when it is empty) and the buffers the mode inserts. A
change is either a resize of an editable gate to another cell of its family (the same
input-ordered truth table, or for a sequential cell the same state behavior, pins and
functions), or the insertion of a non-inverting buffer cell between a net driven by an editable
gate and its least critical sinks (by required time at the sink), keeping at least the most
critical sink on the net and moving at most `buffer_fanout` sinks. Gates with protected or clock
outputs, gates without a family and nets of primary inputs, protected or clock nets are not
changed, and the DAG is preserved. A focus name that is not an instance of the netlist is an
error, and synthesis rejects the option (`S0531`), so the complete refinement of synthesis is
unchanged by it.

**SYNTH-REQ-0562** Each candidate change is applied and the whole network is timed again with
the estimate of SYNTH-REQ-0532 (worst setup slack WNS and total negative slack TNS of the
primary library) and with the electrical rules of every corner. A candidate is acceptable when
it raises WNS by more than 1e-5 ns, or keeps WNS and raises TNS by more than 1e-5 ns, and
increases no capacitance, transition or weighted-fanout violation count of any corner; with an
area limit, a candidate whose area exceeds the limit and the current area is refused. The
acceptable candidates rank by larger WNS, larger TNS, smaller area, then examination order, and
the first is applied. The stop rule (`WnsAtLeast(x)`, `TnsAtLeast(x)` or `NoImprovement`) is
evaluated on the estimate of the starting netlist and after every iteration; the mode stops
with `condition_met`, with `max_iters` after `max_iterations` iterations (1 to 10000), with
`area_limit` when no candidate is acceptable but one was refused only for the area limit, and
with `no_improvement` when no candidate is acceptable. The report gives the iterations run, the
stop reason, the number of editable instances, WNS and TNS before and after, one record per
iteration (change, instance, old and new cell, buffered net and moved sinks, WNS, TNS and area
after it), and the recounted area, resized cells (changed original instances), inserted
buffers, logic depth, estimated slack, violating endpoints and rules of the final netlist.

**SYNTH-REQ-0563** Search breadth per iteration. The candidate gates are the editable gates with
a finite slack at an output, most critical first (smallest slack, then gate index). `low`
examines the 4 most critical gates, only the next smaller and the next larger cell of each
family, and no buffer (at most 64 timed candidates); `medium` 16 gates, every other cell of the
family and the smallest buffer cell keeping one sink (at most 512); `high` 64 gates, every other
cell of the family and every buffer cell keeping one sink or half of the sinks (at most 4096).
The mode is deterministic: the same netlist, libraries and options give the same netlist.

**SYNTH-REQ-0564** The incremental mode preserves the function of the netlist: resizes stay
within a family and buffers are non-inverting, original instances and nets keep their names,
inserted buffers are named `rs_buf<k>` and their nets `rs_net<k>` (SYNTH-REQ-0533). The
command-line tests prove every optimized revision equivalent to its RTL with the independent
checker.

**SYNTH-REQ-0565** Netlist estimate. `netlist_estimate(netlist, library)` recounts the
sequential, combinational and inverter instances and the instances per cell of a written
netlist, and computes its logic depth (the most combinational instances on a path) and the
mapping delay estimate of SYNTH-REQ-0513 in the fixed-point units of SYNTH-REQ-0514: the latest
arrival at a primary output or an input of a sequential cell, with primary inputs, constants and
outputs of sequential and tie cells at 0, through the per-input delays of every combinational
instance; assignments are aliases. Instances of cells outside the classified library start
paths and combinational instances on a cycle are not timed; both are counted. On every netlist
synthesis writes, the counts equal those of its report, and the logic depth and delay are equal
when every cell belongs to the mapped cover and otherwise at least as large (the estimate also
counts cells emitted after covering), by at most one level.
