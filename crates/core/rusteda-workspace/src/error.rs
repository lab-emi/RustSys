// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Errors of this crate and the error codes of the `rusteda.v1` envelope.
//!
//! [`ErrorCode`] lists every code of the contract with its process exit code (WS-REQ-0093).
//! [`Error`] is the error type of the crate: [`Error::code`] gives its envelope code,
//! [`Error::details`] the structured `error.details`, [`Error::help`] a one-line hint for
//! text mode, and [`Error::to_diagnostic`] the diagnostic that text mode prints: a `W` code
//! for an error about the workspace, the application's own code for `USAGE`, `INTERNAL`,
//! `CANCELLED` and `BACKEND_UNAVAILABLE` (WS-REQ-0097). Every envelope code has a variant, so a
//! command logs the code of its envelope in `history.jsonl` (WS-REQ-0093).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::envelope::{DiagCode, Diagnostic, Severity};
use crate::hash::Hash64;
use crate::rev::Rev;

/// The `W` (workspace) diagnostic codes of this crate (WS-REQ-0097).
///
/// - `W0001` to `W0099`: errors about the workspace, one per envelope error code
///   ([`ErrorCode::diag_code`]); `W0001`, `W0011` and `W0013` are not used: `USAGE`, `INTERNAL`
///   and `CANCELLED`, like `BACKEND_UNAVAILABLE` (`A0501`), are the application's errors and
///   print with its `A` code;
/// - `W0100` to `W0199`: warnings of commands that completed;
/// - `W0200` and above are left to the generic `rusteda` command line.
pub mod codes {
    use crate::envelope::DiagCode;

    /// Warning: the command committed (or moved HEAD), but writing its history line failed
    /// (WS-REQ-0076).
    pub const HISTORY_NOT_WRITTEN: DiagCode = DiagCode::of("W0101");
    /// Warning: the command committed (or moved HEAD), but the directory fsync after the
    /// `manifest.json` rename failed, so a crash may still bring back the previous HEAD
    /// (WS-REQ-0076).
    pub const NOT_DURABLE: DiagCode = DiagCode::of("W0102");
}

/// The result type of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Process exit codes of the RustEDA command-line tools (WS-REQ-0093).
///
/// Exit codes 3 and 5 belong to commands that completed: their envelope has `ok: true`.
pub mod exit {
    /// The command completed; a verdict, if any, is positive.
    pub const OK: i32 = 0;
    /// The command was rejected: usage, input, workspace state or I/O.
    pub const REJECTED: i32 = 1;
    /// An internal error, or a table that failed validation.
    pub const INTERNAL: i32 = 2;
    /// The command completed with a negative verdict.
    pub const NEGATIVE_VERDICT: i32 = 3;
    /// `--base-rev` is not the workspace HEAD.
    pub const REVISION_CONFLICT: i32 = 4;
    /// The command completed without a verdict, for example because a budget ran out.
    pub const INCONCLUSIVE: i32 = 5;
    /// The command was cancelled.
    pub const CANCELLED: i32 = 130;
}

/// An error code of the envelope's `error.code` (WS-REQ-0093).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    /// `USAGE`: the command line is wrong.
    Usage,
    /// `INPUT_REJECTED`: an input file or value was rejected.
    InputRejected,
    /// `NOT_A_WORKSPACE`: the directory has no `manifest.json`.
    NotAWorkspace,
    /// `WORKSPACE_EXISTS`: `init` found an existing workspace.
    WorkspaceExists,
    /// `UNKNOWN_REVISION`: the revision does not exist or cannot be named here.
    UnknownRevision,
    /// `MISSING_TABLE`: the revision has no such table.
    MissingTable,
    /// `UNKNOWN_HANDLE`: an object handle does not resolve.
    UnknownHandle,
    /// `SCHEMA_TOO_NEW`: a file was written with a newer major schema.
    SchemaTooNew,
    /// `IO_ERROR`: reading or writing a file failed.
    IoError,
    /// `BACKEND_UNAVAILABLE`: the compute backend asked for is not compiled in or not usable;
    /// never a silent fallback to another backend (contract §7).
    BackendUnavailable,
    /// `REVISION_CONFLICT`: `--base-rev` is not HEAD.
    RevisionConflict,
    /// `INTERNAL`: a bug.
    Internal,
    /// `CORRUPT_TABLE`: header, bounds or hash validation failed.
    CorruptTable,
    /// `CANCELLED`: the command was cancelled.
    Cancelled,
}

impl ErrorCode {
    /// Every code, in the order of the contract.
    pub const ALL: [ErrorCode; 14] = [
        ErrorCode::Usage,
        ErrorCode::InputRejected,
        ErrorCode::NotAWorkspace,
        ErrorCode::WorkspaceExists,
        ErrorCode::UnknownRevision,
        ErrorCode::MissingTable,
        ErrorCode::UnknownHandle,
        ErrorCode::SchemaTooNew,
        ErrorCode::IoError,
        ErrorCode::BackendUnavailable,
        ErrorCode::RevisionConflict,
        ErrorCode::Internal,
        ErrorCode::CorruptTable,
        ErrorCode::Cancelled,
    ];

    /// The code as written in JSON, e.g. `REVISION_CONFLICT`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "USAGE",
            ErrorCode::InputRejected => "INPUT_REJECTED",
            ErrorCode::NotAWorkspace => "NOT_A_WORKSPACE",
            ErrorCode::WorkspaceExists => "WORKSPACE_EXISTS",
            ErrorCode::UnknownRevision => "UNKNOWN_REVISION",
            ErrorCode::MissingTable => "MISSING_TABLE",
            ErrorCode::UnknownHandle => "UNKNOWN_HANDLE",
            ErrorCode::SchemaTooNew => "SCHEMA_TOO_NEW",
            ErrorCode::IoError => "IO_ERROR",
            ErrorCode::BackendUnavailable => "BACKEND_UNAVAILABLE",
            ErrorCode::RevisionConflict => "REVISION_CONFLICT",
            ErrorCode::Internal => "INTERNAL",
            ErrorCode::CorruptTable => "CORRUPT_TABLE",
            ErrorCode::Cancelled => "CANCELLED",
        }
    }

    /// The process exit code of a command that fails with this code.
    pub const fn exit_code(self) -> i32 {
        match self {
            ErrorCode::Usage
            | ErrorCode::InputRejected
            | ErrorCode::NotAWorkspace
            | ErrorCode::WorkspaceExists
            | ErrorCode::UnknownRevision
            | ErrorCode::MissingTable
            | ErrorCode::UnknownHandle
            | ErrorCode::SchemaTooNew
            | ErrorCode::IoError
            | ErrorCode::BackendUnavailable => exit::REJECTED,
            ErrorCode::RevisionConflict => exit::REVISION_CONFLICT,
            ErrorCode::Internal | ErrorCode::CorruptTable => exit::INTERNAL,
            ErrorCode::Cancelled => exit::CANCELLED,
        }
    }

    /// Reads a code as written by [`ErrorCode::as_str`].
    pub fn parse(text: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.into_iter().find(|c| c.as_str() == text)
    }

    /// The `W` code under which text mode prints an error about the workspace with this
    /// envelope code (WS-REQ-0097): `W0002` `INPUT_REJECTED` to `W0010` `REVISION_CONFLICT` and
    /// `W0012` `CORRUPT_TABLE`, the positions of the codes in the list of contract draft 4.
    /// `None` for `USAGE`, `INTERNAL`, `CANCELLED` and `BACKEND_UNAVAILABLE`: a command line, a
    /// bug, a cancellation and a compute backend belong to the application (contract §7: letter
    /// `A`, `A0501` for an unavailable backend), so they print with the application's code
    /// ([`Error::to_diagnostic`]). JSON keeps the envelope code.
    pub const fn diag_code(self) -> Option<DiagCode> {
        Some(match self {
            ErrorCode::Usage
            | ErrorCode::Internal
            | ErrorCode::Cancelled
            | ErrorCode::BackendUnavailable => return None,
            ErrorCode::InputRejected => const { DiagCode::of("W0002") },
            ErrorCode::NotAWorkspace => const { DiagCode::of("W0003") },
            ErrorCode::WorkspaceExists => const { DiagCode::of("W0004") },
            ErrorCode::UnknownRevision => const { DiagCode::of("W0005") },
            ErrorCode::MissingTable => const { DiagCode::of("W0006") },
            ErrorCode::UnknownHandle => const { DiagCode::of("W0007") },
            ErrorCode::SchemaTooNew => const { DiagCode::of("W0008") },
            ErrorCode::IoError => const { DiagCode::of("W0009") },
            ErrorCode::RevisionConflict => const { DiagCode::of("W0010") },
            ErrorCode::CorruptTable => const { DiagCode::of("W0012") },
        })
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        ErrorCode::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown error code '{text}'")))
    }
}

/// Why a table or a workspace file failed validation; reported as `CORRUPT_TABLE`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Corruption {
    /// The file ends before the end of its header.
    Truncated {
        /// Bytes the header needs.
        needed: u64,
        /// Bytes in the file.
        len: u64,
    },
    /// The file does not start with `RUSTEDA\0`.
    BadMagic,
    /// The header names another kind of table.
    WrongKind {
        /// The kind the reader asked for.
        expected: String,
        /// The kind in the header.
        found: String,
    },
    /// The format version is older than the reader supports. (A newer one is
    /// [`Error::FormatTooNew`], `SCHEMA_TOO_NEW`.)
    UnsupportedVersion {
        /// The version in the header.
        found: u32,
        /// The oldest supported version.
        min: u32,
        /// The newest supported version.
        max: u32,
    },
    /// The header flags are not zero.
    Flags {
        /// The flags in the header.
        flags: u32,
    },
    /// `header_len` does not match the section and upstream counts.
    HeaderLen {
        /// The `header_len` in the header.
        declared: u64,
        /// The length the counts give.
        expected: u64,
    },
    /// A kind or upstream name is not ASCII padded with NUL bytes.
    BadName {
        /// Which name.
        field: &'static str,
    },
    /// A section reaches past the end of the file.
    SectionBounds {
        /// The section id.
        id: u32,
        /// Its offset.
        offset: u64,
        /// Its length in bytes.
        len: u64,
        /// The file length.
        file_len: u64,
    },
    /// A section does not start on an 8-byte boundary.
    Misaligned {
        /// The section id.
        id: u32,
        /// Its offset.
        offset: u64,
    },
    /// A section's element size is zero or does not divide its length.
    ElemSize {
        /// The section id.
        id: u32,
        /// Its element size.
        elem_size: u32,
        /// Its length in bytes.
        len: u64,
    },
    /// Section ids are not strictly increasing: unsorted or duplicate.
    SectionOrder {
        /// The first id out of order.
        id: u32,
    },
    /// A section starts before the end of the header or of the previous section.
    Overlap {
        /// The section id.
        id: u32,
    },
    /// The content hash in the header does not match the content.
    HashMismatch {
        /// The hash in the header.
        stored: Hash64,
        /// The hash of the content.
        computed: Hash64,
    },
    /// A section the reader needs is absent.
    MissingSection {
        /// The section id.
        id: u32,
    },
    /// A section's element size is not the size of the requested element type.
    TypeMismatch {
        /// The section id.
        id: u32,
        /// The element size in the file.
        elem_size: u32,
        /// The size of the requested type.
        type_size: usize,
    },
    /// A table's file or directory hash differs from the hash in `meta.json`.
    TableHash {
        /// The table name.
        table: String,
        /// The hash in `meta.json`.
        stored: Hash64,
        /// The hash of the table on disk.
        computed: Hash64,
    },
    /// A workspace JSON file is malformed or inconsistent.
    Json {
        /// What is wrong.
        reason: String,
    },
    /// The table on disk is not a regular file (or directory): a FIFO, a device, a socket or a
    /// symbolic link, which is never opened (WS-REQ-0025).
    FileType {
        /// What the table must be, e.g. `a regular file`.
        expected: &'static str,
    },
    /// The upstream entries of an rbin table's header are not the table's upstream map in
    /// `meta.json` (WS-REQ-0038).
    HeaderUpstream {
        /// The table name.
        table: String,
    },
    /// An entry of a directory table has a name that is not UTF-8, which no directory table
    /// the workspace commits holds (WS-REQ-0012).
    NotUtf8,
}

impl fmt::Display for Corruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Corruption::Truncated { needed, len } => {
                write!(
                    f,
                    "truncated: the file has {len} bytes, its header needs {needed}"
                )
            }
            Corruption::BadMagic => f.write_str("not an rbin file (bad magic)"),
            Corruption::WrongKind { expected, found } => {
                write!(f, "the table kind is {found}, expected {expected}")
            }
            Corruption::UnsupportedVersion { found, min, max } => write!(
                f,
                "format version {found} is not supported (this build reads {min} to {max})"
            ),
            Corruption::Flags { flags } => write!(f, "header flags {flags:#x} are not zero"),
            Corruption::HeaderLen { declared, expected } => write!(
                f,
                "header_len is {declared}, but the section and upstream counts give {expected}"
            ),
            Corruption::BadName { field } => {
                write!(f, "the {field} is not ASCII padded with NUL bytes")
            }
            Corruption::SectionBounds {
                id,
                offset,
                len,
                file_len,
            } => write!(
                f,
                "section {id} ({len} bytes at offset {offset}) ends past the end of the file \
                 ({file_len} bytes)"
            ),
            Corruption::Misaligned { id, offset } => {
                write!(
                    f,
                    "section {id} starts at offset {offset}, not a multiple of 8"
                )
            }
            Corruption::ElemSize { id, elem_size, len } => write!(
                f,
                "section {id}: length {len} is not a multiple of the element size {elem_size}"
            ),
            Corruption::SectionOrder { id } => {
                write!(f, "section ids are not strictly increasing at section {id}")
            }
            Corruption::Overlap { id } => {
                write!(
                    f,
                    "section {id} overlaps the header or the previous section"
                )
            }
            Corruption::HashMismatch { stored, computed } => write!(
                f,
                "content hash mismatch: the header records {stored}, the content hashes to \
                 {computed}"
            ),
            Corruption::MissingSection { id } => write!(f, "section {id} is missing"),
            Corruption::TypeMismatch {
                id,
                elem_size,
                type_size,
            } => write!(
                f,
                "section {id} has {elem_size}-byte elements, the reader expects {type_size} bytes"
            ),
            Corruption::TableHash {
                table,
                stored,
                computed,
            } => write!(
                f,
                "table {table} hashes to {computed}, but meta.json records {stored}"
            ),
            Corruption::Json { reason } => write!(f, "malformed: {reason}"),
            Corruption::FileType { expected } => write!(f, "not {expected}"),
            Corruption::HeaderUpstream { table } => write!(
                f,
                "the header's upstream entries are not the upstream of table {table} in meta.json"
            ),
            Corruption::NotUtf8 => f.write_str("the name is not UTF-8"),
        }
    }
}

/// The error type of this crate; [`Error::code`] gives its envelope error code.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The command line is wrong (`USAGE`).
    #[error("{0}")]
    Usage(String),
    /// An input was rejected (`INPUT_REJECTED`).
    #[error("{0}")]
    InputRejected(String),
    /// The directory is not a workspace (`NOT_A_WORKSPACE`).
    #[error("not a workspace: {} has no manifest.json", .path.display())]
    NotAWorkspace {
        /// The directory.
        path: PathBuf,
    },
    /// `init` found an existing workspace (`WORKSPACE_EXISTS`).
    #[error("a workspace already exists in {}", .path.display())]
    WorkspaceExists {
        /// The directory.
        path: PathBuf,
    },
    /// A revision does not exist or cannot be named here (`UNKNOWN_REVISION`).
    #[error("unknown revision '{spec}': {reason}")]
    UnknownRevision {
        /// The revision as given.
        spec: String,
        /// Why it does not resolve.
        reason: String,
    },
    /// A revision lacks a table (`MISSING_TABLE`).
    #[error("{rev} has no table '{table}'")]
    MissingTable {
        /// The revision.
        rev: Rev,
        /// The table name.
        table: String,
    },
    /// A table of the base revision does not carry into the revision being built
    /// (`MISSING_TABLE`).
    #[error("table '{table}' does not carry into the new revision: {reason}")]
    DroppedTable {
        /// The table name.
        table: String,
        /// Why it is dropped.
        reason: String,
    },
    /// An object handle does not resolve (`UNKNOWN_HANDLE`).
    #[error("unknown handle '{handle}': {reason}")]
    UnknownHandle {
        /// The handle as given.
        handle: String,
        /// Why it does not resolve.
        reason: String,
    },
    /// A file has a newer major schema (`SCHEMA_TOO_NEW`).
    #[error("{} has schema {found}; this build reads {supported}", .path.display())]
    SchemaTooNew {
        /// The file.
        path: PathBuf,
        /// The schema in the file.
        found: String,
        /// The schema this build reads.
        supported: String,
    },
    /// An rbin table has a newer format version than this build reads (`SCHEMA_TOO_NEW`,
    /// WS-REQ-0039): it was written by a newer release.
    #[error(
        "{}: {kind} format version {found} is newer than this build reads ({min} to {max})",
        .path.display()
    )]
    FormatTooNew {
        /// The file.
        path: PathBuf,
        /// The table kind.
        kind: String,
        /// The format version in the header.
        found: u32,
        /// The oldest version this build reads.
        min: u32,
        /// The newest version this build reads.
        max: u32,
    },
    /// Reading or writing a file failed (`IO_ERROR`).
    #[error("{}: {source}", .path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
    /// `--base-rev` is not HEAD (`REVISION_CONFLICT`).
    #[error("revision conflict: --base-rev is {base} but HEAD is {head}")]
    RevisionConflict {
        /// The base revision of the command.
        base: Rev,
        /// The workspace HEAD.
        head: Rev,
    },
    /// A table or workspace file failed validation (`CORRUPT_TABLE`).
    #[error("{}: {what}", .path.display())]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What failed.
        what: Corruption,
    },
    /// A bug: an API was used against its contract (`INTERNAL`).
    #[error("internal error: {0}")]
    Internal(String),
    /// The compute backend asked for is not compiled in or not usable on this machine
    /// (`BACKEND_UNAVAILABLE`, exit 1; contract §7): never a silent fallback to another backend.
    /// The application's error, like `USAGE`: text mode prints it with the application's code
    /// (`A0501`), and it reaches the history of a rejected command through
    /// [`Workspace::log_rejected`](crate::Workspace::log_rejected) or
    /// [`Txn::reject`](crate::Txn::reject) with its own code.
    #[error("{0}")]
    BackendUnavailable(String),
    /// The command was cancelled (`CANCELLED`).
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// A `USAGE` error.
    pub fn usage(message: impl Into<String>) -> Error {
        Error::Usage(message.into())
    }

    /// An `INPUT_REJECTED` error.
    pub fn input_rejected(message: impl Into<String>) -> Error {
        Error::InputRejected(message.into())
    }

    /// An `INTERNAL` error.
    pub fn internal(message: impl Into<String>) -> Error {
        Error::Internal(message.into())
    }

    /// A `BACKEND_UNAVAILABLE` error, e.g. `cuda: no usable driver`.
    pub fn backend_unavailable(message: impl Into<String>) -> Error {
        Error::BackendUnavailable(message.into())
    }

    /// An `IO_ERROR` for `path`.
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Error {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    /// A `CORRUPT_TABLE` error for `path`.
    pub fn corrupt(path: impl Into<PathBuf>, what: Corruption) -> Error {
        Error::Corrupt {
            path: path.into(),
            what,
        }
    }

    /// The envelope error code.
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Usage(_) => ErrorCode::Usage,
            Error::InputRejected(_) => ErrorCode::InputRejected,
            Error::NotAWorkspace { .. } => ErrorCode::NotAWorkspace,
            Error::WorkspaceExists { .. } => ErrorCode::WorkspaceExists,
            Error::UnknownRevision { .. } => ErrorCode::UnknownRevision,
            Error::MissingTable { .. } | Error::DroppedTable { .. } => ErrorCode::MissingTable,
            Error::UnknownHandle { .. } => ErrorCode::UnknownHandle,
            Error::SchemaTooNew { .. } | Error::FormatTooNew { .. } => ErrorCode::SchemaTooNew,
            Error::Io { .. } => ErrorCode::IoError,
            Error::RevisionConflict { .. } => ErrorCode::RevisionConflict,
            Error::Corrupt { .. } => ErrorCode::CorruptTable,
            Error::Internal(_) => ErrorCode::Internal,
            Error::BackendUnavailable(_) => ErrorCode::BackendUnavailable,
            Error::Cancelled => ErrorCode::Cancelled,
        }
    }

    /// The process exit code.
    pub fn exit_code(&self) -> i32 {
        self.code().exit_code()
    }

    /// The `W` diagnostic code of this error for text mode, or `None` for `USAGE`, `INTERNAL`,
    /// `CANCELLED` and `BACKEND_UNAVAILABLE`, which print with the application's code
    /// (WS-REQ-0097).
    pub fn diag_code(&self) -> Option<DiagCode> {
        self.code().diag_code()
    }

    /// The error as a diagnostic: severity `error`, its `W` code ([`Error::diag_code`]), or
    /// `app_code`, the application's code (letter `A`) for `USAGE`, `INTERNAL`, `CANCELLED`
    /// and `BACKEND_UNAVAILABLE` (`A0501`); no location, the message and the help. Text mode prints it on stderr like
    /// any other diagnostic, so the line reads as `error[W0010]: revision conflict: ...` or
    /// `error[A0003]: unknown flag --x` (WS-REQ-0097).
    pub fn to_diagnostic(&self, app_code: DiagCode) -> Diagnostic {
        let code = self.diag_code().unwrap_or(app_code);
        let mut diagnostic = Diagnostic::new(Severity::Error, code, self.to_string());
        diagnostic.help = self.help();
        diagnostic
    }

    /// The `error.details` object of the envelope, or `null`.
    pub fn details(&self) -> Value {
        match self {
            Error::NotAWorkspace { path } | Error::WorkspaceExists { path } => {
                json!({ "path": display(path) })
            }
            Error::UnknownRevision { spec, .. } => json!({ "revision": spec }),
            Error::MissingTable { rev, table } => {
                json!({ "rev": rev.to_string(), "table": table })
            }
            Error::DroppedTable { table, reason } => json!({ "table": table, "reason": reason }),
            Error::UnknownHandle { handle, .. } => json!({ "handle": handle }),
            Error::SchemaTooNew {
                path,
                found,
                supported,
            } => {
                json!({ "file": display(path), "schema": found, "supported": supported })
            }
            Error::FormatTooNew {
                path,
                kind,
                found,
                min,
                max,
            } => json!({
                "file": display(path),
                "kind": kind,
                "format_version": found,
                "min": min,
                "max": max,
            }),
            Error::Io { path, source } => {
                json!({ "path": display(path), "kind": format!("{:?}", source.kind()) })
            }
            Error::RevisionConflict { base, head } => {
                json!({ "base_rev": base.to_string(), "head": head.to_string() })
            }
            Error::Corrupt { path, .. } => json!({ "file": display(path) }),
            Error::Usage(_)
            | Error::InputRejected(_)
            | Error::Internal(_)
            | Error::BackendUnavailable(_)
            | Error::Cancelled => Value::Null,
        }
    }

    /// A one-line hint for text mode, if there is a useful one.
    pub fn help(&self) -> Option<String> {
        let text = match self {
            Error::NotAWorkspace { .. } => {
                "pass the workspace directory with -w, or create one with `rusteda init`".into()
            }
            Error::WorkspaceExists { .. } => {
                "use the existing workspace, or choose another directory".into()
            }
            Error::UnknownRevision { .. } => "list the revisions with `rusteda log`".into(),
            Error::MissingTable { .. } => {
                "run the command that produces this table on this revision first".into()
            }
            Error::DroppedTable { .. } => {
                "compute the table again from the tables of the new revision".into()
            }
            Error::RevisionConflict { head, .. } => {
                format!("HEAD moved; review {head} and rerun with --base-rev {head}")
            }
            Error::SchemaTooNew { .. } | Error::FormatTooNew { .. } => {
                "the workspace was written by a newer RustEDA release; upgrade the tools".into()
            }
            Error::Corrupt { path, what } => corrupt_help(path, what).into(),
            Error::BackendUnavailable(_) => {
                "choose a backend that this build has and this machine runs; cpu always runs".into()
            }
            _ => return None,
        };
        Some(text)
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// What to do about a file that failed validation; it depends on the file (CW-5): a revert
/// helps against a damaged table or revision, not against a damaged workspace file.
fn corrupt_help(path: &Path, what: &Corruption) -> &'static str {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let in_cache = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "cache");
    let old_format = matches!(what, Corruption::UnsupportedVersion { .. });
    match name {
        "manifest.json" => {
            "manifest.json is damaged, so no command can run; repair it by hand: \"head\" is the \
             newest good revision and \"next_rev\" one more than the highest revision directory"
        }
        "history.jsonl" => {
            "history.jsonl is damaged; move it aside to continue (the revisions are not affected)"
        }
        "ui_state.json" => "ui_state.json is damaged; delete it and select again in the GUI",
        "meta.json" => {
            "the metadata of this revision is damaged; point HEAD at another revision with \
             `rusteda revert`"
        }
        _ if in_cache && old_format => {
            "the compiled library has a format this build no longer reads; import the library \
             again, which compiles it again from its cached .lib"
        }
        _ if in_cache => "the cached library is damaged; delete it and import the library again",
        _ if old_format => {
            "the table has a format version this build no longer reads; compute the table again"
        }
        _ => "the table failed validation; revert to an earlier revision or compute it again",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // WS-REQ-0093
    #[test]
    fn codes_and_exit_codes() {
        let expected = [
            ("USAGE", 1),
            ("INPUT_REJECTED", 1),
            ("NOT_A_WORKSPACE", 1),
            ("WORKSPACE_EXISTS", 1),
            ("UNKNOWN_REVISION", 1),
            ("MISSING_TABLE", 1),
            ("UNKNOWN_HANDLE", 1),
            ("SCHEMA_TOO_NEW", 1),
            ("IO_ERROR", 1),
            ("BACKEND_UNAVAILABLE", 1),
            ("REVISION_CONFLICT", 4),
            ("INTERNAL", 2),
            ("CORRUPT_TABLE", 2),
            ("CANCELLED", 130),
        ];
        assert_eq!(ErrorCode::ALL.len(), expected.len());
        for (code, (text, exit)) in ErrorCode::ALL.into_iter().zip(expected) {
            assert_eq!(code.as_str(), text);
            assert_eq!(code.exit_code(), exit, "{text}");
            assert_eq!(ErrorCode::parse(text), Some(code));
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{text}\""));
            assert_eq!(serde_json::from_str::<ErrorCode>(&json).unwrap(), code);
        }
        assert_eq!(ErrorCode::parse("usage"), None);
        assert!(serde_json::from_str::<ErrorCode>("\"NOPE\"").is_err());
    }

    #[test]
    fn errors_map_to_codes_details_and_help() {
        let e = Error::RevisionConflict {
            base: Rev(3),
            head: Rev(4),
        };
        assert_eq!(e.code(), ErrorCode::RevisionConflict);
        assert_eq!(e.exit_code(), 4);
        assert_eq!(
            e.to_string(),
            "revision conflict: --base-rev is rev3 but HEAD is rev4"
        );
        assert_eq!(e.details(), json!({ "base_rev": "rev3", "head": "rev4" }));
        assert_eq!(
            e.help().unwrap(),
            "HEAD moved; review rev4 and rerun with --base-rev rev4"
        );

        let e = Error::corrupt("t.rbin", Corruption::MissingSection { id: 7 });
        assert_eq!(e.code(), ErrorCode::CorruptTable);
        assert_eq!(e.exit_code(), 2);
        assert_eq!(e.to_string(), "t.rbin: section 7 is missing");

        let e = Error::io("x", io::Error::new(io::ErrorKind::NotFound, "gone"));
        assert_eq!(e.code(), ErrorCode::IoError);
        assert_eq!(e.details()["kind"], "NotFound");
        assert!(std::error::Error::source(&e).is_some());

        assert_eq!(Error::Cancelled.exit_code(), 130);
        assert_eq!(Error::usage("bad flag").details(), Value::Null);
        assert_eq!(Error::internal("bug").help(), None);
    }

    // WS-REQ-0097 (review CW-8, CR2-8): every envelope code about the workspace has its own W
    // code, fixed once (the positions of contract draft 4), so the text-mode error line has the
    // shape of a diagnostic line. USAGE, INTERNAL, CANCELLED and BACKEND_UNAVAILABLE have none:
    // they print with the application's A code.
    #[test]
    fn diagnostic_codes() {
        let expected = [
            (ErrorCode::InputRejected, "W0002"),
            (ErrorCode::NotAWorkspace, "W0003"),
            (ErrorCode::WorkspaceExists, "W0004"),
            (ErrorCode::UnknownRevision, "W0005"),
            (ErrorCode::MissingTable, "W0006"),
            (ErrorCode::UnknownHandle, "W0007"),
            (ErrorCode::SchemaTooNew, "W0008"),
            (ErrorCode::IoError, "W0009"),
            (ErrorCode::RevisionConflict, "W0010"),
            (ErrorCode::CorruptTable, "W0012"),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for code in ErrorCode::ALL {
            let Some(d) = code.diag_code() else {
                assert!(
                    matches!(
                        code,
                        ErrorCode::Usage
                            | ErrorCode::Internal
                            | ErrorCode::Cancelled
                            | ErrorCode::BackendUnavailable
                    ),
                    "{code}"
                );
                continue;
            };
            let w = expected.iter().find(|(c, _)| *c == code).unwrap().1;
            assert_eq!(d.as_str(), w, "{code}");
            assert_eq!(d.letter(), 'W');
            assert!(seen.insert(d), "{d} is used twice");
        }
        assert_eq!(seen.len(), expected.len());
        assert_eq!(
            ErrorCode::RevisionConflict.diag_code(),
            Some(DiagCode::of("W0010"))
        );
        assert!(!seen.contains(&codes::HISTORY_NOT_WRITTEN));
        assert!(!seen.contains(&codes::NOT_DURABLE));
        let app = DiagCode::of("A0003");
        let e = Error::RevisionConflict {
            base: Rev(3),
            head: Rev(4),
        };
        let d = e.to_diagnostic(app);
        assert_eq!((d.severity, d.code.as_str()), (Severity::Error, "W0010"));
        assert_eq!(d.location, None);
        assert_eq!(d.message, e.to_string());
        assert_eq!(d.help, e.help());
        assert_eq!(
            e.code(),
            ErrorCode::RevisionConflict,
            "JSON keeps the envelope code"
        );
        // The application's errors take its code (the timing tool's: A0003 command line, A0002
        // internal).
        for (error, code) in [
            (Error::usage("unknown flag --x"), "A0003"),
            (Error::internal("bug"), "A0002"),
            (Error::Cancelled, "A0130"),
        ] {
            assert_eq!(error.diag_code(), None, "{error}");
            let d = error.to_diagnostic(DiagCode::parse(code).unwrap());
            assert_eq!(d.code.as_str(), code, "{error}");
        }
        let corrupt = Error::corrupt("t.rbin", Corruption::BadMagic);
        assert_eq!(corrupt.to_diagnostic(app).code.as_str(), "W0012");
    }

    // WS-REQ-0039 (review CW-5): a newer format version is SCHEMA_TOO_NEW, exit 1.
    #[test]
    fn newer_format_versions() {
        let e = Error::FormatTooNew {
            path: "t.rbin".into(),
            kind: "NETLIST".into(),
            found: 2,
            min: 1,
            max: 1,
        };
        assert_eq!((e.code(), e.exit_code()), (ErrorCode::SchemaTooNew, 1));
        assert_eq!(
            e.to_string(),
            "t.rbin: NETLIST format version 2 is newer than this build reads (1 to 1)"
        );
        assert!(e.help().unwrap().contains("upgrade the tools"));
        assert_eq!(e.details()["format_version"], 2);
    }

    // Review CW-5: the help for a file that failed validation depends on the file.
    #[test]
    fn corrupt_help_depends_on_the_file() {
        let help = |path: &str, what: Corruption| Error::corrupt(path, what).help().unwrap();
        let json = || Corruption::Json { reason: "x".into() };
        let old = || Corruption::UnsupportedVersion {
            found: 1,
            min: 2,
            max: 3,
        };
        assert!(help("ws/manifest.json", json()).contains("repair it by hand"));
        assert!(help("ws/history.jsonl", json()).contains("move it aside"));
        assert!(help("ws/ui_state.json", json()).contains("delete it"));
        assert!(help("ws/revisions/rev0003/meta.json", json()).contains("rusteda revert"));
        let rlib = "ws/cache/a-0123456789abcdef.rlib";
        assert!(help(rlib, Corruption::BadMagic).contains("import the library again"));
        assert!(help(rlib, old()).contains("cached .lib"));
        let table = "ws/revisions/rev0003/sta.rbin";
        assert!(help(table, Corruption::BadMagic).contains("revert to an earlier revision"));
        assert!(help(table, old()).contains("compute the table again"));
        for file in ["ws/manifest.json", "ws/history.jsonl", "ws/ui_state.json"] {
            assert!(!help(file, json()).contains("revert"), "{file}");
        }
    }
}
