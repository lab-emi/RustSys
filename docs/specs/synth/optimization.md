# RustSys logic representation and optimization

## Representation

**SYNTH-REQ-0401** After elaboration the combinational logic is an And-Inverter Graph (AIG): a DAG
of two-input AND nodes whose edges may be complemented, with structural hashing and constant
propagation applied at node creation [@kuehlmann2002robust]. Nodes are stored as structure-of-
arrays: fanin literal arrays indexed by node id, with ids in topological order, so that the graph
can be moved to a device without conversion.

**SYNTH-REQ-0406** `mux_merge` merges nested multiplexers. A multiplexer `s ? t : e` is an OR of
two AND nodes on complementary select literals. When a data input is a multiplexer `s2 ? t2 : e2`
that shares the other data input and feeds nothing else (its top node and both of its AND nodes
have no other fanout), and the select pair `(s, s2)` occurs in at least two such nested pairs, the
two are rebuilt as one multiplexer by the identities `s ? (s2 ? t2 : e) : e = (s & s2) ? t2 : e`,
`s ? (s2 ? e : e2) : e = (s & !s2) ? e2 : e`, `s ? t : (s2 ? t : e2) = (s | s2) ? t : e2` and
`s ? t : (s2 ? t2 : t) = (s | !s2) ? t : t2`, applied again to the result. When both data inputs
are such multiplexers over the same data inputs, `r ? (s ? a : b) : (u ? a : b) = (r ? s : u) ? a :
b` merges them in the same way, under the same conditions for the select triple `(r, s, u)`.
Nested conditional updates of a word (an enable around a decoded array write, for example) and
selections between two words in either order then need one select term shared by all bits and one
multiplexer per bit.

**SYNTH-REQ-0408** `mux_decode` turns nested multiplexers into sums of products. A tree is read
from a multiplexer (SYNTH-REQ-0406, select taken as its regular literal) whose two AND nodes feed
nothing else, descending into data inputs that feed nothing else and are multiplexers of the same
kind or AND nodes with exactly one fanin used elsewhere (that fanin is the select, the other data
input is the constant 0, or 1 for a complemented AND), at most three levels deep. A tree with at
least three root-to-leaf paths is rewritten when the sequence of select literals along its paths
occurs in at least two trees: each path's product of select literals is built once and shared,
paths ending in the same leaf share one term (the OR of their products), and the result is the OR
of the terms, built in the phase in which the leaves are mostly regular literals (a tree's
complement is the same tree with complemented leaves). Inner nodes of a rewritten tree are not
trees of their own. A result with more AND nodes is discarded. The bits of a word that holds, loads
or shifts under shared conditions then share the conditions' products and need one AND-OR
structure per bit.

**SYNTH-REQ-0407** Sums. Elaboration keeps the terms of every sum it builds: rows of the result
width and a constant, modulo `2^w`. In `x + y` and `x - y` (where `-y` is `!y + 1`, and the negation
of a sum complements each row, adds one per row and subtracts the constant), an operand whose bits
are exactly those of an earlier sum of the same width contributes that sum's terms. Two operands
that are not such sums use the adder of SYNTH-REQ-0403 unchanged. Otherwise the terms give weighted
columns (the non-constant row bits in row order, then the bits of the constant term plus every
constant one among the rows), which carry-save rounds as in SYNTH-REQ-0404 reduce while a column
holds more than two bits (the lowest column more than three); one adder then adds the two remaining
rows, with the third bit of the lowest column as its carry-in. A conditional operator whose
operands are both such sums of the same width selects row-wise, since `c ? (t1 + t2) : (f1 + f2)
= (c ? t1 : f1) + (c ? t2 : f2)`: rows common to both stay unselected, the other rows are paired
in order (a missing row is zero) and selected bit by bit, and unequal constants are selected as
one more row. Sums of more than 8192 row bits keep no terms. `$countones` and the counts of
`$onehot` and `$onehot0` reduce all their bits from the lowest column in the same way.

**SYNTH-REQ-0402** The AIG boundary consists of combinational inputs (primary input bits and state
element outputs) and combinational outputs (primary output bits, state element data, enable, clock
and asynchronous control functions).

**SYNTH-REQ-0403** Word-level operators are lowered as follows: addition and subtraction by
ripple-carry chains (or SYNTH-REQ-0405 under electrical constraints); comparison by magnitude
chains or a balanced carry-transfer reduction; multiplication by partial
products reduced with carry-save adders [@wallace1964suggestion] and a final ripple-carry adder
(SYNTH-REQ-0404); division and modulus by restoring division; variable shifts by logarithmic
multiplexer stages; variable bit-selects and array reads by multiplexer trees, or, when it needs
fewer AND nodes (an estimated `2n + (2n - 1) w` against `3 (2^k - 1) w` for `n >= 4` elements of `w`
bits and a `k`-bit position), by a one-hot decoding of the position shared by all bits followed by
an OR of per-element AND terms; variable-position writes by per-position decoded updates.

**SYNTH-REQ-0404** Canonical multiplication structure. The operands `a` and `b`, extended to the
context width `w` (signed or unsigned per IEEE 1800-2017 11.6), give the low `w` bits of the
product as follows. Column `k` (`0 <= k < w`) starts with the partial products `a[j] & b[i]`,
`i + j = k`, in increasing order of `i`, leaving out products that are the constant 0. While some
column holds more than two bits, a reduction round processes the columns in increasing order: the
bits of column `k` are taken in order in consecutive groups of three; each group yields its sum
(exclusive OR of the three), appended to column `k` of the next round, and its carry (majority),
appended to column `k + 1` of the next round unless `k + 1 = w`; the one or two remaining bits
follow the sums in column `k` of the next round. The first and second bit of every column (0 when
absent) form two words that a ripple-carry adder adds. Every step preserves the weighted sum of
the bits, so the structure only fixes where intermediate values appear; the independent checker
builds the same structure so that its equivalence proofs find matching internal points
(CHECK-REQ-0047). When both operands are
signed and at least one is sign-extended (its significant width `n`, the smallest width such that
every bit at or above position `n - 1` is the same bit, is below `w`), the columns are built over
the significant widths `n` and `m` of `a` and `b` and over `min(w, n + m)` positions with the
Baugh–Wooley partial products [@baugh1973twos]: `a[j] & b[i]` when both or neither of `j = n - 1`
and `i = m - 1` hold, its complement when exactly one holds, in increasing order of `i`, leaving
out constant zeros; then a constant 1 is appended to columns `n - 1`, `m - 1` and `n + m - 1` (in
that order, when below the column count). The reduction and final addition are as above, and the
result is sign-extended to `w` bits.

## Optimization

**SYNTH-REQ-0410** Every optimization pass preserves the Boolean function of every combinational
output exactly for all input values (no don't-care exploitation in this version).

**SYNTH-REQ-0411** Passes available (our names): `sweep` removes logic that no combinational output
depends on; `balance` rebuilds multi-input AND trees, and exclusive-OR trees of at least four
operands through exclusive ORs that feed nothing else, by level to reduce depth, combining the two
shallowest operands first and, among equally deep operands, those of equal polarity first; `rewrite` replaces a
node's function over a cut of at most 4 leaves by a precomputed structure when building it adds
fewer nodes than the node's fanout-free cone frees, counting nodes that already exist outside that
cone as free [@mishchenko2006dag]; the structure of a 4-variable function is the cheapest of its
one-variable decompositions (AND, OR, exclusive OR with the variable, multiplexer on it), its
decompositions into two functions of disjoint supports (AND, OR, exclusive OR) and the factored
irredundant cover of the function or its complement [@minato1992fast; @brayton1987factoring],
where the cost counts AND nodes; `rewrite-zero` is `rewrite` that also accepts replacements that
add exactly as many nodes as they free; `refactor` collapses the cone of a node over a
reconvergence-driven cut of at most 10 leaves and replaces it by the factored irredundant cover of
the function or its complement when that needs fewer new nodes than the fanout-free cone frees;
`resub` re-expresses a node, within a window of at most 8 leaves, as an existing node or its
complement or as the AND or OR of two or three existing nodes, when that frees more nodes than it
adds, with reference counts updated after every accepted change [@mishchenko2006scalable];
`merge` merges nodes proven functionally equivalent by random simulation followed by SAT
[@kuehlmann2002robust]; `mux_merge` merges nested multiplexers (SYNTH-REQ-0406); `mux_decode` rewrites nested
multiplexers with repeated selects as sums of products (SYNTH-REQ-0408). A pass whose
result has more AND nodes than its input is discarded.

Balancing collects trees iteratively, with at most 512 visits and 256 retained leaves per
root. A bounded window falls back to the original two-input operation. At most 16,384 node
plans are retained in a batch; shared dense images and liveness replace per-edge hash tables.
Read-only planning can run in parallel, but rebuilding remains in canonical node order.
Memory or node-budget exhaustion leaves the input design unchanged. Internal multiplexer
probes retain their at most four structural readings inline. Resubstitution holds a scratch
shard for a short node batch and still commits every proposal in its original order.

**SYNTH-REQ-0412** The default (`--effort medium`) script is `sweep; mux_decode; balance; rewrite;
refactor; resub; rewrite; balance; mux_merge; sweep`. `--effort low` runs `sweep` only; `--effort high`
follows the default script with `merge; rewrite-zero; balance; refactor; resub; rewrite; refactor;
balance`.

**SYNTH-REQ-0413** Determinism: identical inputs and options produce a byte-identical netlist,
independent of the number of threads.

Parallel rewrite/refactor planning shares immutable graph data and uses ordered commits.
Speculative resubstitution records the properties read by each decision and replays stale
decisions, including unsuccessful candidates. Limits bound worker caches, cuts, windows,
retained read records and proposal storage; budget fallback must preserve functional behavior.
`opt_work` exposes bounded/replayed windows and whole-pass budget fallbacks.

Sweep compaction may reuse the state-observability closure as its copy mask. Mark before
enqueue so reconvergent fanin and feedback through state cannot materialize duplicate paths;
copy in increasing topological node order and retain original state/input order. Check node
and planning-storage limits before mutation, returning the unchanged design on exhaustion.

## Carry depth under electrical constraints

**SYNTH-REQ-0405** When electrical refinement is requested, addition and subtraction of at
least eight bits use groups of at most four local carry steps. A group is represented by
`(G, P)`, meaning `c_out = G | (P & c_in)`. The associative composition of adjacent low and
high groups is `(G_high | P_high & G_low, P_high & P_low)`. Pairwise reduction followed by
expansion computes all group prefixes with linear work and logarithmic depth, using the
prefix principle of [@brent1982regular]. Local sums remain the exclusive OR of operand bits
and their incoming carry. Partial last groups, constant operands and incoming carry 0, 1
or a signal are supported. A comparison of at least thirty-two bits that needs only the final carry uses sixteen-bit local
transfer functions followed by a balanced reduction, without prefix expansion. Multiplication's final addition may use the same
structure; its weighted partial products and arithmetic meaning remain unchanged.

Validation covers all eight-bit operand pairs with both incoming carry values, carry
propagation across group boundaries, odd widths and arithmetic near 64-bit boundaries.

**SYNTH-REQ-0418** At medium effort with electrical refinement, the existing medium script is
followed by `merge_local; rewrite_zero; balance; refactor; resub; rewrite; refactor; balance; sweep`. Bounded local proofs and
equal-area structural alternatives expose further factoring opportunities before cell covering.
Absent electrical constraints, the original effort scripts are unchanged.


**SYNTH-REQ-0414** Local equivalence merging uses bounded cone abstractions. Shared nodes and
window boundaries become independent inputs; proving equality in this stronger abstraction is
sufficient for equality in the original graph. Word simulation only rejects candidates; it never
accepts a merge. At most 5,000 candidates, 256 nodes per breadth-first root window and 64 SAT conflicts per
candidate bound medium-effort work. Global high-effort merging retains its own policy.


**SYNTH-REQ-0415** Refactoring may cache deterministic factored forms by the complete truth table
and its variable count. Graph-specific reuse cost is always recomputed. A synthesis invocation may share the cache across its refactoring passes; capacity is 16,384 entries.
Eviction cannot change the chosen form. Exact area recovery may cache subcover cost per cut and
complemented-leaf mask while retaining all cell choices. On one worker, use the same sequential
commit order directly instead of constructing speculative reference-count overlays.


**SYNTH-REQ-0416** Truth-table construction inside a refactoring cone may use dense word storage
and node-to-offset lookup. It must retain the full variable count, complement mask and truth table;
this representation optimization must not change candidates, their order or selected logic.

**SYNTH-REQ-0417** Truth tables of at most ten variables require at most sixteen 64-bit words.
Refactoring may keep these bounded tables in inline storage to reduce allocation overhead,
without changing functions, factoring decisions or graph-specific cost checks.
