// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The RustEDA disk workspace, shared verbatim by all RustEDA tools.
//!
//! A workspace is a directory of immutable revisions. Each revision is a directory of tables
//! (`rbin` containers, JSON files or directory snapshots) and a `meta.json` that records the
//! hash of every table and the hashes of the tables it was computed from. Tools are stateless
//! processes: a query maps the tables it needs; a mutation names its base revision, computes
//! its tables lock-free into a staging directory and commits them as a new revision under an
//! exclusive lock, failing with `REVISION_CONFLICT` when HEAD moved in the meantime.
//!
//! The crate is tool-neutral: it knows tables, hashes, revisions and envelopes, never what a
//! netlist or a timing graph is. The NETLIST and LIBERTY codecs that the contract (§1, §5)
//! assigns to this crate, and with them the `rlib` module that holds the normative LIBERTY v1
//! grammar, are deferred until `rusteda-core` is available: this crate does not depend on any
//! other RustEDA crate yet.
//!
//! | Module | Content |
//! | --- | --- |
//! | [`hash`] | xxh3-64 hashes of bytes, files, directories and lists; `xxh3:<16 hex>` |
//! | [`rbin`] | the little-endian `rbin` container: deterministic writer, validating zero-copy reader |
//! | [`rev`] | revisions `rev<N>`, `HEAD` for queries only, object handles `<prefix><N>` |
//! | [`schema`] | `manifest.json`, `meta.json`, `history.jsonl` and `ui_state.json` |
//! | [`sta`] | the read-only view of the timing table `sta.rbin` (`STA` v1) and a builder of such tables |
//! | [`workspace`] | init, open, resolve, transactions and the commit protocol, revert, history, and the locks on `workspace.lock` they take |
//! | [`cache`] | the library cache `cache/<name>-<h16>.lib` / `.rlib` |
//! | [`envelope`] | the `rusteda.v1` JSON envelope, diagnostics and ordered JSON objects |
//! | [`error`] | the error type, envelope error codes, exit codes and `W` diagnostic codes |
//! | [`json`] | the order-preserving JSON reformatter behind the envelope |
//! | [`text`] | text-mode output lines and POSIX shell quoting |
//!
//! Comments cite the requirements of the workspace specification as `WS-REQ-nnnn`.

#![warn(missing_docs)]

// WS-REQ-0035: rbin tables are little-endian and are mapped without conversion.
#[cfg(not(target_endian = "little"))]
compile_error!(
    "rusteda-workspace supports little-endian targets only: rbin tables are little-endian"
);

pub mod cache;
pub mod envelope;
pub mod error;
mod fsutil;
pub mod hash;
pub mod json;
mod lock;
mod mmap;
pub mod netlist;
pub mod rbin;
pub mod rev;
pub mod rlib;
pub mod schema;
pub mod sta;
pub mod text;
pub mod workspace;

pub use cache::{CachedLibrary, LibraryCache, PutError};
pub use envelope::{
    DiagCode, DiagLocation, Diagnostic, Envelope, ErrorInfo, JsonObject, RawJson, Severity,
};
pub use error::{Corruption, Error, ErrorCode, Result, codes, exit};
pub use hash::Hash64;
pub use json::JsonStyle;
pub use rbin::{Rbin, RbinWriter, SectionInfo, UpstreamInfo};
pub use rev::{BadHandle, BaseRev, Handle, Rev, RevSpec};
pub use schema::{
    DroppedEntry, HistoryEntry, HistoryError, JsonMap, LibraryEntry, Manifest, RevisionMeta,
    SourceEntry, TableEntry, TableValues, UiState, Upstream,
};
pub use workspace::{
    Committed, Forked, Initialized, Invocation, Reverted, Revision, Txn, Workspace,
};

#[cfg(test)]
pub(crate) mod testutil;

/// The README's example, compiled and run as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
#[derive(Debug)]
pub struct ReadmeDoctests;

/// The lock on `workspace.lock` is internal (WS-REQ-0051): every `Workspace` method takes the
/// lock it needs and releases it before it returns, so a caller can never hold the lock while
/// calling the workspace, which would wait for itself.
///
/// ```compile_fail
/// let _held = rusteda_workspace::WorkspaceLock::exclusive(std::path::Path::new("ws"));
/// ```
///
/// ```compile_fail
/// let _held = rusteda_workspace::lock::WorkspaceLock::exclusive(std::path::Path::new("ws"));
/// ```
///
/// ```
/// let _open = rusteda_workspace::Workspace::open(std::path::Path::new("no-workspace-here"));
/// ```
#[cfg(doctest)]
#[derive(Debug)]
pub struct LockIsInternal;

/// Only files that the workspace never writes again are memory-mapped (WS-REQ-0037, review
/// R2-C1): there is no public way to map an arbitrary path, so safe code cannot hold a `&[T]`
/// that a write or a truncation of the file changes. A file outside a workspace is read.
///
/// ```compile_fail
/// let _ = rusteda_workspace::Rbin::open("t.rbin", "STA", 1..=1);
/// ```
///
/// ```compile_fail
/// let _ = rusteda_workspace::Rbin::map_immutable(std::path::Path::new("t.rbin"), "STA", 1..=1);
/// ```
///
/// ```
/// let _ = rusteda_workspace::Rbin::read("no-such-table.rbin", "STA", 1..=1);
/// ```
#[cfg(doctest)]
#[derive(Debug)]
pub struct MapsAreInternal;
