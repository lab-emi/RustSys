// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The library cache (WS-REQ-0080 to WS-REQ-0082) and the library entries of a revision
//! (WS-REQ-0073).

mod common;

use std::cell::Cell;
use std::fs;

use common::{entries, inv, new_workspace};
use rusteda_workspace::{
    Error, ErrorCode, Hash64, JsonMap, LibraryEntry, PutError, Rbin, RbinWriter, Rev,
};

/// A stand-in compiler: an rbin of kind LIBERTY whose section 1 holds the library bytes
/// reversed (the codec itself is deferred).
fn reversed(bytes: &[u8]) -> Vec<u8> {
    compiled(&bytes.iter().rev().copied().collect::<Vec<u8>>())
}

/// An rbin of kind LIBERTY whose section 1 holds `content`.
fn compiled(content: &[u8]) -> Vec<u8> {
    let mut w = RbinWriter::new("LIBERTY", 1).unwrap();
    w.section_bytes(1, 1, content.to_vec()).unwrap();
    w.to_bytes()
}

/// The content of section 1 of a compiled library.
fn content(rlib: &[u8]) -> Vec<u8> {
    let r = Rbin::from_bytes(rlib.to_vec(), "LIBERTY", 1..=1).unwrap();
    r.section::<u8>(1).unwrap().to_vec()
}

#[test]
fn put_library_stores_once() {
    let ws = new_workspace("cache");
    let cache = ws.cache();
    let calls = Cell::new(0);
    let compile = |bytes: &[u8]| -> Result<Vec<u8>, String> {
        calls.set(calls.get() + 1);
        Ok(reversed(bytes))
    };
    let lib_bytes = b"library (cells_a) { }";
    let h16 = Hash64::of(lib_bytes).hex();

    let a = cache
        .put_library("cells_a", lib_bytes, "LIBERTY", 1..=1, compile)
        .unwrap();
    assert!(a.compiled_now);
    assert_eq!(calls.get(), 1);
    assert_eq!(a.hash, Hash64::of(lib_bytes));
    assert_eq!(a.lib, format!("cache/cells_a-{h16}.lib"));
    assert_eq!(a.rlib, format!("cache/cells_a-{h16}.rlib"));
    assert_eq!(a.lib_path, ws.root().join(&a.lib));
    assert_eq!(fs::read(&a.lib_path).unwrap(), lib_bytes);
    assert_eq!(fs::read(&a.rlib_path).unwrap(), reversed(lib_bytes));
    assert_eq!(
        content(&fs::read(&a.rlib_path).unwrap()),
        lib_bytes.iter().rev().copied().collect::<Vec<u8>>()
    );

    // Both files exist: nothing is compiled or written.
    let again = cache
        .put_library("cells_a", lib_bytes, "LIBERTY", 1..=1, compile)
        .unwrap();
    assert!(!again.compiled_now);
    assert_eq!(calls.get(), 1);
    assert_eq!(
        entries(&cache.dir()),
        [format!("cells_a-{h16}.lib"), format!("cells_a-{h16}.rlib")]
    );

    // Other bytes, other files.
    let b = cache
        .put_library(
            "cells_a",
            b"library (cells_a) { cell (x) { } }",
            "LIBERTY",
            1..=1,
            compile,
        )
        .unwrap();
    assert_ne!(b.hash, a.hash);
    assert_eq!(calls.get(), 2);
    assert_eq!(entries(&cache.dir()).len(), 4);

    // A missing .rlib is compiled again from the bytes.
    fs::remove_file(&a.rlib_path).unwrap();
    assert!(cache.lookup("cells_a", a.hash).is_none());
    let rebuilt = cache
        .put_library("cells_a", lib_bytes, "LIBERTY", 1..=1, compile)
        .unwrap();
    assert!(rebuilt.compiled_now);
    assert_eq!(calls.get(), 3);

    // Lookup by name and hash, or by hash alone.
    assert_eq!(cache.lookup("cells_a", a.hash).unwrap(), again);
    assert!(cache.lookup("cells_b", a.hash).is_none());
    assert!(cache.lookup("../cells_a", a.hash).is_none());
    cache
        .put_library("alias", lib_bytes, "LIBERTY", 1..=1, compile)
        .unwrap();
    let names: Vec<String> = cache
        .find(a.hash)
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, ["alias", "cells_a"]);
    assert!(cache.find(Hash64(1)).unwrap().is_empty());

    let entry = a.entry("default", "libs/cells_a.lib");
    assert_eq!(
        (entry.name.as_str(), entry.corner.as_str()),
        ("cells_a", "default")
    );
    assert_eq!(
        (entry.hash, entry.compiled.as_str()),
        (a.hash, a.rlib.as_str())
    );
    assert_eq!(entry.source, "libs/cells_a.lib");
}

#[test]
fn compile_errors_and_recompile() {
    let ws = new_workspace("cache-errors");
    let cache = ws.cache();
    let failed = cache.put_library("x", b"bad", "LIBERTY", 1..=1, |_| {
        Err::<Vec<u8>, _>("syntax error at 1:1")
    });
    match failed {
        Err(PutError::Compile(message)) => assert_eq!(message, "syntax error at 1:1"),
        other => panic!("expected a compile error: {other:?}"),
    }
    let names = entries(&cache.dir());
    assert_eq!(
        names.len(),
        1,
        "the verbatim copy stays, no .rlib: {names:?}"
    );
    assert!(names[0].ends_with(".lib"));

    let ok = cache
        .put_library("x", b"bad", "LIBERTY", 1..=1, |b| {
            Ok::<_, String>(compiled(b))
        })
        .unwrap();
    assert!(ok.compiled_now);
    let again = cache
        .recompile("x", ok.hash, "LIBERTY", 1..=1, |b| {
            Ok::<_, String>(compiled(&[b.len() as u8]))
        })
        .unwrap();
    assert!(again.compiled_now);
    assert_eq!(content(&fs::read(&ok.rlib_path).unwrap()), [3]);

    fs::write(&ok.lib_path, b"changed").unwrap();
    match cache.recompile("x", ok.hash, "LIBERTY", 1..=1, |b| {
        Ok::<_, String>(compiled(b))
    }) {
        Err(PutError::Workspace(e)) => assert_eq!(e.code(), ErrorCode::CorruptTable),
        other => panic!("expected a hash mismatch: {other:?}"),
    }
    for bad in ["", ".x", "a/b", "a b", "../x", "x\0"] {
        match cache.put_library(bad, b"lib", "LIBERTY", 1..=1, |b| {
            Ok::<_, String>(compiled(b))
        }) {
            Err(PutError::Workspace(e)) => {
                assert_eq!(e.code(), ErrorCode::InputRejected, "{bad:?}")
            }
            other => panic!("{bad:?}: {other:?}"),
        }
    }
}

#[test]
fn library_in_a_revision() {
    let ws = new_workspace("cache-revision");
    let cached = ws
        .cache()
        .put_library("cells_a", b"lib", "LIBERTY", 1..=1, |b| {
            Ok::<_, String>(compiled(b))
        })
        .unwrap();
    let mut txn = ws
        .begin(
            ws.resolve_base("rev0").unwrap(),
            &inv("rusteda import --lib a.lib"),
        )
        .unwrap();
    txn.add_library(cached.entry("default", "a.lib")).unwrap();
    let upstream = txn.upstream(&["libraries"]).unwrap();
    txn.add_bytes("liberty", "liberty.json", b"{}", upstream)
        .unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    assert_eq!(c.meta.libraries[0].compiled, cached.rlib);
    let revision = ws.revision(c.rev).unwrap();
    let library = &revision.meta().libraries[0];
    assert_eq!(
        ws.cache()
            .lookup(&library.name, library.hash)
            .unwrap()
            .rlib_path,
        cached.rlib_path
    );
    assert_eq!(ws.manifest().unwrap().corners, ["default"]);
}

// WS-REQ-0080 (review CR2-3): the compiled form of a library is a valid rbin file (structure
// and content hash), checked before it is written; a compiler that returns anything else is a
// bug (INTERNAL), and nothing is stored. A cached .rlib that fails the check, left by an older
// release or damaged, is compiled again by the next put_library instead of being kept.
#[test]
fn compiled_libraries_are_checked() {
    let ws = new_workspace("cache-checked");
    let cache = ws.cache();
    let lib = b"library(cells) {}";
    let good = compiled(b"model");
    let mut flipped = good.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0xff;
    for (what, bytes) in [
        ("not an rbin", b"not an rbin".to_vec()),
        ("content hash", flipped),
        ("truncated", good[..good.len() - 8].to_vec()),
    ] {
        let err = cache
            .put_library("cells", lib, "LIBERTY", 1..=1, |_| {
                Ok::<_, String>(bytes.clone())
            })
            .unwrap_err();
        match err {
            PutError::Workspace(e) => assert_eq!(e.code(), ErrorCode::Internal, "{what}: {e}"),
            other => panic!("{what}: {other:?}"),
        }
        let names = entries(&cache.dir());
        assert!(
            names.iter().all(|n| !n.ends_with(".rlib")),
            "{what}: {names:?}"
        );
    }
    let stored = cache
        .put_library("cells", lib, "LIBERTY", 1..=1, |_| {
            Ok::<_, String>(good.clone())
        })
        .unwrap();
    assert!(stored.compiled_now);

    // recompile refuses a bad result too, and keeps the old .rlib.
    let err = cache
        .recompile("cells", stored.hash, "LIBERTY", 1..=1, |_| {
            Ok::<_, String>(b"junk".to_vec())
        })
        .unwrap_err();
    assert!(matches!(err, PutError::Workspace(ref e) if e.code() == ErrorCode::Internal));
    assert_eq!(fs::read(&stored.rlib_path).unwrap(), good);

    // A damaged .rlib in the cache is compiled again, not kept forever.
    let mut damaged = good.clone();
    damaged[last] ^= 0x01;
    fs::remove_file(&stored.rlib_path).unwrap();
    fs::write(&stored.rlib_path, &damaged).unwrap();
    let healed = cache
        .put_library("cells", lib, "LIBERTY", 1..=1, |_| {
            Ok::<_, String>(good.clone())
        })
        .unwrap();
    assert!(healed.compiled_now, "the damaged .rlib is compiled again");
    assert_eq!(fs::read(&stored.rlib_path).unwrap(), good);
    let again = cache
        .put_library(
            "cells",
            lib,
            "LIBERTY",
            1..=1,
            |_| -> Result<Vec<u8>, String> { panic!("a valid .rlib is not compiled again") },
        )
        .unwrap();
    assert!(!again.compiled_now);
}

// WS-REQ-0025, WS-REQ-0082 (review CR2-5): the cache reads regular files only and never
// follows a symbolic link: a .rlib replaced by a link is not found, not mapped, and is
// replaced by a regular file on the next put_library. open_rlib maps a valid one.
#[cfg(unix)]
#[test]
fn cached_libraries_are_regular_files() {
    let ws = new_workspace("cache-regular");
    let cache = ws.cache();
    let lib = b"library(cells) {}";
    let stored = cache
        .put_library("cells", lib, "LIBERTY", 1..=1, |b| {
            Ok::<_, String>(compiled(b))
        })
        .unwrap();
    let rlib = cache
        .open_rlib("cells", stored.hash, "LIBERTY", 1..=1)
        .unwrap();
    rlib.verify_hash().unwrap();
    assert_eq!(rlib.section::<u8>(1).unwrap(), lib);
    let err = cache
        .open_rlib("cells", stored.hash, "STA", 1..=1)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CorruptTable);
    let err = cache
        .open_rlib("../cells", stored.hash, "LIBERTY", 1..=1)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InputRejected);

    let outside = ws.root().parent().unwrap().join("outside.rlib");
    fs::write(&outside, compiled(b"elsewhere")).unwrap();
    fs::remove_file(&stored.rlib_path).unwrap();
    std::os::unix::fs::symlink(&outside, &stored.rlib_path).unwrap();
    assert!(cache.lookup("cells", stored.hash).is_none());
    assert!(cache.find(stored.hash).unwrap().is_empty());
    let err = cache
        .open_rlib("cells", stored.hash, "LIBERTY", 1..=1)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CorruptTable, "{err}");
    let healed = cache
        .put_library("cells", lib, "LIBERTY", 1..=1, |b| {
            Ok::<_, String>(compiled(b))
        })
        .unwrap();
    assert!(healed.compiled_now);
    let meta = fs::symlink_metadata(&stored.rlib_path).unwrap();
    assert!(meta.file_type().is_file(), "the link is replaced");
    assert_eq!(
        fs::read(&outside).unwrap(),
        compiled(b"elsewhere"),
        "not written through"
    );
}

/// A library entry from its parts.
fn entry(name: &str, hash: u64, compiled: &str) -> LibraryEntry {
    LibraryEntry {
        name: name.into(),
        corner: "default".into(),
        hash: Hash64(hash),
        source: format!("{name}.lib"),
        compiled: compiled.into(),
    }
}

// WS-REQ-0073, WS-REQ-0082, WS-REQ-0083, WS-REQ-0084 (review CR2-5): a library entry names its
// compiled form at its cache path, cache/<name>-<h16>.rlib, on write and on read, and a
// revision's library is mapped through Revision::open_library, which never leaves the cache.
#[test]
fn library_entries_point_into_the_cache() {
    let ws = new_workspace("cache-entries");
    let cached = ws
        .cache()
        .put_library("cells", b"library(cells) {}", "LIBERTY", 1..=1, |b| {
            Ok::<_, String>(compiled(b))
        })
        .unwrap();
    let mut txn = ws
        .begin(
            ws.resolve_base("rev0").unwrap(),
            &inv("rusteda import --lib cells.lib"),
        )
        .unwrap();
    let canonical = format!("cache/cells-{}.rlib", Hash64(5).hex());
    for (library, code) in [
        (entry("my lib", 5, &canonical), ErrorCode::InputRejected),
        (entry(".hidden", 5, &canonical), ErrorCode::InputRejected),
        (entry("cells", 5, "../outside.rlib"), ErrorCode::Internal),
        (entry("cells", 5, "/etc/hostname"), ErrorCode::Internal),
        (entry("cells", 6, &canonical), ErrorCode::Internal),
        (
            entry("cells", 5, "cache/other-0000000000000005.rlib"),
            ErrorCode::Internal,
        ),
    ] {
        let err = txn.add_library(library.clone()).unwrap_err();
        assert_eq!(err.code(), code, "{library:?}: {err}");
        assert!(txn.set_libraries(vec![library]).is_err());
    }
    txn.add_library(cached.entry("default", "cells.lib"))
        .unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    let rev = ws.revision(c.rev).unwrap();
    let library = &rev.meta().libraries[0];
    let rlib = rev.open_library(library, "LIBERTY", 1..=1).unwrap();
    assert_eq!(rlib.path(), cached.rlib_path);
    rlib.verify_hash().unwrap();
    // A library that is not one of the revision's is a bug of the caller.
    let foreign = entry("cells", 7, &format!("cache/cells-{}.rlib", Hash64(7).hex()));
    let err = rev.open_library(&foreign, "LIBERTY", 1..=1).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal);

    // On read, an entry that leaves the cache makes the revision CORRUPT_TABLE.
    let meta = ws.revision_dir(Rev(1)).join("meta.json");
    let text = fs::read_to_string(&meta).unwrap();
    for (from, to) in [
        (cached.rlib.as_str(), "../outside.rlib"),
        ("\"name\": \"cells\"", "\"name\": \"my lib\""),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&meta, text.replace(from, to)).unwrap();
        let err = ws.revision(Rev(1)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable, "{to}: {err}");
        assert!(matches!(err, Error::Corrupt { .. }));
    }
    let twice = text.replacen(
        "\"libraries\": [",
        &format!(
            "\"libraries\": [{},",
            serde_json::to_string(&rev.meta().libraries[0]).unwrap()
        ),
        1,
    );
    fs::write(&meta, twice).unwrap();
    let err = ws.revision(Rev(1)).unwrap_err();
    assert!(err.to_string().contains("appears twice"), "{err}");
    fs::write(&meta, &text).unwrap();
    ws.revision(Rev(1)).unwrap();
}
