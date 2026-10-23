# Synthesize your design

RustSys reads synthesizable SystemVerilog, elaborates hierarchy and parameters, optimizes a bit-level logic network, and maps it to cells from your Liberty libraries. Its output is flat structural Verilog plus an optional JSON report.

## Specify the design explicitly

```sh
rustsys --threads 4 synth \
  --top chip_top \
  -I rtl/include -D FEATURE=1 -P WIDTH=32 \
  --lib cells.lib \
  --out .work/chip_mapped.v \
  --report .work/chip.json \
  rtl/types_pkg.sv rtl/datapath.sv rtl/chip_top.sv
```

List packages before modules that use them. Repeat `-I`, `-D`, `-P` and `--lib` when needed. `SYNTHESIS` is predefined unless `--no-predefined-macros` is given. If `--top` is omitted, RustSys requires an unambiguous module that no other module instantiates.

Keep the same source files, macros, parameters, libraries and semantic options when checking. Recording a run and passing `check synth --from-run` avoids copying that configuration manually.

## Use EZ130 cells on IHP SG13G2

The optional [EZ130 8T v1.1 library](https://iip.ethz.ch/ez-library/ihp130-8t.html)
targets IHP SG13G2 130 nm. The benchmark pins TT, 1.20 V and 25°C, plus the
download and Liberty checksums. From the repository root:

```sh
(
  ulimit -v 2097152
  timeout 120s python3 verification/benchmarks/open-hardware/fetch_additional_pdks.py
)
mkdir -p .work
rustsys --threads 16 synth --top counter --goal area --effort medium \
  --lib target/pdk/ez130_8t_tt_1p20v_25c.lib \
  --out .work/counter_ez130.v --report .work/counter_ez130.json \
  verification/fixtures/synthetic/synth/rtl/counter/counter.sv
```

The Python helper downloads library data; synthesis runs in the native Rust CLI.
Keep the exact Liberty file with the result. See [library provenance](../verification/pdk.md#additional-ihp-sg13g2-benchmark-library)
and the [five-design comparison](benchmarks.md).

EZ130 v1.1 has a documented tie-cell driver-type conflict between Liberty and
the vendor Verilog models. The benchmark verification workflow certifies an
explicit functional supplement for these two cells; the native checker keeps
rejecting unmodeled strength/high-impedance attributes in an unadapted library.
Read the [cell-model scope](../verification/pdk.md#additional-ihp-sg13g2-benchmark-library)
when reproducing its LEC results.

## Choose the mapping tradeoff

| Option | Effect |
|---|---|
| `--goal delay` | Prioritize estimated delay; the default. |
| `--goal area` | Prioritize total cell area. |
| `--effort low`, `medium`, `high` | Control optimization/search effort; `medium` is the default. |
| `--delay-relax 0.1` | Permit relative delay growth during area recovery. |
| `--dont-use '*_x8'` | Exclude a cell name or quoted glob; repeat as needed. |

Higher effort can take substantially longer. Compare netlist quality together with end-to-end time and peak memory. The report records the requested and effective priority-cut counts and the mapping cut-storage budget; large designs may use fewer cuts to stay within that budget.

Inspect cell use and unused-library reasons:

```sh
rustsys show .work/chip.json --section cells --sort area_total --reverse --limit 20
rustsys show .work/chip.json --section unused
rustsys show .work/chip.json --section phases
```

## Keep arbitrary initial states

By default, state starts arbitrarily and state elements are preserved. Do not add `--after-reset` or `--uninitialized-dont-care` to an arbitrary-initial-state comparison. These flags deliberately change the contract: the former requires a reset sequence, and the latter permits choices for otherwise unspecified initial state. See [equivalence scopes](checking.md#state-and-language-scope).

Likewise, `--assume-full-case` and `--assume-parallel-case` change the semantics of annotated case statements. The default retains ordinary priority behavior, even when an annotation suggests a synthesis assumption. Every enabled assumption belongs in the report and proof record.

## Other input formats

The positional source extension selects BLIF (`.blif`), AIGER (`.aag`, `.aig`) or RTL. A run uses one format; AIGER takes a single file. A structural netlist is explicitly selected with `--gate-netlist` and its cell library with `--gate-lib`.

```sh
rustsys synth --lib cells.lib --out .work/adder.v adder.aig
rustsys synth --lib new_cells.lib --gate-netlist mapped.v \
  --gate-lib old_cells.lib --out .work/remapped.v
```

BLIF and AIGER can declare initial values. Their default `--initial-values reset` represents those values through a generated asynchronous initialization port. Review that interface rather than treating it as an implicit reset assumption. `--initial-values arbitrary` omits implementation of declared initial values and emits a warning. See the complete [input-format contract](../specs/synth/inputs.md).

## Use the output

Keep the mapped Verilog, exact Liberty files and check report together. The emitted cell instances need the matching simulation models for gate-level simulation. Placement, routing, clock-tree synthesis, parasitic extraction, STA and DRC are downstream tasks; a successful synthesis command does not establish those results.

Add [load-aware timing refinement](gpu-and-timing.md) when appropriate, then [check equivalence](checking.md) for the final exported file.
