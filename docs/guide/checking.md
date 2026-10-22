# Check a synthesized netlist

Check the exact final netlist against the exact inputs used for synthesis. A successful synthesis command establishes that a netlist was produced; an equivalence check establishes a separate property.

## Run the native checker

```sh
rustsys check synth --top chip_top \
  -I rtl/include -D FEATURE=1 -P WIDTH=32 \
  --lib cells.lib --netlist .work/chip_mapped.v \
  --report .work/chip_check.json \
  rtl/types_pkg.sv rtl/datapath.sv rtl/chip_top.sv
```

For a recorded synthesis run:

```sh
rustsys check synth --from-run .work/chip_run --run-dir .work/chip_check_run
```

The checker has its own RTL interpreter, cell-function evaluator and SAT proof flow. It shares the SystemVerilog parser and core data types with synthesis, and does not depend on the synthesis engine. This separation reduces common implementation errors; it does not eliminate the need for independent external validation.

## Interpret the verdict

| Verdict | Exit | What to do |
|---|---:|---|
| `equivalent` | 0 | Accept only within the recorded inputs, language model and assumptions. |
| `initial-state-refinement` | 0 | Review the reported choices of unspecified initial state; this is a different contract. |
| `not-equivalent` | 3 | Inspect the mismatches and replayed counterexamples. |
| `undecided` | 5 | Keep verification incomplete; inspect the exhausted proof or search budget. |
| `error` | 1 or 2 | Fix rejected inputs or investigate an internal failure; no proof was established. |

An exhausted budget cannot become an equivalence verdict. A real counterexample can still decide `not-equivalent` even if another obligation exhausted its budget. See [exit codes](../reference/cli.md#exit-codes) and [report fields](../reference/reports.md#check-report).

## State and language scope

The default check preserves **arbitrary initial state**, with the reported correspondence between RTL state and implementation state. It adds no reset sequence or input constraint. Outputs, next-state behavior and relevant clock/enable and asynchronous controls are checked under that correspondence.

RustSys's RTL model is two-valued: ordinary `x` and `z` data bits become zero, as do undriven RTL bits. Wildcards in `casez`, `casex`, `==?` and `inside` retain their specified matching behavior, including concatenated and replicated labels. This is not a four-state simulation-equivalence claim. The structural netlist checker rejects unknown netlist literals and unsupported electrical or sequential cell behavior explicitly.

These options change the requested scope and must be retained with any result:

| Option | Scope change |
|---|---|
| `--after-reset NAME=0\|1` | Compare after the explicitly declared reset sequence; outputs before it are outside the contract. |
| `--uninitialized-dont-care` | Permit proved choices for otherwise unspecified initial state; may yield `initial-state-refinement`. |
| `--assume-full-case` | Apply completeness assumptions to marked case statements. |
| `--assume-parallel-case` | Apply parallel-case assumptions to marked statements. |

Leave all four disabled for the default arbitrary-initial-state, ordinary-case comparison. A reset pin in the RTL is a circuit input; its presence alone does not mean the proof assumed a reset. The [reset contract](../user/synth/reset.md) explains the optional weaker observation scope.

Power and ground connections are outside the logical comparison: cells are assumed powered. Supply-port exclusions are listed in `supply_ports`; do not treat this as a power-aware equivalence proof.

## Increase budgets deliberately

```sh
rustsys check synth --from-run .work/chip_run \
  --budget 10000000 --model-budget 1000000 \
  --report .work/chip_check_larger_budget.json
```

`--budget` is the SAT conflict limit **per final obligation**, default 2,000,000. `--model-budget` is the limit per proof needed to construct the checker's RTL model, default 200,000. They are not seconds or total-memory limits. Optional sweeping candidates are bounded separately by `--sweep-max-candidates`; using zero disables that optimization while retaining final proof obligations.

Use `--cycles N` for additional seeded sequential simulation where supported. Simulation can expose a mismatch, but cannot replace a completed formal proof. The report records skipped simulation and its reason.

## External LEC evidence

The common five-design benchmark protocol uses the same native two-valued RTL event model for both tools. External ABC checks Boolean obligations; inferred arithmetic uses exact Arisca component proofs, checked reconstruction, complete Boolean context and transport to the actual measured netlist. Every original input/state quantifier, output and retained event obligation remains in scope, with no reset sequence or initial-state choice. [The complete campaign](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/results/2026-10-21-default-multicore/README.md) records the tool versions, bounds and raw receipts.

Historical campaigns used different transports and undefined-value scopes, which remain documented with their own certificates. A native check, external RTL LEC and synthesis timing result are distinct artifacts.

See [verification evidence](../verification/index.md) for the current campaigns, their exact archives, known invalidations and the limits of each claim. For detailed native behavior, read the [checker specification](../specs/check/synth.md).
