// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Rules about the crate's own sources: license headers, where `unsafe` may appear, and the
//! big-endian guard (WS-REQ-0035, WS-REQ-0037).

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, fs::read_to_string(&p).unwrap())
        })
        .collect()
}

#[test]
fn every_rust_file_has_the_license_header() {
    for (path, text) in sources() {
        // REUSE-IgnoreStart
        assert!(
            text.starts_with(
                "// SPDX-License-Identifier: Apache-2.0\n\
                 // SPDX-FileCopyrightText: 2026 RustEDA contributors\n"
            ),
            "{path}"
        );
        // REUSE-IgnoreEnd
    }
}

// WS-REQ-0037: `unsafe` only in the mmap module, under a module-scoped allow, with a SAFETY
// comment before it.
#[test]
fn unsafe_code_only_in_the_mmap_module() {
    let word = ["un", "safe"].concat();
    for (path, text) in sources() {
        let uses: Vec<(usize, &str)> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                let code = l.split("//").next().unwrap_or("");
                code.contains(&format!("{word} {{"))
                    || code.contains(&format!("{word} fn"))
                    || code.contains(&format!("{word} impl"))
            })
            .collect();
        if path == "src/mmap.rs" {
            assert_eq!(uses.len(), 1, "{path}: {uses:?}");
            assert!(text.contains(&format!("#![allow({word}_code)]")), "{path}");
            let before = &text[..text.find(uses[0].1).unwrap()];
            assert!(before.contains("// SAFETY: "), "{path}: no SAFETY comment");
        } else {
            assert!(uses.is_empty(), "{path}: {uses:?}");
            assert!(!text.contains(&format!("allow({word}_code)")), "{path}");
        }
    }
}

// WS-REQ-0037 (review R2-C1): the map is reached from two places only, both for files the
// workspace never writes again: Revision::open_rbin (tables of committed revisions) and
// LibraryCache::open_rlib (cached .rlib files). The SAFETY argument names exactly these.
#[test]
fn files_are_mapped_from_two_places_only() {
    let calls = |needle: &str| -> Vec<(String, usize)> {
        sources()
            .into_iter()
            .filter(|(path, _)| path.starts_with("src/"))
            .filter_map(|(path, text)| {
                let n = text
                    .lines()
                    .filter(|l| l.split("//").next().unwrap_or("").contains(needle))
                    .count();
                (n > 0).then_some((path, n))
            })
            .collect()
    };
    assert_eq!(
        calls("map_readonly("),
        [
            ("src/mmap.rs".to_string(), 1),
            ("src/rbin.rs".to_string(), 1)
        ],
        "the definition and its one caller, Rbin::map_immutable"
    );
    assert_eq!(
        calls("Rbin::map_immutable("),
        [
            ("src/cache.rs".to_string(), 1),
            ("src/workspace.rs".to_string(), 1)
        ],
        "LibraryCache::open_rlib and Revision::open_rbin"
    );
    let rbin = sources()
        .into_iter()
        .find(|(path, _)| path == "src/rbin.rs")
        .unwrap()
        .1;
    assert!(rbin.contains("pub(crate) fn map_immutable("));
    assert!(!rbin.contains("pub fn open("));
    let mmap =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mmap.rs")).unwrap();
    for entry in ["`Revision::open_rbin`", "`LibraryCache::open_rlib`"] {
        assert!(
            mmap.contains(entry),
            "the SAFETY comment argues for {entry}"
        );
    }
}

// Review CW-12: the NETLIST and LIBERTY codecs that the contract assigns to this crate are
// deferred, and the crate documentation says so instead of contradicting the contract.
#[test]
fn the_deferred_codecs_are_documented() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // The prose, with line breaks and doc-comment markers taken out.
    let prose = |text: String| {
        text.split_whitespace()
            .filter(|word| *word != "//!")
            .collect::<Vec<_>>()
            .join(" ")
    };
    let readme = prose(fs::read_to_string(root.join("README.md")).unwrap());
    let lib = prose(fs::read_to_string(root.join("src/lib.rs")).unwrap());
    for text in [&readme, &lib] {
        for needle in [
            "The NETLIST and LIBERTY codecs that the contract (§1, §5) assigns to this crate",
            "the normative LIBERTY v1 grammar, are deferred until `rusteda-core` is available",
        ] {
            assert!(text.contains(needle), "{needle}");
        }
    }
    assert!(!readme.contains("does not know what a netlist, a library or a timing graph"));
}

// WS-REQ-0035
#[test]
fn big_endian_targets_are_rejected() {
    let lib = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")).unwrap();
    let guard = lib
        .find("#[cfg(not(target_endian = \"little\"))]")
        .expect("the endian guard");
    assert!(
        lib[guard..]
            .trim_start_matches(|c| c != '\n')
            .trim_start()
            .starts_with("compile_error!")
    );
}
