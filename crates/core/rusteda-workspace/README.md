# rusteda-workspace

The disk workspace of RustEDA, shared verbatim by every RustEDA tool. A workspace is a
directory of immutable revisions; every tool is a stateless process that opens it, maps the
tables it needs and, for a mutation, commits a new revision with optimistic concurrency. There
is no daemon and no hidden state: what is on disk is the truth.

The crate is tool-neutral. It knows tables, hashes, revisions, locks, the history and the JSON
envelope of the command line, and leaves the content of tables to the tools. The NETLIST and
LIBERTY codecs that the contract (§1, §5) assigns to this crate, and with them the `rlib`
module that holds the normative LIBERTY v1 grammar, are deferred until `rusteda-core` is
available: this crate does not depend on any other RustEDA crate yet.

## Layout

```text
<ws>/manifest.json        HEAD: {"schema","head","next_rev","corners"}
<ws>/workspace.lock       shared lock for reads of the manifest, exclusive lock for changes
<ws>/history.jsonl        one line per workspace-changing command, rejected ones included
<ws>/ui_state.json        reserved: the selection of a future GUI (read by --target sel<N>)
<ws>/cache/<name>-<h16>.lib    library, verbatim (h16 = xxh3-64 of its bytes)
<ws>/cache/<name>-<h16>.rlib   library, compiled
<ws>/revisions/rev<NNNN>/meta.json + tables
<ws>/revisions/.staging-<pid>-<nanos>/   a transaction in progress (ignored by readers)
<ws>/revisions/.orphan-*                 set-aside leftovers of an interrupted commit
```

A revision prints as `rev<N>` (padded input such as `rev0003` is accepted) and lives in
`rev<N zero-padded to at least 4 digits>`. Queries take `HEAD` or `rev<N>`; mutations take
`--base-rev rev<N>` and never `HEAD`. The two resolvers return different types
(`Rev` and `BaseRev`), so a query revision cannot be passed where a base revision is required.
Object handles are revision-scoped `<prefix><N>` (`inst12`, `path3`, `sel4`); every handle
matches `[A-Za-z0-9_-]+`, and the handles of `ui_state.json`, written by another program, are
checked when the file is read.

## Tables and hashes

`meta.json` lists the tables of a revision with their path, their hash and the hashes of the
tables they were computed from (`upstream`). Hashes are xxh3-64, written `xxh3:<16 hex>`:

- a file table hashes its bytes;
- a directory table (such as an `rtl/` snapshot) hashes the sorted records
  `relative path, NUL, file-hash hex, LF` of its files;
- the `libraries` pseudo-table hashes one record `name, NUL, corner, NUL, hash hex, LF` per
  library, in list order;
- the `libraries.<corner>` pseudo-table hashes the records of the libraries of one corner. A
  table computed from one corner (a netlist mapped against `default`) names it as its
  upstream, so importing a library for another analysis corner does not drop it, while timing
  over all corners names `libraries`. This pseudo-table extends the contract, which names only
  `libraries`.

A table path in `meta.json` is one entry of its revision directory; anything else (`..`, an
absolute path, a subdirectory) makes the revision `CORRUPT_TABLE`. Tables are opened only as
regular files (or directories), checked before and after the open, so a FIFO, a device or a
symbolic link in a revision, also inside a directory table, is `CORRUPT_TABLE` and never blocks
a reader. A directory table (an `rtl/` snapshot) is read with `Revision::table_files`, which
lists its files with their hashes after checking the directory hash against `meta.json`, and
`Revision::read_table_file`, which reads one of them without following symbolic links;
`Revision::read_table` reads file tables.

Binary tables use the `rbin` container: a little-endian header (magic `RUSTEDA\0`, kind,
format version, flags, content hash, upstream entries, section table) followed by sections of
fixed-size elements at 8-byte aligned offsets. `RbinWriter` writes it deterministically. Every
open validates the header (magic, kind, version, `header_len`, bounds, alignment, element
sizes, order and overlap of sections) and hands out zero-copy `&[T]` views;
`Rbin::verify_hash` checks the content hash on request. Any failure is `CORRUPT_TABLE`, except
a format version newer than the reader's, which is `SCHEMA_TOO_NEW` (exit 1: upgrade the
tools).

A map is sound only while nobody writes the file, so only files that the workspace never
writes again are mapped: the tables of committed revisions (`Revision::open_rbin`) and the
compiled libraries of the cache (`LibraryCache::open_rlib`, `Revision::open_library`). Any
other file is read into an 8-byte aligned buffer with `Rbin::read`, and bytes in memory are
opened with `Rbin::from_bytes`; a later write of the file does not change a table read so.

A table enters a revision checked. Its files are written by the tool (a copy, a reflink copy
included, is a new file) or are hard links to files of the base revision; a staging file that
shares its inode with anything else, such as a user's source file, would let an edit of that
file rewrite the committed revision, so `add_file` and `add_dir` refuse it (`INTERNAL`). A
table that is an rbin file must pass the structural checks of every open and its content hash
(contract §5: "content hash on import"), whatever its kind; `add_bytes` and `add_file` refuse
a damaged one (`INTERNAL`) instead of committing it under its own file hash.

The header of an rbin table records the table's upstream map: one entry per upstream table
with the hash `meta.json` records for it (for an rbin upstream, its file hash), under an
8-byte name. A name of up to 8 bytes is used as it is (`netlist`, `sta`); a longer one becomes
its first 3 bytes, `.` and 4 hex digits derived from its hash (`constraints`, `libraries`),
moved to the next free digits when another name of the same map has them, so the names of one
map never collide (two PDK corners whose 16-bit hashes collide included); see
`rbin::upstream_names`. The names depend on the map only, so the writer and every reader
derive the same ones. `Txn::add_rbin` writes these entries; `add_bytes` and `add_file` check
them in a table that is an rbin file, and `Revision::open_rbin` checks them again.

A library entry of `meta.json` names its compiled form at its cache path,
`cache/<name>-<h16>.rlib`, checked when a transaction adds it and when a revision is read. The
cache stores a compiled library only if it is a valid rbin file (structure and content hash),
compiles a cached `.rlib` that fails that check again, and never follows a symbolic link.

## Summary and status

`summary` and `status` of `meta.json` are kept per table, `{table: {key: value}}`, with keys
only for tables of the revision: contract §3, rule CA-STATUS. A value describes the table it is
set for (`Txn::set_summary(table, key, value)`, only for a table the transaction adds): a
carried table's values are copied from the base revision, and a dropped table drops them. A
status therefore never outlives the table it describes: when a new netlist drops `check`, the
revision no longer claims an equivalence, so an unchecked property is never reported as
passing. A property whose table is absent is unchecked; `rusteda show` and `diff` render a
missing `sta` status as timing `not_analyzed` and a missing `check` status as equivalence
`unchecked`. So a property is recorded under the table of the command that checks it
(equivalence under `check`), not as an `unchecked` value under another table, where a later
check could not replace it. A `meta.json` whose `summary` or `status` breaks the rule, such as
a flat `{"equivalence": "unchecked"}`, is `CORRUPT_TABLE` with a reason that names it.

The `metric_deltas` of a history line are kept per table too, which extends the contract (it
leaves their inner shape open): for each table the command added, its numeric summary values
minus those of the table's previous version, found in the base or, when the base dropped the
table, in the nearest ancestor that has it (at most 64 revisions back). So the history line of
`ruststa run` after `rustsys opt-timing` shows the change of WNS since the last timing run. A
revert compares the tables that differ between the two revisions.

## The commit protocol

1. `Workspace::begin(base)` reads the manifest under the shared lock. If HEAD is not `base` it
   fails with `REVISION_CONFLICT` before any work, and logs the conflict. Otherwise it creates
   `revisions/.staging-<pid>-<nanos>/`.
2. The tool computes lock-free from the immutable base revision and adds its tables to the
   transaction (`add_rbin`, `add_bytes`, `add_file`, `add_dir`), each with its upstream hashes;
   the crate checks them as they enter (see above) and computes the table hashes.
3. `Txn::commit` takes the exclusive lock and re-checks HEAD; on a conflict it deletes the
   staging directory and logs the command with `ok: false`, exit 4. Otherwise:
   - the revision number is `N = max(next_rev, 1 + highest revision directory)`, so a revision
     directory left by a crash between its rename and the manifest update is skipped (and set
     aside as `.orphan-*`; readers never see it, since it is not below `next_rev`);
   - carry-forward: the new revision starts from the base's tables; a carried table survives
     only if each of its upstream hashes equals the hash of that table in the new revision,
     otherwise it is dropped and listed in `dropped` with the reason; this repeats to a
     fixpoint, so dropping a netlist also drops the timing computed from it;
   - every upstream hash of a new table must hold in the new revision, or the commit fails;
   - carried tables are hard-linked from the base (copied where links are impossible);
   - `meta.json` is written, files and directories are fsynced, the staging directory is
     renamed to `rev<NNNN>` and `revisions/` is fsynced;
   - `manifest.json` is replaced through a temporary file, fsync, rename and directory fsync;
   - the history line is appended and fsynced, and the lock is released.

The rename of `manifest.json` is the commit point. A failure before it leaves HEAD where it was
and is an error. After it the commit has happened: a failed directory fsync or history append
does not make it fail; `Committed::warnings` carries it as a `W0102` or `W0101` warning, for a
result with `ok: true` and `new_rev` set, and no failure line is written. `init` and `revert`
follow the same rule (`Initialized::warnings`, `Reverted::warnings`).

A command that commits with a negative or inconclusive verdict sets its exit code with
`Txn::set_exit(3)` or `Txn::set_exit(5)`; the history line records it with `ok: true`.

`Workspace::revert(base, to)` takes the exclusive lock, checks HEAD, points HEAD at `to` and
logs the command; it creates no revision, and numbering continues after the highest revision.
Committed revisions are never written again.

Every workspace-changing command gets one history line, rejected and conflicted ones
included. `init`, `fork`, `commit` and `revert` log their own outcome, success or failure (a
rejected `init` in the existing workspace, when its manifest can be read), and `begin` logs its
failure, the conflict included, so `ws.begin(base, &inv)?` loses no line. The caller logs only
what fails outside them: before `begin` (a command line, a `--base-rev` that does not resolve)
with `Workspace::log_rejected`, and between `begin` and `commit` with `Txn::reject`, which
writes the line before it removes the staging directory: a directory that cannot be removed
stays behind as a `.staging-*` entry, ignored by readers, without costing the command its line.

## Forks

Agents fork an experiment by copying the workspace (plan §1.1). `Workspace::fork(dst, inv)`
makes the copy under the shared lock, so it is one state of the workspace: HEAD and every
revision below `next_rev`, never a HEAD whose revision is still being committed. Revision files
and cached libraries are hard-linked, which is safe because no workspace writes them again
(copied where links are impossible); `history.jsonl` and `ui_state.json` are copied; staging
and orphan directories are left out; the fork gets a history line of its own. A plain copy of
the directory (`cp -r`, `cp --reflink`) is consistent only while it holds a shared lock on
`workspace.lock` (`flock -s ws/workspace.lock cp -r ws fork`), or if it copies `manifest.json`
before `revisions/`: a revision committed during the copy is then above the fork's `next_rev`,
an orphan the fork sets aside.

## The envelope

In JSON mode every command prints exactly one `rusteda.v1` envelope. `Envelope` writes the keys
in the contract's order: `schema`, `ok`, `command`, `workspace`, `base_rev`, `rev`, `new_rev`,
`scope`, `completeness`, `metrics`, the tool's own fields in insertion order, `diagnostics`,
`error`, `next_actions`. `scope`, `completeness` and `metrics` are JSON objects, set with
`set_scope`, `set_completeness` and `set_metrics`; no value of the envelope may hold an object
with two equal keys; and every diagnostic code is a `DiagCode`, a letter of the contract's
areas (`ABCDGLSTVW`) and four digits, so every diagnostic line parses.

Nested values are kept as `RawJson` text, so key order survives without `serde_json`'s
`preserve_order` where the text is made in order: a struct keeps its field order
(`RawJson::new`), a `JsonObject` keeps its insertion order (for keys known only at run time,
such as one per corner), and `RawJson::parse` keeps the given text. `JsonObject` and `RawJson`
also serialize in that order as fields of a struct, so a per-corner object inside each entry of
a tool field keeps the tool's corner order. A map does not: a `BTreeMap`, `JsonMap`,
`serde_json::Value` or `json!` object serializes its keys sorted, and a `HashMap` in an
arbitrary order. A small reformatter prints the result pretty (two-space indents, the layout of
`serde_json::to_string_pretty`: every array element on its own line, numbers as serialized) or
compact; plan Example 1 is illustrative in that respect, binding in its keys, their order and
their values.

`ErrorCode` maps every error code to its exit code (1, 2, 4 or 130). In text mode the error of
a failed command is printed like any other diagnostic: an error about the workspace with its
`W` code, `error[W0010]: revision conflict: --base-rev is rev3 but HEAD is rev4`, followed by
`  help: ...`; `USAGE`, `INTERNAL`, `CANCELLED` and `BACKEND_UNAVAILABLE`, the application's
errors (contract §7: letter `A`), with the code the application passes, such as
`error[A0003]: unknown flag --x` (`Error::to_diagnostic`, `text::render_workspace_error`). JSON
keeps the envelope code. The `text` module has the text-mode lines and the POSIX quoting of
`next_actions`.

## Example

```rust
use rusteda_workspace::{Envelope, Invocation, JsonMap, JsonObject, JsonStyle, RbinWriter};
use rusteda_workspace::{Upstream, Workspace};

# fn main() -> Result<(), rusteda_workspace::Error> {
# let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/tmp/readme-example");
# let _ = std::fs::remove_dir_all(&root);
// `rusteda init`: rev0 without tables.
let init = Invocation::new("rusteda", "0.1.0", "rusteda init", ["rusteda", "init", "-w", "ws"]);
let ws = Workspace::init(&root, &init)?.workspace;

// A mutation names its base revision; HEAD is refused here.
let base = ws.resolve_base("rev0")?;
let build = Invocation::new("demo", "0.1.0", "demo build", ["demo", "build", "--base-rev", "rev0"]);
let mut txn = ws.begin(base, &build)?; // REVISION_CONFLICT if HEAD moved
let cells: Vec<u32> = (0..1000).collect();
let mut table = RbinWriter::new("DEMO", 1)?;
table.section(1, &cells)?;
txn.add_rbin("demo", "demo.rbin", &table, Upstream::new())?;
txn.set_summary("demo", "cells", 1000)?;
let committed = txn.commit(JsonMap::new())?;
assert_eq!(committed.rev.to_string(), "rev1");
assert!(committed.warnings.is_empty());

// A query: HEAD is fine, and the table is mapped, not read.
let rev = ws.resolve_query("HEAD")?;
let demo = ws.revision(rev)?.open_rbin("demo", "DEMO", 1..=1)?;
let view: &[u32] = demo.section(1)?;
assert_eq!(view[999], 999);

// The command's envelope, keys in contract order.
let mut envelope = Envelope::new("demo build");
envelope.workspace = Some("ws".into());
envelope.base_rev = Some(base.rev());
envelope.new_rev = Some(committed.rev);
let mut metrics = JsonObject::new();
metrics.insert("cells", &1000)?;
envelope.set_metrics(metrics)?;
envelope.diagnostics.extend(committed.warnings);
envelope.push_next_action(&["demo", "report", "-w", "ws", "--rev", "rev1"]);
let text = envelope.to_json(JsonStyle::Pretty);
assert!(text.starts_with("{\n  \"schema\": \"rusteda.v1\",\n  \"ok\": true,"));
# std::fs::remove_dir_all(&root).ok();
# Ok(())
# }
```

## Dependencies and safety

Exact pins: `serde`, `serde_json` (`float_roundtrip` only), `thiserror`, `memmap2`, `bytemuck`
and `xxhash-rust` (`xxh3`). Locks are the standard library's `File::lock` family on a separate
open file description per lock; a wait interrupted by a signal starts again. The lock is
internal: every `Workspace` method takes the lock it needs and releases it before it returns,
so no caller can hold it across calls and wait for itself. The only `unsafe` code is the
read-only memory map, in one module with a `SAFETY` comment. It maps only files that the
workspace never writes again, the tables of committed revisions and the cached compiled
libraries, through `Revision::open_rbin` and `LibraryCache::open_rlib`; no public function maps
an arbitrary path, and a source test pins the callers.

## Tests

`cargo test` runs the unit tests and the integration tests (`tests/`): the rbin round trip,
every corruption class, the checks of a table on import (mutated tables included) and the
header upstream entries; reads that survive writes and truncation of the file; a chain of five
revisions with carried and dropped tables and their values; staging files linked to outside
files; conflicts found early and at commit between two threads; revert; forks, also during
commits; crash leftovers; failures after the commit point and of the cleanup of a rejected
command; hostile table paths and special files, inside directory tables too; the history schema
and exit codes; the reserved `ui_state.json`; corners; the library cache and library entries;
the envelope layout, its checks and determinism.
`cargo test --test bench -- --ignored --nocapture` runs the benchmark: open, validate and one
lookup on a 500k-row table set in a fresh process with a warm page cache.

Requirement ids `WS-REQ-nnnn` in comments and tests refer to the workspace specification.
