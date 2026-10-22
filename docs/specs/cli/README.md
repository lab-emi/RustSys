# The `rustsys` command

`rustsys` (`crates/apps/rustsys-cli`) is the one command of RustSys. Synthesis and the synthesis check are
subcommands; their options are specified with each (SYNTH-REQ-0701, CHECK-REQ-0001). This
document specifies what all subcommands share, and the application commands `show` and
`completions`.

```text
rustsys [GLOBAL] synth       [OPTIONS] (--lib <LIB>... (<SOURCE>... | --gate-netlist <V> --gate-lib <LIB>...) | -w <WS> --base-rev <REV> [--check])
rustsys [GLOBAL] check synth [OPTIONS] (--lib <LIB>... --netlist <V>... (<SOURCE>... | --gate-netlist <V> --gate-lib <LIB>...) | --from-run <RUN> | -w <WS> [--base-rev <REV>])
rustsys [GLOBAL] show        [OPTIONS] [RUN]
rustsys [GLOBAL] report      -w <WS> [--rev <REV>] [--table <synth|check>] [--section <NAME>...]
rustsys [GLOBAL] resize      -w <WS> --base-rev <REV> --target <HANDLE>... [--cell <CELL> | --drive-multiplier <K>] [--request-id <ID>]
rustsys [GLOBAL] opt-timing  -w <WS> --base-rev <REV> [--target <HANDLE>...] [--max-iters <N>] [--stop-when <COND>]
                             [--effort <low|medium|high>] [--area-limit <A>] [--max-area-growth-pct <P>]
                             [--timing-period <NS>] [--input-delay <NS>] [--output-delay <NS>] [--input-slew <NS>]
                             [--clock-slew <NS>] [--output-load <PF>] [--timing-margin <NS>] [--buffer-fanout <N>]
                             [--request-id <ID>]
rustsys [GLOBAL] completions [SHELL]
```

## Global options

**CLI-REQ-0001** Global options are accepted before or after the subcommand: `-w, --workspace <DIR>`
(selects workspace-first mode), `--format <text|json>` (workspace output format, default `text`),
`--threads <N>`
(worker threads, 0 = one per available CPU), `-q, --quiet` (standard error keeps only errors and
warnings), `-v, --verbose` (adds verbose notes and implies `--progress log` unless `--progress` is
given), `--color <auto|always|never>` (default `auto`: colour when standard error is a terminal,
`NO_COLOR` is unset or empty and `TERM` is not `dumb`), `--progress <auto|live|log|off>` (default
`auto`: `live` when standard error is a terminal and `TERM` is not `dumb`, else `off`), and
`--runs-root <DIR>` (default: the environment variable `RUSTSYS_RUNS_ROOT`, else none).
`-V, --version` prints the version and the compiled GPU runtimes.

## Streams and messages

**CLI-REQ-0002** Standard output carries results only: summaries, verdicts, `mismatch` lines,
`show` output and completion scripts. Standard error carries only coded messages, their `  help:`
lines and progress. Nothing is printed on standard output when a command fails before its results
are complete.

**CLI-REQ-0003** `--print <text|json|none>` on `synth` and `check synth` selects standard output:
the text result (default), the report JSON (byte-identical to the report file) or nothing.

**CLI-REQ-0004** Messages have the form `<severity>[<code>]: <location>: <text>` (SYNTH-REQ-0702),
optionally followed by one line `  help: <text>`. The code letter names the reporting command
(`S` synth, `C` check, `A` the application); codes of the format readers pass through. A
multi-line engine error is split into lines, each printed as a coded message: lines that already
are messages unchanged, other lines with the command's general code.

**CLI-REQ-0005** Invalid command lines are reported as `error[A0003]` with our own wording, a
`help:` line with the closest valid option when there is one, and a line naming the help command;
exit status 1. Help and version requests print on standard output with exit status 0.

**CLI-REQ-0006** Application message codes: `A0001` worker threads cannot start (exit 2), `A0002`
internal failure (a panic caught at the top level, or an internal failure of the checker,
CHECK-REQ-0001; exit 2), `A0003` invalid command line, `A0004`
an application file or standard output cannot be written, `A0005` a run or report cannot be read,
`A0006` unknown or ambiguous run reference, `A0007` run directory unusable, `A0008` no runs root,
`A0009` `--from-run` of the wrong product or without the needed output, `A0010` a view that the run
cannot provide, `A0020` not a report or run, `A0021` a newer report format, `A0022` a section, key
or filter that does not apply, `A0030` shell not detected, `A0110` note naming a recorded run,
`A0111` note that a request id was already committed (CLI-REQ-0212). All are exit status 1 unless
stated.

**CLI-REQ-0007** Colour: only the severity word on standard error is coloured (`error` red,
`warning` yellow, `note` cyan, bold); standard output is never coloured.

**CLI-REQ-0008** Progress in `live` mode is one line on standard error, drawn first after 300 ms,
redrawn at most every 100 ms, cleared before every message and at the end, cut to the terminal
width, ASCII only: `<product>  <phase>  [<step> i/n]  [<done>/<total> <p>%]  <m:ss.t>`. In `log`
mode each finished phase prints `progress: <elapsed> s <product> <phase> done in <seconds> s`.
`off` and `--quiet` print no progress. Progress is driven by engine events (JOB-REQ-0003) and
never by the engine writing to a stream.

**CLI-REQ-0009** Exit statuses, for every command: 0 success (verdict `equivalent` or
`initial-state-refinement`; `show`, `completions`, help, version), 1 input rejected, 2 internal
failure, 3 negative verdict (`not-equivalent`), 4 revision conflict (`--base-rev` is not workspace
`HEAD`), 5 inconclusive verdict (`undecided`: a proof or
search of the check stopped at its budget, CHECK-REQ-0002). With 3 and 5, standard output carries
the result as with 0. The command line passes no cancellation token and
installs no interrupt handler: an interrupt ends the process at once (a shell reports 130 for
SIGINT), and a recorded run keeps the status `running`, without outputs; its `events.jsonl` may
lack the latest events. The status 130 of a cancelled run (PROTO-REQ-0031) arises only for
library callers that pass a cancellation token to `execute`.

**CLI-REQ-0010** Output files and run records are written before standard output (PROTO-REQ-0032)
and atomically (PROTO-REQ-0033); a write failure is an input error (exit 1) with the product's
write code (`S0702`, `C0004`) or `A0004`.

**CLI-REQ-0011** A broken pipe on standard output ends output silently; the exit status is that of
the computation.

**CLI-REQ-0012** A panic anywhere in a command is caught and reported as `error[A0002]` with the
message and location, exit status 2.

## `rustsys show`

**CLI-REQ-0100** `rustsys show [RUN]` (default `@`) prints sections of a run or report:
`--section <NAME>` (repeatable), `--print <text|json|tsv>`, `--limit <N>` (rows per table, default
20, 0 = all), `--sort <KEY>` and `--reverse`, `--filter <EXPR>` (PROTO-REQ-0131), `--exact`
(shortest round-trip numbers) and `--sources` (the source of each summary value, PROTO-REQ-0110).
Sections: synthesis `summary` (default), `cells`, `unused`, `phases`, `command`; check synth
`summary` and `mismatches` (default). `command` needs a run record (`A0010`). An unknown section
or key is `A0022` and lists the valid names.

**CLI-REQ-0101** The default sections of a report print byte for byte what the command that
produced it printed on standard output with `--print text`.

**CLI-REQ-0102** `--print json` without `--section` prints the report file's JSON; with one table
section, an array of rows keyed by column key; `--print tsv` needs exactly one table section and
prints a header of column keys and exact numbers.

## `rustsys completions`

**CLI-REQ-0150** `rustsys completions [SHELL]` prints the completion script for `bash`, `elvish`,
`fish`, `powershell` or `zsh`; without an argument the shell is taken from the environment, and
`A0030` names the choices when that fails.

## Workspace-first mode (`-w, --workspace`)

**CLI-REQ-0200** Global `-w, --workspace <DIR>` and `--format <text|json>` select workspace-first
mode (WS-REQ-0090). In `--format json`, every command — including usage, missing-table,
unknown-handle, and revision-conflict failures — writes a single `rusteda.v1` JSON envelope on
standard output; in `--format text`, diagnostics go to standard error and summaries plus `next:`
action lines go to standard output.

**CLI-REQ-0201** `rustsys -w <WS> synth --base-rev <REV> [--check]` reads the `rtl` snapshot and
compiled `.rlib` libraries of `<REV>`, synthesizes the design, writes `tables/netlist.rbin`
(`upstream: [rtl, libraries]`) and `synth.json` (`upstream: [rtl, libraries]`), optionally runs
equivalence checking when `--check` is given (`check.json` with `upstream: [rtl, netlist,
libraries]`), and commits a new revision under optimistic concurrency (`REVISION_CONFLICT`, exit 4,
when `<REV>` is not `HEAD`).

**CLI-REQ-0202** `rustsys -w <WS> check [--base-rev <REV>]` (also spelled `rustsys -w <WS> check
synth`) reads `rtl`, `netlist`, and `libraries` of `<REV>` (default `HEAD`), runs the independent
equivalence checker, writes `check.json` (`upstream: [rtl, netlist, libraries]`) with
`status.check.equivalence`, commits a new revision, and exits `0` (`equivalent` or
`initial_state_refinement`), `3` (`not_equivalent`), or `5` (`undecided`).

**CLI-REQ-0203** `rustsys -w <WS> report [--rev <REV>] [--table <synth|check>] [--section
<NAME>...]` is a read-only query (`new_rev: null`) that renders sections of `synth.json` or
`check.json` at `<REV>` (default `HEAD`) and attaches 1-based object handles (`cell1`, `mm1`,
`phase1`, `unused1`) to each returned row.

**CLI-REQ-0204** `rustsys -w <WS> resize --base-rev <REV> --target <HANDLE>... [--cell <CELL> |
--drive-multiplier <K>] [--request-id <ID>]` resolves its targets per CLI-REQ-0206 and CLI-REQ-0207
against `<REV>` and gives every target instance another cell of the same function (same pins,
directions, output functions and sequential behavior, usable per the library): `--cell <CELL>`
names that cell, which must be of the same function (else `INPUT_REJECTED`); otherwise
`--drive-multiplier K` (positive, default 2.0) is an area ratio, not a drive strength: with
`K >= 1` the instance moves to the strictly larger variant whose area is closest to `K` times its
current area, with `K < 1` to the strictly smaller variant closest to `K` times its area (ties: the
smaller area, then the cell name); when no such variant exists the instance keeps its cell and is
not listed as changed. It commits the updated `netlist` and `synth` tables (CLI-REQ-0213); tables
computed from the old netlist, such as `check` and `sta`, are dropped by carry-forward. The
envelope lists the targets (`scope.targets`, `target_source`, `target_instances`,
`target_paths`), `resized_cells` and `changes` (CLI-REQ-0211).

**CLI-REQ-0205** `rustsys -w <WS> opt-timing --base-rev <REV> [OPTIONS]` runs one bounded step of
timing-driven post-mapping optimization on `<REV>`'s netlist and commits it as a new revision:
the targets of CLI-REQ-0206 to CLI-REQ-0208 are the focus of the incremental electrical mode of
the engine (SYNTH-REQ-0561 to SYNTH-REQ-0564), which starts from the cells of the netlist and
changes only the target instances (resizes within their function) and the nets they drive
(non-inverting buffers, named `rs_buf<k>`, on new nets `rs_net<k>`), at most one change per
iteration, under the timing target of CLI-REQ-0209 and the bounds and stop condition of
CLI-REQ-0210. It commits the updated `netlist` and `synth` tables with the metrics of
CLI-REQ-0213; `next_actions` are exactly `rustsys -w <WS> check --base-rev <NEW>` and
`rusteda diff -w <WS> --from <REV> --to <NEW>`.

**CLI-REQ-0206** Targets of `resize` and `opt-timing`: each `--target` is an instance name of the
netlist, `inst<N>` (row `N - 1` of its instances), `net<N>` (every instance connected to net row
`N - 1`), `sel<N>` (the instances and paths of the selection of `ui_state.json` for `<REV>`,
WS-REQ-0043), or `path<N>` and `ep<N>` of the timing table (CLI-REQ-0207); the instances are
collected once each in the order given. An unknown handle, a row out of range, an empty
selection or another prefix is `UNKNOWN_HANDLE`; targets that name no instance are
`INPUT_REJECTED`. The envelope reports `target_source: "cli"`, the number of instances
(`target_instances`) and the ranked paths used (`target_paths`: handle, check, corner, slack,
whether the slack is exact, endpoint, instance count).

**CLI-REQ-0207** `path<N>` and `ep<N>` are resolved through the read-only view of the timing
table (WS-REQ-0126 to WS-REQ-0134) of `<REV>`, never by position in the netlist: `path<N>` is
ranked path row `N - 1` and names the instances of the pins on that path (the instance prefixes
of their names, mapped to the netlist's instances); `ep<N>` is the `N`-th endpoint by ascending
setup slack of the first corner (`EP_ORDER`) and names the instances of its worst ranked setup
path, or, when it has none, the instance of its data pin; a port endpoint without a ranked setup
path is `UNKNOWN_HANDLE`. Without a timing table in `<REV>` the command fails with
`MISSING_TABLE` and a help line to run the static timing analysis tool of the suite on `<REV>`
first. The table's content hash is verified, and a table whose upstream `netlist` hash is not
that of `<REV>`'s netlist, or that names an instance the netlist lacks, is `INPUT_REJECTED`.

**CLI-REQ-0208** Without `--target`, `opt-timing` takes as targets the instances on the ranked
paths of `<REV>`'s timing table whose check is setup and whose slack is negative
(`target_source: "timing_table"`, with their count and paths in the scope); when there is none,
warning `S0537` says so and no instance may change. Without a timing table every instance may
change, chosen by the estimate (`target_source: "all_instances"`, note `S0537`).

**CLI-REQ-0209** Timing target. When `<REV>` has a `constraints` table (compiled SDC, `CONSTR`
v1, SDC-REQ-0040), its content hash is verified and the uniform ideal-clock target of the
estimate is derived from it: the period is the smallest `create_clock -period`, the input delay
the largest `set_input_delay`, the output delay the largest `set_output_delay`, the output load
the largest `set_load` on ports, the input transition the largest `set_input_transition`, the
clock transition the largest `set_clock_transition` (each over the commands that apply to the
maximum, `-max` or neither), and the margin is `--timing-margin` (default 0.05 ns) plus the
largest setup `set_clock_uncertainty`; times and loads of constraints without `set_units` are
in the units of the primary library. A value the constraints do not give takes its default
(10 ns period, delays 0, transitions 0.1 ns, load 0.01 pF). Command-line flags override derived
values and are listed in `scope.cli_overrides`. Content that would change the target but that the
uniform estimate does not model is reported by warning `S0535`, once per command kind with its
count and first location: several clock periods (the smallest is used), negative delays (0 is
used), delays with `-clock_fall`, `-level_sensitive`, `-reference_pin` or latency options,
`set_load` on nets, pins or cells or with `-subtract_pin_load`, and every command whose effect the
estimate does not model (generated clocks, clock latency and propagation, driving cells,
derates, path exceptions and `set_max_delay`, case analysis and disabled timing, clock groups,
time borrowing, ideal networks, operating conditions and wire-load models, design-rule limits,
and synthesis directives). `set_units`, `current_design`, `group_path`, `set_min_pulse_width`,
`set_min_delay`, `set_min_capacitance`, power limits, `unset_propagated_clock`, hold-only and
minimum-only values have no effect on the setup estimate. A table without clock gives warning
`S0536`; a revision without `constraints` table gives warning `S0536` that the target is not
derived from constraints, with the command-line values and defaults. The scope states
`constraints_source` (`sdc`, `cli` when flags but no table, `defaults`), `period_ns`, the
resolved `constraints` values and `constraints_clocks`.

**CLI-REQ-0210** Bounds of `opt-timing`: `--max-iters N` (1 to 10000, default 16) bounds the
iterations of the incremental mode; `--stop-when COND` with `COND` one of `wns_ns >= X`,
`tns_ns >= X` (spaces optional, `X` a finite number in ns; `wns` and `tns` are accepted for the
names) or `no-improvement` (default `wns_ns >= 0`) is evaluated on the estimate of the starting
netlist and after every iteration; another condition is `USAGE`. The mode also stops when no
examined change improves the estimate. `--effort low|medium|high` (default `medium`) sets the
search breadth of SYNTH-REQ-0563. The area limit is the smaller of `--area-limit A` and the area
of `<REV>`'s netlist times `1 + P/100` for `--max-area-growth-pct P` (finite, at least 0). The
envelope reports `iterations_run` and `stop_reason`: `condition_met`, `max_iters`,
`no_improvement` or `area_limit` (an improving change exists, but every one exceeds the area
limit), and `iterations` (per iteration: the change, the instance, the old and new cell, the net
and moved sinks of a buffer, and the estimated WNS, TNS and area after it).

**CLI-REQ-0211** `changes` lists, in the instance order of the new netlist, every instance whose
cell changed (`change: "resize"`, `from_cell`, `to_cell`) and every new instance
(`change: "insert"`), each with its `inst<N>` handle in the new revision and its name.

**CLI-REQ-0212** `--request-id ID` of `opt-timing` and `resize`: `ID` is 1 to 64 characters of
`[A-Za-z0-9_-]` (Rule 4.1; else `USAGE`) and is recorded in the history line through the
argument vector (WS-REQ-0042). When `history.jsonl` already has a successful line of the same
command with the same request id and a new revision, the command runs nothing and commits
nothing: it answers with that revision (`new_rev`, `rev`), its base revision, the metrics of the
line, `replayed: true`, the request id and note `A0111`, exit status 0, whatever `HEAD` is now,
and writes no history line. That line must name the same base revision; another base is
`INPUT_REJECTED`. Every other answer of these commands has `replayed: false` and the request id
(or null).

**CLI-REQ-0213** Honest results of `opt-timing` and `resize`: every metric is computed from the
committed netlist: `area_um2`, `cells`, `logic_depth` and `estimated_delay` (the netlist estimate
of SYNTH-REQ-0565, in library time units), and for `opt-timing` `estimated_slack_ns`,
`estimated_tns_ns` and the values before the step, `area_growth_pct`, `buffers_added`,
`resized_cells` and `iterations_run`. The `opt-timing` scope states `timing_source: "estimate"`,
`clock_model: "ideal"`, `wire_model: "none"` and `period_ns` with the target of CLI-REQ-0209:
the estimate is not a timing signoff. The republished `synth` report carries the area, cells,
cell areas, cell classes, logic depth and delay estimate of the new netlist and no electrical
report of synthesis, which describes the old netlist; when the delay estimate leaves out cells
outside the classified library or on combinational cycles, warning `S0538` says so.

## Sources other than RTL

**CLI-REQ-0220** `synth` and `check synth` take SystemVerilog, BLIF (`.blif`) and AIGER (`.aag`,
`.aig`) sources, the format chosen by the extension (SYNTH-REQ-0900). Sources of several formats,
and several AIGER files, are an invalid command line `A0300`. `-D`, `-I`, `-P`,
`--no-predefined-macros`, `--assume-full-case` and `--assume-parallel-case` with a source that is not
SystemVerilog, and `--initial-values` with a SystemVerilog source or a gate-level netlist, are
`A0301`: those options have no meaning there (SYNTH-REQ-0901, 0906).

**CLI-REQ-0221** `--gate-netlist <FILE>` gives a structural gate-level netlist as the source of
`synth` and as the reference of `check synth` (SYNTH-REQ-0910, CHECK-REQ-0103), and `--gate-lib
<FILE>` (repeatable) the Liberty libraries of its cells and of the `.gate` and `.mlatch` cells of a
BLIF source (SYNTH-REQ-0909). `--gate-netlist` with positional sources is `A0302`; `--gate-netlist`
without `--gate-lib`, and `--gate-lib` with a SystemVerilog or AIGER source, are `A0303`. A library
that does not read is `S0500` for `synth` and `C0003` for `check synth`.

**CLI-REQ-0222** `--initial-values <reset|arbitrary>` (default `reset`) selects what the declared
initial values of BLIF and AIGER state become (SYNTH-REQ-0906, CHECK-REQ-0105); `check synth` must
be given the mode of the synthesis run, which `--from-run` supplies.

**CLI-REQ-0223** Jobs and run records keep `gate_netlist`, `gate_libs` and `initial_values` (absent
when not given, so that jobs written before the fields still read); the gate-level netlist is
recorded as an input of role `source` and its libraries of role `lib`. The canonical command
(PROTO-REQ-0020) spells out `--gate-netlist` and every `--gate-lib`, and for BLIF and AIGER sources
`--initial-values` with its value, the default included. `check synth --from-run` takes the gate-level
netlist, its libraries and the initial-value mode of the synthesis run unless the command line
gives them (PROTO-REQ-0061).

**CLI-REQ-0224** Workspace mode: `rustsys -w <WS> synth --base-rev <REV> [--check]` and
`rustsys -w <WS> check` read an `rtl` snapshot of BLIF or AIGER files (RCLI-REQ-0021) like one of
SystemVerilog files, the format chosen by the extensions of its files, the top (model or module
name) from the snapshot unless `--top` is given. `--initial-values` applies to both commands;
without it, `check` takes the mode that the revision's synthesis report records
(`initial_values_mode`), else `reset`.
