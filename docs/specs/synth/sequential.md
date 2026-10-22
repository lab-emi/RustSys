# RustSys registers and latches

**SYNTH-REQ-0301** An `always_ff` procedure, or an `always` procedure whose leading event control
contains only edge events (`posedge`, `negedge`), is an edge procedure. Every variable bit it
assigns is a state bit of an edge-triggered register. State bits whose value can never reach a
primary output or another observable state bit are removed (SYNTH-REQ-0308). Consequently, a
variable that is written with blocking assignments and only used inside the procedure after being
written produces no register.

**SYNTH-REQ-0302** Clock and asynchronous controls. Each event of the leading event control is a
single-bit expression with an edge. The clock is the unique event whose expression is not
referenced by the procedure body. Every other event is an asynchronous control, active while its
expression is 0 for `negedge` and 1 for `posedge`. If zero or several events qualify as the clock,
the procedure is rejected.

**SYNTH-REQ-0303** For each state bit b and each asynchronous control c, let v be the value that the
procedure gives b when c is active and every other asynchronous control is inactive (after
applying SYNTH-REQ-0201):
- if v is a constant, c is an asynchronous control of b with asynchronous value v;
- if v is b's current value (the procedure leaves b unchanged), c is not an asynchronous control
  of b; c then takes part in b's synchronous next-state function like any other signal;
- otherwise the procedure is rejected ("asynchronous load").
The synchronous next state of b is the value the procedure gives b when every asynchronous
control of b is inactive.

**SYNTH-REQ-0304** A state bit has at most one asynchronous control; more are rejected. Every
asynchronous control must be an asynchronous control of at least one state bit of its procedure;
otherwise the procedure is rejected.

**SYNTH-REQ-0305** Latches. A bit of a combinational or latch procedure whose write condition W is
neither always 0 nor always 1 (SYNTH-REQ-0212) is a level-sensitive latch with enable W and data V.
A warning names every latch inferred from an `always_comb` or `always` procedure. Bits of an
`always_latch` procedure whose W is always 1 are combinational (with a warning). Only the value of
V while W is 1 matters. When W is a conjunction of literals of combinational inputs (for example
`!clk` or `clk & en_q`), V is replaced by its cofactor with those literals true, which equals V
wherever W is 1. Then, when V depends on the latch's own previous value and the two cofactors of V
with respect to that value are proven equal whenever W is 1, the data input is the cofactor for 0
(the hold path of the procedure disappears).

**SYNTH-REQ-0306** State elements have no initial value (SYNTH-REQ-0201). Declaration initializers
of variables driven by edge procedures are ignored with a warning.

**SYNTH-REQ-0307** The clock of an edge-triggered state bit is the value of the clock expression;
a `negedge` clock is equivalent to a `posedge` clock of the complemented expression.

**SYNTH-REQ-0308** Default state preservation. Except for the explicitly enabled reset mode
(SYNTH-REQ-0309), every retained state bit of the RTL is implemented by exactly
one sequential library cell (plus inverters where SYNTH-REQ-0520 requires them). State bits are
never merged, duplicated, retimed or re-encoded. A state bit is removed only when it is
unobservable: no primary output and no retained state bit depends on it.


**SYNTH-REQ-0309** Explicit reset mode. Repeated `--after-reset NAME=0|1` options describe scalar
primary inputs asserted together long enough to establish their asynchronous reset values before
any output is observed. Keep clock inputs stable while asserting the pulse and until asynchronous
state has settled; then clocks may run with reset active or inactive. State in any clock, latch-enable
or asynchronous-control cone is retained to preserve initialization events. Subsequent inputs and clock events are unrestricted. This contract does
not assume a power-on value or ongoing reset assertion. Empty options retain SYNTH-REQ-0308.

Only flip-flops whose asynchronous control is a literal of a specified primary input are
candidates for constant-state removal. The pulse must make that control active. Starting with
these reset values, remove a candidate unless its data equals its reset value under the joint
candidate invariant and its own reset inactive. Iterate to a fixed point. All other state and
inputs are unconstrained. A syntactic cofactor simplification or a bounded UNSAT proof must prove
each accepted constant. Simulation can reject candidates but can never accept a constant.
This establishes a joint invariant even across different clocks. Substitute accepted constants,
then remove unobservable state and logic. Preserve a named scalar alias for each removed constant
state so the independent checker can validate it without trusting synthesis metadata. Reports
record the reset assignments and the names, values and reset inputs of removed constant states.
Latches, synchronous initialization and arbitrary reset expressions are not inferred by this mode.


**SYNTH-REQ-0310** In explicit reset mode, state bits with the same clock literal and reset literal may be merged after inductive partition
refinement. Normalize each candidate by its reset value, allowing equal or complementary relations.
Start from reset-compatible classes and split classes unless their normalized next-state functions
are structurally equal or, for complementary candidates, equal by a bounded local proof. Simulation
only rejects candidates. At most 128 local proofs per refinement bound this additional work.
No unrelated reset or input is assumed inactive for this relation. States used by sequential
controls are excluded. Keep a physical representative and named aliases for all merged members;
report the member and representative names and polarity. The default arbitrary-initial-state mode does not
perform these merges. Independent checking must establish the reset base case and prove each
member's clock, asynchronous control and next-state obligations.

**SYNTH-REQ-0311** `--uninitialized-dont-care` is an explicit, default-off initial-state
refinement mode. It permits selecting an otherwise unspecified initial value of an unreset
flip-flop, separately from `--after-reset`. The current bounded search seeds eligible unreset
flops with zero and reset-established flops with their reset value, then withdraws candidates
until every next-state function preserves the joint constants for arbitrary future inputs and
remaining states. This is an invariant proof, not an assumption that a synchronous reset occurs.
States in sequential-control cones and latches remain ineligible. Named constant aliases remain
in the netlist; a missing `reset_input` in the constant-state record identifies a selected
uninitialized state. The report and canonical command record the enabled mode. No option changes
the default or reset-only semantics. Explicit RTL initial procedures and driven declaration
initializers retain their existing unsupported/ignored frontend diagnostics; this mode does not
add support for those constructs. Benchmarks must separate this refinement from all-initial-state
or reset-only equivalence.
