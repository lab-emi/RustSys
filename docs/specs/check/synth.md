# Synthesis checker (`rustsys-check`, `rustsys check synth`)

The synthesis checker decides whether a gate-level netlist implements an RTL design according to
the RustSys specification. It is the independent checker of RustSys and follows the independence
rules of the verification plan: of the workspace crates it depends only on `rusteda-formats` and
`rusteda-core`, never on `rustsys`; it interprets the RTL with its own code and is simpler than
the engine (no optimization, no mapping).

## Inputs and outputs

**CHECK-REQ-0001** Inputs: the RTL sources with the same `--top`, `-D`, `-I`, `-P` and
`--no-predefined-macros` options as `rustsys synth`; one or more Liberty files; the netlist file;
the conflict budgets of CHECK-REQ-0002, `--budget <N>` (default 2 000 000) and
`--model-budget <N>` (default 200 000). Output: a verdict (CHECK-REQ-0002), diagnostics, and
optionally a JSON report (CHECK-REQ-0060). Exit status: 0 for `equivalent` and
`initial-state-refinement`, 3 for `not-equivalent`, 5 for `undecided`, 1 for input errors
(`error`), 2 for internal failures (`error` with `internal_error`).

**CHECK-REQ-0002** Verdicts:
- `equivalent`: every obligation (CHECK-REQ-0040, CHECK-REQ-0041) is proved;
- `initial-state-refinement`: every obligation is proved under the reported choices of otherwise
  unspecified initial state (CHECK-REQ-0050, CHECK-REQ-0051);
- `not-equivalent`: a port difference (CHECK-REQ-0030), a state kind difference, or a
  counterexample, found by simulation (CHECK-REQ-0043, CHECK-REQ-0045) or by SAT and replayed
  (CHECK-REQ-0044); a counterexample decides even when other proofs ran out of their budgets,
  and a port difference even when discovery stopped at its repetition limit (CHECK-REQ-0032);
  the report still lists those in `undecided`;
- `undecided`: no difference is found, but a proof or search stopped at its limit before deciding:
  the conflict budget of an obligation (`--budget`, `CheckOptions::budget`, default 2 000 000
  conflicts per proof), the conflict budget of a proof that the RTL model needs
  (`--model-budget`, `CheckOptions::model_budget`, default 200 000 conflicts per proof: whether a
  bit of a combinational procedure is written on every path, which decides whether it is a
  latch; whether a combinational bit depends on its own value; whether an asynchronous value is
  constant), or the repetition limit of discovery (CHECK-REQ-0032). The report lists each in
  `undecided`, naming the obligation or step and the budget that ran out; the message of an
  obligation also gives the sweeping candidates left undecided at their budget
  (`CheckOptions::lemma_budget`). Such candidates are only not merged (CHECK-REQ-0046) and never
  make the verdict `undecided` by themselves. When a proof of the RTL model runs out of its
  budget, the netlist is still read and built on its own, with variables of its own for its
  inputs: an erroneous netlist makes the verdict `error`, as more budget would not change it;
- `error`: the inputs are erroneous or use a construct outside the checker's model
  (CHECK-REQ-0023), or the checker failed internally, such as a counterexample that does not
  replay (CHECK-REQ-0044); the report lists the reasons in `errors`, and `internal_error` is true
  for an internal failure.

## RTL model

**CHECK-REQ-0010** The checker parses the RTL with `rusteda-formats` (`rusteda_formats::verilog`) and builds its own
bit-level model: every net and variable bit becomes a model variable, every continuous assignment,
gate primitive and procedure becomes a set of gate equations defining the bits it drives in terms
of model variables. Procedures are evaluated by direct symbolic interpretation of their statements.
The model follows SYNTH-REQ-0201 to SYNTH-REQ-0217 and SYNTH-REQ-0301 to SYNTH-REQ-0307.

**CHECK-REQ-0011** RTL state bits are named per SYNTH-REQ-0603.

**CHECK-REQ-0012** With `--assume-full-case` the checker's RTL model applies the complete-case
assumption of SYNTH-REQ-0224 by its own interpretation: a case statement marked complete (`full_case`
attribute, `unique` or `priority`) without a `default` item runs its last item under the condition
that no earlier item matched, without evaluating that item's labels. The report records the option.
Without the option such a netlist is compared against first-match semantics and may be reported
not equivalent.

**CHECK-REQ-0013** With `--assume-parallel-case` the checker's RTL model applies the parallel-case
assumption of SYNTH-REQ-0225 by its own interpretation: it saves the procedure state before a case
statement marked parallel (`parallel_case` attribute, `unique` or `unique0`), runs each item from
that saved state under its own match condition, runs the default item from it under the condition
that no item matches, and combines every written bit, pending nonblocking value, write condition
and kill condition as the OR of the matching items' results, or the default's result when none
matches. The report records the option.

**CHECK-REQ-0014** RTL statements outside the model. The checker's RTL model rejects, with the
verdict `error` and a message coded `C0106` at the RTL file, line and column, every procedural
statement that the reader keeps without its operands and that changes when or whether other
statements run: `wait` and `wait_order`, `disable`, and event triggers (`->`, `->>`). Such a
statement is never skipped, so that a netlist is never compared against a procedure without the
control flow the statement adds. An immediate assertion does not change what the procedure
computes and is not evaluated.

## Netlist model

**CHECK-REQ-0020** The checker reads the netlist subset of SYNTH-REQ-0601 with
`rusteda-formats` (`rusteda_formats::verilog`), with ports declared either in the module header or, in non-ANSI style, by
port declarations in the body (optionally repeated as net declarations of the same range), and
evaluates each cell from the Liberty `function`, `ff` and `latch` descriptions, using its own parser
of Liberty Boolean functions (LIBERTY-REQ-0020). Structural errors are reported with their netlist
location: unknown cells or pins, unconnected cell inputs, undriven nets and output ports, multiple
drivers, combinational cycles, a pin connected twice, a port declared twice, and a bit index outside
the declared range, the value of a signed index literal being negative when its top bit is set. An
empty connection `.pin()` leaves an output pin unconnected. Net declaration assignments, unpacked
net and port dimensions, port values and interface ports, data types other than `logic`, `reg` and
`bit`, names in a package scope, arrays of instances, parameters of cell instances, a module or
primitive of the netlist instantiated as a cell, a `bind` and a second definition of the netlist
module are rejected, never skipped. So are gate primitives, procedures, parameters, `defparam`,
generate constructs and the other module items outside the subset, with their kind and location;
assertions, `specify` blocks and time declarations drive no net and are ignored.

Explicit-width wire assignments may use whole vectors, constant part-selects,
concatenations, scalar bit references and known sized constants. Both sides must
have the same explicit width. Unknown literals and unsupported expressions are
rejected, including inside concatenations. Each assignment's iterative syntax
walk and bit result are limited to 1,048,576 entries; exhaustion is undecided
with `C0005`, not an equivalence verdict.

The workspace checker may read validated `FrozenNetlist` tables directly. It
evaluates every instance and assignment with the same independent Liberty
semantics as the text reader, checks serialized directions against Liberty,
and rejects missing inputs and multiple drivers. Its name lookup index retains
ports and requested RTL state probes; this projects only lookup metadata, never
the gate equations or proof obligations. Configurable table, entity, auxiliary
allocation and circuit-definition limits are checked before growing the
corresponding buffers. Budget exhaustion is undecided (`C0005`).

**CHECK-REQ-0021** Each sequential cell instance is a netlist state element with its clock (or
enable) function, data function, and asynchronous clear and preset functions, all expressed as
functions of netlist variables.

The library API can export all induction obligations as two standard binary
AIGER combinational networks for an external CEC executable. Export requires
matching interfaces and state kinds, a complete one-to-one named implementation
state correspondence, and no reset pulse, initial-state choice, complete-case
or parallel-case assumption, or discovered relation. Every RTL input and state
is a universally quantified input on both sides. For each original output and
retained state clock/enable, clear, preset and next-data obligation, the two
networks emit the compared value AND its original obligation guard. The relation
is also serialized and checked against the actual circuit bindings. AND count
and output count have explicit configurable caps; node storage grows in bounded
chunks. Export alone is always undecided. A separate harness must check all
interface counts, complete state coverage, immutable artifact hashes and a
successful external CEC verdict or a complete checked arithmetic/context
composition covering every original obligation. This API retains the native model's literal
X/Z-as-zero policy; no four-state equality claim is added by AIGER export.

Optional `ObservationProbe` requests supplement this export with unconditional
equalities of named packed-signal slices. They never replace original port or
state/event obligations, constrain original inputs/states, or assume an internal
correspondence. Requests are limited to 64 probes, 65,536 bytes per name, 4,096
bits per slice and 262,144 bits in total. Missing or ambiguous RTL names, invalid
slices and unavailable gate probes fail explicitly. The manifest records each
probe's name, slice and exact output range. Export remains undecided. The normal
checker has no external-tool dependency and the default request list is empty.
An external harness using arithmetic observations must prove the complete
components and Boolean context, validate extraction/reconstruction and bind the
final transport to the actual exported netlist. No arithmetic-result abstraction
may alter an original input, state quantifier, reset policy or initial-state
choice. Native unit coverage includes `tests/observations.rs`; the benchmark
harness supplies independent dual-PDK data/clock and arithmetic corruption
controls.

**CHECK-REQ-0022** Supply ports. A netlist port whose name is a power or ground pin name of the
libraries (a Liberty `pg_pin` group of any cell) or is given with `--supply-port` (repeatable) is a
supply port. It may be declared `input` or `inout`, it takes no part in the port correspondence of
CHECK-REQ-0030, and instance connections to cell power and ground pins are not evaluated: the
checker takes every cell to be powered, and only an `x` or `z` literal on such a pin is rejected
(CHECK-REQ-0023). Reading a supply port as a logic value (a cell input, an assignment or an output
port) is a structural error. The report lists the supply ports (`supply_ports`).

**CHECK-REQ-0023** Constructs outside the model. The checker rejects a netlist that uses a
construct its model does not describe, with the verdict `error` and a coded message with the
netlist location that names the construct and where it occurs (the instance and its cell, the
assignment or the net), instead of evaluating the rest of the cell or reading the construct as
something else:
- `C0101`: high impedance and drive strengths: an instance of a cell with a three-state pin
  (Liberty `three_state`), enabled or not, or with a pin `driver_type` (open-drain, open-source,
  pull, bus-hold or resistive drivers); a continuous assignment with a `highz0` or `highz1` drive
  strength (other drive strengths do not change the value of a net with one driver); a
  `bufif0`, `bufif1`, `notif0`, `notif1`, MOS, `cmos`, `tran` or `pullup`/`pulldown` primitive;
- `C0102`: an instance of a cell whose sequential group has `clocked_on_also` (a dual-edge or
  master-slave element) or `enable_also` (a master-slave latch pair);
- `C0103`: a literal with an `x`, `z` or `?` bit (`'x` and `'z` included) in an assignment, a
  connection (power and ground pins included), a bit index or a declared range;
- `C0104`: an instance of a cell described by a `statetable`, with `bus` or `bundle` pins, with
  more than one `ff` or `latch` group, with an `ff_bank` or `latch_bank` group or with a pin
  described twice; of a cell with a pin `x_function` (an unknown output), `state_function`,
  `internal_node` or `retention_pin`, or with `retention_cell`; of a cell whose `ff` or `latch`
  group has an attribute or a group that the library model does not keep (it keeps `next_state`
  and `clocked_on` of an `ff` group, `data_in` and `enable` of a `latch` group, and `clear`,
  `preset`, `clear_preset_var1` and `clear_preset_var2` of both; `power_down_function` is left
  to CHECK-REQ-0022); of a cell with `clock_gating_integrated_cell` but without an `ff` or
  `latch` group, as the latch styles of that attribute (`latch_posedge` and the like) hold an
  enable latch that a function alone does not describe, and the checker does not tell the styles
  apart; an instance whose cell function reads anything but its input pins and the state of its
  one `ff` or `latch` group (an internal, bidirectional or output pin), or that connects an
  output without a `function`; or a connection to an `inout` or `internal` pin;
- `C0105`: a net or port declared `wand`, `wor`, `triand`, `trior`, `tri0`, `tri1`, `trireg` or
  `interconnect`, or a module under such a `` `default_nettype ``; a logic port declared
  `supply0` or `supply1`; a driver of a supply net. The model gives every net one driver; an
  undriven `supply0` or `supply1` net is the constant 0 or 1.

An instance of a cell that two library definitions describe differently (pins, functions,
`three_state`, power and ground pins, state tables, bus pins or `ff` and `latch` groups) is
rejected with `C0003`, so that the verdict never depends on the order of the libraries; identical
copies, such as the corners of one library, are one cell. An instance of a cell that the library
describes with errors (LIBERTY-REQ-0021: a function or an `ff` or `latch` group that does not
read, which the library model then drops, a name that is neither a pin nor a state, an invalid
direction or limit) is `C0003` with the errors named, before any function of the cell is
evaluated; a cell function that does not read is `C0003` as well.

The library model of `rusteda-formats` (`rusteda_formats::liberty`) does not keep `driver_type`, `x_function`,
`state_function`, `internal_node`, `retention_pin`, `retention_cell`, `enable_also`, `ff_bank`
and `latch_bank` groups, or the other attributes of `ff` and `latch` groups. `rustsys check synth`
therefore reads them from the text of each library (`liberty::unread`), which it reads once for
the model and for them, so that none is skipped; a library caller passes them in
`CheckOptions::unread_liberty`. A `test_cell` group describes the cell again for test and is not
read. `clear_preset_var1` and `clear_preset_var2` are not needed: the obligations of
CHECK-REQ-0040 require the clear and preset conditions of every corresponding element to equal
the one asynchronous control of its RTL state bit, so both are never active at once in a check
that succeeds. Power attributes such as `power_down_function` are not read (CHECK-REQ-0022).

## Correspondence

**CHECK-REQ-0030** Primary inputs and outputs correspond by name and bit position (SYNTH-REQ-0602).
Missing or extra ports, direction or width mismatches make the verdict `not-equivalent`. The order
of the ports in the module header is not compared, and supply ports (CHECK-REQ-0022) take no part.

**CHECK-REQ-0031** A netlist state element corresponds to the RTL state bit whose name equals the
name of the wire it drives, directly or through a chain of inverters and buffers with even or odd
inversion parity (SYNTH-REQ-0604). A wire is a scalar net of that name or, when the state name
ends in an index `[k]` and no such scalar exists, ordinal k of a vector net named by the rest of the
state name (the form in which later tools write escaped names that end in an index). A named wire
that no state element drives through inverters and buffers (undriven, or driven by other logic)
gives no correspondence: the state bit has no named counterpart. A netlist state element without an RTL counterpart, by name or
by CHECK-REQ-0032, is an error. An RTL state bit without a netlist counterpart is treated as an
unconstrained free input of the RTL model (a cut point); this is conservative: it can cause a
`not-equivalent` verdict, never a false `equivalent` verdict.

State names use the canonical least-significant-bit ordinal of the RTL model. An explicit
scalar alias takes precedence; otherwise a vector's stored least-significant-first bits are
matched by ordinal, independent of nonzero or ascending declaration bounds. General netlist
expression indexing still uses the declared indices.

The frozen-table name lookup may propose a counterpart after removing anonymous
`genblk`-number hierarchy components inserted by a foreign frontend. Exact names
take precedence. The proposal must be unique; ambiguous aliases remain undecided.
This adapts only the bounded probe index: no physical net, state element, original
output or event obligation is removed. The proposed state/data/control relation
still has to pass the complete proof with the same arbitrary initial-state rules.

**CHECK-REQ-0032** Discovered correspondence. Names decide first; a netlist written by a later tool
(placement, buffering, renaming) may have lost the wires that name constant or shared state, or
the names of state outputs. The checker then discovers correspondences by itself:
- Scope: for relations (a) and (b), RTL state bits without a named counterpart that the compared
  logic of the named correspondence reads, transitively (outputs, and the clock, asynchronous and
  next-state functions of named and of such state); for (c), every RTL state bit and every
  netlist state element without a counterpart. A narrower scope only leaves more cut points, which
  can cost an `equivalent` verdict but never cause a false one.
- Candidate relations: (a) an RTL flip-flop is the constant its after-reset pulse establishes
  (CHECK-REQ-0048), or under CHECK-REQ-0050 an unreset flip-flop is a chosen constant; (b) an RTL
  flip-flop whose base value the pulse establishes shares the netlist state element of a
  corresponding RTL flip-flop whose base value the pulse establishes, with the parity of the two
  base values (CHECK-REQ-0049); (c) a netlist state element without a counterpart corresponds to
  an RTL state bit of the same kind (flip-flop or latch) without a counterpart, with either parity,
  and with matching base values when either is established by the pulse. Latches and state in a
  sequential-control cone take part only in (c).
- Search: signal correspondence by induction [@vaneijk2000sequential], decided by seeded random
  simulation. Starting from the largest candidate set, every relation is assumed for the current
  state values; next-state, clock and asynchronous functions are evaluated with random primary
  inputs and random values for all other state in 64-lane rounds (default 32 rounds); a relation
  whose obligation fails in any lane is dropped, least trusted kinds first ((b), then (c), then
  (a)). Relations of kind (b) and (c) are proposed together for all states still without one,
  between members whose evaluated functions agree in at least three quarters of the lanes; a
  shared state prefers a representative of the same RTL variable, then the representative whose
  next-state function reads the most similar set of signals (Jaccard index of the supports), as
  random simulation cannot tell apart registers that rarely change. Cell candidates with the
  same simulation disagreement prefer more similar input and corresponding-state supports.
  Current state correspondences canonicalize those supports; unknown state symbols and
  hypothesized constants are omitted. This is a ranking hint, including when it uses a proposed
  correspondence, and never replaces the proof below. Proposing and refining repeat until no
  relation changes, without a bound on the rounds: a round that changes the relations either
  counts a failure of a dropped relation, and a relation that failed twice is not proposed again,
  or only adds relations or replaces a constant by a cell relation, so the rounds end. A chain of
  state whose relations each need the one before takes a round per link.
- Discovered relations then go through exactly the base-case validation and proof obligations of
  named ones (CHECK-REQ-0040, CHECK-REQ-0048 to CHECK-REQ-0050). Discovery only selects which
  obligations are posed, never their outcome, so it cannot cause a false `equivalent` verdict.
  Random simulation rarely activates rare conditions; relations that the proof rejects are
  therefore excluded, and discovery and proof are repeated without them (at most four times). A
  wrong relation that simulation could not refute makes the obligations of correct relations that
  read it fail as well, so of the rejected relations only those of the first kind present in the
  order (a), (b), (c) are excluded at a time: a constant that holds except under a rare condition
  is the usual cause, and a netlist element whose relation (c) is excluded is left without a
  counterpart. Such a wrong relation can also keep simulation from accepting the relation of an
  element; while discovery leaves netlist state elements without a counterpart, the proof
  therefore still runs with their values cut free, only to reject discovered relations, and an
  element without a counterpart once the proof rejects no discovered relation is an error
  (CHECK-REQ-0031). The verdict and mismatches are those of the last proof, unless the
  repetitions end at their limit while the proof still rejects discovered relations or an element
  is still without a counterpart: the verdict is then `undecided` (CHECK-REQ-0002), as the
  failures of relations that discovery proposed are no evidence of a difference. A port
  difference (CHECK-REQ-0030) does not depend on the correspondence and still makes it
  `not-equivalent`; the limit is then listed in `undecided`.
- The random values of discovery depend only on the seed and the two designs, so a check is
  reproducible (CHECK-REQ-0043).
- Discovery runs only when names leave a netlist state element without a counterpart, or when
  random simulation refutes an obligation while RTL state without a counterpart is cut free;
  checks that names decide behave as without discovery.
- The report lists every discovered relation in `discovered_states` as the RTL state name and the
  relation: `constant 0`, `constant 1`, `shares <RTL state name>` (with `inverted` when the parity
  is odd) or `cell <instance>` (with `inverted`).

## Checks

**CHECK-REQ-0040** For every corresponding pair of state elements the checker proves, as functions
of primary inputs and corresponding state values:
- clock (or enable) equality, after normalizing the edge (or level) polarity;
- equality of the asynchronous clear and preset conditions and of the asynchronous value;
- equality of the next-state function whenever no asynchronous control is active (for latches:
  whenever the enable is active).

**CHECK-REQ-0041** For every primary output bit the checker proves equality of the output
functions of primary inputs and corresponding state values.

**CHECK-REQ-0042** Proofs use a SAT solver on a miter in conjunctive normal form built with the
Tseitin transformation [@tseitin1968complexity]; the solver is a conflict-driven clause-learning
solver [@marquessilva1999grasp; @moskewicz2001chaff; @een2003extensible] implemented in the checker
crate. A satisfying assignment is a counterexample and is reported with the input and state
values that expose the difference. A proof that reaches its conflict budget is undecided
(CHECK-REQ-0002).

The initial cone-limited decision attempt and subsequent full proof share the obligation's
conflict budget; conflicts already spent are deducted before the fallback.

**CHECK-REQ-0043** Before SAT, the checker simulates both models with seeded random values for
primary inputs and state (default 1024 patterns) and reports the first mismatch found. The seed is
reported and reproducible.

**CHECK-REQ-0044** Every counterexample found by SAT is replayed by simulation of both models; a
counterexample that does not replay is an internal failure.

**CHECK-REQ-0045** Sequential simulation: optionally (`--cycles N`), both designs are simulated for
N clock cycles from a common random initial state with random inputs, applying each asynchronous
control for at least one cycle at the start; outputs are compared every cycle. This check needs
a single clock domain (every state element clocked by the same primary input, of either
polarity) and no latches; otherwise it is skipped and the report says why. Simulation runs 64
lanes in parallel; each cycle applies new random inputs with the clock low, then raises and
lowers the clock, comparing outputs in both phases. Asynchronous controls driven by a primary
input are active in the first cycle and afterwards in about one lane in 64 per cycle. The report
field `seq_sim` gives the cycles simulated or the reason for skipping.

**CHECK-REQ-0046** Equivalence sweeping: before the obligations are solved, the checker groups
the variables of the joint circuit into candidate classes by random simulation (default 16384
patterns, seeded from the reported seed), encodes the
circuit in topological order into one incremental solver, and proves each variable equal (or
complementary) to the first variable of its class under solver assumptions
[@kuehlmann2002robust; @een2003extensible]. A proven variable reuses that variable's solver
literal; a refuted candidate's counterexample is added to the simulation patterns to split the
classes; an undecided candidate (per-candidate conflict budget, default 2000) is simply not
merged. `--sweep-max-candidates` (`CheckOptions::sweep_max_candidates`, default 32768) bounds
the number of optional candidate attempts. At that limit the remaining variables are encoded
with structural hashing, without candidate SAT calls or further refinement batches. All final
proof obligations still run; skipped candidates alone never imply equivalence or `undecided`.
A value of 0 selects structural-only sweeping. Free-variable simulation words use dense
per-circuit tables, and counterexample batches contain at most 64 patterns; both use the same
pattern-major seed order. A candidate proof decides only variables of the candidate's cone: first the gates
nearest to the candidate, then all inputs of the cone; a satisfying assignment is accepted once
every one of them is assigned without conflict, because unit propagation then determines the
whole cone. Sweeping changes run time only: every obligation is still decided by SAT on clauses that
are implied by the circuit, and every counterexample is replayed (CHECK-REQ-0044).

**CHECK-REQ-0047** The checker's RTL model expands multiplication with the canonical structure of
SYNTH-REQ-0404, built from that description and not from engine code. Correctness does not depend
on the choice: every full-adder step preserves the weighted sum, and the checker's construction is
tested against integer multiplication exhaustively for all widths up to 6 (signed and unsigned,
and signed operands narrower than the product) and for random operands up to 64 bits. The shared structure makes equivalent internal points of the RTL and netlist models
likely, which the sweeping of CHECK-REQ-0046 needs for arithmetic that plain SAT cannot prove.

## Sources other than RTL

**CHECK-REQ-0100** `rustsys check synth` takes the sources that synthesis takes (SYNTH-REQ-0900):
SystemVerilog, BLIF (`.blif`) and AIGER (`.aag`, `.aig`) chosen by extension, or a gate-level
reference netlist (`--gate-netlist FILE` with its `--gate-lib` libraries), with
`--initial-values` as given to synthesis. The checker reads BLIF and AIGER with readers of its own
in `rustsys-check` (modules `blif` and `aiger`), not with the shared readers
`rusteda_formats::blif` and `rusteda_formats::aiger` that the engine uses, and builds the reference
model itself, as it builds the RTL model of CHECK-REQ-0010 for SystemVerilog. The proofs, the
correspondence and the report are those of the RTL model (CHECK-REQ-0030 to 0060).

**CHECK-REQ-0101** BLIF model. The checker's BLIF reader accepts the text, models, covers, latches,
cells and hierarchy of BLIF-REQ-0010 to 0042 with the semantics stated there, and builds one model
input per primary input and per generated port (SYNTH-REQ-0903, 0904, 0906), one state per latch
and per sequential cell instance named by SYNTH-REQ-0907, their clocks, enables and data
(SYNTH-REQ-0905), and the outputs. A signal without a driver reads as the constant 0
(SYNTH-REQ-0908), with a warning `C0903`. The constructs of BLIF-REQ-0050 and malformed input make
the verdict `error` with a message `C0900`; the constructs of BLIF-REQ-0051 and 0052 are warnings
`C0901`, never skipped silently.

**CHECK-REQ-0102** AIGER model. The checker's AIGER reader accepts the ASCII and the binary form
with the header, the latch reset values, the symbol table and the comment section of
AIGER-REQ-0010 to 0024, and builds the inputs, generated ports, states (one per latch, clocked by
the global clock) and outputs named by SYNTH-REQ-0903 and 0907. Properties (bad-state, constraint,
justice and fairness sections), malformed and truncated input make the verdict `error` with a
message `C0900`.

**CHECK-REQ-0103** Gate-level reference. With `--gate-netlist`, the checker builds the reference
model from the reference netlist with its own netlist model (CHECK-REQ-0020 to 0023) over the cells
of the `--gate-lib` libraries, not with the structural reader that the engine uses: the input ports
of the reference are the model inputs, its output ports the model outputs, and each sequential
instance a state named after the instance (SYNTH-REQ-0912) with the clock or enable, the next state
or data, and the one of its clear and preset functions that is not constant 0 as its asynchronous
control, forcing 0 or 1. An instance whose clear and preset are both not constant 0 makes the
verdict `error` (`C0902`), as does every construct that CHECK-REQ-0020 to 0023 reject.

**CHECK-REQ-0104** Cells of BLIF sources. `.gate` and `.mlatch` instances are evaluated from the
Liberty descriptions of the `--gate-lib` cells with the checker's own reader of Liberty functions
(LIBERTY-REQ-0020), with the pin rules of SYNTH-REQ-0909 and the cell rules of SYNTH-REQ-0911; an
instance outside them, or any instance without `--gate-lib`, makes the verdict `error` (`C0902`).

**CHECK-REQ-0105** Initial values. Under `--initial-values reset` the model gives every state with a
declared initial value the asynchronous control of SYNTH-REQ-0906, driven by the generated port,
so that the obligation on asynchronous values (CHECK-REQ-0040) compares the declared initial value
with the netlist's: a netlist whose state element takes another initial value is
`not-equivalent`. Under `--initial-values arbitrary` the obligations hold for every initial state.
The report records `source_format`, `initial_values_mode` and the declared `initial_values`.

**CHECK-REQ-0106** Planted defects of sources: against the netlist synthesized from an unchanged
source, a changed row of a BLIF cover, a swapped initial value of a BLIF latch (under
`--initial-values reset`), a flipped inversion bit of an AIGER literal and a gate-level reference
with one cell replaced by a cell of another function each make the verdict `not-equivalent`.

**CHECK-REQ-0107** The checker's readers bound what they allocate by the size of their input (a
BLIF hierarchy by the limits of BLIF-REQ-0060, an AIGER header by the data that follows it), walk
hierarchies and graphs with explicit stacks, and never panic on malformed input.

## Report

**CHECK-REQ-0060** The JSON report contains `verdict` (CHECK-REQ-0002), `outputs_checked`,
`state_pairs_checked`, `rtl_state_bits`, `netlist_state_elements`, `unmatched_rtl_state_bits`
(names), `mismatches` (a list of objects with `kind`, `name`, and `counterexample`), `errors`
(the reasons of `error`), `internal_error` (the reasons are an internal failure of the checker,
CHECK-REQ-0001), `undecided` (what stopped at its budget, CHECK-REQ-0002), `sat_calls`,
`sweep` (candidates, merged, refuted, undecided, solver calls and skipped candidates), `supply_ports` (CHECK-REQ-0022),
`discovered_states` (CHECK-REQ-0032), `seed`, and `runtime_s`.

## Verification of the checker

**CHECK-REQ-0070** The checker must report `not-equivalent` for every planted defect class of
`rustsys-defects`: cell function substitution, input pin swap on an asymmetric cell, inverted
net, net stuck at 0 or 1, rewired cell input, removed inverter, clock edge inversion, asynchronous
control polarity inversion, asynchronous value inversion, and swapped state names.

**CHECK-REQ-0080** `check_with(options, libraries, ctx)` runs the check with a context
(JOB-REQ-0002): phases `rtl`, `netlist`, `simulate`, `sweep`, `prove`, with progress of the sweep;
verbose notes are `note[C0002]` messages instead of lines written by the library; `check` is
`check_with` with `Ctx::NONE`, and verdicts do not depend on observation.

**CHECK-REQ-0081** `rustsys check synth` prints on standard output the `mismatch[<kind>]: <name>`
lines and `verdict: <verdict> (outputs <o>, state pairs <s>, unmatched RTL state bits <u>)` with the
verdict spelled as in the JSON report (`equivalent`, `initial-state-refinement`, `not-equivalent`,
`undecided`); a report whose verdict is `error` shows `verdict: error` only in `rustsys show`, as
the command then prints no result (CLI-REQ-0002). Each entry of `undecided` is also printed as
`warning[C0005]` on standard error.

**CHECK-REQ-0082** `--netlist` is repeatable; `--from-run <RUN>` of a synthesis run supplies
sources, front-end options, libraries, top and netlist (PROTO-REQ-0061); Liberty errors,
conflicting cell definitions and instances of cells described with errors included, are `C0003`,
report write errors `C0004`, budgets that ran out `C0005` (CHECK-REQ-0002), constructs outside the
model `C0101` to `C0105` (CHECK-REQ-0023) and `C0106` (CHECK-REQ-0014), internal failures of the checker `A0002`
(CHECK-REQ-0001), other check errors `C0001`.

**CHECK-REQ-0085** The JSON report starts with `format` (`rustsys-check-synth-report`) and
`format_version` (2). Version 2 spells the initial-state refinement `initial-state-refinement`
(version 1: `refinement`) and adds the verdict `undecided` and the fields `undecided` and
`internal_error`; reports of version 1 are still read.


**CHECK-REQ-0048** Explicit reset induction. With `--after-reset NAME=0|1`, constant aliases of
RTL state names may replace sequential counterparts. The checker independently validates every
base case on the original RTL circuit: the state must be a flip-flop, its asynchronous control
must trace through aliases/inversions to an asserted scalar primary input, and its reset value
must equal the netlist constant. A constant state in any RTL sequential-control cone is rejected,
independently of the synthesis report. The pulse must settle before clock sampling as specified
in SYNTH-REQ-0309. Conflicting or unknown pulse inputs are errors. Only after all
base cases have been validated may the checker constrain these state variables simultaneously.
For each constant it proves that the original next-state function equals the constant whenever
that state's reset is inactive. Outputs and all retained state behavior are proved under this
joint invariant, with subsequent primary inputs unrestricted. A failed step rejects equivalence;
proof exhaustion makes the verdict `undecided` (CHECK-REQ-0002). The report lists pulse
assignments and each constant checked.
Without the option, constant state aliases continue to be rejected. Sequential simulation applies
the requested simultaneous pulse in its first cycle; unsupported clock/latch cases remain reported
as skipped, never as a completed simulation.


**CHECK-REQ-0049** Multiple named RTL states may share one physical state in explicit reset
mode, or under the explicit initial-state refinement of CHECK-REQ-0051. Independently establish every reset relation before tying state variables; require matching
or complementary base values with the physical output parity. Reject latches and state in control
cones. Under the joint relation, prove every member's data, clock and asynchronous obligations.
Retain the original independent-state sequential replay before introducing the proof assumptions.
Default mode still rejects two RTL state names driven by one physical element.

**CHECK-REQ-0050** With explicit `--uninitialized-dont-care`, the checker may independently infer
an initial value from a netlist constant alias for an unreset RTL flip-flop outside every
sequential-control cone. Latches and asynchronous reset values without an established pulse
remain ineligible. No synthesis metadata is trusted. After establishing each base case through
either the specified pulse or an authorized unspecified initial choice, prove all joint invariant
steps with future inputs unrestricted. In particular, an unreset state's next function must equal
its chosen value unconditionally. Sequential replay starts those RTL states at the recorded
values before clocking, while all other initial state stays random. Successful checks that use
any such choice return `initial-state-refinement`, list `uninitialized_choices`, and must not be
described as arbitrary-initial-state equivalence. Without the option those aliases remain
rejected. Saved-job and `--from-run` checking preserve and display this semantic option.


**CHECK-REQ-0051** With explicit `--uninitialized-dont-care`, two unreset RTL flip-flops
outside all sequential-control cones may select initially equal or complementary values
when a netlist element drives both names. Reject mixed reset/unreset bases, latches and
control states. Prove every member's next-state, clock and asynchronous obligations under
the joint relation with future inputs unrestricted. Initialize sequential replay consistently
with the selected relation, and test bad next-state functions with replay disabled as well.
Report these assumptions separately as `uninitialized_state_relations`; any successful check
using one is `initial-state-refinement`, never arbitrary-initial-state equivalence. Without the
option, retain the strict rejection of such sharing.
