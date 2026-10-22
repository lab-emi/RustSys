# rusteda-core

The core of the RustEDA tools, shared by every tool of the suite (`docs/specs/core/`).

- `diag`: `Diag`, a coded message with severity, code, optional file location and help, rendered
  and parsed in the one message format (JOB-REQ-0007).
- `job`: the contract between an engine run and its observers (JOB-REQ): `CancelToken` and `Ctx`
  for cooperative cancellation at checkpoints between parallel sections, `Event` and `Observer`
  for the phases, steps, progress, metrics and messages of a run.
- `liberty`: the Liberty library model: `Library` with its units, templates (in name order) and
  cells, `Cell`, `Pin`, `TimingArc`, `InternalPower`, `Sequential`; `Table` with bilinear lookup
  and linear extrapolation; `Func`, a Boolean function with evaluation and truth tables. The reader
  that builds the model from Liberty text is `rusteda_formats::liberty` (`docs/specs/liberty/`).
- `symbol`: `SymbolInterner`, one per database, with dense `SymbolId`s in insertion order.
- `arena`: generational `Arena<T>` with typed `Id<T>`; a removed entry's id is stale and is
  reported, never reused for another entry; freed slots are reused last-freed-first.
- `names`: printable-ASCII and hierarchical name rules (`check_verbatim`, `check_hierarchical`),
  segment escaping and unescaping (`escape_segment`, `join`, `unescape`, `segments`), and simple
  identifier checks (`is_simple_identifier`).
- `netlist`: `GateNetlist`, the editable netlist of one flat module (cell types, ports, nets,
  instances, pins, assignments) with checked edits: connect, disconnect and move pins, remove
  instances, nets and assignments, pin-compatible cell swaps, and buffer insertion and removal.
  `freeze` gives the `FrozenNetlist` (`NETLIST v1` sections), `validate` checks untrusted tables
  without panicking, and `thaw` rebuilds the editable netlist.

It uses no other workspace crate. Engines take a `Ctx` and never depend on a command line.
