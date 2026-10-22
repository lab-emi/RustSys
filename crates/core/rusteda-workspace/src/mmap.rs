// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Read-only memory maps of immutable workspace files: the only `unsafe` code of the crate
//! (WS-REQ-0037).

#![allow(unsafe_code)]

use std::fs::File;
use std::io;

use memmap2::Mmap;

/// Maps a whole file read-only. Crate-private, called only by `Rbin::map_immutable` for a file
/// the workspace never writes again (see the SAFETY comment); a source test pins the callers.
pub(crate) fn map_readonly(file: &File) -> io::Result<Mmap> {
    // SAFETY: `Mmap::map` is unsafe because the bytes behind the map, and every `&[T]`
    // borrowed from it, would change if the file were written while it is mapped, and the
    // process would fault (SIGBUS) on a page that a truncation removed. Neither happens to the
    // files mapped here. The only caller is `Rbin::map_immutable`, which is crate-private and
    // called from exactly two places, both public functions whose files the workspace protocol
    // never writes or truncates:
    //
    // - `Revision::open_rbin` maps a table of a committed revision (a `Revision` exists only
    //   for a visible revision directory, WS-REQ-0023), at a path from its checked `meta.json`
    //   that names an entry of that directory (WS-REQ-0024). Such a file was created new in a
    //   staging directory, written once and fsynced, and published by renaming the directory
    //   (WS-REQ-0036, WS-REQ-0066). Nothing in the crate opens it for writing again
    //   (WS-REQ-0067): carry-forward and forks add hard links to it, never write through them.
    //   No name outside the workspace shares its inode either, since a staging file with a
    //   second link to anything but a file of the base revision is refused when it is added
    //   (WS-REQ-0077).
    // - `LibraryCache::open_rlib` maps `cache/<name>-<h16>.rlib`, which is written to a new
    //   temporary file, fsynced and renamed into place, and is replaced only by another
    //   rename (WS-REQ-0080); a rename leaves the inode that a map holds unchanged.
    //
    // Both check on the open handle that the file is a regular file (WS-REQ-0025), and the map
    // is read-only, so this process cannot write through it. Every other file, in particular
    // any file outside a workspace, is read into memory instead (`Rbin::read`). A program that
    // bypasses the workspace and writes a committed file in place damages the workspace
    // itself; the protocol excludes it, as it excludes deleting revisions.
    unsafe { Mmap::map(file) }
}
