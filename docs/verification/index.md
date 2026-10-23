# Verification evidence and limits

The current [common five-design campaign](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md) binds each measured implementation to its original RTL, Liberty library and exact exported Verilog bytes. Performance sampling, logical equivalence and timing analysis are separate operations with separate receipts.

## Current common campaign

The selected matrix is five designs × two libraries × RustSys/Yosys at both 1 and 16 physical host cores: forty configurations. Each selected configuration has three measured runs after one warm-up; all repetitions must complete. Each tool's 1/16-core netlists must be byte-identical.

Functional validation uses the independent RTL event model and external ABC CEC. Inferred multiplication/addition uses exact observed-operand components checked by Arisca, with complete ABC checks of extraction, context reconstruction, the Boolean context and transport to the actual measured netlist. Observations add obligations; they do not assume internal signals equivalent. The model completes ordinary X/Z data and undriven values to zero while keeping **arbitrary register initial states**. It adds no reset pulse, reachability restriction or new input assumption. Wildcards retain their language matching semantics.

The event export quantifies the original inputs and RTL states, retains every original output, and covers every implementation state element's data, clock/enable and asynchronous controls under a checked one-to-one correspondence. Names supply bindings first; when mapping removes them, a bounded search can propose bindings without fixing any initial value or sharing state. Restricting proposal candidates leaves every original RTL state free. The relation ledger must cover all implementation state. A successful export remains undecided until every external comparison completes.

Each certificate checks the input, library, gate implementation and obligation hashes. A previously completed certificate can be reused only for those exact bytes and the same scope. Changed measured netlists require a new proof. Data and clock corruption controls are included on both baseline PDKs.

The native interpreter shares format readers and core types with synthesis, while its RTL interpretation, cell evaluation and proof implementation are separate. External tool calibration and independently specified controls address common-error risk; this is not a claim of a fully independent frontend or four-state simulation equivalence.

## Timing evidence

An independent OpenSTA executable analyzes the exact mapped netlist under a common TT, ideal-clock, zero-wire model. Its setup-Fmax estimate is separate from functional equivalence and from routed timing closure. Read the [timing assumptions](../guide/performance.md#interpret-the-timing-estimate), excluded path classes and raw constraint-check output before using the number.

No timing result implies physical DRC, placement/routing, clock-tree correctness or hold closure. Missing results remain missing; they are never printed as zero violations.

## Historical proof scopes

The [earlier four-core campaign](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-20-lec-complete/README.md) used EQY/SBY, ABC CEC and undef-aware Yosys SAT with checked arithmetic compositions. It established defined-bit refinement under its documented state correspondence. The two historical native VexRiscv SAT checks stayed unverified under their budgets, despite completed external proofs in that campaign.

The [corrected single-run OpenC910 campaign](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-20-openc910-verified/README.md) calibrated the independent two-valued RTL model against actual Yosys netlists and checked complete projected obligations with ABC. Its producer and source snapshots are retained. It is a different measurement profile from the current common default-mapping campaign.

### Invalidations remain part of the record

A concatenated/replicated wildcard-label bug affected both synthesis and its native checker. The correction was checked against an independently captured simulation truth table, exhaustive small controls, external formal comparisons and negative old-netlist controls. Earlier certificates were explicitly invalidated in the [registry](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/proof-invalidations.json).

A historical `equivalent` report does not override a later invalidation. Use each campaign's dated archive and declared semantic model; do not relabel an old certificate as proof of a changed netlist.

## Regression coverage

The repository includes synthetic known-answer fixtures, generated and metamorphic designs, deliberately faulty netlists, resource-budget controls, and both Sky130 and GF180 coverage. Requirement identifiers connect behavior to implementations and tests.

- [Requirement-to-test matrix](requirements.md).
- [PDK verification matrix](pdk.md).
- [Contributor workflow](../contributing.md).
- [Logical checking and verdicts](../guide/checking.md).
