// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Helpers shared by the integration tests.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use rusteda_workspace::{Invocation, Workspace};

/// A fresh, empty directory `CARGO_TARGET_TMPDIR/rusteda-workspace/<test crate>/<name>`; test
/// names are unique within a test crate, and a later run starts from an empty directory.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("rusteda-workspace")
        .join(env!("CARGO_CRATE_NAME"))
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// An invocation of `command`, its words as argv.
pub fn inv(command: &str) -> Invocation {
    let words: Vec<&str> = command.split(' ').collect();
    Invocation::new(words[0], "0.1.0", command, words)
}

/// A new workspace in a fresh directory.
pub fn new_workspace(name: &str) -> Workspace {
    let root = temp_dir(name).join("ws");
    let init = Workspace::init(&root, &inv("rusteda init")).unwrap();
    assert!(init.warnings.is_empty(), "{:?}", init.warnings);
    init.workspace
}

/// The entries of a directory, sorted.
pub fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// The keys of the top-level object of compact or pretty JSON text, in order of appearance.
pub fn top_level_keys(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let (mut depth, mut i, mut keys) = (0usize, 0usize, Vec::new());
    let mut expect_key = false;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => {
                depth += 1;
                expect_key = depth == 1 && bytes[i] == b'{';
            }
            b'}' | b']' => depth -= 1,
            b',' if depth == 1 => expect_key = true,
            b'"' => {
                let start = i + 1;
                i += 1;
                while bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                if expect_key {
                    keys.push(text[start..i].to_string());
                    expect_key = false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    keys
}
