# The `rustsys` command

`rustsys` runs synthesis and the synthesis check from one command line. The specification is
`docs/specs/cli/README.md`.

```text
rustsys [GLOBAL OPTIONS] <COMMAND>

  synth         synthesize SystemVerilog RTL into a netlist of library cells
  check synth   check that a netlist implements its RTL
  show          print sections of a run or a report
  completions   print a shell completion script
```

`rustsys help <command>` and `rustsys <command> --help` list every option with its meaning.

## Global options

These are accepted before or after the command.

| Option | Meaning |
|---|---|
| `--threads N` | worker threads of the parallel phases (0: one per available CPU) |
| `-q`, `--quiet` | standard error keeps only errors and warnings |
| `-v`, `--verbose` | more notes, and one progress line per phase |
| `--color auto\|always\|never` | colour of message severities on standard error (`NO_COLOR` is honoured) |
| `--progress auto\|live\|log\|off` | progress on standard error: a live line on a terminal, one line per phase, or none |
| `--runs-root DIR` | where `--record` puts runs and where `@` references look (default: `$RUSTSYS_RUNS_ROOT`) |

## Streams and exit statuses

Standard output carries only results: the text summary, a JSON report with `--print json`, or
nothing with `--print none`. Messages and progress go to standard error, one message per line:

```text
error[S0133]: top.sv:12:5: module has no port 'clk'
  help: <what to do about it>
```

| Status | Meaning |
|---|---|
| 0 | success |
| 1 | the input was rejected (a message says why) |
| 2 | internal failure |
| 3 | the check found differences (`check synth`) |
| 5 | the check could not decide within its budgets (`check synth`, verdict `undecided`; a `warning[C0005]` names the proof or search that stopped, and `--budget` or `--model-budget` raises the conflict budgets) |
| 130 | interrupted, for example with Ctrl-C (reported by the shell; a recorded run keeps the status `running`) |

## Recorded runs

`--record` stores a run in a new directory under the runs root; `--run-dir DIR` stores it in
`DIR`. A run directory holds the outputs (`netlist.v`, `report.json`, `check.json`),
`events.jsonl` (every phase, step, metric and message with its time) and `run.json`, which
records the canonical command, every input with its hash, the outputs, the timing and the key
figures. `--label TEXT` stores a free text with the run.

The canonical command is the command line that reproduces the run: every option that can change
a result is spelled out, defaults included. `rustsys show RUN --section command` prints it.

## Run references

Wherever a run is expected (`--from-run`, `show`), a run reference may be given:

| Reference | Run |
|---|---|
| `@` | the latest run under the runs root |
| `@synth`, `@check-synth` | the latest run of that product |
| `@~1`, `@synth~1` | the run before the latest (of that product) |
| a directory | that run directory |
| a file | that report |

`--from-run` completes a job from a finished run; options given on the command line win:

```bash
rustsys --runs-root runs synth --lib cells.lib --record rtl/*.sv
rustsys --runs-root runs check synth --from-run @synth --record
```

## `show`

`rustsys show [RUN]` prints sections of a run or report (default: the latest run).

| Option | Meaning |
|---|---|
| `--section NAME` | a section; repeatable. Synthesis: `summary`, `cells`, `unused`, `phases`, `command`. Check: `mismatches`, `summary`, `command` |
| `--print text\|json\|tsv` | text tables, JSON, or tab-separated values of one table section |
| `--filter EXPR` | keep rows matching the filter expression |
| `--sort KEY`, `--reverse` | sort by a column key |
| `--limit N` | rows per table section (0: all) |
| `--exact` | numbers with every digit needed to read them back |
| `--sources` | where each summary value comes from (a JSON pointer into the report, or the rule that derives it) |

Filter expressions are words separated by spaces; a row must satisfy every word:

| Word | Keeps rows where |
|---|---|
| `*_x1` | the name matches the pattern (`*` and `?`; case-insensitive unless the word has capitals) |
| `dff` | the name contains `dff` |
| `count>10`, `share>=5`, `area_total<2.5` | the column compares so with the number |
| `reason=dont_use` | the column contains the word |

Column keys are listed by `--sort` errors and in the TSV header.

```bash
rustsys show @synth --section cells --filter 'count>10' --sort area_total --reverse --limit 0
rustsys show @synth --section cells --print tsv > cells.tsv
rustsys show @check-synth --section mismatches
```

## `completions`

`rustsys completions bash > ~/.local/share/bash-completion/completions/rustsys` installs
completion for bash; `zsh`, `fish`, `elvish` and `powershell` are supported too.
