// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Content hashes: xxh3-64, written `xxh3:<16 lowercase hex digits>` (WS-REQ-0010).
//!
//! - File hash: xxh3-64 of the file bytes, computed streaming ([`hash_file`], WS-REQ-0011).
//! - Directory hash: xxh3-64 over one record per file below the directory, sorted by the
//!   relative path with `/` separators: `path NUL file-hash-hex LF` ([`hash_dir`],
//!   WS-REQ-0012).
//! - List hash: xxh3-64 over one record per element, in list order, fields joined by NUL and
//!   ended by LF ([`hash_records`]); the `libraries` pseudo-table uses it (WS-REQ-0013).

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use xxhash_rust::xxh3::{Xxh3Default, xxh3_64};

/// An xxh3-64 hash, written `xxh3:<16 lowercase hex digits>`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Hash64(pub u64);

impl Hash64 {
    /// The prefix of the notation.
    pub const PREFIX: &'static str = "xxh3:";

    /// Hashes bytes.
    pub fn of(bytes: &[u8]) -> Hash64 {
        Hash64(xxh3_64(bytes))
    }

    /// The 16 lowercase hex digits without the prefix (`h16` in cache file names).
    pub fn hex(self) -> String {
        format!("{:016x}", self.0)
    }

    /// Reads `xxh3:<16 lowercase hex digits>`; any other text gives `None`.
    pub fn parse(text: &str) -> Option<Hash64> {
        Hash64::parse_hex(text.strip_prefix(Hash64::PREFIX)?)
    }

    /// Reads exactly 16 lowercase hex digits.
    pub fn parse_hex(hex: &str) -> Option<Hash64> {
        let valid = hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !valid {
            return None;
        }
        u64::from_str_radix(hex, 16).ok().map(Hash64)
    }
}

impl fmt::Display for Hash64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{:016x}", Hash64::PREFIX, self.0)
    }
}

impl fmt::Debug for Hash64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash64({self})")
    }
}

/// The error of parsing a [`Hash64`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("'{0}' is not a hash: expected xxh3:<16 lowercase hex digits>")]
pub struct ParseHashError(pub String);

impl FromStr for Hash64 {
    type Err = ParseHashError;

    fn from_str(text: &str) -> Result<Hash64, ParseHashError> {
        Hash64::parse(text).ok_or_else(|| ParseHashError(text.to_string()))
    }
}

impl Serialize for Hash64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hash64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Hash64, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A streaming xxh3-64 hasher; also an [`io::Write`] sink.
#[derive(Clone, Default)]
pub struct Hasher(Xxh3Default);

impl Hasher {
    /// A hasher of the empty input.
    pub fn new() -> Hasher {
        Hasher::default()
    }

    /// Appends bytes.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The hash of everything appended so far.
    pub fn finish(&self) -> Hash64 {
        Hash64(self.0.digest())
    }
}

impl fmt::Debug for Hasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hasher").finish_non_exhaustive()
    }
}

impl Write for Hasher {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Hashes everything a reader yields.
pub fn hash_reader(mut reader: impl Read) -> io::Result<Hash64> {
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(hasher.finish()),
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// The file hash: xxh3-64 of the file bytes, read in chunks (WS-REQ-0011).
pub fn hash_file(path: &Path) -> io::Result<Hash64> {
    hash_reader(File::open(path)?)
}

/// The list hash: xxh3-64 over the records in order, each record's fields joined by NUL and
/// ended by LF (WS-REQ-0013).
pub fn hash_records<R, F>(records: impl IntoIterator<Item = R>) -> Hash64
where
    R: IntoIterator<Item = F>,
    F: AsRef<[u8]>,
{
    let mut hasher = Hasher::new();
    for record in records {
        for (i, field) in record.into_iter().enumerate() {
            if i > 0 {
                hasher.update(b"\0");
            }
            hasher.update(field.as_ref());
        }
        hasher.update(b"\n");
    }
    hasher.finish()
}

/// The files below `dir` with their file hashes, sorted by relative path (`/` separators,
/// byte order). Symbolic links, special files and names that are not UTF-8 are errors
/// (`InvalidData`); empty directories contribute nothing.
pub fn dir_file_hashes(dir: &Path) -> io::Result<Vec<(String, Hash64)>> {
    walk_files(dir)
        .map_err(WalkError::into_io)?
        .into_iter()
        .map(|(rel, path)| Ok((rel, hash_file(&path)?)))
        .collect()
}

/// The directory hash: xxh3-64 over the records `relative path, NUL, file-hash hex, LF` of
/// [`dir_file_hashes`] (WS-REQ-0012).
pub fn hash_dir(dir: &Path) -> io::Result<Hash64> {
    Ok(dir_hash(&dir_file_hashes(dir)?))
}

/// The directory hash of files with their hashes, sorted by relative path (WS-REQ-0012).
pub(crate) fn dir_hash(files: &[(String, Hash64)]) -> Hash64 {
    hash_records(
        files
            .iter()
            .map(|(rel, hash)| [rel.as_bytes().to_vec(), hash.hex().into_bytes()]),
    )
}

/// Why a directory tree cannot be walked as a directory table.
#[derive(Debug)]
pub(crate) enum WalkError {
    /// Reading a directory failed.
    Io(PathBuf, io::Error),
    /// An entry is not a regular file or a directory: a symbolic link, FIFO, device or socket.
    Special {
        /// The relative path, `/`-separated.
        rel: String,
        /// The full path.
        path: PathBuf,
    },
    /// An entry name is not UTF-8.
    NotUtf8 {
        /// The relative path, `/`-separated, with the name shown lossily.
        rel: String,
        /// The full path.
        path: PathBuf,
    },
}

impl WalkError {
    /// The error of the public `io::Result` functions: `InvalidData` for a special file or a
    /// name that is not UTF-8.
    pub(crate) fn into_io(self) -> io::Error {
        match self {
            WalkError::Io(_, e) => e,
            WalkError::Special { rel, .. } => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{rel}: not a regular file or directory"),
            ),
            WalkError::NotUtf8 { rel, .. } => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{rel}: name is not UTF-8"),
            ),
        }
    }
}

/// The regular files below `dir`, as (relative path with `/` separators, full path), sorted by
/// the bytes of the relative path. Symbolic links are not followed: they, special files and
/// names that are not UTF-8 are errors; empty directories contribute nothing.
pub(crate) fn walk_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, WalkError> {
    let mut files = Vec::new();
    collect_files(dir, "", &mut files)?;
    files.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

fn collect_files(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), WalkError> {
    let io_err = |e| WalkError::Io(dir.to_path_buf(), e);
    for entry in fs::read_dir(dir).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let join = |name: &str| {
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            }
        };
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(name) => {
                return Err(WalkError::NotUtf8 {
                    rel: join(&Path::new(&name).display().to_string()),
                    path: entry.path(),
                });
            }
        };
        let rel = join(&name);
        let kind = entry.file_type().map_err(io_err)?;
        if kind.is_dir() {
            collect_files(&entry.path(), &rel, out)?;
        } else if kind.is_file() {
            out.push((rel, entry.path()));
        } else {
            let path = entry.path();
            return Err(WalkError::Special { rel, path });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    // WS-REQ-0010
    #[test]
    fn notation() {
        let h = Hash64(0x0123_4567_89ab_cdef);
        assert_eq!(h.to_string(), "xxh3:0123456789abcdef");
        assert_eq!(h.hex(), "0123456789abcdef");
        assert_eq!(Hash64(1).to_string(), "xxh3:0000000000000001");
        assert_eq!(Hash64::parse("xxh3:0123456789abcdef"), Some(h));
        assert_eq!("xxh3:0123456789abcdef".parse::<Hash64>(), Ok(h));
        for bad in [
            "",
            "0123456789abcdef",
            "xxh3:0123456789ABCDEF",
            "xxh3:0123456789abcde",
            "xxh3:0123456789abcdef0",
            "xxh3:+123456789abcdef",
            "xxh64:0123456789abcdef",
            "xxh3: 123456789abcdef",
        ] {
            assert_eq!(Hash64::parse(bad), None, "{bad}");
        }
        assert_eq!(
            serde_json::to_string(&h).unwrap(),
            "\"xxh3:0123456789abcdef\""
        );
        assert_eq!(
            serde_json::from_str::<Hash64>("\"xxh3:0123456789abcdef\"").unwrap(),
            h
        );
        assert!(serde_json::from_str::<Hash64>("\"xxh3:XYZ\"").is_err());
        assert_eq!(format!("{h:?}"), "Hash64(xxh3:0123456789abcdef)");
    }

    // WS-REQ-0011: streaming equals one-shot hashing, whatever the chunking.
    #[test]
    fn streaming_matches_one_shot() {
        // The xxh3-64 of the empty input (default seed and secret).
        assert_eq!(Hash64::of(b""), Hash64(0x2d06_8005_38d3_94c2));
        let data: Vec<u8> = (0..1_000_003u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        let whole = Hash64::of(&data);
        for chunk in [1, 7, 64, 240, 4096, 1 << 20] {
            let mut h = Hasher::new();
            for part in data.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(h.finish(), whole, "chunk {chunk}");
        }
        assert_eq!(hash_reader(&data[..]).unwrap(), whole);
        let dir = temp_dir("hash-file");
        let path = dir.join("f.bin");
        fs::write(&path, &data).unwrap();
        assert_eq!(hash_file(&path).unwrap(), whole);
    }

    // WS-REQ-0013
    #[test]
    fn records() {
        let h = hash_records([["a", "b"], ["c", "d"]]);
        assert_eq!(h, Hash64::of(b"a\0b\nc\0d\n"));
        assert_eq!(hash_records(Vec::<Vec<&str>>::new()), Hash64::of(b""));
        assert_ne!(
            hash_records([["a", "b"], ["c", "d"]]),
            hash_records([["c", "d"], ["a", "b"]])
        );
    }

    // WS-REQ-0012
    #[test]
    fn directory_hash() {
        let dir = temp_dir("hash-dir");
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::create_dir_all(dir.join("empty")).unwrap();
        fs::write(dir.join("a.txt"), b"x").unwrap();
        fs::write(dir.join("a/b/c.v"), b"module c; endmodule\n").unwrap();
        fs::write(dir.join("a/z"), b"").unwrap();
        let files = dir_file_hashes(&dir).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
        // Byte order of the '/'-separated paths: '.' (0x2e) sorts before '/' (0x2f).
        assert_eq!(names, ["a.txt", "a/b/c.v", "a/z"]);
        let mut text = Vec::new();
        for (rel, data) in [
            ("a.txt", &b"x"[..]),
            ("a/b/c.v", b"module c; endmodule\n"),
            ("a/z", b""),
        ] {
            text.extend_from_slice(rel.as_bytes());
            text.push(0);
            text.extend_from_slice(Hash64::of(data).hex().as_bytes());
            text.push(b'\n');
        }
        assert_eq!(hash_dir(&dir).unwrap(), Hash64::of(&text));
        // Content and names both count.
        fs::write(dir.join("a/z"), b"1").unwrap();
        assert_ne!(hash_dir(&dir).unwrap(), Hash64::of(&text));
    }

    #[cfg(unix)]
    #[test]
    fn directory_hash_rejects_symlinks() {
        let dir = temp_dir("hash-dir-symlink");
        fs::write(dir.join("f"), b"x").unwrap();
        std::os::unix::fs::symlink("f", dir.join("link")).unwrap();
        let err = hash_dir(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("link"), "{err}");
    }
}
