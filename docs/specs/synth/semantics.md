# RustSys semantics

## Value system

**SYNTH-REQ-0201** The netlist implements the RTL under a two-valued interpretation:
- every `x` or `z` bit of a literal (including `'x`, `'z` and `?` digits) is 0, except inside
  case items of `casez`/`casex`, in the right operand of `==?`/`!=?`, and in `inside` ranges, where
  the standard gives these bits wildcard meaning;
- `===` and `!==` behave as `==` and `!=`; `$isunknown` is 0;
- any operation that the standard defines to produce `x` bits (division by zero, out-of-range
  reads, reads of undriven nets) produces 0 bits;
- state elements without an asynchronous initialization have an unconstrained initial value.

This is a deliberate modeling choice: RustSys does not exploit `x` as a don't-care in this
version, which makes the synthesized netlist a deterministic function of the RTL.

## Expressions

**SYNTH-REQ-0202** Expression bit lengths and signedness follow the standard, clauses 11.6 to 11.8:
self-determined sizes are computed bottom-up; context-determined operands are extended to the
maximum size of the context, including the assignment target; operands are sign-extended only if
the context is signed; the result is truncated to the target.

**SYNTH-REQ-0203** Integral arithmetic is modulo 2^w where w is the context size. Division and
modulus follow the standard's truncating definition for signed operands. Division or modulus by
zero yields 0 (SYNTH-REQ-0201).

**SYNTH-REQ-0204** Shift amounts are unsigned. A shift by at least the operand width yields all
zeros, or all sign bits for `>>>` on a signed operand.

**SYNTH-REQ-0205** `a ** b` is supported when both operands are constant, when `a` is the constant
2 (lowered to a shift) or when `b` is a constant (lowered to repeated multiplication). Constant
evaluation follows the standard's table for negative, zero and one values of the operands. Other
forms are rejected.

**SYNTH-REQ-0206** A bit-select, part-select or array index whose position lies outside the declared
range reads as 0 and makes a write have no effect. Partially out-of-range part-selects read the
in-range bits and 0 for the others.

**SYNTH-REQ-0207** Enumeration variables hold their base-type value; values outside the enumeration
are not restricted. Enumeration constants without explicit values are numbered per the standard,
clause 6.19.

**SYNTH-REQ-0208** Structure and union member selections map to fixed bit ranges of the packed
value: the first declared member occupies the most significant bits.

## Procedures

**SYNTH-REQ-0210** Continuous assignments and procedures describe drivers. Every bit of a net or
variable has at most one driver: a continuous assignment, a gate primitive output, an instance
output port, a single procedure, or a declaration initializer (SYNTH-REQ-0112). Two drivers of
one bit are an error.

**SYNTH-REQ-0211** Inside a procedure, blocking assignments take effect immediately; nonblocking
assignments take effect after the procedure completes; a later nonblocking assignment to the same
bits overrides an earlier one. A variable assigned by both kinds of assignment is rejected.

**SYNTH-REQ-0212** `always_comb`, `always @*`, `always @(<list without edges>)` and `always_latch`
are combinational or latch procedures. For every bit b written by such a procedure, let W be the
condition under which the procedure writes b and V the value b has when the procedure ends, both
as functions of the values the procedure reads (signals read by the procedure are treated as
independent variables, including b's own value before the procedure):
- if W is 0 for all values, the procedure does not drive b;
- if W is 1 for all values, b is combinational and driven by V; V must not depend on b's own
  earlier value (otherwise it is a combinational loop, SYNTH-REQ-0214);
- otherwise b is a latch (SYNTH-REQ-0305).
An `always` without an event control at its head is rejected.

**SYNTH-REQ-0213** A net or variable bit with no driver reads as 0, and a warning names it. An
unconnected module input reads as 0.

**SYNTH-REQ-0214** A combinational cycle at bit level (a bit whose value depends on itself without
a state element in between) is an error.

**SYNTH-REQ-0215** `if` and `case` qualifiers (`unique`, `unique0`, `priority`) and synthesis
attributes do not change the synthesized function: the first matching branch in source order is
selected, and when no branch matches and there is no `else`/`default`, no assignment takes place.
The only exceptions are the opt-in complete-case and parallel-case assumptions of SYNTH-REQ-0224
and SYNTH-REQ-0225.

**SYNTH-REQ-0220** The case expression and every case item expression (including both bounds of
the ranges of `case ... inside`) are context-determined operands of the comparison: each is
evaluated at the width of the widest of them, as signed only if all of them are signed
(IEEE 1800-2017 12.5 and the sizing rules of 11.6).

**SYNTH-REQ-0224** Complete-case assumption. With `--assume-full-case`, a `case`, `casez` or
`casex` statement marked complete, by a `full_case` attribute (IEEE 1364.1) or the `unique` or
`priority` qualifier (IEEE 1800-2017 12.5.3), that has no `default` item behaves as if its last
item were the `default` item: that item's labels are not evaluated and its statement runs whenever
no earlier item matches. Everything else follows SYNTH-REQ-0215 (first match in source order). The
rule changes the function only for selections the designer declared impossible; it is a
synthesis-semantics refinement that the independent checker applies identically (CHECK-REQ-0012),
and the report records the option. `parallel_case` and `unique0` remain without effect. Tests: a
three-item `full_case` over a two-bit selector infers no latch and selects the last item for the
unlisted value; without the option the latch remains; the checker rejects the netlist without the
option and accepts it with the option.

**SYNTH-REQ-0225** Parallel-case assumption. With `--assume-parallel-case`, a `case`, `casez` or
`casex` statement marked parallel, by a `parallel_case` attribute (IEEE 1364.1) or the `unique` or
`unique0` qualifier (IEEE 1800-2017 12.5.3), runs every item under its own match condition m_i,
without the priority of earlier items, each item starting from the state before the statement;
the default item (or, under SYNTH-REQ-0224, the last item) runs under the condition that no item
matches. Every bit the statement writes (its value, its nonblocking value and the condition under
which it is written) and every kill condition of `break`, `continue` and `return` becomes

    OR_i (m_i AND x_i)  OR  (NOT OR_i m_i AND x_d)

where x_i is the bit after item i and x_d after the default item (the state before the statement
when there is none). When at most one item matches this is the first-match result of
SYNTH-REQ-0215; when several match, the result is the OR of their results, for selections that
the marking declares impossible. The rule is defined on values, not on how a tool represents them,
so the independent checker applies it identically (CHECK-REQ-0013); the report records the option.
Tests: a `parallel_case` over a one-hot selector written as `case (1'b1)` with signal labels needs
no priority logic (fewer gates than without the option); the checker accepts the netlist with the
option and rejects it without; two matching items give the OR of their values in both models.

**SYNTH-REQ-0216** Loops are unrolled. A loop whose iteration count cannot be determined as a
constant, or that exceeds 65 536 iterations in one activation, is an error. `break`, `continue`
and `return` may depend on non-constant conditions; statements after them execute only on paths
where they were not taken.

**SYNTH-REQ-0217** A tri-state assignment (`z` value) is interpreted per SYNTH-REQ-0201 and issues a
warning; no tri-state cells are produced.

**SYNTH-REQ-0218** Variables declared inside procedural blocks, loop variables declared in `for`
headers, and subroutine arguments and locals are automatic: each activation starts them at 0,
a declaration initializer is executed as a blocking assignment where the declaration appears, and
they never become state elements.

**SYNTH-REQ-0219** A subroutine argument with direction `output`, `inout` or `ref` is copied to the
actual argument when the call returns; the actual must be an assignment target.

**SYNTH-REQ-0221** A replication with a zero count has size zero and contributes no bits. It is
accepted only as a direct operand of a concatenation whose remaining operands have a positive
total size (IEEE 1800-2017 11.4.12.1); anywhere else, or when every operand of the concatenation
has size zero, it is an error.

**SYNTH-REQ-0222** When a conditional operator `c ? x : y` is the source of an assignment, an
initializer, a parameter value or an argument (an assignment-like context, IEEE 1800-2017 10.8),
and one of its operands is an assignment pattern without a type, or the target has an unpacked
type, both operands are converted to the target type before the selection. Operands that are
themselves conditional operators follow the same rule.

**SYNTH-REQ-0223** The `default` key of an assignment pattern sets every element or member that no
index or member key sets (IEEE 1800-2017 10.9.1, 10.9.2). When that element or member is a single
bit, a one-dimensional packed array of bits, an enumeration or a packed union, or has the type of
the default value itself, the value is converted to its type as in an assignment; an untyped
assignment pattern used as the default value takes the element's type. Any other element or
member — a structure, or a packed or unpacked array whose elements are not single bits — is set by
applying the default again to each of its own elements or members. For example,
`logic [1:0][7:0] v = '{default: 1};` gives `v == 16'h0101`.
