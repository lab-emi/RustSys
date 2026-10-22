# rustsys_cli::proto: the job protocol of the command line

`rustsys_cli::proto` (`crates/apps/rustsys-cli/src/proto/`, ADR 0003) is the job protocol between
the `rustsys` command and the engines: typed jobs, their canonical command lines, their execution
with events, run records, report loading, text rendering, tables, filters and sort orders. It
contains no user interface toolkit and starts no long-lived thread. The synthesis engine is reached
only through its `view` module (PROTO-REQ-0002).

| Document | Requirements |
|---|---|
| this file | PROTO-REQ-0001 to 0099: jobs, canonical commands, execution, run records |
| [tables.md](tables.md) | PROTO-REQ-0100 to 0199: numbers, fields, text, tables, filters |

## Jobs and commands

**PROTO-REQ-0001** A `Job` is one of `Synth` and `CheckSynth`, holding every option that can
change a result, with paths as given. Front ends build jobs; only `execute` runs them.

**PROTO-REQ-0002** Source files of this crate name synthesis engine items only through
`rustsys::view` (tested by scanning the sources).

**PROTO-REQ-0010** The help text of every job option comes from one table (`options`), used by the
command line's help.

**PROTO-REQ-0020** The canonical command of a job is rendered from the job, never from the
original arguments: `rustsys <product> …` with every result-affecting option spelled out, defaults
included (`--effort`, `--goal`, `--delay-relax`, `--backend`, `--seed`, `--cycles`, `--budget`,
`--model-budget`), `--threads N` when N is not 0, outputs, and nothing about presentation.
Arguments are quoted for a POSIX shell so that `split(quote(x)) == x` for every string `x`. A job
written before an option existed has that option's default.

**PROTO-REQ-0021** Executed in the recorded working directory with unchanged inputs, the canonical
command reproduces the run's outputs, except run times. A `--record` run is reproduced in a new run
directory; a `--run-dir DIR` run once DIR no longer holds the run, since a run directory holds one
run (PROTO-REQ-0050). An option value that starts with `-` is written as one word (`--label=-x`,
`-D-X`) and sources that start with `-` follow `--`.

## Execution

**PROTO-REQ-0030** `execute(job, placement, ctx)` reads the inputs, runs the engine with the
context, writes the outputs atomically (PROTO-REQ-0033) and returns the outcome: exit class, report
and text result. Messages go to the observer as `Diagnostic` events in the order in which they
arise; nothing is printed.

**PROTO-REQ-0031** Exit classes and statuses: `Ok` 0, `Rejected` 1 (input rejected or an output
cannot be written), `Internal` 2 (a panic, or verdict `error` with `internal_error`), `Negative` 3
(verdict `not-equivalent`), `Undecided` 5 (verdict `undecided`), `Cancelled` 130. The outcome
holds the report and the text result for `Ok`, `Negative` and `Undecided`.

**PROTO-REQ-0032** The order of effects of a run is: run record with status `running` (when there is
a run directory), the engine, the output files, the final run record. On any error before the
outputs are complete, no output file is left half written.

**PROTO-REQ-0033** Files are written to a temporary file in the target directory and renamed into
place; parent directories are created. JSON is streamed; its bytes equal those of
`serde_json::to_string_pretty` of the same value.

## Run records

**PROTO-REQ-0050** A run directory holds `run.json`, the product's outputs that were not named
explicitly (`netlist.v` and `report.json` for synthesis, `check.json` for the check) and
`events.jsonl`. `--run-dir DIR` uses DIR, which must not already hold a `run.json` (`A0007`).
`--record` allocates `<runs root>/<run id>/` (`A0008` without a runs root).

**PROTO-REQ-0051** Run ids are `<yyyymmdd>T<hhmmss>Z-<product>-<4 hex digits>` in UTC, products
`synth` and `check-synth`; the directory is created atomically and the suffix is incremented on
collision.

**PROTO-REQ-0052** `run.json` holds: `format` `rustsys-run`, `format_version` 1, id, product, label,
status (`running`, `ok`, `rejected`, `internal`, `negative`, `undecided`, `cancelled`), exit
status, the original arguments, the canonical command, the absolute working directory, the job,
program version, compiled runtimes, threads, start and finish times (UTC), load, engine, write and
total seconds, inputs (role, path, bytes, 64-bit FNV-1a hash), outputs (role, path), a summary of
key figures and message counts per severity.

**PROTO-REQ-0053** `events.jsonl` holds one JSON object per event of the run, in order, with the
seconds since the start of the run.

**PROTO-REQ-0060** Run references: `@` (latest run under the runs root, by start time), `@~N`
(N-th before the latest), `@<product>` and `@<product>~N` (the same among runs of one product), an
id prefix of at least 4 characters (unique under the runs root), a path to a run directory, or a
path to a report file (a run without record: report views only). `@` forms and prefixes need a runs
root (`A0008`); an unknown or ambiguous reference is `A0006` and lists up to 5 candidates.

**PROTO-REQ-0061** `--from-run <RUN>` completes a job from a finished run: `check synth` takes
sources, front-end options, libraries, top and netlist of a synthesis run. Options given explicitly
win. A run of another product or without the needed output is `A0009`. Recorded relative paths are
resolved against the run's working directory.

**PROTO-REQ-0062** Loading reads a run directory or a report file. The kind of a report is its
`format` field, or, for reports written before that field existed, the presence of
`aig_nodes_before` (synthesis) or `outputs_checked` (check synth). Anything else is `A0020`; a
`format_version` newer than this program reads for the kind (synthesis 1, check synth 2) is
`A0021`.
