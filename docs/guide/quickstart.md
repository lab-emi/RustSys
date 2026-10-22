# Your first netlist

Run these commands from the repository root after [installation](installation.md). They synthesize a counter, inspect its cell usage, and independently check the exported netlist against its RTL. No PDK download is needed.

## 1. Synthesize

```sh
FIX=verification/fixtures/synthetic/synth
mkdir -p .work

rustsys --threads 1 synth \
  --top counter \
  --lib "$FIX/lib/wt_rich.lib" \
  --out .work/counter_mapped.v \
  --report .work/counter.json \
  "$FIX/rtl/counter/counter.sv"
```

The result is a flat Verilog module containing standard-cell instances. The bundled fixture produces **45 cells**, **8 retained state bits**, **area 115** in this synthetic library's area units, and **logic depth 4**. Runtime varies by machine. The delay is a library-based estimate, not a routed timing result.

```sh
rustsys show .work/counter.json --section cells --sort count --reverse
```

## 2. Check the actual exported file

```sh
rustsys --threads 1 check synth \
  --top counter \
  --lib "$FIX/lib/wt_rich.lib" \
  --netlist .work/counter_mapped.v \
  --report .work/counter_check.json \
  "$FIX/rtl/counter/counter.sv"
```

Expected result:

```text
verdict: equivalent (outputs 9, state pairs 8, unmatched RTL state bits 0)
```

The default contract keeps arbitrary register initial states. These commands add no reset assumption. The native check uses RustSys's documented two-valued RTL interpretation; read [what the proof establishes](checking.md) before using a result as a signoff claim.

## 3. Produce machine-readable results

```sh
rustsys --color never --progress off show .work/counter.json \
  --section summary --print json > .work/counter_summary.json

rustsys show .work/counter.json --section cells --print tsv > .work/cells.tsv
```

Direct synthesis and check commands accept `--print json` to put the complete report on stdout. Diagnostics stay on stderr. Workspace commands use `--format json` and return the versioned `rusteda.v1` envelope instead. See [automation](agents.md) and the [report reference](../reference/reports.md).

## 4. Preserve a reproducible run

Use an unused directory for each `--run-dir`:

```sh
rustsys --threads 1 synth --top counter --lib "$FIX/lib/wt_rich.lib" \
  --run-dir .work/counter_run "$FIX/rtl/counter/counter.sv"

rustsys --threads 1 check synth --from-run .work/counter_run \
  --run-dir .work/counter_check_run

rustsys show .work/counter_run --section command
```

The run contains input hashes, its canonical command, outputs, phase events and reports. `--from-run` carries the source, library and front-end options into the checker. Explicit command-line options override recorded ones, so preserve those overrides when recording evidence.

Next, [map your own RTL](synthesis.md) or [use immutable workspace revisions](agents.md#workspaces-for-concurrent-agents).
