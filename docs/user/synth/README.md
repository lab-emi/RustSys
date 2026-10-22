# RustSys user manual

RustSys turns synthesizable SystemVerilog RTL into a flat gate-level netlist of standard cells
from one or more Liberty libraries. The synthesis checker (`rustsys check synth`) proves that a
netlist implements its RTL.

## Synthesize

```bash
rustsys synth --top counter --lib cells.lib --out counter_mapped.v --report counter.json counter.sv
```

| Option | Meaning |
|---|---|
| `--top NAME` | top module; optional when exactly one module is not instantiated by another |
| `-D NAME[=VALUE]` | define a preprocessor macro (`SYNTHESIS` is predefined) |
| `-I DIR` | include directory |
| `-P NAME=EXPR` | override a top-level parameter (`-P W=16`, `-P "Seed=16'hACE1"`) |
| `--lib FILE` | Liberty library; repeat for several |
| `--dont-use GLOB` | never use matching cells (`--dont-use '*_x8'`) |
| `--effort low\|medium\|high` | optimization and mapping effort (default `medium`); `high` adds SAT-based node merging, zero-gain rewriting and a larger cut search, and runs several times longer |
| `--goal delay\|area` | mapping objective |
| `--delay-relax R` | allowed delay increase during area recovery (default 0.1) |
| `--backend cpu\|gpu\|cuda` | where the mapper's cut passes run (default `cpu`); the netlist is the same for every backend |
| `--out FILE` | output netlist |
| `--report FILE` | JSON report: cell counts, area, depth, estimated delay, runtime |

Exit status: 0 on success, 1 when the input is rejected (diagnostics on standard error), 2 on an
internal failure.

The global option `--threads N`, placed before the command (`rustsys --threads 8 synth ...`), sets
the number of worker threads of the parallel phases; the default uses one per available CPU. The
netlist does not depend on it.

### GPU mapping

With `--backend gpu` or `cuda`, the delay and area-flow passes of the mapper
run on a GPU; the exact area-recovery passes stay on the CPU. `gpu` takes the first runtime of the
build that passes a device self-test; `cuda` runs kernels written in CUDA C on NVIDIA GPUs. There is
no Metal runtime for Apple GPUs yet (planned for Phase 4). The CUDA runtime needs
a CUDA toolkit (`CUDA_PATH` set to its root, its libraries including NVRTC visible to the dynamic
loader). The device is prepared while the design is read and optimized. The report's `map_backend`
names the runtime that ran and `map_phase_s` the time of each mapping step. A missing or failing
runtime is an error (`S0530`), never a silent fallback.

## Check

```bash
rustsys check synth --top counter --lib cells.lib --netlist counter_mapped.v counter.sv
```

The checker accepts the same front-end options as `rustsys synth`.

| Option | Meaning |
|---|---|
| `--netlist FILE` | netlist to check; ports may be declared in the module header or in the body |
| `--seed N` | seed of the random simulation (reported) |
| `--cycles N` | also simulate both designs for N clock cycles from a common random state (single clock domain, no latches; otherwise skipped with a reason in the report) |
| `--budget N` | SAT conflict budget of each proof obligation (default 2000000); a proof that reaches it leaves the check `undecided` |
| `--model-budget N` | SAT conflict budget of each proof that the checker's own RTL model needs (default 200000), for example whether a combinational bit is written on every path |
| `--sweep-max-candidates N` | optional equivalence-sweeping candidates (default 32768; 0 uses structural hashing only); final proofs are still required |
| `--verbose` | phase times on standard error |
| `--report FILE` | JSON report with the verdict, mismatches and statistics |

Its verdict is one of:

| Verdict | Exit status | Meaning |
|---|---|---|
| `equivalent` | 0 | every output and state obligation is proved |
| `initial-state-refinement` | 0 | proved for the reported choices of unspecified initial state (`--uninitialized-dont-care`) |
| `not-equivalent` | 3 | a difference, with counterexamples in the JSON report |
| `undecided` | 5 | no difference found, but a proof or search stopped at its budget; a `warning[C0005]` names it, and `--budget` or `--model-budget` raises the conflict budgets |
| `error` | 1 | the input is erroneous or outside what the checker models (2 when the checker itself failed: `internal_error` in the report) |

A counterexample decides even when another proof ran out of its budget, and a port difference
even when the search for state correspondences stopped at its limit: the verdict is
`not-equivalent` (exit status 3) and a `warning[C0005]` still names the budget. A netlist that
the checker rejects is `error` (exit status 1) even when a proof of its RTL model ran out of its
budget.

Netlists with constructs the checker does not model are rejected, not checked in part: high
impedance and drive strengths, that is cells with three-state outputs or a `driver_type`
(open-drain, open-source, pull-up, pull-down, bus-hold), `highz0`/`highz1` drive strengths and
primitives such as `bufif1` or `pullup` (`C0101`); dual-edge or master-slave flip-flops and latch
pairs (`clocked_on_also`, `enable_also`, `C0102`); `x` or `z` literals, on power and ground pins
too (`C0103`); cells described by a state table, with bus or bundle pins, several sequential
groups or banked ones, with `x_function`, internal nodes or retention, with sequential group
attributes the checker does not read, or clock-gating cells without their enable latch; cell
functions that read internal pins; connections to bidirectional or internal pins (`C0104`); and
nets that resolve several drivers, pull or store charge (`wand`, `wor`, `tri0`, `tri1`, `trireg`
and the like), logic ports declared as supply nets and driven supply nets (`C0105`). An undriven
`supply0` or `supply1` net is a constant. A cell that two libraries define differently, or that
its library describes with errors, is rejected (`C0003`). The checker reads the Liberty
attributes that the library reader does not keep from the library text, so none is skipped.
Power and ground connections are not evaluated: every cell is taken to be powered.

## BLIF, AIGER and gate-level netlists

Besides SystemVerilog, `rustsys synth` and `rustsys check synth` take logic networks in BLIF
(`.blif`), and-inverter graphs in AIGER (`.aag`, `.aig`), and structural netlists of the cells of
another library (`docs/specs/synth/inputs.md`). The format of the positional sources is chosen by
their extension; one run reads one format, and one AIGER file.

```bash
rustsys synth --lib cells.lib --out adder_mapped.v adder.aig
rustsys check synth --lib cells.lib --netlist adder_mapped.v adder.aig
rustsys synth --lib cells.lib --gate-lib mapped.lib --out top.v design.blif
rustsys synth --lib cells.lib --gate-netlist golden.v --gate-lib other.lib --out resynth.v
rustsys check synth --lib cells.lib --netlist resynth.v --gate-netlist golden.v --gate-lib other.lib
```

| Option | Meaning |
|---|---|
| `--top NAME` | BLIF: the top model (default the first model of the first file); AIGER: the module name (default the file name without extension); gate-level netlist: must name its module |
| `--gate-netlist FILE` | a structural netlist of `--gate-lib` cells as the source (`synth`) or the reference (`check synth`); its logic is taken from the Liberty cell functions |
| `--gate-lib FILE` | Liberty library of the cells of `--gate-netlist` and of BLIF `.gate` and `.mlatch` (repeatable) |
| `--initial-values reset\|arbitrary` | BLIF and AIGER: `reset` (default) implements declared initial values (BLIF `.latch` init 0 or 1, AIGER reset 0 or 1) with an asynchronous control from a generated `init` input port; `arbitrary` leaves every state uninitialized and warns (`S0913`). `check synth` must be given the same mode |

Ports of BLIF and AIGER sources are scalar and named after their signals or symbols (AIGER
default names `i<k>`, `o<k>`, latches `l<k>`). BLIF latches without a clock and AIGER latches use
a generated input port `clk`; BLIF `init` 2 and 3 and uninitialized AIGER latches have no
initial value. `-D`, `-I`, `-P`, `--no-predefined-macros` and the case assumptions apply to
SystemVerilog only. BLIF constructs that would change the function if ignored (`.search`, state
transition tables, black boxes, latch type `as`, unknown commands) are errors; timing and
annotation constructs and `.exdc` are skipped with a warning. The checker reads these sources
with readers of its own; with `--from-run` it takes them, and the mode, from the synthesis run.

## What the netlist looks like

- One module named as the top module, with the RTL ports in RTL order; multi-bit ports are
  declared `[W-1:0]`, bit `k` being bit `k` of the RTL value.
- Every register or latch bit is visible as a wire named after the RTL variable, for example
  `\u_fifo.mem_q[3][7] ` (instance path, unpacked indices, bit). A one-bit output port that is a
  register is its own state wire.
- Constants come from tie cells when the library has them.

## Language support

Supported: modules, packages, parameters (value and type), `typedef`, enumerations, packed
structures and unions, packed and unpacked arrays, `always_comb`, `always_ff`, `always_latch`,
`always @*`/`@(...)`, continuous assignments, gate primitives, generate loops, conditionals and
case, functions and tasks (including `return`, `break`, `continue` and output arguments),
`for`/`while`/`do`/`repeat`/`foreach` loops with constant bounds, `case`/`casez`/`casex`/
`case inside` with `unique`/`priority`, assignment patterns, casts, and the system functions
`$clog2`, `$bits`, `$signed`, `$unsigned`, `$size`, `$left`, `$right`, `$high`, `$low`,
`$countones`, `$onehot`, `$onehot0`. Assertions, `initial` blocks and simulation tasks are ignored.

Not supported in this version: interfaces and modports, arrays of instances, `defparam`,
tri-state logic (`z` is treated as 0), asynchronous load, registers with two asynchronous controls,
real numbers and dynamic data types.

Semantics that differ from a four-valued simulator: `x` and `z` are 0; uninitialized registers
have no initial value; variables declared inside procedural blocks are automatic.

## Libraries

RustSys uses single-output combinational cells with up to six inputs, D flip-flops and latches
with optional asynchronous clear or preset, and tie cells. Scan, enable, multi-output,
three-state, clock-gating and `dont_use` cells are not used. Missing negative-edge or active-high
variants are built with inverters; a missing preset is built by storing the inverted value.

## Runs and reports

`--record`, `--run-dir`, `--label`, `--print` and `--from-run`, run references such as `@synth`,
`rustsys show` for the sections of a report, and the exit statuses common to every command are
described in `docs/user/cli/README.md`.

## Electrical constraints

For an ideal-clock target, add `--timing-period NS`. Times are in ns and `--output-load` is in
pF regardless of Liberty units. For example:

```sh
rustsys synth --lib cells.lib --goal area --timing-period 10 --area-limit 100000 \
  --input-delay 2 --output-delay 2 --input-slew 0.1 --clock-slew 0.1 \
  --output-load 0.01 --timing-margin 0.05 --buffer-fanout 16 --out mapped.v top.sv
```

The target enables sparse arithmetic carry structures, cover compaction, critical-load
buffering and cell sizing. `--goal` still controls the initial Boolean cover. An area limit
reserves room for sizing after buffering; it cannot guarantee that the initial design fits.
The JSON `electrical` object reports initial, compacted and final area, transformation counts,
estimated worst setup slack and explicit area/timing feasibility flags. An infeasible target
still produces a netlist and reports the unmet target in JSON, the displayed summary and
diagnostics S0532/S0533. The older `estimated_delay` is the Boolean cover estimate before
electrical refinement; `logic_depth` and the `electrical` statistics describe the final netlist.

The estimate uses Liberty NLDM, actual connected pin loads, uniform input/output budgets and
ideal clocks. Check the emitted netlist with a static timing analysis under the intended timing
constraints. Hold, clock phases, latch borrowing, exceptions and interconnect parasitics require
that separate analysis. Without `--timing-period`, the existing synthesis flow is retained. The
same constraints are available in the typed job's optional `electrical` object.
