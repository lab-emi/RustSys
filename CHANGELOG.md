# Changelog

All notable changes to this project are documented in this file.
The format follows the "Keep a Changelog" convention; versions follow semantic versioning.

## [Unreleased]

### Added
- Task-based documentation site with installation, synthesis, agent automation, proof scope,
  command/report references, search, and a validated GitHub Pages publishing workflow.
- RustEDA Signal Forge visual identity, RustSys vector marks, and a reproducible animated
  Verilog-to-netlist banner with a static, reduced-motion alternative.
- Sources other than RTL: `rusteda_formats::blif` and `rusteda_formats::aiger` readers
  (`docs/specs/blif/`, `docs/specs/aiger/`); `rustsys synth` reads BLIF and AIGER sources and,
  with `--gate-netlist` and `--gate-lib`, mapped gate-level netlists of other libraries, with the
  `--initial-values reset|arbitrary` mode for declared initial values
  (`docs/specs/synth/inputs.md`); `rustsys check synth` proves netlists against them with its own
  BLIF and AIGER readers and gate-level reference model (CHECK-REQ-0100 to 0107); `rusteda import
  --rtl` stores BLIF and AIGER snapshots with their format, and the workspace commands use them;
  diagnostic code letters `B` and `G` (WS-REQ-0190).
- RustSys synthesis engine (`rustsys`, `rustsys synth`): elaboration of synthesizable
  SystemVerilog to a bit-level And-Inverter Graph, logic optimization, cut-based standard-cell
  mapping, optional electrical refinement under an ideal-clock timing target, a flat structural
  netlist and a JSON report.
- Independent synthesis checker (`rustsys-check`, `rustsys check synth`): its own RTL
  interpretation, netlist evaluation from the Liberty cell functions, random and sequential
  simulation, equivalence sweeping and SAT-based equivalence proofs with replayed
  counterexamples.
- The `rustsys` command (`rustsys-cli`): standalone `synth`, `check synth`, `show` and
  `completions`; workspace-first (`-w/--workspace`, `--format text|json`, `rusteda.v1` JSON
  envelopes) `synth`, `check`, `report`, `opt-timing` and `resize` with `--base-rev` optimistic
  concurrency and handle-pinned cone/cell scoping; global `--threads`, `--quiet`, `--verbose`,
  `--color`, `--progress` and `--runs-root`; `--record`, `--run-dir`, `--label` and `--print` on
  standalone runs and `--from-run` on `check synth`; one table of exit statuses and coded messages
  with help lines; and the job protocol module (`rustsys_cli::proto`).
- `rusteda-workspace` and `rusteda-cli` (`rusteda`): shared workspace layout, immutable revisions
  (`rev<N>`), `.rbin` binary tables (`NETLIST v1`, `RLIB v1`, `CONSTR v1`), handle validation,
  `rusteda.v1` JSON envelopes and the `rusteda` command (`init`, `import`, `log`, `show`, `diff`,
  `revert`, `export`, `session`).
- `rusteda-core`: cancellation, progress events and coded messages of engine runs (`job`), source
  diagnostics (`diag`), the Liberty library model (`liberty`), interned symbols (`symbol`), typed
  arenas (`arena`), hierarchical design names (`names`) and the gate-level netlist (`netlist`).
- `rusteda-formats`: SystemVerilog preprocessor, parser and streaming structural netlist reader and
  writer (`verilog`), Liberty library reader (`liberty`) and Synopsys Design Constraints reader
  (`sdc`).
- `rusteda-compute`: priority-cut enumeration and selection (`cut`) and batched timing propagation
  (`timing`) on CPU, CUDA (`cuda`) and Metal (`metal`), with runtime selection and a per-process
  device self-test.
- `rustsys-fixtures` (synthetic cell libraries and RTL) and `rustsys-defects` (planted-defect
  generator for testing the checker), plus dual-PDK verification (`sky130_fd_sc_hd`, `gf180mcu`).
- Cargo workspace, repository automation (`cargo xtask`) and policy files.
- `rusteda_workspace::sta`: a validated, read-only, zero-copy view of the timing table
  `sta.rbin` (`STA` v1) that the static timing analysis tool of the suite commits (summary,
  endpoints, `path<N>` ranked paths with the instances of their pins, in the full and summary
  store modes), and `StaTableBuilder`, which writes minimal valid timing tables
  (WS-REQ-0126 to WS-REQ-0136).
- `rustsys -w <WS> opt-timing`: targets from the timing table (`path<N>`, `ep<N>`, `sel<N>`
  paths, and by default the ranked setup paths with negative slack), the timing target from the
  `constraints` table, `--max-iters`, `--stop-when 'wns_ns >= X' | 'tns_ns >= X' |
  no-improvement`, `--effort`, `--max-area-growth-pct` and `--request-id`; the engine's
  incremental electrical mode starts from the cells of the netlist and changes only the targets
  and the nets they drive (CLI-REQ-0205 to CLI-REQ-0213, SYNTH-REQ-0561 to SYNTH-REQ-0565).
  `rustsys -w <WS> resize` takes the same targets and `--request-id`.
- `cargo xtask verify` runs each `cargo test` step with `RUST_TEST_THREADS=4`, a timeout and,
  where the user's service manager accepts transient scopes, a memory limit; `verify pdk` runs
  every synthesis and check step in such a scope (`RUSTEDA_VERIFY_MEMORY_MAX`,
  `RUSTEDA_VERIFY_TIMEOUT_S`, `RUSTEDA_VERIFY_NO_SCOPE`).
- Shared crates converged with those of the suite's timing tool: both repositories vendor the
  same 209 shared files with the same `verification/shared.sha256`, and `cargo xtask shared diff`
  finds no difference. From the timing tool's side this adds:
  - `rusteda import --spef FILE --spef-corner NAME` (repeatable, paired in order) with the budgets
    `--max-spef-mib`, `--max-rc-nets`, `--max-rc-nodes`, `--max-rc-elements` and
    `--max-rc-name-mib`: bounded detailed RC import into the `parasitics` table through the new
    reader `rusteda_formats::spef` (plain or gzip sources, `T0071` to `T0074`), and a warning
    `T0140` when a later import drops that table (RCLI-REQ-0070 to RCLI-REQ-0076,
    `docs/specs/rusteda-cli/spef.md`);
  - `rusteda_workspace::edits`: netlist edit files `rusteda.edits.v1` and their application;
  - `rusteda_compute::rc` (effective-capacitance driver and wire waveform models) and prepared
    per-arc values (`ArcValues`) in the timing kernel interface, unchanged for callers that pass
    none (COMPUTE-REQ-0030), and `rc::gate_detail_with_resistance`: the bounded charge solve
    and waveform of `rc::gate_detail` for a caller-supplied driver resistance, with
    `rc::gate_detail` unchanged bit for bit;
  - the Liberty pin attributes `min_pulse_width_high`, `min_pulse_width_low` and `min_period`
    on `Pin` instead of ignored (LIBERTY-REQ-0049), and the compiled library format `LIBERTY` v4
    that carries them (WS-REQ-0117): a cached v3 `.rlib` is compiled again from its `.lib`;
  - `set_propagated_clock` and `unset_propagated_clock` are typed constraints kept in
    `CONSTR v1` (SDC-REQ-0070) instead of error `D0010`; `rustsys -w <WS> opt-timing` reports
    `set_propagated_clock` with warning `S0535` as clock propagation that its ideal-clock
    estimate does not model;
  - `rusteda show` reports `drv_status: "unchecked"` and `null` design-rule counts for a timing
    summary without `drv_status` (RCLI-REQ-0023).

### Changed
- The README now leads with CLI and agent workflows, recorded runs, structured output, and
  revision-aware workspaces while retaining the measured RustSys / Yosys comparison tables.
- `rustsys check synth`: a proof or search that stops at its budget (the conflict budget of an
  obligation or of a proof of the RTL model, or the repetition limit of state-correspondence
  discovery) gives the new verdict `undecided`, exit status 5, with a `warning[C0005]` that
  names the budget, instead of `error` (exit status 1) or, at the discovery limit,
  `not-equivalent`. A port difference still decides (`not-equivalent`, exit status 3) when
  discovery stops at its repetition limit, and a `warning[C0005]` still names the limit; an
  erroneous netlist is still rejected (exit status 1) when a proof of the RTL model runs out of
  its budget. The new options `--budget` and `--model-budget` set the two conflict budgets, and
  the canonical command spells them out. Run records can have the status `undecided`.
- The initial-state refinement verdict is spelled `initial-state-refinement` in the JSON check
  report, as in the text output. The check report's `format_version` is 2 and it gains the fields
  `undecided` and `internal_error`; reports of version 1 are still read.
- An internal failure of the checker, such as a counterexample that does not replay, is exit
  status 2 with `error[A0002]` and `internal_error` in the report instead of an input error
  (exit status 1).
- The checker rejects `wand`, `wor`, `triand`, `trior`, `tri0`, `tri1`, `trireg` and
  `interconnect` nets, logic ports declared `supply0` or `supply1`, and driven supply nets
  (`C0105`); an undriven supply net is a constant.
- `rusteda_workspace::sta` reads timing tables of format versions 1 and 2 (`STA` v2 stores
  endpoint rows, corners and path-chain nodes at full width; WS-REQ-0126, WS-REQ-0127,
  WS-REQ-0134), so `opt-timing` and `resize` read the tables that the suite's timing tool
  commits; `StaTableBuilder` writes version 2 with `null` design-rule counts and
  `drv_status: "unchecked"` (WS-REQ-0135, WS-REQ-0137).
- The warning `D0021` of a design-rule constraint now reads "recorded in CONSTR v1; the analysis
  that reads it reports whether it is checked".
- Debug builds of `rusteda import --netlist` check the fast flat reader against the full reader
  (structural or extended) on small inputs.
- The 64 MiB budget of the full netlist reader, for a netlist that the streaming reader cannot
  take, is the option `rusteda import --max-netlist-ast-mib` (default 64, RCLI-REQ-0024); its
  `V0300` error names the budget and the option.
- `rusteda-formats` lists SPEF among its formats; the external-simulator oracle tests of
  `rusteda-compute` (ignored by default) read `RUSTEDA_SPICE_SIMULATOR`,
  `RUSTEDA_RC_ORACLE_WORKSPACE` and `RUSTEDA_RC_WIRE_WORKSPACE`.

### Fixed
- The README, the user guide, the synthesis specification (SYNTH-REQ-0516), the roadmap and ADRs
  0002 and 0004 described a `metal` GPU runtime (`--backend metal`, marked done in `WP2`) and a
  `rusteda session` command; neither exists. They now describe the `cpu` and `cuda` runtimes
  that exist and name Metal and session export as planned.
- `set_clock_uncertainty -rise` and `-fall` were accepted and dropped by the shared constraints
  reader; they are now error `D0011` (SDC-REQ-0023). The shared crates are again identical to
  those of the suite's timing tool, and the `xxhash-rust` pin is the same in both (`=0.8.19`).
- `rustsys -w <WS> opt-timing` and `resize` resolved `path<N>` and `ep<N>` to an instance chosen
  by position in the netlist, unrelated to any timing path; they now read the timing table of the
  base revision and fail with `MISSING_TABLE` without one. `opt-timing` ignored `--max-iters` and
  `--stop-when`, rejected `wns_ns >= 0.0`, used a 10 ns period whatever the constraints said,
  upsized explicit targets when its refinement changed nothing, reset every cell to its smallest
  variant first, and republished a stale logic depth and delay estimate; each is fixed, and every
  reported metric is computed from the committed netlist.
- `wait`, `wait_order`, `disable` and event triggers (`->`, `->>`) in procedures were dropped with
  a note by synthesis and skipped by the checker, which could prove a netlist against a procedure
  without its control flow; they are rejected by synthesis (`S0235`) and by the checker
  (`C0106`).
- The checker's netlist reader evaluated cells with three-state outputs as plain drivers,
  ignored `clocked_on_also` of dual-edge flip-flops and read `x` and `z` literal bits as 0, so
  such netlists could be reported `equivalent`. These are now rejected with the codes `C0101` to
  `C0103`; cells with state tables, bus or bundle pins, several sequential groups or connected
  bidirectional or internal pins with `C0104`; net declaration assignments, unpacked net
  dimensions, arrays of instances and parameters of cell instances with `C0001`.
- The checker read a driven `supply0` or `supply1` net as a plain wire, a continuous assignment
  with a `highz0` or `highz1` drive strength as a plain driver (now `C0101`), `x` and `z`
  literals on power and ground pins without notice (now `C0103`), only the last connection of a
  pin connected twice, signed and wide bit index literals without their sign and upper bits, and
  the first of two different library definitions of a cell (now `C0003`); each of these could
  report `equivalent`. A cell function that reads an internal pin or the state of an `ff_bank`
  or `latch_bank` group, and a connected output without a function, are `C0104` with the
  instance named; a module or primitive of the netlist instantiated as a cell, a `bind`, a second
  definition of the netlist module, names in a package scope, data types other than `logic`,
  `reg` and `bit`, and a port declared twice are rejected.
- The checker ignored the Liberty attributes that the library reader does not keep, so an
  open-drain, open-source or pull-up output was checked as a plain driver, an output with
  `x_function` as always known and a latch with `enable_also` as a plain latch, each reported
  `equivalent`; a clock-gating cell described by a function alone was checked as a gate, and a
  cell whose `ff` or `latch` group did not read was rejected for the state its output then reads.
  `rustsys check synth` now reads those attributes from the library text: pin `driver_type` is
  `C0101`, `enable_also` `C0102`, pin `x_function`, `state_function`, `internal_node` and
  `retention_pin`, `retention_cell`, `ff_bank` and `latch_bank` groups, unread attributes of `ff`
  and `latch` groups and `clock_gating_integrated_cell` without an `ff` or `latch` group are
  `C0104`, and a cell that its library describes with errors is `C0003` with the errors named.
- State-correspondence discovery stopped after eight propose-and-refine rounds, so a chain of
  renamed state of more than eight stages ended as an element without a counterpart (`error`) or
  as state cut free with a counterexample that no run reaches (`not-equivalent`); discovery now
  repeats until no relation changes.
- Gate primitives and other module items outside the netlist subset were reported with a dump of
  the syntax tree; they are named with their location, and primitives that can float, switch or
  pull a net are `C0101`.
- The checker's RTL model took a bit of a combinational procedure for a latch when the proof of
  whether it is written on every path ran out of its budget, and reported a difference that does
  not exist; the check is now `undecided`.
- The defects suite counts only `not-equivalent` as a detected defect, and the planted
  asynchronous-value defect keeps the netlist well formed: it rebuilds a complement output that
  the replacement cell lacks with an inverter instead of leaving its net undriven.
