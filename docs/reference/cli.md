# CLI reference

Use `rustsys --help`, `rustsys <command> --help`, and `rusteda <command> --help` for the complete option list in the installed build. This page groups the stable interfaces by task.

## Commands

| Command | Purpose |
|---|---|
| `rustsys synth` | Read a design, optimize it, map standard cells, and emit a netlist/report. |
| `rustsys check synth` | Compare RTL or another supported source with a structural implementation. |
| `rustsys show [RUN]` | Query a recorded run or report file; default to the latest run under the runs root. |
| `rustsys -w DIR report` | Query `synth.json` or `check.json` in a workspace revision. |
| `rustsys -w DIR opt-timing` | Apply bounded incremental timing/load optimization to a revision. |
| `rustsys -w DIR resize` | Change drive variants of selected instances. |
| `rustsys completions SHELL` | Generate bash, zsh, fish, elvish or PowerShell completion. |
| `rusteda init/import/log/show/diff/revert/export` | Manage shared workspace inputs, immutable revisions and exports. |

## Global options

| Option | Meaning |
|---|---|
| `--threads N` | Worker count for parallel phases; 0 uses the available CPU count. |
| `--color auto\|always\|never` | Control diagnostic colors; `NO_COLOR` is honored. |
| `--progress auto\|live\|log\|off` | Control RustSys progress on stderr. |
| `--quiet`, `--verbose` | Reduce notes or add detail. |
| `--runs-root DIR` | Resolve `--record` and `@` run references; also configurable with `RUSTSYS_RUNS_ROOT`. |
| `-w DIR`, `--workspace DIR` | Select workspace mode for revision-aware commands. |
| `--format text\|json` | Workspace output; JSON is one `rusteda.v1` envelope. |

`rusteda` has its own progress values (`auto`, `always`, `never`). Inspect its help rather than forwarding RustSys-specific presentation flags blindly.

## Synthesis and source configuration

| Option | Meaning |
|---|---|
| `--top NAME` | Top module; inferred only when unambiguous. |
| `-I DIR`, `-D NAME[=VALUE]`, `-P NAME=EXPR` | Include paths, macros and top-level parameter overrides; repeatable. |
| `--no-predefined-macros` | Disable the predefined `SYNTHESIS` macro. |
| `--lib FILE` | Target Liberty library; repeatable. |
| `--dont-use GLOB` | Exclude cells by name/glob; quote shell wildcard characters. |
| `--goal delay\|area` | Mapping objective; default `delay`. |
| `--effort low\|medium\|high` | Search effort; default `medium`. |
| `--delay-relax R` | Allowed relative delay growth during area recovery; default 0.1. |
| `--backend cpu\|gpu\|cuda` | Mapping backend; default `cpu`. |
| `--out FILE`, `--report FILE` | Netlist and JSON report paths. |
| `--timing-period NS` | Enable load-aware ideal-clock refinement. |
| `--area-limit AREA`, `--electrical-corner FILE` | Optional area target and additional same-cell electrical corners. |

The [GPU and timing guide](../guide/gpu-and-timing.md) covers arrival, slew, load, margin and fanout settings. The [language reference](language-support.md) describes the source contract and semantic options. `--gate-netlist`, `--gate-lib`, and `--initial-values` are covered by the [input-format specification](../specs/synth/inputs.md).

## Checking

| Option | Meaning |
|---|---|
| `--netlist FILE` | Structural Verilog implementation; repeatable. |
| `--from-run RUN` | Complete the configuration from a finished synthesis run. |
| `--budget N` | SAT conflicts per final obligation; default 2,000,000. |
| `--model-budget N` | SAT conflicts per RTL-model proof; default 200,000. |
| `--sweep-max-candidates N` | Optional sweeping candidates; default 32,768; 0 retains only structural hashing before final proofs. |
| `--seed N`, `--cycles N` | Seeded simulation; cycles default to 0 (off). |
| `--supply-port NAME` | Explicit supply-port exclusion; repeatable and reported. |

Front-end and semantic options must match synthesis. See [checking](../guide/checking.md) for verdict interpretation, arbitrary initial states and the two-valued model.

## Recording and querying

`--run-dir DIR` records one run in an unused directory; `--record` allocates a fresh directory under the runs root. `--label TEXT` attaches a note. `--print text|json|none` controls stdout for direct synthesis/checking. A recorded run retains `run.json`, product reports, outputs and `events.jsonl`.

For `show`, use repeatable `--section`, `--limit N` (0 means all), `--sort KEY`, `--reverse`, `--filter EXPR`, `--exact`, and `--sources`. `--print text|json|tsv` selects its output; TSV requires a single table section.

| Source | Sections |
|---|---|
| Synthesis report/run | `summary`, `cells`, `unused`, `phases`, `command` |
| Check report/run | `summary`, `mismatches`, `command` |

`command` requires a recorded run rather than a bare product report. Use `@`, `@synth`, `@check-synth`, `@~1` or explicit directories. See the [complete run protocol](../specs/cli/protocol.md) and [table/filter specification](../specs/cli/tables.md).

## Workspace mutations

Mutations of existing workspaces require `--base-rev revN`; initialization creates `rev0`. `rustsys -w DIR synth --check` performs synthesis and checking in the same new revision. `opt-timing` and `resize` also accept `--request-id ID` for idempotent requests. Every target handle belongs to a specific revision.

Queries accept `--rev HEAD` or a concrete revision. Mutation bases must name a concrete revision; a stale base returns a conflict. See the [agent walkthrough](../guide/agents.md#workspaces-for-concurrent-agents).

## Exit codes

| Exit | Meaning |
|---:|---|
| 0 | Completed successfully; inspect the verdict and scope for check/refinement commands. |
| 1 | Usage, input or workspace rejection. |
| 2 | Internal failure or corrupt workspace data. |
| 3 | Check found inequivalence. |
| 4 | Workspace `REVISION_CONFLICT`. |
| 5 | Check undecided after exhausting a proof/search budget. |
| 130 | Interrupted/cancelled. |

An external process guard may return a different status, such as 124 for a wall-clock timeout. That is not a completed RustSys result. Workspace envelopes may have `ok: true` for a completed check with exit 3 or 5; evaluate the verdict separately.
