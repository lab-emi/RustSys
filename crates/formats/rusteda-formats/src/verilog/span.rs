// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Source texts and locations (VERILOG-REQ-0002).

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Index of a file in a [`SourceMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// A byte range in one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file: FileId, start: usize, end: usize) -> Span {
        Span {
            file,
            start: start as u32,
            end: end as u32,
        }
    }

    /// Smallest span covering both (same file assumed; otherwise `self`).
    pub fn to(self, other: Span) -> Span {
        if self.file != other.file {
            return self;
        }
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// One source file.
#[derive(Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    pub text: Arc<str>,
    line_starts: Vec<u32>,
}

impl SourceFile {
    fn new(path: PathBuf, text: Arc<str>) -> SourceFile {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFile {
            path,
            text,
            line_starts,
        }
    }

    /// 1-based line and column of a byte offset.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let col = offset - self.line_starts.get(line).copied().unwrap_or(0);
        (line as u32 + 1, col + 1)
    }
}

/// All source files of a run.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    /// Adds a file with the given text.
    pub fn add(&mut self, path: impl Into<PathBuf>, text: impl Into<Arc<str>>) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile::new(path.into(), text.into()));
        id
    }

    /// Reads and adds a file from disk.
    pub fn load(&mut self, path: &Path) -> std::io::Result<FileId> {
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(self.add(path.to_path_buf(), text))
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn get(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    /// Finds an already loaded file by path.
    pub fn find(&self, path: &Path) -> Option<FileId> {
        self.files
            .iter()
            .position(|f| f.path == path)
            .map(|i| FileId(i as u32))
    }

    /// `path:line:col` of a span start.
    pub fn describe(&self, span: Span) -> String {
        match self.get(span.file) {
            Some(f) => {
                let (l, c) = f.line_col(span.start);
                format!("{}:{}:{}", f.path.display(), l, c)
            }
            None => String::from("<unknown>"),
        }
    }

    /// Source text covered by a span.
    pub fn snippet(&self, span: Span) -> &str {
        match self.get(span.file) {
            Some(f) => f
                .text
                .get(span.start as usize..span.end as usize)
                .unwrap_or(""),
            None => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_and_column() {
        let mut sm = SourceMap::new();
        let id = sm.add("a.sv", "ab\ncd\n\nx");
        let f = sm.file(id);
        assert_eq!(f.line_col(0), (1, 1));
        assert_eq!(f.line_col(1), (1, 2));
        assert_eq!(f.line_col(3), (2, 1));
        assert_eq!(f.line_col(7), (4, 1));
        assert_eq!(sm.describe(Span::new(id, 4, 5)), "a.sv:2:2");
    }
}
