// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The library cache `cache/` of a workspace (WS-REQ-0080 to WS-REQ-0084).
//!
//! Every imported library is kept verbatim as `cache/<name>-<h16>.lib` and compiled as
//! `cache/<name>-<h16>.rlib`, where `h16` is the xxh3-64 of the library bytes in 16 hex digits.
//! Both files are written atomically (temporary file, fsync, rename), so concurrent imports of
//! the same library are harmless, and a library whose two files exist is not compiled again
//! while its `.rlib` is current: a valid rbin file (structure and content hash) of the kind and
//! a format version that the build reads. The caller names that kind and those versions, so a
//! compiled form left by an older release after a format version bump, a damaged one or one of
//! another kind is compiled again from the bytes (contract §5: "format_version bump =>
//! recompile from cache/*.lib"); one of a newer release is `SCHEMA_TOO_NEW` and stays.
//! [`LibraryCache::recompile`] rebuilds the `.rlib` from the verbatim copy, and
//! [`LibraryCache::open_rlib_or_recompile`] and
//! [`Revision::open_library_or_recompile`](crate::Revision::open_library_or_recompile) open a
//! compiled library, compiling it again first when it is not current.
//!
//! A cache belongs to a workspace: it is reached through
//! [`Workspace::cache`](crate::Workspace::cache) and
//! [`Revision::open_library`](crate::Revision::open_library), never for an arbitrary directory,
//! because its compiled libraries are mapped (WS-REQ-0037). The cache reads regular files only
//! and never follows a symbolic link (WS-REQ-0025).

use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use crate::error::{Corruption, Error, Result};
use crate::fsutil;
use crate::hash::Hash64;
use crate::rbin::Rbin;
use crate::schema::LibraryEntry;
use crate::workspace::layout;

/// A failure of [`LibraryCache::put_library`], [`LibraryCache::recompile`] or an open that
/// compiles again.
#[derive(Debug, thiserror::Error)]
pub enum PutError<E> {
    /// The compiler rejected the library.
    #[error("compiling the library failed: {0}")]
    Compile(E),
    /// The cache could not be read or written; a cached file is damaged (`CORRUPT_TABLE`) or
    /// of a newer release (`SCHEMA_TOO_NEW`); or the compiler returned something that is not a
    /// valid rbin file of the kind and versions asked for (`INTERNAL`).
    #[error(transparent)]
    Workspace(#[from] Error),
}

/// A library in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedLibrary {
    /// The library name.
    pub name: String,
    /// The hash of the library bytes.
    pub hash: Hash64,
    /// The verbatim copy, relative to the workspace: `cache/<name>-<h16>.lib`.
    pub lib: String,
    /// The compiled library, relative to the workspace: `cache/<name>-<h16>.rlib`.
    pub rlib: String,
    /// The verbatim copy.
    pub lib_path: PathBuf,
    /// The compiled library.
    pub rlib_path: PathBuf,
    /// Whether this call ran the compiler.
    pub compiled_now: bool,
}

impl CachedLibrary {
    /// The `meta.json` entry of this library in `corner`, imported from `source`.
    pub fn entry(&self, corner: impl Into<String>, source: impl Into<String>) -> LibraryEntry {
        LibraryEntry {
            name: self.name.clone(),
            corner: corner.into(),
            hash: self.hash,
            source: source.into(),
            compiled: self.rlib.clone(),
        }
    }
}

/// The library cache of a workspace, from [`Workspace::cache`](crate::Workspace::cache).
#[derive(Debug, Clone)]
pub struct LibraryCache {
    root: PathBuf,
}

impl LibraryCache {
    /// The cache of the workspace at `workspace_root`. Crate-private (WS-REQ-0037): a cache
    /// exists only for an open workspace (`Workspace::cache`), since its compiled libraries
    /// are mapped, and a map is sound only for files that the workspace never writes again.
    pub(crate) fn new(workspace_root: impl Into<PathBuf>) -> LibraryCache {
        LibraryCache {
            root: workspace_root.into(),
        }
    }

    /// The cache directory.
    pub fn dir(&self) -> PathBuf {
        self.root.join(layout::CACHE)
    }

    fn located(&self, name: &str, hash: Hash64) -> CachedLibrary {
        let stem = format!("{name}-{}", hash.hex());
        let lib = format!("{}/{stem}.lib", layout::CACHE);
        let rlib = rlib_path(name, hash);
        CachedLibrary {
            name: name.to_string(),
            hash,
            lib_path: self.root.join(&lib),
            rlib_path: self.root.join(&rlib),
            lib,
            rlib,
            compiled_now: false,
        }
    }

    /// Stores library `name` with its bytes and compiled form; `kind` and `versions` are the
    /// rbin kind and the format versions of compiled libraries that this build reads, such as
    /// `"LIBERTY", 1..=1`. The compiler runs only when the `.rlib` is not current: missing, not
    /// a valid rbin file (structure and content hash), of another kind, or of a format version
    /// below `versions`, as after a format version bump (WS-REQ-0080, WS-REQ-0083). Its output
    /// must be a valid rbin file of `kind` and `versions` (otherwise `INTERNAL`). Nothing is
    /// written when both files exist and the `.rlib` is current. An `.rlib` of a format
    /// version above `versions`, written by a newer release, is `SCHEMA_TOO_NEW` and stays
    /// (WS-REQ-0039). Library names are 1 to 128 characters of `[A-Za-z0-9._+-]`, not starting
    /// with `.`.
    pub fn put_library<E>(
        &self,
        name: &str,
        lib_bytes: &[u8],
        kind: &str,
        versions: RangeInclusive<u32>,
        compile: impl FnOnce(&[u8]) -> std::result::Result<Vec<u8>, E>,
    ) -> std::result::Result<CachedLibrary, PutError<E>> {
        check_library_name(name)?;
        let mut library = self.located(name, Hash64::of(lib_bytes));
        let has_lib = is_regular_file(&library.lib_path);
        let current = is_current_rlib(&library.rlib_path, kind, &versions)?;
        if has_lib && current {
            return Ok(library);
        }
        fsutil::create_dir_all(&self.dir())?;
        if !has_lib {
            fsutil::write_atomic(&library.lib_path, lib_bytes)?;
        }
        if !current {
            let compiled = compile(lib_bytes).map_err(PutError::Compile)?;
            let compiled = check_compiled(name, compiled, kind, &versions)?;
            fsutil::write_atomic(&library.rlib_path, compiled.as_bytes())?;
            library.compiled_now = true;
        }
        Ok(library)
    }

    /// Compiles the cached verbatim library again and replaces its `.rlib` atomically, e.g.
    /// after a format version change (WS-REQ-0083). The verbatim copy must still hash to
    /// `hash` (otherwise `CORRUPT_TABLE`), and the compiled bytes must be a valid rbin file of
    /// `kind` and `versions` (otherwise `INTERNAL`); on failure the old `.rlib` stays.
    pub fn recompile<E>(
        &self,
        name: &str,
        hash: Hash64,
        kind: &str,
        versions: RangeInclusive<u32>,
        compile: impl FnOnce(&[u8]) -> std::result::Result<Vec<u8>, E>,
    ) -> std::result::Result<CachedLibrary, PutError<E>> {
        check_library_name(name)?;
        let mut library = self.located(name, hash);
        // WS-REQ-0025: a regular file only; a FIFO in its place would block the read.
        let bytes = fsutil::read_regular(&library.lib_path)?;
        let computed = Hash64::of(&bytes);
        if computed != hash {
            let what = Corruption::TableHash {
                table: name.to_string(),
                stored: hash,
                computed,
            };
            return Err(Error::corrupt(&library.lib_path, what).into());
        }
        let compiled = compile(&bytes).map_err(PutError::Compile)?;
        let compiled = check_compiled(name, compiled, kind, &versions)?;
        fsutil::write_atomic(&library.rlib_path, compiled.as_bytes())?;
        library.compiled_now = true;
        Ok(library)
    }

    /// The cached library `name` with `hash`, if both of its files exist as regular files
    /// (WS-REQ-0081; a symbolic link is not followed). Whether its `.rlib` is current for a
    /// build is checked when it is stored ([`LibraryCache::put_library`]) and opened.
    pub fn lookup(&self, name: &str, hash: Hash64) -> Option<CachedLibrary> {
        check_library_name(name).ok()?;
        let library = self.located(name, hash);
        (is_regular_file(&library.lib_path) && is_regular_file(&library.rlib_path))
            .then_some(library)
    }

    /// Every cached library with `hash`, whatever its name, sorted by name (WS-REQ-0081).
    pub fn find(&self, hash: Hash64) -> Result<Vec<CachedLibrary>> {
        let dir = self.dir();
        let suffix = format!("-{}.lib", hash.hex());
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(dir, e)),
        };
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(&dir, e))?;
            let Ok(file) = entry.file_name().into_string() else {
                continue;
            };
            if let Some(name) = file.strip_suffix(&suffix) {
                if let Some(library) = self.lookup(name, hash) {
                    found.push(library);
                }
            }
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }

    /// Maps the compiled library `cache/<name>-<h16>.rlib` of this workspace read-only and
    /// validates its header (`kind`, `versions`; WS-REQ-0032, WS-REQ-0082). The workspace
    /// replaces the file only by a rename, never writes it in place, so it can be mapped
    /// (WS-REQ-0037). It must be a regular file, checked without following a symbolic link
    /// (WS-REQ-0025); otherwise `CORRUPT_TABLE`, as is a format version below `versions`
    /// (see [`LibraryCache::open_rlib_or_recompile`]); a missing file is `IO_ERROR`.
    pub fn open_rlib(
        &self,
        name: &str,
        hash: Hash64,
        kind: &str,
        versions: RangeInclusive<u32>,
    ) -> Result<Rbin> {
        check_library_name(name)?;
        Rbin::map_immutable(&self.located(name, hash).rlib_path, kind, versions)
    }

    /// [`LibraryCache::open_rlib`], compiling the library again from its verbatim copy first
    /// when its `.rlib` cannot be opened as `kind` and `versions` but the copy can replace it
    /// (WS-REQ-0084): the `.rlib` is missing, damaged, of another kind, or of a format version
    /// below `versions` (contract §5: "format_version bump => recompile from cache/*.lib"). The
    /// copy must hash to `hash` ([`LibraryCache::recompile`]). An `.rlib` of a newer release is
    /// `SCHEMA_TOO_NEW` and stays; the compiler runs at most once.
    pub fn open_rlib_or_recompile<E>(
        &self,
        name: &str,
        hash: Hash64,
        kind: &str,
        versions: RangeInclusive<u32>,
        compile: impl FnOnce(&[u8]) -> std::result::Result<Vec<u8>, E>,
    ) -> std::result::Result<Rbin, PutError<E>> {
        match self.open_rlib(name, hash, kind, versions.clone()) {
            Err(e) if is_stale(&e) => {
                self.recompile(name, hash, kind, versions.clone(), compile)?;
                Ok(self.open_rlib(name, hash, kind, versions)?)
            }
            opened => Ok(opened?),
        }
    }
}

/// The path of the compiled library `name` with `hash`, relative to the workspace:
/// `cache/<name>-<h16>.rlib` (contract §2), as a library entry of `meta.json` records it.
pub(crate) fn rlib_path(name: &str, hash: Hash64) -> String {
    format!("{}/{name}-{}.rlib", layout::CACHE, hash.hex())
}

/// Whether `path` itself, not following a symbolic link, is a regular file.
fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Whether the cached `.rlib` at `path` is current for a build that reads `kind` and
/// `versions` (WS-REQ-0080, WS-REQ-0083): a regular file and a valid rbin file of that kind and
/// a format version in `versions`, with a matching content hash. `false` when it is missing or
/// fails a check, so it is compiled again; one of a newer format version is `SCHEMA_TOO_NEW`.
fn is_current_rlib(path: &Path, kind: &str, versions: &RangeInclusive<u32>) -> Result<bool> {
    let opened = Rbin::read(path, kind, versions.clone()).and_then(|rbin| rbin.verify_hash());
    match opened {
        Ok(()) => Ok(true),
        Err(e) if is_stale(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether an error of opening a cached `.rlib` means that compiling the library again from
/// its verbatim copy replaces it: the file is missing or fails validation, a format version
/// below the build's included. A newer format version (`SCHEMA_TOO_NEW`) or any other I/O
/// error does not.
fn is_stale(error: &Error) -> bool {
    match error {
        Error::Corrupt { .. } => true,
        Error::Io { source, .. } => source.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

/// The compiled form of a library is a valid rbin file (structure and content hash) of `kind`
/// and a format version in `versions`; anything else is a bug of the compiler, `INTERNAL`
/// (WS-REQ-0080). Returns it, validated.
fn check_compiled(
    name: &str,
    compiled: Vec<u8>,
    kind: &str,
    versions: &RangeInclusive<u32>,
) -> Result<Rbin> {
    Rbin::from_bytes(compiled, kind, versions.clone())
        .and_then(|rbin| rbin.verify_hash().map(|()| rbin))
        .map_err(|e| {
            Error::internal(format!(
                "the compiled form of library {name} is not a valid rbin file of kind {kind} and \
                 format version {}..={}: {e}",
                versions.start(),
                versions.end()
            ))
        })
}

/// Whether `name` is a library name of the cache: 1 to 128 characters of `[A-Za-z0-9._+-]`,
/// not starting with `.`.
pub(crate) fn is_library_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

fn check_library_name(name: &str) -> Result<()> {
    if !is_library_name(name) {
        return Err(Error::input_rejected(format!(
            "library name '{name}' must be 1 to 128 characters of [A-Za-z0-9._+-], not starting \
             with '.'"
        )));
    }
    Ok(())
}
