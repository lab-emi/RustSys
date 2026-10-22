# RustSys command line, diagnostics and reports

**SYNTH-REQ-0701** Command:

```text
rustsys [--threads <N>] synth [OPTIONS] --lib <LIBERTY>... --out <NETLIST> <SOURCE>...
```

| Option | Meaning |
|---|---|
| `--top <NAME>` | top module (SYNTH-REQ-0104) |
| `--assume-full-case` | complete-case assumption (SYNTH-REQ-0224); also accepted by `rustsys check synth` |
| `--assume-parallel-case` | parallel-case assumption (SYNTH-REQ-0225); also accepted by `rustsys check synth` |
| `-D <NAME>[=<VALUE>]` | define a macro (SYNTH-REQ-0102) |
| `-I <DIR>` | include directory (SYNTH-REQ-0102) |
| `-P <NAME>=<EXPR>` | override a top-level parameter (SYNTH-REQ-0105) |
| `--no-predefined-macros` | do not predefine `SYNTHESIS` |
| `--lib <FILE>` | Liberty library, repeatable (SYNTH-REQ-0501) |
| `--dont-use <GLOB>` | exclude cells by name, repeatable; `*` and `?` wildcards |
| `--effort <low\|medium\|high>` | optimization script (SYNTH-REQ-0412), default `medium` |
| `--goal <delay\|area>` | mapping goal (SYNTH-REQ-0512), default `delay` |
| `--delay-relax <R>` | relative arrival slack for area recovery, default 0.1 |
| `--backend <cpu\|gpu\|cuda>` | where the mapper's cut passes run (SYNTH-REQ-0516), default `cpu` |
| `--out <FILE>` | output netlist |
| `--report <FILE>` | JSON report (SYNTH-REQ-0703) |
| `--run-dir <DIR>` | directory for auxiliary outputs; nothing is written elsewhere |

The global option `--threads <N>` of `rustsys` sets the number of worker threads of every parallel
CPU phase of any command (0, the default: one per available CPU). Results do not depend on it. When
the platform cannot start the threads, the command stops with `error[A0001]` and exit status 2.

**SYNTH-REQ-0702** Diagnostics are written to standard error as
`<severity>[<code>]: <file>:<line>:<column>: <message>`, where severity is `error`, `warning` or
`note` and code is a stable identifier of the form `S<nnnn>`. Exit status: 0 on success, 1 when
the input is rejected (at least one error diagnostic, or an invalid command line), 2 on an internal failure. Malformed input
never causes a panic.

**SYNTH-REQ-0703** The JSON report contains: `top`, `cells` (instance count per cell name),
`area` (sum of cell areas in library area units), `sequential_cells`, `combinational_cells`,
`inverters`, `logic_depth` (maximum number of cells on a combinational path),
`estimated_delay` (SYNTH-REQ-0513 units), `aig_nodes_before`, `aig_nodes_after`,
`state_bits_rtl`, `state_bits_retained`, `latches`, `warnings`, `unused_cells` (library cells
synthesis cannot use, each with the reason: `dont_use`, scan, clock gating, state table, bus,
three-state, several outputs, more than six inputs, unsupported sequential function, or a reading
problem), `runtime_s`, `phase_s` (seconds per phase in execution order: `read` for
preprocessing and parsing, `elaborate`, `optimize`, `map` for library classification, sequential
cell selection and mapping, and `emit` for netlist construction and writing), `map_backend` (where
the cut passes ran: `cpu` or `cuda`) and `map_phase_s` (seconds per step of
the map phase in execution order: with a GPU backend first `gpu_wait` for waiting on the device
preparation of SYNTH-REQ-0518; then `setup`; `cut_passes` on the CPU, or on a GPU `gpu_start` for
runtime selection not done in advance, `gpu_setup` for packing and uploading, `gpu_kernels`,
`gpu_readback`, `gpu_decode` and `cpu_between` for the host work between passes; then
`exact_passes`).

**SYNTH-REQ-0704** On success, standard output carries the summary (with `--print text`):

```text
top: <name>
cells: <n> (sequential <s>, combinational <c>, inverters <i>)
area: <area>
logic depth: <d>
estimated delay: <delay> (<unit>)
AIG AND nodes: <before> -> <after>
state bits: <rtl> in RTL, <retained> retained, <latches> latches
warnings: <w>
```

where `<unit>` is `time unit <t> ns` from the first library, or `library time units` for reports
without the time unit.

**SYNTH-REQ-0705** `--out` may be omitted when the run has a run directory (`--run-dir`,
`--record`): the netlist is then `netlist.v` in it; without either, `error[S0703]` with a help line.
`--report` defaults to `report.json` in the run directory. A failed write is `error[S0702]`,
exit 1.

**SYNTH-REQ-0706** `--delay-relax` must be finite and not negative (command-line error otherwise).

**SYNTH-REQ-0707** The JSON report starts with `format` (`rustsys-synth-report`) and
`format_version` (1) and also contains `cell_area` (area of each used cell, as mapped) and
`time_unit_ns` (the time unit of the first library in ns, or null). Reports written before these
fields load with them absent.

**SYNTH-REQ-0708** Messages are streamed as they arise (JOB-REQ-0003), in order; on success the
streamed messages equal the run's messages; on failure they end with the failure's messages.

**SYNTH-REQ-0710** `synthesize_with(sources, libraries, options, ctx)` runs synthesis with a context
(JOB-REQ-0002): phases `read`, `elaborate`, `optimize`, `map`, `emit`; optimization steps named
after their passes (`sweep`, `balance`, `rewrite`, `refactor`, `resub`, `merge`, `rewrite_zero`);
mapping steps `gpu_wait`, `delay_pass`, `area_pass`, `exact_pass`; level progress of the mapping
passes; metrics `aig_ands` after elaboration and each optimization step, `map_arrival` after each
mapping pass, `map_gates` before emission. `synthesize` is `synthesize_with` with `Ctx::NONE`.

**SYNTH-REQ-0711** Observing a synthesis run or passing a cancel token that is never set changes
no byte of its netlist and no field of its report other than run times (JOB-REQ-0005). A cancelled
synthesis returns `JobError::Cancelled` after at most one level of a mapping pass, one GPU pass or
the GPU preparation.
