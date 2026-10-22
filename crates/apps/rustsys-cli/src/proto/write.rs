// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Atomic, streamed writes (PROTO-REQ-0033).

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Writes `path` through `f` into a temporary file in the same directory, then renames it into
/// place; creates parent directories; removes the temporary file on any error.
pub fn write_atomic(
    path: &Path,
    f: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".into());
    // One temporary name per write, so that overlapping writes of one file do not share it.
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.partial-{}-{n}", std::process::id()));
    let result = (|| {
        let file = File::create(&tmp)?;
        let mut w = BufWriter::new(file);
        f(&mut w)?;
        let file = w.into_inner().map_err(|e| e.into_error())?;
        file.sync_data()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Pretty JSON of `v`, streamed; the bytes equal those of `serde_json::to_string_pretty`.
pub fn write_json_atomic<T: serde::Serialize + ?Sized>(path: &Path, v: &T) -> io::Result<()> {
    write_atomic(path, |w| {
        serde_json::to_writer_pretty(&mut *w, v).map_err(io::Error::other)
    })
}

/// Text written atomically.
pub fn write_text_atomic(path: &Path, text: &str) -> io::Result<()> {
    write_atomic(path, |w| w.write_all(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // PROTO-REQ-0033
    #[test]
    fn atomic_and_equal_bytes() {
        let dir = std::env::temp_dir().join(format!("rustsys-write-{}", std::process::id()));
        let p = dir.join("sub/r.json");
        let v = serde_json::json!({"a": [1, 2.5, null], "b": "x"});
        write_json_atomic(&p, &v).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            serde_json::to_string_pretty(&v).unwrap()
        );
        // A failing writer leaves neither the file nor the temporary file.
        let q = dir.join("sub/fail.txt");
        let r = write_atomic(&q, |_| Err(io::Error::other("boom")));
        assert!(r.is_err());
        assert!(!q.exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("sub"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("partial"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
