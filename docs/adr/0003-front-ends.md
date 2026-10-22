# ADR 0003: The command line and the engines share one job protocol

- Status: proposed
- Date: 2026-09-29; amended 2026-10-16 (the job contract is part of `rusteda-core`, the protocol a
  module of the command line)

## Context
RustSys ran only as blocking library calls behind a thin command line: no progress, no
cancellation, results only as JSON files, and diagnostics and results mixed on standard error.
RustSys needs a scriptable command line whose runs can be recorded, reproduced and inspected later,
with the same numbers in every view of a run, while the engines stay free of user interface
dependencies (AGENTS rules 9.2).

## Decision
- The module `job` of `rusteda-core` (`crates/core/rusteda-core`, std only) defines cancel tokens,
  contexts, events and messages (JOB-REQ). Engines gain `*_with(…, Ctx)` entry points and `view`
  modules; hooks run only on the orchestrating thread, so results do not change (JOB-REQ-0005).
- The module `proto` of `rustsys-cli` (`crates/apps/rustsys-cli/src/proto/`) is the job protocol
  between the command line and the engines: typed jobs, canonical commands, execution with events,
  run records, report loading, text renderers, tables, filters and sort orders (PROTO-REQ). It has
  no toolkit and no long-lived thread, and is kept apart from argument parsing and terminal output.
- The module `ws` of `rustsys-cli` (`crates/apps/rustsys-cli/src/ws/`) implements the
  workspace-first CLI mode (`-w/--workspace`, `--format text|json`, `--base-rev`, `--rev`) over
  `rusteda-workspace`, producing `rusteda.v1` JSON envelopes and immutable revisions for `synth`,
  `check`, `report`, `opt-timing`, and `resize` (CLI-REQ-0200 to 0205).
- The command line (`rustsys-cli`, `crates/apps/rustsys-cli`) becomes a library plus a thin binary
  with the application commands `show`, `report`, `opt-timing`, `resize` and `completions`, coded
  messages on standard error and results on standard output (CLI-REQ).
- Dependencies, exact versions and licenses: clap 4.6.7 with `color` and `suggestions` (MIT OR
  Apache-2.0; anstream, anstyle, colorchoice, strsim MIT or Apache-2.0), clap_complete 4.6.11 (MIT
  OR Apache-2.0), crossterm 0.29.0 (MIT; the terminal width of the progress line).

## Consequences
- Checker verdicts and mismatch lines move from standard error to standard output; Liberty errors
  of the check get their own code.
- The protocol is part of the command line (`crates/apps/rustsys-cli/src/proto/`), the one front
  end so far, which no crate depends on (AGENTS rules 6.2); a later front end that needs it can
  take it into a crate of its own.
- Shared format and workspace front-ends live in `rusteda-formats`, `rusteda-workspace` and
  `rusteda-cli`; the SDC constraint reader, canonical writer, `CONSTR v1` binary sections, and
  `rusteda import --sdc` command are recorded in [ADR 0005](0005-sdc-layer.md).
