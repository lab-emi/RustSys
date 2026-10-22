# Automate with CLI tools and agents

RustSys fits into a process runner: provide files and flags, collect structured output, inspect an exit code, and keep the resulting artifacts. It needs no resident service or interactive session. Choose direct files for a single job, or a disk workspace for shared revision-aware work.

## Two interfaces, one synthesis engine

| Workflow | Commands | Machine output | State |
|---|---|---|---|
| Direct files | `rustsys synth`, `rustsys check synth`, `rustsys show` | `--print json` | Explicit file paths; optional recorded runs. |
| Disk workspace | `rusteda init/import/show/diff/export`, `rustsys -w DIR synth/check/report` | `--format json` | Immutable revisions, cached libraries and table dependencies. |

Do not interchange these JSON switches. `--print json` returns a product report. `--format json` in workspace mode returns a `rusteda.v1` envelope with revision, scope, diagnostics and next actions.

## Direct jobs: parse results, preserve evidence

Use a new run directory for each job. This example starts from the repository root:

```sh
FIX=verification/fixtures/synthetic/synth
mkdir -p .work

rustsys --threads 1 --color never --progress off synth \
  --top counter --lib "$FIX/lib/wt_rich.lib" \
  --run-dir .work/agent_synth --print json \
  "$FIX/rtl/counter/counter.sv" > .work/agent_synth_result.json

rustsys --threads 1 --color never --progress off check synth \
  --from-run .work/agent_synth --run-dir .work/agent_check \
  --print json > .work/agent_check_result.json
```

Create `.work/` before redirecting output. The process writes diagnostics to stderr and JSON to stdout. Read both the exit status and the verdict; exit 0 also covers the explicitly weaker `initial-state-refinement` verdict.

A useful acceptance predicate for an arbitrary-initial-state native check is:

```python
import json

with open(".work/agent_check_result.json") as stream:
    report = json.load(stream)

assert report["format"] == "rustsys-check-synth-report"
assert report["format_version"] == 2
assert report["verdict"] == "equivalent"
assert report["after_reset"] == []
assert report["uninitialized_dont_care"] is False
assert report["assume_full_case"] is False
assert report["assume_parallel_case"] is False
```

The caller must also require successful process completion, bind the report to the exact source/library/netlist bytes, and retain the [two-valued language scope](checking.md#state-and-language-scope). A stale report file is not evidence for a new run.

For exploration, `--record` creates unique runs under an explicit root:

```sh
rustsys --threads 1 --runs-root .work/runs synth --top counter \
  --lib "$FIX/lib/wt_rich.lib" --record "$FIX/rtl/counter/counter.sv"
rustsys --threads 1 --runs-root .work/runs check synth --from-run @synth --record
rustsys --runs-root .work/runs show @check-synth --section summary --print json
```

References such as `@synth` mean the latest matching run. Pin an exact run directory for concurrent automation, where another process may create a newer run.

## Workspaces for concurrent agents

Install both binaries using the [installation guide](installation.md). This complete flow commits imported inputs as `rev1`, synthesizes and checks `rev2`, then exports the immutable result:

```sh
FIX=verification/fixtures/synthetic/synth
WS=.work/counter_workspace

rusteda -w "$WS" --format json init
rusteda -w "$WS" --format json import --base-rev rev0 \
  --lib "$FIX/lib/wt_rich.lib" \
  --rtl "$FIX/rtl/counter/counter.sv" --top counter

rustsys -w "$WS" --format json --threads 1 synth --base-rev rev1 --check
rusteda -w "$WS" --format json show --rev rev2
rustsys -w "$WS" --format json report --rev rev2 --section cells
rusteda -w "$WS" export --rev rev2 --netlist .work/counter_workspace.v
```

`rusteda` manages shared storage; `rustsys` runs synthesis and its checker. Read operations use shared locks, and commits use an exclusive lock on `workspace.lock`. Mutations of an existing workspace require a concrete `--base-rev`; `HEAD` is a query reference and is rejected as a mutation base. Initialization creates `rev0` in a new workspace.

If another agent commits first, the command returns exit **4** with `REVISION_CONFLICT`. Reload the current revision, inspect the changes, and decide whether to retry against that new base. Do not silently substitute `HEAD` or overwrite revision files.

The JSON envelope includes `schema`, `ok`, `command`, `workspace`, `base_rev`, `rev`, `new_rev`, `scope`, `completeness`, `metrics`, `diagnostics`, `error`, and `next_actions`. A completed negative or undecided check can have **`ok: true`**: this means the operation completed and committed its result. Require the expected verdict as well as `ok`, and handle the process exit code. See [reports](../reference/reports.md#workspace-envelopes).

Inspect changes with:

```sh
rusteda -w "$WS" --format json diff --from rev1 --to rev2
rusteda -w "$WS" --format json log --limit 5
```

Revision handles and object handles use `[A-Za-z0-9_-]+`. A handle belongs to its revision; refresh handles after changing revisions.

## Reproducibility and bounded execution

Use explicit input lists, top modules, parameters, libraries, goals, effort and worker counts. Preserve the executable version and input hashes. Recorded canonical commands spell out result-affecting defaults; their execution directory is recorded too. Identical inputs produce deterministic netlists across supported thread counts and mapping backends. Timings, run IDs and timestamps vary.

Give each child process a wall-clock limit and an OS memory limit. Keep the proof conflict budgets separate from the wall-clock budget. Exceeding either is incomplete verification, never success. See [resource limits](troubleshooting.md#resource-limits).

For a compact result, query an existing report rather than rerun synthesis:

```sh
rustsys show .work/agent_synth --section cells --filter 'count>10' \
  --sort area_total --reverse --limit 20 --print json
```

There is no `rustsys runs` subcommand. Run records are inspected with `rustsys show`, and workspace revisions with `rusteda show` or `rustsys report`.
