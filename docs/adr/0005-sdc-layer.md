# ADR 0005: Shared SDC layer, bounded Tcl subset, and CONSTR v1 binary sections

- Status: accepted
- Date: 2026-10-17

## Context
Timing constraints arrive as Synopsys Design Constraints (SDC) files (`[@sdc2016]`) written in a
subset of Tcl (`[@tcl86]`). Embedding a third-party C Tcl interpreter would pull in an unsafe runtime
with filesystem, process, and socket access, breaking the pure-Rust, deterministic, dependency-free
contract of `rusteda-formats` (`std` + `rusteda-core` only). At the same time, real synthesis and
place-and-route constraint scripts use Tcl variables, environment dictionary reads and writes,
arithmetic expressions, conditionals with `info exists`, list manipulation, plain-list `foreach`
loops, and relative `source` includes alongside the 26 Phase 2a timing and environment commands,
6 electrical DRC commands, and 7 synthesis constraint commands. Furthermore, imported constraints
need a zero-copy-friendly binary section layout (`CONSTR v1`) inside the workspace container so
engines can load normalized canonical constraints without re-parsing Tcl.

## Decision
- The module `sdc` of `rusteda-formats` (`crates/formats/rusteda-formats/src/sdc.rs` and
  `src/sdc/`) implements the complete Phase 2a SDC reader, declarative command schema, canonical
  writer, and `CONSTR v1` binary codec (`SDC-REQ-0001` to `SDC-REQ-0055`, specified in
  [docs/specs/sdc/sdc.md](../specs/sdc/sdc.md)) with zero external dependencies beyond `std` and
  `rusteda-core`.
- A bounded, deterministic Tcl subset interpreter (`src/sdc/eval.rs`, `src/sdc/expr.rs`) evaluates
  SDC scripts without a general Tcl runtime (`SDC-REQ-0007` to `SDC-REQ-0015`):
  - Variables (`set`, `unset`, `$v`, `${v}`, `::` global qualifier, `{*}` argument expansion,
    undefined variable `D0007`) and seeded `$::env(VAR)` / `$env(VAR)` array access (`Env` table,
    never reading process environment variables implicitly).
  - Typed `[expr {...}]` arithmetic (`i64`, `f64`, float-promoting `/`, string comparisons,
    logical/ternary operators, and math functions `abs`, `min`, `max`, `int`, `double`, `round`).
  - `if` / `elseif` / `else` with short-circuiting `[info exists var]` guards, `foreach` over plain
    Tcl lists (rejecting object collections with `D0006`), `puts` captured as `D0040`, list and
    string commands (`list`, `concat`, `llength`, `lindex`, `lappend`, `split`, `join`, `string`,
    `lsearch` + `lreplace`), collection helpers (`add_to_collection`, `remove_from_collection`),
    and relative `source` resolution (rejecting absolute paths and `..` traversal with `D0006`).
  - Explicit step (`Limits::max_steps = 100_000`) and recursion depth (`Limits::max_depth = 32`)
    budgets enforced with `D0008`, and explicit `D0006` diagnostics for out-of-subset Tcl
    constructs (`proc`, `while`, `for`, `switch`, `eval`, `uplevel`, `upvar`, `namespace`,
    `regexp`, `regsub`, `open`, `exec`, `socket`, `glob`, `array`, `catch`).
- Every SDC command is validated against a declarative `CmdSchema` table (`src/sdc/schema.rs`) that
  checks unknown commands (`D0010`), unknown options (`D0003`), missing/extra/duplicate/mutually
  exclusive options (`D0004`), argument types (`D0005`), and Phase 2b commands/options (`D0010` /
  `D0011`) before converting numeric arguments into canonical internal units (`ns`, `pF`, `kOhm`,
  `V`, `mA`, `nW`) according to the active `Units` state (`set_units`, `D0030`).
- DRC commands (`set_max_transition`, `set_max_capacitance`, `set_min_capacitance`,
  `set_max_fanout`, `set_fanout_load`, `set_port_fanout_number`) emit `D0021`
  (`Severity::Warning`) and synthesis commands (`set_dont_use`, `set_dont_touch`,
  `set_dont_touch_network`, `set_size_only`, `set_max_area`, `set_max_dynamic_power`,
  `set_max_leakage_power`) emit `D0020` (`Severity::Note`), while preserving typed `Cmd` AST
  records for round-trip writing and binary storage.
- `sdc::write` (`src/sdc/write.rs`) emits canonical, variable-free SDC text whose re-parse is
  idempotent (`read_str(&write(&s)).0 == s`, `SDC-REQ-0003`).
- `Sdc::to_sections` and `Sdc::from_sections` (`src/sdc/constr.rs`) encode and decode the 13
  little-endian `CONSTR v1` sections (`STR_BYTES`, `STR_OFFS`, `HEADER`, `SOURCES`, `ENV`, `CMDS`,
  `ARGS`, `F64`, `OBJS`, `OBJ_CHILD`, `PATS`, `CANON`, `DIAGS`), and `rusteda import --sdc`
  (`crates/apps/rusteda-cli/src/import_sdc.rs`) writes them to the workspace table `constraints`
  (`constraints.rbin`, kind `"CONSTR"`, version `1`).

## Consequences
- `rusteda-formats` parses, validates, canonicalizes, and binary-serializes Phase 2a SDC files
  deterministically in safe Rust without external crates.
- All numeric quantities in `Sdc` and `CONSTR v1` are scaled on ingestion to canonical units (`ns`,
  `pF`, `kOhm`, `V`, `mA`, `nW`), while `Units` preserves the active unit flags for canonical
  output and bit-exact binary round tripping (`Sdc::exact_eq`).
- Phase 2b features (`create_generated_clock`, `set_propagated_clock`, `unset_propagated_clock`,
  `set_timing_derate`, `set_min_pulse_width`, `set_ideal_network`, `-through`, and
  `-reference_pin`) fail fast with structured `D0010` / `D0011` diagnostics carrying exact source
  locations rather than being silently ignored.
