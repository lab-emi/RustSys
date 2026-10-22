// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The `rbin` table container.
//!
//! An `rbin` file is a little-endian header followed by sections of fixed-size elements
//! (WS-REQ-0030):
//!
//! | offset | size | field |
//! | ---: | ---: | --- |
//! | 0 | 8 | magic `RUSTEDA\0` |
//! | 8 | 8 | kind: ASCII padded with NUL bytes, e.g. `NETLIST`, `LIBERTY`, `CONSTR`, `STA` |
//! | 16 | 4 | `format_version` of the kind |
//! | 20 | 4 | `flags`, always 0 |
//! | 24 | 8 | `content_hash`: xxh3-64 of the bytes `[header_len, EOF)` |
//! | 32 | 4 | `section_count` |
//! | 36 | 4 | `upstream_count` |
//! | 40 | 8 | `header_len` = 48 + 16 `upstream_count` + 24 `section_count` |
//! | 48 | 16 each | upstream entries: `name` (8, ASCII, NUL-padded), `hash` u64 |
//! | | 24 each | section entries: `id` u32, `elem_size` u32, `offset` u64, `len` u64 (bytes) |
//!
//! The sections follow the header in strictly increasing id order, each at an 8-byte aligned
//! offset; the gaps and the end of the file are zero padding (WS-REQ-0031). [`RbinWriter`]
//! writes this layout deterministically: the bytes depend only on the kind, the version, the
//! upstream entries (sorted by name) and the sections (sorted by id).
//!
//! Opening an rbin always validates the header: magic, kind, version, flags, `header_len`,
//! bounds, alignment, `len % elem_size == 0`, strictly increasing ids and non-overlapping
//! sections (WS-REQ-0032). [`Rbin::section`] then returns zero-copy slices (WS-REQ-0033);
//! [`Rbin::verify_hash`] checks the content hash on request (WS-REQ-0034). Every validation
//! failure is an [`Error::Corrupt`] (`CORRUPT_TABLE`), except a format version newer than the
//! reader's, which is [`Error::FormatTooNew`] (`SCHEMA_TOO_NEW`, WS-REQ-0039).
//!
//! Only files that nobody writes while they are mapped are mapped (WS-REQ-0037): the tables of
//! committed revisions, through [`Revision::open_rbin`](crate::Revision::open_rbin), and the
//! compiled libraries of the cache, through
//! [`LibraryCache::open_rlib`](crate::LibraryCache::open_rlib) and
//! [`Revision::open_library`](crate::Revision::open_library). Any other file is read into an
//! 8-byte aligned buffer with [`Rbin::read`], and bytes in memory are opened with
//! [`Rbin::from_bytes`]; the views are zero-copy within that buffer.
//!
//! A table that enters a revision as an rbin file is checked as it enters: its structure (the
//! checks above except kind and version, which only the tool knows) and its content hash
//! (contract §5, "content hash on import"; WS-REQ-0078).
//!
//! # Upstream entries of workspace tables
//!
//! The header of an rbin table of a revision records the table's upstream map from `meta.json`
//! (WS-REQ-0038): one entry per upstream table (or libraries pseudo-table) holding the hash
//! that `meta.json` records for it, under the 8-byte name [`upstream_names`] gives. A name of at
//! most 8 bytes is used as it is (`netlist`, `rtl`, `sta`); a longer one is shortened to its
//! first 3 bytes, `.` and 4 hex digits derived from its hash (`constraints`, `libraries`),
//! chosen so that no two names of one map collide. The names depend only on the map, so the
//! writer and every reader derive the same ones.
//! [`Txn::add_rbin`](crate::Txn::add_rbin) writes these entries, the other `add_*` methods
//! check them in a table that is an rbin file, and
//! [`Revision::open_rbin`](crate::Revision::open_rbin) checks them again.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use bytemuck::Pod;
use memmap2::Mmap;

use crate::error::{Corruption, Error, Result};
use crate::fsutil;
use crate::hash::{Hash64, Hasher};
use crate::mmap;
use crate::schema::Upstream;

/// The first 8 bytes of every rbin file.
pub const MAGIC: [u8; 8] = *b"RUSTEDA\0";
/// The length of the fixed part of the header.
pub const FIXED_HEADER_LEN: u64 = 48;
/// The length of an upstream entry.
pub const UPSTREAM_ENTRY_LEN: u64 = 16;
/// The length of a section entry.
pub const SECTION_ENTRY_LEN: u64 = 24;
/// Section alignment.
pub const ALIGN: u64 = 8;

/// The header length for the given counts.
pub fn header_len(upstream_count: u64, section_count: u64) -> u64 {
    FIXED_HEADER_LEN + UPSTREAM_ENTRY_LEN * upstream_count + SECTION_ENTRY_LEN * section_count
}

fn align_up(n: u64) -> u64 {
    n.div_ceil(ALIGN) * ALIGN
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
}

/// A kind or upstream name: 1 to 8 bytes of `[A-Za-z0-9_.-]`, padded with NUL bytes.
fn encode_name(name: &str, what: &str) -> Result<[u8; 8]> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 8 || !bytes.iter().copied().all(name_byte) {
        return Err(Error::internal(format!(
            "rbin {what} '{name}' must be 1 to 8 characters of [A-Za-z0-9_.-]"
        )));
    }
    let mut out = [0u8; 8];
    out[..bytes.len()].copy_from_slice(bytes);
    Ok(out)
}

fn decode_name(raw: &[u8]) -> Option<String> {
    let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let valid =
        len > 0 && raw[..len].iter().copied().all(name_byte) && raw[len..].iter().all(|&b| b == 0);
    valid.then(|| raw[..len].iter().map(|&b| char::from(b)).collect())
}

/// The 8-byte header names of the upstream tables of a workspace table, by table name
/// (WS-REQ-0038). A name of at most 8 bytes is its own header name. A longer one becomes its
/// first 3 bytes, `.` and 4 hex digits: the low 16 bits of its xxh3-64, plus one (modulo
/// 2^16) for as long as the result is taken by a name before it (the short names first, then
/// the long ones in byte order). The header names of one map are therefore distinct, and they
/// depend on the map only, so the writer and every reader of a table derive the same names
/// from its `meta.json` upstream map.
///
/// ```
/// use rusteda_workspace::{Hash64, Upstream};
/// use rusteda_workspace::rbin::upstream_names;
///
/// // Two corners whose 16-bit hashes collide still get distinct names.
/// let upstream = Upstream::from([
///     ("netlist".to_string(), Hash64(1)),
///     ("libraries.ss_150C_1v60".to_string(), Hash64(2)),
///     ("libraries.sf_n40C_1v65".to_string(), Hash64(3)),
/// ]);
/// let names = upstream_names(&upstream)?;
/// assert_eq!(names["netlist"], "netlist");
/// assert_eq!(names["libraries.sf_n40C_1v65"], "lib.95ae");
/// assert_eq!(names["libraries.ss_150C_1v60"], "lib.95af");
/// # Ok::<(), rusteda_workspace::Error>(())
/// ```
pub fn upstream_names(upstream: &Upstream) -> Result<BTreeMap<String, String>> {
    let mut names = BTreeMap::new();
    let mut taken = BTreeSet::new();
    for table in upstream.keys() {
        let bytes = table.as_bytes();
        if bytes.is_empty() || !bytes.iter().copied().all(name_byte) {
            return Err(Error::internal(format!(
                "'{table}' cannot name an rbin upstream entry: it needs [A-Za-z0-9_.-]"
            )));
        }
        if bytes.len() <= 8 {
            taken.insert(table.clone());
            names.insert(table.clone(), table.clone());
        }
    }
    for table in upstream.keys().filter(|t| t.len() > 8) {
        let mut suffix = Hash64::of(table.as_bytes()).0 & 0xffff;
        let mut name = None;
        for _ in 0..=0xffff {
            // The bytes are ASCII, so 3 is a char boundary.
            let candidate = format!("{}.{suffix:04x}", &table[..3]);
            if taken.insert(candidate.clone()) {
                name = Some(candidate);
                break;
            }
            suffix = (suffix + 1) & 0xffff;
        }
        let Some(name) = name else {
            return Err(Error::internal(format!(
                "too many upstream tables share the prefix of {table}"
            )));
        };
        names.insert(table.clone(), name);
    }
    Ok(names)
}

/// The header upstream entries of a workspace table with upstream map `upstream`, keyed by
/// padded name (WS-REQ-0038).
pub(crate) fn header_upstream(upstream: &Upstream) -> Result<BTreeMap<[u8; 8], u64>> {
    let names = upstream_names(upstream)?;
    let mut entries = BTreeMap::new();
    for (table, hash) in upstream {
        entries.insert(encode_name(&names[table], "upstream name")?, hash.0);
    }
    Ok(entries)
}

/// Whether header upstream entries, in any order, are exactly `expected`.
fn same_upstream(entries: &[UpstreamInfo], expected: &BTreeMap<[u8; 8], u64>) -> bool {
    let mut entries: Vec<([u8; 8], u64)> = entries
        .iter()
        .map(|u| {
            let mut name = [0u8; 8];
            name[..u.name.len()].copy_from_slice(u.name.as_bytes());
            (name, u.hash.0)
        })
        .collect();
    entries.sort_unstable();
    entries.len() == expected.len()
        && entries
            .iter()
            .zip(expected)
            .all(|(entry, (name, hash))| entry.0 == *name && entry.1 == *hash)
}

/// What the import check of a table file found (WS-REQ-0078).
#[derive(Debug)]
pub(crate) enum Inspected {
    /// Not an rbin file (it does not start with the magic): its file hash.
    Plain(Hash64),
    /// An rbin file with a valid structure and content hash.
    Rbin {
        /// The file hash, the table hash of `meta.json`.
        file_hash: Hash64,
        /// The header's upstream entries.
        upstream: Vec<UpstreamInfo>,
    },
    /// An rbin file that fails validation.
    Corrupt(Corruption),
}

impl Inspected {
    /// Whether an rbin file's upstream entries are exactly `expected`; true for other files.
    pub(crate) fn records_upstream(&self, expected: &BTreeMap<[u8; 8], u64>) -> bool {
        match self {
            Inspected::Rbin { upstream, .. } => same_upstream(upstream, expected),
            _ => true,
        }
    }
}

/// Reads a table file of `file_len` bytes once (WS-REQ-0078): its file hash, and for an rbin
/// file, one that starts with the magic, the checks of every open except kind and version
/// (WS-REQ-0032) and its content hash (WS-REQ-0034).
pub(crate) fn inspect(mut reader: impl Read, file_len: u64) -> io::Result<Inspected> {
    let mut head = Vec::new();
    read_up_to(&mut reader, &mut head, FIXED_HEADER_LEN)?;
    let mut file_hash = Hasher::new();
    if !head.starts_with(&MAGIC) {
        file_hash.update(&head);
        stream(&mut reader, |chunk| file_hash.update(chunk))?;
        return Ok(Inspected::Plain(file_hash.finish()));
    }
    if head.len() as u64 == FIXED_HEADER_LEN {
        // The whole header, when its length is consistent; otherwise the parse reports it.
        let declared = read_u64(&head, 40);
        let counts = header_len(
            u64::from(read_u32(&head, 36)),
            u64::from(read_u32(&head, 32)),
        );
        if declared == counts && declared <= file_len {
            read_up_to(&mut reader, &mut head, declared)?;
        }
    }
    let header = match parse_header(&head, file_len, None) {
        Ok(header) => header,
        Err(what) => return Ok(Inspected::Corrupt(what)),
    };
    file_hash.update(&head);
    let mut content = Hasher::new();
    let rest = stream(&mut reader, |chunk| {
        file_hash.update(chunk);
        content.update(chunk);
    })?;
    if head.len() as u64 + rest != file_len {
        return Err(io::Error::other("the file changed while it was read"));
    }
    let computed = content.finish();
    if computed != header.content_hash {
        return Ok(Inspected::Corrupt(Corruption::HashMismatch {
            stored: header.content_hash,
            computed,
        }));
    }
    Ok(Inspected::Rbin {
        file_hash: file_hash.finish(),
        upstream: header.upstream,
    })
}

/// Reads from `reader` until `buf` holds `target` bytes or the input ends.
fn read_up_to(reader: &mut impl Read, buf: &mut Vec<u8>, target: u64) -> io::Result<()> {
    let want = target.saturating_sub(buf.len() as u64);
    reader.by_ref().take(want).read_to_end(buf)?;
    Ok(())
}

/// Passes the rest of `reader` to `each` in chunks; returns the number of bytes.
fn stream(reader: &mut impl Read, mut each: impl FnMut(&[u8])) -> io::Result<u64> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => {
                each(&buf[..n]);
                total += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Reads all of `reader` into an 8-byte aligned buffer, so that sections can be viewed in
/// place; `hint` is the expected length. Returns the words and the length in bytes.
fn read_aligned(reader: &mut impl Read, hint: u64) -> io::Result<(Vec<u64>, usize)> {
    // One word more than the hint, so that the end of the input is seen without growing.
    let mut words = vec![0u64; usize::try_from(hint).unwrap_or(0).div_ceil(8) + 1];
    let mut len = 0;
    loop {
        if len == words.len() * 8 {
            words.resize(words.len() * 2, 0);
        }
        let buf = bytemuck::cast_slice_mut::<u64, u8>(&mut words);
        match reader.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    words.truncate(len.div_ceil(8));
    Ok((words, len))
}

/// The element size of `T` as stored in a section entry.
fn elem_size_of<T: Pod>() -> Result<u32> {
    let size = size_of::<T>();
    if size == 0 || align_of::<T>() > ALIGN as usize {
        return Err(Error::internal(format!(
            "rbin element type {} has size {size} and alignment {}; sections need a non-zero \
             size and an alignment of at most 8",
            std::any::type_name::<T>(),
            align_of::<T>()
        )));
    }
    u32::try_from(size).map_err(|_| Error::internal("rbin element size exceeds u32"))
}

#[derive(Clone)]
struct PendingSection<'a> {
    elem_size: u32,
    bytes: Cow<'a, [u8]>,
}

impl fmt::Debug for PendingSection<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingSection")
            .field("elem_size", &self.elem_size)
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// Builds an rbin file from sections that borrow (or own) their data.
///
/// ```
/// use rusteda_workspace::{Hash64, Rbin, RbinWriter};
///
/// let names: Vec<u32> = vec![10, 20, 30];
/// let mut w = RbinWriter::new("DEMO", 1)?;
/// w.upstream("NETLIST", Hash64(7))?;
/// w.section(1, &names)?;
/// let table = Rbin::from_bytes(w.to_bytes(), "DEMO", 1..=1)?;
/// assert_eq!(table.section::<u32>(1)?, &[10, 20, 30]);
/// # Ok::<(), rusteda_workspace::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct RbinWriter<'a> {
    kind: [u8; 8],
    format_version: u32,
    upstream: BTreeMap<[u8; 8], u64>,
    sections: BTreeMap<u32, PendingSection<'a>>,
}

impl<'a> RbinWriter<'a> {
    /// A writer for a table kind (1 to 8 characters of `[A-Za-z0-9_.-]`) and format version.
    pub fn new(kind: &str, format_version: u32) -> Result<RbinWriter<'a>> {
        Ok(RbinWriter {
            kind: encode_name(kind, "kind")?,
            format_version,
            upstream: BTreeMap::new(),
            sections: BTreeMap::new(),
        })
    }

    /// Records the hash of something this file was computed from, under a raw header name of
    /// 1 to 8 characters of `[A-Za-z0-9_.-]`; names are unique, and the header lists them
    /// sorted. A table of a workspace revision records its upstream tables instead, with
    /// [`RbinWriter::upstream_tables`] or through [`Txn::add_rbin`](crate::Txn::add_rbin).
    pub fn upstream(&mut self, name: &str, hash: Hash64) -> Result<&mut Self> {
        let key = encode_name(name, "upstream name")?;
        if self.upstream.insert(key, hash.0).is_some() {
            return Err(Error::internal(format!(
                "rbin upstream '{name}' given twice"
            )));
        }
        Ok(self)
    }

    /// Records the upstream map of a workspace table: one entry per upstream table, named by
    /// [`upstream_names`], holding the hash `meta.json` records for it (WS-REQ-0038).
    pub fn upstream_tables(&mut self, upstream: &Upstream) -> Result<&mut Self> {
        for (name, hash) in header_upstream(upstream)? {
            if self.upstream.insert(name, hash).is_some() {
                let shown = decode_name(&name).unwrap_or_default();
                return Err(Error::internal(format!(
                    "rbin upstream '{shown}' given twice"
                )));
            }
        }
        Ok(self)
    }

    /// The upstream entries set so far.
    pub(crate) fn upstream_entries(&self) -> &BTreeMap<[u8; 8], u64> {
        &self.upstream
    }

    /// Adds a section that borrows `data`; its element size is `size_of::<T>()`.
    pub fn section<T: Pod>(&mut self, id: u32, data: &'a [T]) -> Result<&mut Self> {
        let elem_size = elem_size_of::<T>()?;
        self.insert(id, elem_size, Cow::Borrowed(bytemuck::cast_slice(data)))
    }

    /// Adds a section that owns a copy of `data`.
    pub fn section_vec<T: Pod>(&mut self, id: u32, data: Vec<T>) -> Result<&mut Self> {
        let elem_size = elem_size_of::<T>()?;
        let bytes = bytemuck::cast_slice::<T, u8>(&data).to_vec();
        self.insert(id, elem_size, Cow::Owned(bytes))
    }

    /// Adds a section of raw bytes holding elements of `elem_size` bytes.
    pub fn section_bytes(
        &mut self,
        id: u32,
        elem_size: u32,
        bytes: impl Into<Cow<'a, [u8]>>,
    ) -> Result<&mut Self> {
        let bytes = bytes.into();
        if elem_size == 0 || !(bytes.len() as u64).is_multiple_of(u64::from(elem_size)) {
            return Err(Error::internal(format!(
                "rbin section {id}: {} bytes are not a whole number of {elem_size}-byte elements",
                bytes.len()
            )));
        }
        self.insert(id, elem_size, bytes)
    }

    fn insert(&mut self, id: u32, elem_size: u32, bytes: Cow<'a, [u8]>) -> Result<&mut Self> {
        if self.sections.contains_key(&id) {
            return Err(Error::internal(format!("rbin section {id} given twice")));
        }
        self.sections
            .insert(id, PendingSection { elem_size, bytes });
        Ok(self)
    }

    /// The header length.
    pub fn header_len(&self) -> u64 {
        header_len(self.upstream.len() as u64, self.sections.len() as u64)
    }

    /// The length of the file this writer produces.
    pub fn file_len(&self) -> u64 {
        self.sections.values().fold(self.header_len(), |end, s| {
            align_up(end + s.bytes.len() as u64)
        })
    }

    /// The content hash the header will record: xxh3-64 of the bytes after the header.
    pub fn content_hash(&self) -> Hash64 {
        let mut hasher = Hasher::new();
        self.for_each_content_chunk(|chunk| hasher.update(chunk));
        hasher.finish()
    }

    fn for_each_content_chunk(&self, mut f: impl FnMut(&[u8])) {
        const ZEROS: [u8; ALIGN as usize] = [0; ALIGN as usize];
        for section in self.sections.values() {
            let len = section.bytes.len() as u64;
            f(&section.bytes);
            let pad = (align_up(len) - len) as usize;
            if pad > 0 {
                f(&ZEROS[..pad]);
            }
        }
    }

    fn header(&self, content_hash: Hash64, upstream: &BTreeMap<[u8; 8], u64>) -> Vec<u8> {
        let header_len = header_len(upstream.len() as u64, self.sections.len() as u64);
        let mut h = Vec::with_capacity(header_len as usize);
        h.extend_from_slice(&MAGIC);
        h.extend_from_slice(&self.kind);
        h.extend_from_slice(&self.format_version.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(&content_hash.0.to_le_bytes());
        h.extend_from_slice(&(self.sections.len() as u32).to_le_bytes());
        h.extend_from_slice(&(upstream.len() as u32).to_le_bytes());
        h.extend_from_slice(&header_len.to_le_bytes());
        for (name, hash) in upstream {
            h.extend_from_slice(name);
            h.extend_from_slice(&hash.to_le_bytes());
        }
        let mut offset = header_len;
        for (id, section) in &self.sections {
            let len = section.bytes.len() as u64;
            h.extend_from_slice(&id.to_le_bytes());
            h.extend_from_slice(&section.elem_size.to_le_bytes());
            h.extend_from_slice(&offset.to_le_bytes());
            h.extend_from_slice(&len.to_le_bytes());
            offset = align_up(offset + len);
        }
        h
    }

    /// The whole file in memory.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.header(self.content_hash(), &self.upstream);
        out.reserve(self.file_len() as usize - out.len());
        self.for_each_content_chunk(|chunk| out.extend_from_slice(chunk));
        out
    }

    /// Writes the file to a new path (an existing file is never overwritten) and fsyncs it
    /// (WS-REQ-0036). Returns the file hash, xxh3-64 of all its bytes, which is the table hash
    /// recorded in `meta.json`.
    pub fn write_to(&self, path: impl AsRef<Path>) -> Result<Hash64> {
        self.write_with_upstream(path.as_ref(), &self.upstream)
    }

    /// [`RbinWriter::write_to`] with the header upstream entries `upstream` in place of the
    /// writer's own.
    pub(crate) fn write_with_upstream(
        &self,
        path: &Path,
        upstream: &BTreeMap<[u8; 8], u64>,
    ) -> Result<Hash64> {
        let header = self.header(self.content_hash(), upstream);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| Error::io(path, e))?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        let mut file_hash = Hasher::new();
        file_hash.update(&header);
        let mut written = out.write_all(&header);
        self.for_each_content_chunk(|chunk| {
            if written.is_ok() {
                file_hash.update(chunk);
                written = out.write_all(chunk);
            }
        });
        let synced = written
            .and_then(|()| out.into_inner().map_err(|e| e.into_error()))
            .and_then(|file| file.sync_all());
        if let Err(e) = synced {
            let _ = fs::remove_file(path);
            return Err(Error::io(path, e));
        }
        Ok(file_hash.finish())
    }
}

/// An upstream entry of an rbin header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamInfo {
    /// The name, without NUL padding.
    pub name: String,
    /// The hash.
    pub hash: Hash64,
}

/// A section entry of an rbin header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionInfo {
    /// The section id.
    pub id: u32,
    /// The element size in bytes.
    pub elem_size: u32,
    /// The byte offset in the file.
    pub offset: u64,
    /// The length in bytes.
    pub len: u64,
}

impl SectionInfo {
    /// The number of elements.
    pub fn count(&self) -> u64 {
        self.len / u64::from(self.elem_size)
    }
}

/// The bytes of an rbin: a read-only map, or an 8-byte aligned buffer.
enum Storage {
    Mapped(Mmap),
    Owned { words: Vec<u64>, len: usize },
}

impl Storage {
    fn bytes(&self) -> &[u8] {
        match self {
            Storage::Mapped(map) => map,
            Storage::Owned { words, len } => &bytemuck::cast_slice::<u64, u8>(words)[..*len],
        }
    }
}

impl fmt::Debug for Storage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Storage::Mapped(map) => write!(f, "Mapped({} bytes)", map.len()),
            Storage::Owned { len, .. } => write!(f, "Owned({len} bytes)"),
        }
    }
}

/// A validated rbin table: header fields and zero-copy access to its sections.
#[derive(Debug)]
pub struct Rbin {
    path: PathBuf,
    storage: Storage,
    kind: String,
    format_version: u32,
    content_hash: Hash64,
    header_len: u64,
    upstream: Vec<UpstreamInfo>,
    sections: Vec<SectionInfo>,
}

/// The parsed header, before it is attached to its storage.
struct Header {
    kind: String,
    format_version: u32,
    content_hash: Hash64,
    header_len: u64,
    upstream: Vec<UpstreamInfo>,
    sections: Vec<SectionInfo>,
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(b)
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}

impl Rbin {
    /// Maps `path` read-only and validates its header (WS-REQ-0032); `kind` is the expected
    /// kind and `versions` the format versions the caller reads.
    ///
    /// Crate-private, because a map is sound only while nobody writes or truncates the file
    /// (WS-REQ-0037, see `mmap::map_readonly`): it is called for the tables of committed
    /// revisions ([`Revision::open_rbin`](crate::Revision::open_rbin)) and the compiled
    /// libraries of the cache ([`LibraryCache::open_rlib`](crate::LibraryCache::open_rlib))
    /// only, which the workspace never writes again. Only a regular file is mapped, checked
    /// without following a symbolic link and again on the open handle (WS-REQ-0025).
    pub(crate) fn map_immutable(
        path: &Path,
        kind: &str,
        versions: RangeInclusive<u32>,
    ) -> Result<Rbin> {
        let file = fsutil::open_regular(path)?;
        let map = mmap::map_readonly(&file).map_err(|e| Error::io(path, e))?;
        Rbin::validate(path.to_path_buf(), Storage::Mapped(map), kind, versions)
    }

    /// Reads the rbin file `path` into an 8-byte aligned buffer and validates its header
    /// (WS-REQ-0032): for a file outside a workspace, which may change after it is read; the
    /// table in memory does not. `kind` is the expected kind and `versions` the format
    /// versions the caller reads. `path` must be a regular file; anything else (a FIFO, a
    /// device, a directory, a symbolic link) is `CORRUPT_TABLE` and never blocks
    /// (WS-REQ-0025). A format version above `versions` is `SCHEMA_TOO_NEW` (WS-REQ-0039).
    ///
    /// ```
    /// use rusteda_workspace::{Rbin, RbinWriter};
    ///
    /// # let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/tmp/rbin-read-doc");
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// let path = dir.join("cells.rbin");
    /// # let _ = std::fs::remove_file(&path);
    /// let mut w = RbinWriter::new("DEMO", 1)?;
    /// w.section_vec(1, vec![7u32, 8, 9])?;
    /// w.write_to(&path)?;
    /// let table = Rbin::read(&path, "DEMO", 1..=1)?;
    /// std::fs::write(&path, b"rewritten")?; // the table in memory stays as it was read
    /// assert_eq!(table.section::<u32>(1)?, &[7, 8, 9]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read(path: impl AsRef<Path>, kind: &str, versions: RangeInclusive<u32>) -> Result<Rbin> {
        let path = path.as_ref();
        let mut file = fsutil::open_regular(path)?;
        let io_err = |e| Error::io(path, e);
        let hint = file.metadata().map_err(io_err)?.len();
        let (words, len) = read_aligned(&mut file, hint).map_err(io_err)?;
        Rbin::validate(
            path.to_path_buf(),
            Storage::Owned { words, len },
            kind,
            versions,
        )
    }

    /// Validates an rbin held in memory. The bytes are copied once into an 8-byte aligned
    /// buffer so that sections can be viewed in place.
    pub fn from_bytes(bytes: Vec<u8>, kind: &str, versions: RangeInclusive<u32>) -> Result<Rbin> {
        let len = bytes.len();
        let mut words = vec![0u64; len.div_ceil(8)];
        bytemuck::cast_slice_mut::<u64, u8>(&mut words)[..len].copy_from_slice(&bytes);
        Rbin::validate(
            PathBuf::from("<memory>"),
            Storage::Owned { words, len },
            kind,
            versions,
        )
    }

    fn validate(
        path: PathBuf,
        storage: Storage,
        kind: &str,
        versions: RangeInclusive<u32>,
    ) -> Result<Rbin> {
        let expect = Expect {
            kind: encode_name(kind, "kind")?,
            name: kind,
            versions: &versions,
        };
        let bytes = storage.bytes();
        match parse_header(bytes, bytes.len() as u64, Some(&expect)) {
            Ok(h) => Ok(Rbin {
                path,
                storage,
                kind: h.kind,
                format_version: h.format_version,
                content_hash: h.content_hash,
                header_len: h.header_len,
                upstream: h.upstream,
                sections: h.sections,
            }),
            // WS-REQ-0039: written by a newer release, not damaged.
            Err(Corruption::UnsupportedVersion { found, min, max }) if found > max => {
                Err(Error::FormatTooNew {
                    path,
                    kind: kind.to_string(),
                    found,
                    min,
                    max,
                })
            }
            Err(what) => Err(Error::corrupt(path, what)),
        }
    }

    /// The file path (`<memory>` for [`Rbin::from_bytes`]).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The table kind.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The format version.
    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    /// The content hash recorded in the header (not verified; see [`Rbin::verify_hash`]).
    pub fn content_hash(&self) -> Hash64 {
        self.content_hash
    }

    /// The header length.
    pub fn header_len(&self) -> u64 {
        self.header_len
    }

    /// The file length.
    pub fn file_len(&self) -> u64 {
        self.as_bytes().len() as u64
    }

    /// The upstream entries, in header order.
    pub fn upstream(&self) -> &[UpstreamInfo] {
        &self.upstream
    }

    /// The hash of the upstream entry with header name `name`; for a workspace table, the
    /// name of an upstream table is given by [`upstream_names`] of the table's upstream map.
    pub fn upstream_hash(&self, name: &str) -> Option<Hash64> {
        self.upstream
            .iter()
            .find(|u| u.name == name)
            .map(|u| u.hash)
    }

    /// Whether the header's upstream entries are exactly `expected`.
    pub(crate) fn has_upstream(&self, expected: &BTreeMap<[u8; 8], u64>) -> bool {
        same_upstream(&self.upstream, expected)
    }

    /// The section entries, in increasing id order.
    pub fn sections(&self) -> &[SectionInfo] {
        &self.sections
    }

    /// The entry of section `id`.
    pub fn section_info(&self, id: u32) -> Option<&SectionInfo> {
        self.sections
            .binary_search_by_key(&id, |s| s.id)
            .ok()
            .map(|i| &self.sections[i])
    }

    /// Whether section `id` exists.
    pub fn has_section(&self, id: u32) -> bool {
        self.section_info(id).is_some()
    }

    /// The bytes of section `id`; a missing section is `CORRUPT_TABLE`.
    pub fn section_bytes(&self, id: u32) -> Result<&[u8]> {
        let s = self
            .section_info(id)
            .ok_or_else(|| Error::corrupt(&self.path, Corruption::MissingSection { id }))?;
        // Validation checked offset + len <= file length.
        Ok(&self.as_bytes()[s.offset as usize..(s.offset + s.len) as usize])
    }

    /// Section `id` as a zero-copy slice of `T` (WS-REQ-0033). A missing section, or one whose
    /// element size is not `size_of::<T>()`, is `CORRUPT_TABLE`.
    pub fn section<T: Pod>(&self, id: u32) -> Result<&[T]> {
        let elem_size = elem_size_of::<T>()?;
        let s = self
            .section_info(id)
            .ok_or_else(|| Error::corrupt(&self.path, Corruption::MissingSection { id }))?;
        if s.elem_size != elem_size {
            let what = Corruption::TypeMismatch {
                id,
                elem_size: s.elem_size,
                type_size: size_of::<T>(),
            };
            return Err(Error::corrupt(&self.path, what));
        }
        let bytes = self.section_bytes(id)?;
        // Sections start at multiples of 8 in a page-aligned map or an 8-byte aligned buffer,
        // and `elem_size_of` admits alignments up to 8, so the cast cannot fail.
        bytemuck::try_cast_slice(bytes)
            .map_err(|e| Error::internal(format!("section {id} cannot be viewed: {e}")))
    }

    /// Recomputes the content hash and compares it with the header (WS-REQ-0034).
    pub fn verify_hash(&self) -> Result<()> {
        let computed = Hash64::of(&self.as_bytes()[self.header_len as usize..]);
        if computed != self.content_hash {
            let what = Corruption::HashMismatch {
                stored: self.content_hash,
                computed,
            };
            return Err(Error::corrupt(&self.path, what));
        }
        Ok(())
    }

    /// The file hash: xxh3-64 of all bytes, the table hash that `meta.json` records.
    pub fn file_hash(&self) -> Hash64 {
        Hash64::of(self.as_bytes())
    }

    /// All bytes of the file.
    pub fn as_bytes(&self) -> &[u8] {
        self.storage.bytes()
    }
}

/// What a reader expects of a header besides its structure: the kind and format versions.
struct Expect<'a> {
    kind: [u8; 8],
    name: &'a str,
    versions: &'a RangeInclusive<u32>,
}

/// Parses and validates the header at the start of `bytes`, the first bytes of a file of
/// `file_len` bytes; `bytes` holds the whole header when the file does. With `expect` the kind
/// and the format version are checked too; without it only what every rbin satisfies, as when
/// a table enters a revision (WS-REQ-0078).
fn parse_header(
    bytes: &[u8],
    file_len: u64,
    expect: Option<&Expect<'_>>,
) -> std::result::Result<Header, Corruption> {
    // The bytes at hand: the whole file, or for a streamed file its first bytes, fewer than
    // `file_len` only when the file shrank while it was read.
    let have = (bytes.len() as u64).min(file_len);
    if have < FIXED_HEADER_LEN {
        return Err(Corruption::Truncated {
            needed: FIXED_HEADER_LEN,
            len: have,
        });
    }
    if bytes[..8] != MAGIC {
        return Err(Corruption::BadMagic);
    }
    let kind = decode_name(&bytes[8..16]).ok_or(Corruption::BadName { field: "kind" })?;
    let format_version = read_u32(bytes, 16);
    if let Some(expect) = expect {
        if bytes[8..16] != expect.kind[..] {
            return Err(Corruption::WrongKind {
                expected: expect.name.to_string(),
                found: kind,
            });
        }
        if !expect.versions.contains(&format_version) {
            return Err(Corruption::UnsupportedVersion {
                found: format_version,
                min: *expect.versions.start(),
                max: *expect.versions.end(),
            });
        }
    }
    let flags = read_u32(bytes, 20);
    if flags != 0 {
        return Err(Corruption::Flags { flags });
    }
    let content_hash = Hash64(read_u64(bytes, 24));
    let section_count = u64::from(read_u32(bytes, 32));
    let upstream_count = u64::from(read_u32(bytes, 36));
    let declared = read_u64(bytes, 40);
    let expected_len = header_len(upstream_count, section_count);
    if declared != expected_len {
        return Err(Corruption::HeaderLen {
            declared,
            expected: expected_len,
        });
    }
    if declared > file_len {
        return Err(Corruption::Truncated {
            needed: declared,
            len: file_len,
        });
    }
    if declared > have {
        return Err(Corruption::Truncated {
            needed: declared,
            len: have,
        });
    }
    let mut upstream = Vec::with_capacity(upstream_count as usize);
    for i in 0..upstream_count {
        let at = (FIXED_HEADER_LEN + UPSTREAM_ENTRY_LEN * i) as usize;
        let name = decode_name(&bytes[at..at + 8]).ok_or(Corruption::BadName {
            field: "upstream name",
        })?;
        upstream.push(UpstreamInfo {
            name,
            hash: Hash64(read_u64(bytes, at + 8)),
        });
    }
    let table = FIXED_HEADER_LEN + UPSTREAM_ENTRY_LEN * upstream_count;
    let mut sections = Vec::with_capacity(section_count as usize);
    let mut previous_end = declared;
    for i in 0..section_count {
        let at = (table + SECTION_ENTRY_LEN * i) as usize;
        let s = SectionInfo {
            id: read_u32(bytes, at),
            elem_size: read_u32(bytes, at + 4),
            offset: read_u64(bytes, at + 8),
            len: read_u64(bytes, at + 16),
        };
        if sections.last().is_some_and(|p: &SectionInfo| s.id <= p.id) {
            return Err(Corruption::SectionOrder { id: s.id });
        }
        if !s.offset.is_multiple_of(ALIGN) {
            return Err(Corruption::Misaligned {
                id: s.id,
                offset: s.offset,
            });
        }
        if s.elem_size == 0 || !s.len.is_multiple_of(u64::from(s.elem_size)) {
            return Err(Corruption::ElemSize {
                id: s.id,
                elem_size: s.elem_size,
                len: s.len,
            });
        }
        let end = match s.offset.checked_add(s.len) {
            Some(end) if end <= file_len => end,
            _ => {
                let (id, offset, len) = (s.id, s.offset, s.len);
                return Err(Corruption::SectionBounds {
                    id,
                    offset,
                    len,
                    file_len,
                });
            }
        };
        if s.offset < previous_end {
            return Err(Corruption::Overlap { id: s.id });
        }
        previous_end = end;
        sections.push(s);
    }
    Ok(Header {
        kind,
        format_version,
        content_hash,
        header_len: declared,
        upstream,
        sections,
    })
}

#[cfg(test)]
#[path = "rbin_tests.rs"]
mod tests;
