# Reports and machine contracts

RustSys exposes product reports for direct jobs and a workspace envelope for revision-aware commands. They are different JSON contracts. Read the format/schema before interpreting fields, and keep the exit status and exact input artifacts alongside the JSON.

## Synthesis report

`rustsys synth --report FILE` writes a JSON report. In direct file mode, `--print json` also emits it on stdout. The counter quickstart includes these values; this is a selected-field excerpt, not the whole object:

```json
{
  "format": "rustsys-synth-report",
  "format_version": 1,
  "top": "counter",
  "area": 115.0,
  "sequential_cells": 8,
  "combinational_cells": 37,
  "logic_depth": 4,
  "estimated_delay": 0.3,
  "state_bits_rtl": 8,
  "state_bits_retained": 8,
  "after_reset": [],
  "uninitialized_dont_care": false,
  "assume_full_case": false,
  "assume_parallel_case": false,
  "map_backend": "cpu",
  "time_unit_ns": 1.0
}
```

| Field | Interpretation |
|---|---|
| `cells` | Map from cell name to instance count; sum counts for total cells. |
| `cell_area`, `area` | Library cell areas and total area in library area units. |
| `logic_depth`, `estimated_delay` | Mapping quality estimates; delay uses the library time unit reported by `time_unit_ns`. |
| `aig_nodes_before`, `aig_nodes_after` | Logic size before/after optimization. |
| `map_requested_cuts`, `map_effective_cuts` | Requested search breadth and the breadth actually used under the memory budget. |
| `map_cut_memory_budget_bytes` | Cut-storage budget; not measured process RSS. |
| `runtime_s`, `phase_s`, `map_phase_s` | Engine/phase timing; not a substitute for externally measured end-to-end wall time. |
| `unused_cells` | Cells that cannot be used and the corresponding reasons. |
| `electrical` | Optional load-aware refinement results and unresolved targets. |

Timing estimates do not include routed wires or clock-tree effects. Without a separate analysis, timing signoff and DRC remain unchecked. No missing result should be translated into zero violations.

## Check report

The native report format is `rustsys-check-synth-report`, version **2**. Selected fields from the quickstart:

```json
{
  "format": "rustsys-check-synth-report",
  "format_version": 2,
  "verdict": "equivalent",
  "outputs_checked": 9,
  "state_pairs_checked": 8,
  "rtl_state_bits": 8,
  "netlist_state_elements": 8,
  "unmatched_rtl_state_bits": [],
  "after_reset": [],
  "uninitialized_dont_care": false,
  "assume_full_case": false,
  "assume_parallel_case": false,
  "mismatches": [],
  "errors": [],
  "internal_error": false,
  "undecided": []
}
```

`mismatches` holds differences and counterexample information. `errors` identifies rejected models or failures. `undecided` records exhausted proof/search budgets. `supply_ports`, discovered state relations, constant-state checks and uninitialized choices make scope changes inspectable. `sweep` describes an optimization of the proof flow, and `seq_sim` records optional simulation, including a skip reason where applicable.

Use the verdict directly. Do not infer equivalence from an empty mismatch list, zero simulation failures, a successful model export, or the number of obligations attempted. See [proof scope](../guide/checking.md).

## Workspace envelopes

`rusteda --format json` and `rustsys -w DIR --format json` print one `rusteda.v1` object on stdout. Standard keys are always present; tool-specific keys are added by the command.

| Key | Meaning |
|---|---|
| `schema` | Envelope contract, currently `rusteda.v1`. |
| `ok` | Whether the operation completed, rather than whether every analyzed property passed. |
| `command`, `workspace` | Invoked operation and selected workspace. |
| `base_rev`, `rev`, `new_rev` | Revision context; absent values are `null`. |
| `scope` | Analysis scope, such as top, corners and equivalence status. |
| `completeness` | Explicit completeness metadata when supplied. |
| `metrics` | Command summary metrics. |
| `diagnostics` | Structured severity, code, location, message and help. |
| `error` | Structured operation failure, or `null`. |
| `next_actions` | Suggested canonical POSIX-shell command lines. |

Workspace synthesis can embed its product report as `report` and, with `--check`, its check report as `check`. A successful counter example has `new_rev: "rev2"`, `scope.equivalence: "equivalent"` and `check.verdict: "equivalent"`. The scope's refinement spelling uses `initial_state_refinement`, whereas the product verdict is `initial-state-refinement`; parse the correct field for the selected contract.

A completed negative/undecided check can commit evidence and set `ok: true` while returning exit **3**/**5**. Gate an acceptance decision on operation status, the expected verdict, and its scope. `timing: "not_analyzed"` and `equivalence: "unchecked"` are explicit missing analyses, not passing results.

The [workspace specification](../specs/workspace/README.md#envelope-and-text-mode) defines all envelope keys, diagnostic codes and commit behavior.

## Run records and event streams

`run.json` uses `format: "rustsys-run"`, `format_version: 1`. It records the command, canonical command, working directory, program version, compiled runtimes, input file roles/paths/sizes/hashes, output paths, timings and terminal status. `events.jsonl` stores one event per line with elapsed time.

The recorded input hashes are 64-bit FNV-1a values; workspace tables use `xxh3` hashes. Neither is a cryptographic authenticity proof. Benchmark evidence additionally pins artifacts with SHA-256 inventories. Retain exact bytes and stronger hashes when your audit requirements call for them.

Run IDs and timestamps vary. Netlist bytes are deterministic for identical inputs/options on supported backends; runtime fields are not. Report readers reject newer unsupported format versions rather than treating them as compatible. For the complete contract, see the [job protocol](../specs/cli/protocol.md).
