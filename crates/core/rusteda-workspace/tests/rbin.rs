// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The rbin container on disk: round trip, zero-copy views, every corruption class
//! (WS-REQ-0030 to WS-REQ-0036), what may be mapped (WS-REQ-0037) and the checks of a table
//! that enters a revision (WS-REQ-0078).

mod common;

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use common::{inv, new_workspace, temp_dir};
use rusteda_workspace::rbin::upstream_names;
use rusteda_workspace::{
    Corruption, Error, ErrorCode, Hash64, JsonMap, LibraryEntry, Rbin, RbinWriter, Rev, Upstream,
};

/// A 16-byte record like the netlist's PORTS section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
#[repr(C)]
struct PortRec {
    name: u32,
    dir: u8,
    is_bus: u8,
    pad: u16,
    msb: i32,
    lsb: i32,
}

struct Data {
    strings: Vec<u8>,
    offsets: Vec<u32>,
    ports: Vec<PortRec>,
    nets: Vec<[u32; 2]>,
}

fn data() -> Data {
    Data {
        strings: b"topclkdata_in".to_vec(),
        offsets: vec![0, 3, 6, 13],
        ports: vec![
            PortRec {
                name: 1,
                dir: 0,
                is_bus: 0,
                pad: 0,
                msb: 0,
                lsb: 0,
            },
            PortRec {
                name: 2,
                dir: 0,
                is_bus: 1,
                pad: 0,
                msb: 7,
                lsb: 0,
            },
        ],
        nets: (0..9).map(|i| [i, i * 2]).collect(),
    }
}

/// Upstream entries at 48 and 64; section entries at 80 + 24 i: ids 1, 2, 8, 11.
fn writer(d: &Data) -> RbinWriter<'_> {
    let mut w = RbinWriter::new("NETLIST", 1).unwrap();
    w.upstream("rtl", Hash64(0xaaaa)).unwrap();
    w.upstream("LIBS", Hash64(0xbbbb)).unwrap();
    w.section(1, &d.strings).unwrap();
    w.section(2, &d.offsets).unwrap();
    w.section(8, &d.ports).unwrap();
    w.section(11, &d.nets).unwrap();
    w
}

const SECTIONS_AT: usize = 80;

fn entry_at(i: usize) -> usize {
    SECTIONS_AT + 24 * i
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn get_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// Opens corrupted bytes both from a file and from memory; both must fail the same way.
fn corruption(dir: &Path, name: &str, bytes: &[u8]) -> Corruption {
    let path = write_file(dir, name, bytes);
    let from_file = Rbin::read(&path, "NETLIST", 1..=1).unwrap_err();
    let from_memory = Rbin::from_bytes(bytes.to_vec(), "NETLIST", 1..=1).unwrap_err();
    assert_eq!(
        from_file.code(),
        ErrorCode::CorruptTable,
        "{name}: {from_file}"
    );
    assert_eq!(from_file.exit_code(), 2);
    let (Error::Corrupt { path: p, what: a }, Error::Corrupt { what: b, .. }) =
        (from_file, from_memory)
    else {
        panic!("{name}: not a corruption error");
    };
    assert_eq!(p, path);
    assert_eq!(a, b, "{name}");
    a
}

// WS-REQ-0030, WS-REQ-0033, WS-REQ-0036
#[test]
fn round_trip_through_a_file() {
    let dir = temp_dir("rbin-round-trip");
    let d = data();
    let w = writer(&d);
    let path = dir.join("netlist.rbin");
    let file_hash = w.write_to(&path).unwrap();
    assert_eq!(file_hash, Hash64::of(&fs::read(&path).unwrap()));
    assert_eq!(fs::read(&path).unwrap(), w.to_bytes());
    assert_eq!(fs::metadata(&path).unwrap().len(), w.file_len());

    let r = Rbin::read(&path, "NETLIST", 1..=1).unwrap();
    assert_eq!(r.path(), path);
    assert_eq!((r.kind(), r.format_version()), ("NETLIST", 1));
    assert_eq!(r.file_hash(), file_hash);
    assert_eq!(r.content_hash(), w.content_hash());
    r.verify_hash().unwrap();
    let names: Vec<&str> = r.upstream().iter().map(|u| u.name.as_str()).collect();
    assert_eq!(names, ["LIBS", "rtl"]);
    assert_eq!(r.upstream_hash("rtl"), Some(Hash64(0xaaaa)));
    assert_eq!(r.section::<u8>(1).unwrap(), d.strings.as_slice());
    assert_eq!(r.section::<u32>(2).unwrap(), d.offsets.as_slice());
    assert_eq!(r.section::<PortRec>(8).unwrap(), d.ports.as_slice());
    assert_eq!(r.section::<[u32; 2]>(11).unwrap(), d.nets.as_slice());
    assert_eq!(r.section_info(8).unwrap().count(), 2);
    assert!(!r.has_section(3));

    // A file is never overwritten.
    let err = w.write_to(&path).unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoError);
    assert_eq!(fs::read(&path).unwrap(), w.to_bytes());
}

/// Views point into the table's bytes, at the section offsets, without copies.
fn assert_zero_copy(r: &Rbin) {
    let base = r.as_bytes().as_ptr() as usize;
    for s in r.sections() {
        let view = r.section_bytes(s.id).unwrap();
        assert_eq!(
            view.as_ptr() as usize - base,
            s.offset as usize,
            "section {}",
            s.id
        );
        assert_eq!(s.offset % 8, 0);
    }
    let ports = r.section::<PortRec>(8).unwrap();
    assert_eq!(
        ports.as_ptr() as usize - base,
        r.section_info(8).unwrap().offset as usize
    );
    assert!(std::ptr::eq(ports, r.section::<PortRec>(8).unwrap()));
}

// WS-REQ-0033: views point into the mapped table of a revision, and into the buffer of a table
// read from a file or memory, at the section offsets, without copies.
#[test]
fn zero_copy_views() {
    let ws = new_workspace("rbin-zero-copy");
    let d = data();
    // A table of a revision records its upstream map, here an empty one.
    let mut w = RbinWriter::new("NETLIST", 1).unwrap();
    w.section(1, &d.strings).unwrap();
    w.section(2, &d.offsets).unwrap();
    w.section(8, &d.ports).unwrap();
    w.section(11, &d.nets).unwrap();
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool put"))
        .unwrap();
    txn.add_rbin("t", "t.rbin", &w, Upstream::new()).unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    let mapped = ws
        .revision(c.rev)
        .unwrap()
        .open_rbin("t", "NETLIST", 1..=1)
        .unwrap();
    assert_zero_copy(&mapped);
    let read = Rbin::read(c.dir.join("t.rbin"), "NETLIST", 1..=1).unwrap();
    assert_zero_copy(&read);
    let in_memory = Rbin::from_bytes(w.to_bytes(), "NETLIST", 1..=1).unwrap();
    assert_zero_copy(&in_memory);
    let ports = mapped.section::<PortRec>(8).unwrap();
    assert_eq!(ports, d.ports.as_slice());
    assert_eq!(read.section::<PortRec>(8).unwrap(), ports);
    assert_eq!(in_memory.section::<PortRec>(8).unwrap(), ports);
}

// WS-REQ-0032: every corruption class is caught when the file is opened.
#[test]
fn corruption_classes() {
    let dir = temp_dir("rbin-corrupt");
    let d = data();
    let good = writer(&d).to_bytes();
    let header_len = get_u64(&good, 40);
    assert_eq!(header_len, (SECTIONS_AT + 4 * 24) as u64);
    let offset_of = |i: usize| get_u64(&good, entry_at(i) + 8);
    let len_of = |i: usize| get_u64(&good, entry_at(i) + 16);
    let patched = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut b = good.clone();
        f(&mut b);
        b
    };

    assert_eq!(
        corruption(&dir, "empty", &[]),
        Corruption::Truncated { needed: 48, len: 0 }
    );
    assert_eq!(
        corruption(&dir, "short", &good[..40]),
        Corruption::Truncated {
            needed: 48,
            len: 40
        }
    );
    assert_eq!(
        corruption(&dir, "header-cut", &good[..100]),
        Corruption::Truncated {
            needed: header_len,
            len: 100
        }
    );
    assert_eq!(
        corruption(&dir, "magic", &patched(&|b| b[0] = b'X')),
        Corruption::BadMagic
    );
    assert_eq!(
        corruption(&dir, "version", &patched(&|b| put_u32(b, 16, 0))),
        Corruption::UnsupportedVersion {
            found: 0,
            min: 1,
            max: 1
        }
    );
    // WS-REQ-0039 (review CW-5): a newer version comes from a newer release: SCHEMA_TOO_NEW,
    // exit 1, "upgrade the tools", from a file and from memory alike.
    let newer = patched(&|b| put_u32(b, 16, 2));
    let path = write_file(&dir, "newer", &newer);
    for err in [
        Rbin::read(&path, "NETLIST", 1..=1).unwrap_err(),
        Rbin::from_bytes(newer.clone(), "NETLIST", 1..=1).unwrap_err(),
    ] {
        assert_eq!((err.code(), err.exit_code()), (ErrorCode::SchemaTooNew, 1));
        assert!(
            matches!(
                err,
                Error::FormatTooNew {
                    found: 2,
                    min: 1,
                    max: 1,
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.help().unwrap().contains("upgrade the tools"), "{err}");
    }
    assert_eq!(
        corruption(&dir, "flags", &patched(&|b| put_u32(b, 20, 1))),
        Corruption::Flags { flags: 1 }
    );
    assert_eq!(
        corruption(&dir, "kind-name", &patched(&|b| b[9] = 0)),
        Corruption::BadName { field: "kind" }
    );
    assert_eq!(
        corruption(&dir, "upstream-name", &patched(&|b| b[48] = b' ')),
        Corruption::BadName {
            field: "upstream name"
        }
    );
    assert_eq!(
        corruption(
            &dir,
            "header-len-long",
            &patched(&|b| put_u64(b, 40, header_len + 8))
        ),
        Corruption::HeaderLen {
            declared: header_len + 8,
            expected: header_len
        }
    );
    assert_eq!(
        corruption(
            &dir,
            "header-len-short",
            &patched(&|b| put_u64(b, 40, header_len - 24))
        ),
        Corruption::HeaderLen {
            declared: header_len - 24,
            expected: header_len
        }
    );
    assert!(matches!(
        corruption(
            &dir,
            "section-count",
            &patched(&|b| put_u32(b, 32, u32::MAX))
        ),
        Corruption::HeaderLen { .. }
    ));
    let truncated = &good[..good.len() - 8];
    assert_eq!(
        corruption(&dir, "truncated", truncated),
        Corruption::SectionBounds {
            id: 11,
            offset: offset_of(3),
            len: len_of(3),
            file_len: truncated.len() as u64
        }
    );
    assert!(matches!(
        corruption(
            &dir,
            "bounds-overflow",
            &patched(&|b| put_u64(b, entry_at(3) + 16, u64::MAX - 7))
        ),
        Corruption::SectionBounds { id: 11, .. }
    ));
    assert_eq!(
        corruption(
            &dir,
            "misaligned",
            &patched(&|b| put_u64(b, entry_at(1) + 8, offset_of(1) + 4))
        ),
        Corruption::Misaligned {
            id: 2,
            offset: offset_of(1) + 4
        }
    );
    assert_eq!(
        corruption(
            &dir,
            "len-elem",
            &patched(&|b| put_u64(b, entry_at(1) + 16, len_of(1) - 1))
        ),
        Corruption::ElemSize {
            id: 2,
            elem_size: 4,
            len: len_of(1) - 1
        }
    );
    assert_eq!(
        corruption(
            &dir,
            "elem-zero",
            &patched(&|b| put_u32(b, entry_at(2) + 4, 0))
        ),
        Corruption::ElemSize {
            id: 8,
            elem_size: 0,
            len: len_of(2)
        }
    );
    assert_eq!(
        corruption(
            &dir,
            "overlap",
            &patched(&|b| put_u64(b, entry_at(2) + 8, offset_of(1)))
        ),
        Corruption::Overlap { id: 8 }
    );
    assert_eq!(
        corruption(
            &dir,
            "overlap-header",
            &patched(&|b| put_u64(b, entry_at(0) + 8, 8))
        ),
        Corruption::Overlap { id: 1 }
    );
    let swapped = patched(&|b| {
        let first: Vec<u8> = b[entry_at(0)..entry_at(1)].to_vec();
        b.copy_within(entry_at(1)..entry_at(2), entry_at(0));
        b[entry_at(1)..entry_at(2)].copy_from_slice(&first);
    });
    assert_eq!(
        corruption(&dir, "unsorted", &swapped),
        Corruption::SectionOrder { id: 1 }
    );
    assert_eq!(
        corruption(&dir, "duplicate", &patched(&|b| put_u32(b, entry_at(1), 1))),
        Corruption::SectionOrder { id: 1 }
    );

    // A wrong kind, or a version this reader does not support.
    let path = write_file(&dir, "good", &good);
    let err = Rbin::read(&path, "STA", 1..=1).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Corrupt {
                what: Corruption::WrongKind { .. },
                ..
            }
        ),
        "{err}"
    );
    assert!(
        err.to_string()
            .contains("the table kind is NETLIST, expected STA"),
        "{err}"
    );
    let err = Rbin::read(&path, "NETLIST", 2..=3).unwrap_err();
    assert!(matches!(
        err,
        Error::Corrupt {
            what: Corruption::UnsupportedVersion {
                found: 1,
                min: 2,
                max: 3
            },
            ..
        }
    ));
    let err = Rbin::read(dir.join("absent"), "NETLIST", 1..=1).unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoError);
    // WS-REQ-0025: only regular files are read, and symbolic links are not followed.
    let err = Rbin::read(&dir, "NETLIST", 1..=1).unwrap_err();
    assert_eq!(err.code(), ErrorCode::CorruptTable);
    assert!(err.to_string().ends_with("not a regular file"), "{err}");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&path, dir.join("link")).unwrap();
        let err = Rbin::read(dir.join("link"), "NETLIST", 1..=1).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
    }
}

/// An rbin table of `kind` with one small section.
fn small(kind: &str) -> RbinWriter<'static> {
    let mut w = RbinWriter::new(kind, 1).unwrap();
    w.section_vec(1, vec![kind.len() as u32; 4]).unwrap();
    w
}

// WS-REQ-0038 (review CW-3): an rbin table of a revision records its upstream map in its
// header: one entry per upstream table, the long contract names included, under the canonical
// 8-byte name and with the hash meta.json records. add_rbin writes the entries, the other add_*
// methods and open_rbin check them.
#[test]
fn header_upstream_of_workspace_tables() {
    let ws = new_workspace("rbin-header-upstream");
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("rusteda import"))
        .unwrap();
    txn.add_library(LibraryEntry {
        name: "cells".into(),
        corner: "default".into(),
        hash: Hash64(5),
        source: "cells.lib".into(),
        compiled: "cache/cells-0000000000000005.rlib".into(),
    })
    .unwrap();
    let netlist = txn
        .add_rbin(
            "netlist",
            "netlist.rbin",
            &small("NETLIST"),
            Upstream::new(),
        )
        .unwrap();
    let constraints = txn
        .add_rbin(
            "constraints",
            "constraints.rbin",
            &small("CONSTR"),
            Upstream::new(),
        )
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();

    let mut txn = ws
        .begin(ws.resolve_base("rev1").unwrap(), &inv("ruststa run"))
        .unwrap();
    let upstream = txn
        .upstream(&["netlist", "constraints", "libraries"])
        .unwrap();
    txn.add_rbin("sta", "sta.rbin", &small("STA"), upstream.clone())
        .unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    let rev2 = ws.revision(c.rev).unwrap();
    let sta = rev2.open_rbin("sta", "STA", 1..=1).unwrap();
    assert_eq!(sta.upstream().len(), 3);
    let names = upstream_names(&upstream).unwrap();
    for (table, hash) in &upstream {
        assert_eq!(sta.upstream_hash(&names[table]), Some(*hash), "{table}");
    }
    // The table hashes of meta.json: for an rbin upstream, the file hash, not its content hash.
    assert_eq!(sta.upstream_hash("netlist"), Some(netlist));
    assert_eq!(sta.upstream_hash(&names["constraints"]), Some(constraints));
    let constraints_table = rev2.open_rbin("constraints", "CONSTR", 1..=1).unwrap();
    assert_ne!(constraints_table.content_hash(), constraints);
    assert_eq!(constraints_table.file_hash(), constraints);
    assert_eq!(
        sta.upstream_hash(&names["libraries"]),
        rev2.table_hash("libraries")
    );

    // Header entries other than the upstream map are refused when the table is added.
    let mut txn = ws
        .begin(ws.resolve_base("rev2").unwrap(), &inv("ruststa run"))
        .unwrap();
    let mut ad_hoc = small("STA");
    ad_hoc.upstream("LIBS", Hash64(1)).unwrap();
    let err = txn
        .add_rbin("sta", "sta.rbin", &ad_hoc, upstream.clone())
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal);
    let bare = small("STA");
    let err = txn
        .add_bytes("sta2", "sta2.rbin", &bare.to_bytes(), upstream.clone())
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal);
    assert!(
        err.to_string().contains("does not record its upstream map"),
        "{err}"
    );
    let stray = txn.staging_path("sta3.rbin").unwrap();
    bare.write_to(&stray).unwrap();
    let err = txn
        .add_file("sta3", "sta3.rbin", upstream.clone())
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal);
    fs::remove_file(&stray).unwrap();
    // The canonical entries, set by the tool itself, are accepted by every add_* method.
    let mut canonical = small("STA");
    canonical.upstream_tables(&upstream).unwrap();
    txn.add_rbin("sta", "sta.rbin", &canonical, upstream.clone())
        .unwrap();
    txn.add_bytes("sta2", "sta2.rbin", &canonical.to_bytes(), upstream.clone())
        .unwrap();
    canonical
        .write_to(txn.staging_path("sta3.rbin").unwrap())
        .unwrap();
    txn.add_file("sta3", "sta3.rbin", upstream.clone()).unwrap();
    let c3 = txn.commit(JsonMap::new()).unwrap();
    let rev3 = ws.revision(c3.rev).unwrap();
    for name in ["sta", "sta2", "sta3"] {
        rev3.open_rbin(name, "STA", 1..=1).unwrap();
    }

    // A header that disagrees with meta.json is CORRUPT_TABLE when the table is opened.
    let meta = ws.revision_dir(c3.rev).join("meta.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&meta).unwrap()).unwrap();
    value["tables"]["sta"]["upstream"]["netlist"] = Hash64(1).to_string().into();
    fs::write(&meta, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    let err = ws
        .revision(c3.rev)
        .unwrap()
        .open_rbin("sta", "STA", 1..=1)
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Corrupt {
                what: Corruption::HeaderUpstream { .. },
                ..
            }
        ),
        "{err}"
    );
}

// WS-REQ-0034: a changed byte passes the structural checks but not the hash check.
#[test]
fn hash_mismatch_on_verify() {
    let dir = temp_dir("rbin-hash");
    let d = data();
    let good = writer(&d).to_bytes();
    let mut bad = good.clone();
    let at = get_u64(&good, entry_at(2) + 8) as usize; // first byte of the ports
    bad[at] ^= 0x40;
    let path = write_file(&dir, "bad", &bad);
    let r = Rbin::read(&path, "NETLIST", 1..=1).expect("structure is intact");
    assert_eq!(r.section::<PortRec>(8).unwrap()[0].name, 1 ^ 0x40);
    let err = r.verify_hash().unwrap_err();
    let Error::Corrupt {
        what: Corruption::HashMismatch { stored, computed },
        ..
    } = err
    else {
        panic!("expected a hash mismatch: {err}");
    };
    assert_eq!(stored, Hash64(get_u64(&good, 24)));
    assert_ne!(stored, computed);
    // The padding after a section is covered too.
    let mut padded = good.clone();
    let last = padded.len() - 1;
    let pad_at = get_u64(&good, entry_at(0) + 8) as usize + d.strings.len();
    assert!(!pad_at.is_multiple_of(8) && pad_at < last);
    padded[pad_at] = 1;
    let r = Rbin::from_bytes(padded, "NETLIST", 1..=1).unwrap();
    assert_eq!(r.verify_hash().unwrap_err().code(), ErrorCode::CorruptTable);
}

// WS-REQ-0033
#[test]
fn missing_sections_and_type_mismatch() {
    let d = data();
    let r = Rbin::from_bytes(writer(&d).to_bytes(), "NETLIST", 1..=1).unwrap();
    let err = r.section::<u32>(4).unwrap_err();
    assert!(matches!(
        err,
        Error::Corrupt {
            what: Corruption::MissingSection { id: 4 },
            ..
        }
    ));
    assert!(r.section_bytes(4).is_err());
    let err = r.section::<u64>(8).unwrap_err();
    assert!(matches!(
        err,
        Error::Corrupt {
            what: Corruption::TypeMismatch {
                id: 8,
                elem_size: 16,
                type_size: 8
            },
            ..
        }
    ));
    // Same size, other layout: allowed (the caller owns the record type).
    assert_eq!(r.section::<[u32; 4]>(8).unwrap().len(), 2);
}

// A larger table: several types, odd lengths, empty sections.
#[test]
fn many_sections() {
    let dir = temp_dir("rbin-many");
    let a: Vec<u8> = (0..100_003u32).map(|i| i as u8).collect();
    let b: Vec<u16> = (0..77_777u32).map(|i| i as u16).collect();
    let c: Vec<f64> = (0..50_001).map(|i| f64::from(i) * 0.5).collect();
    let e: Vec<u32> = Vec::new();
    let mut w = RbinWriter::new("STA", 3).unwrap();
    w.section(7, &c)
        .unwrap()
        .section(3, &a)
        .unwrap()
        .section(5, &b)
        .unwrap()
        .section(9, &e)
        .unwrap();
    let path = dir.join("sta.rbin");
    w.write_to(&path).unwrap();
    let r = Rbin::read(&path, "STA", 1..=3).unwrap();
    r.verify_hash().unwrap();
    assert_eq!(r.section::<u8>(3).unwrap(), a.as_slice());
    assert_eq!(r.section::<u16>(5).unwrap(), b.as_slice());
    assert_eq!(r.section::<f64>(7).unwrap(), c.as_slice());
    assert!(r.section::<u32>(9).unwrap().is_empty());
    assert_eq!(r.file_len() % 8, 0);
}

/// A table of `kind` with one section of `n` u64 words of `value`.
fn words(kind: &str, n: usize, value: u64) -> RbinWriter<'static> {
    let mut w = RbinWriter::new(kind, 1).unwrap();
    w.section_vec(1, vec![value; n]).unwrap();
    w
}

/// Overwrites the first element of section 1 in place: no new inode, no truncation.
fn poke(path: &Path, offset: u64) {
    let mut f = OpenOptions::new().write(true).open(path).unwrap();
    f.seek(SeekFrom::Start(offset)).unwrap();
    f.write_all(&0x2222_2222_2222_2222u64.to_le_bytes())
        .unwrap();
    f.sync_all().unwrap();
}

// WS-REQ-0037 (review R2-C1): a file outside a workspace is read, not mapped, so a later write
// in place does not change the table a caller holds. (That `Rbin::open`, which mapped any
// path, is no longer public is a compile_fail doctest of the crate.)
#[test]
fn read_tables_do_not_change_when_the_file_does() {
    let dir = temp_dir("rbin-read-in-place");
    let path = dir.join("t.rbin");
    words("ADV", 1024, 0x1111_1111_1111_1111)
        .write_to(&path)
        .unwrap();
    let r = Rbin::read(&path, "ADV", 1..=1).unwrap();
    let view: &[u64] = r.section(1).unwrap();
    poke(&path, r.section_info(1).unwrap().offset);
    assert_eq!(std::hint::black_box(view)[0], 0x1111_1111_1111_1111);
    r.verify_hash().unwrap();
    fs::write(&path, b"").unwrap();
    assert_eq!(std::hint::black_box(view)[1023], 0x1111_1111_1111_1111);
}

/// The child of `a_truncated_file_does_not_kill_a_reader`: reads the table, waits for the go
/// file, then sums the whole section.
#[test]
#[ignore = "child process of a_truncated_file_does_not_kill_a_reader"]
fn truncation_child() {
    let (Some(table), Some(go)) = (
        std::env::var_os("RUSTEDA_WS_READ_TABLE"),
        std::env::var_os("RUSTEDA_WS_READ_GO"),
    ) else {
        return;
    };
    let go = PathBuf::from(go);
    let r = Rbin::read(PathBuf::from(table), "ADV", 1..=1).unwrap();
    let view: &[u64] = r.section(1).unwrap();
    fs::write(go.with_extension("ready"), b"").unwrap();
    let start = Instant::now();
    while !go.exists() {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    let sum = view.iter().fold(0u64, |a, &v| a.wrapping_add(v));
    eprintln!("sum {sum:#x}");
}

// WS-REQ-0037 (review R2-C1): a reader of a file outside a workspace survives the truncation of
// the file, which kills a reader that maps it (SIGBUS).
#[cfg(unix)]
#[test]
fn a_truncated_file_does_not_kill_a_reader() {
    let dir = temp_dir("rbin-read-truncated");
    let table = dir.join("t.rbin");
    words("ADV", 1 << 16, 1).write_to(&table).unwrap();
    let go = dir.join("go");
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "truncation_child",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("RUSTEDA_WS_READ_TABLE", &table)
        .env("RUSTEDA_WS_READ_GO", &go)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !go.with_extension("ready").exists() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the child did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    OpenOptions::new()
        .write(true)
        .open(&table)
        .unwrap()
        .set_len(0)
        .unwrap();
    fs::write(&go, b"").unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the reader died: {:?}", out.status);
    assert!(stderr.contains("sum 0x10000"), "{stderr}");
}

/// Corrupted copies of a valid table: (what, bytes).
fn corrupted(good: &[u8]) -> Vec<(&'static str, Vec<u8>)> {
    let header_len = u64::from_le_bytes(good[40..48].try_into().unwrap()) as usize;
    let mut flipped = good.to_vec();
    flipped[header_len] ^= 0xff;
    let mut oob = good.to_vec();
    oob[48 + 16..48 + 24].copy_from_slice(&4096u64.to_le_bytes());
    let mut misaligned = good.to_vec();
    let offset = u64::from_le_bytes(good[48 + 8..48 + 16].try_into().unwrap());
    misaligned[48 + 8..48 + 16].copy_from_slice(&(offset + 4).to_le_bytes());
    vec![
        ("content byte flipped", flipped),
        ("section past the end", oob),
        ("misaligned section", misaligned),
        ("truncated", good[..good.len() - 8].to_vec()),
        ("header only", good[..header_len].to_vec()),
        ("magic only", good[..8].to_vec()),
    ]
}

// WS-REQ-0078 (review R2-C4, CR2-3): contract §5, "content hash on import". A table that enters
// a revision as an rbin file must pass the structural checks of every open and its content
// hash, whatever its kind; add_bytes and add_file refuse a damaged one (INTERNAL), so it is
// never committed under its own file hash and carried into later revisions.
#[test]
fn damaged_rbin_tables_are_refused_on_import() {
    let ws = new_workspace("rbin-import-checks");
    let good = words("STA", 8, 5).to_bytes();
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool import"))
        .unwrap();
    for (what, bytes) in corrupted(&good) {
        let err = txn
            .add_bytes("t", "t.rbin", &bytes, Upstream::new())
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal, "{what}: {err}");
        assert!(
            err.to_string().contains("fails validation"),
            "{what}: {err}"
        );
        let staged = txn.staging_path("t.rbin").unwrap();
        assert!(!staged.exists(), "{what}: nothing is written");
        fs::write(&staged, &bytes).unwrap();
        let err = txn.add_file("t", "t.rbin", Upstream::new()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal, "{what}: {err}");
        fs::remove_file(&staged).unwrap();
    }
    // The kind and version are the tool's business: any valid rbin is accepted.
    let mut other = RbinWriter::new("ZZZ", 9).unwrap();
    other.section_vec(3, vec![1u16, 2, 3]).unwrap();
    txn.add_bytes("other", "other.rbin", &other.to_bytes(), Upstream::new())
        .unwrap();
    let good_file = txn.staging_path("t.rbin").unwrap();
    fs::write(&good_file, &good).unwrap();
    let hash = txn.add_file("t", "t.rbin", Upstream::new()).unwrap();
    assert_eq!(hash, Hash64::of(&good));
    // Text that merely starts like an rbin magic is no rbin file.
    txn.add_bytes("note", "note.txt", b"RUSTEDA", Upstream::new())
        .unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    let r = ws.revision(c.rev).unwrap();
    r.open_rbin("t", "STA", 1..=1)
        .unwrap()
        .verify_hash()
        .unwrap();
    r.open_rbin("other", "ZZZ", 9..=9)
        .unwrap()
        .verify_hash()
        .unwrap();
}

/// A tiny deterministic generator for the mutation test.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

// WS-REQ-0078 (review R2-C4): mutated tables through add_bytes and add_file: no panic, every
// refusal INTERNAL, and every accepted rbin passes the checks of an open of its own kind and
// version and its content hash.
#[test]
fn mutated_tables_on_import() {
    const EXTREMES: [u64; 8] = [
        0,
        1,
        3,
        48,
        u32::MAX as u64,
        u32::MAX as u64 + 1,
        i64::MAX as u64,
        u64::MAX,
    ];
    let ws = new_workspace("rbin-import-mutations");
    let upstream = Upstream::from([
        ("netlist".to_string(), Hash64(7)),
        ("constraints".to_string(), Hash64(9)),
    ]);
    let mut w = RbinWriter::new("STA", 1).unwrap();
    w.upstream_tables(&upstream).unwrap();
    w.section_vec(1, vec![5u32; 7]).unwrap();
    w.section_vec(2, vec![1u8; 3]).unwrap();
    let good = w.to_bytes();
    let mut rng = Rng(0xfeed_beef_1234_5678);
    let mut txn = ws
        .begin(ws.resolve_base("rev0").unwrap(), &inv("tool mutate"))
        .unwrap();
    let (mut accepted, mut refused) = (0, 0);
    for round in 0..4000u32 {
        let mut bytes = good.clone();
        for _ in 0..1 + rng.below(3) {
            match rng.below(4) {
                0 => {
                    let at = rng.below(bytes.len() as u64) as usize;
                    bytes[at] = rng.next() as u8;
                }
                1 => {
                    let at = (rng.below(bytes.len() as u64 / 4) * 4) as usize;
                    let v = EXTREMES[rng.below(8) as usize] as u32;
                    bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
                }
                2 => {
                    let len = rng.below(bytes.len() as u64 + 1) as usize;
                    bytes.truncate(len.max(8));
                }
                _ => {
                    let at = (rng.below(bytes.len() as u64 / 8) * 8) as usize;
                    if at + 8 <= bytes.len() {
                        let v = EXTREMES[rng.below(8) as usize];
                        bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
        }
        let (name, path) = (format!("t{round}"), format!("t{round}.rbin"));
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            if round % 2 == 1 {
                let staged = txn.staging_path(&path).unwrap();
                fs::write(&staged, &bytes).unwrap();
                let added = txn.add_file(&name, &path, upstream.clone());
                if added.is_err() {
                    fs::remove_file(&staged).unwrap();
                }
                added
            } else {
                txn.add_bytes(&name, &path, &bytes, upstream.clone())
            }
        }));
        match outcome {
            Ok(Ok(_)) => {
                accepted += 1;
                if bytes.starts_with(b"RUSTEDA\0") {
                    let kind_len = bytes[8..16].iter().position(|&b| b == 0).unwrap_or(8);
                    let kind = std::str::from_utf8(&bytes[8..8 + kind_len]).unwrap();
                    let version = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
                    let r = Rbin::from_bytes(bytes.clone(), kind, version..=version)
                        .unwrap_or_else(|e| panic!("round {round}: accepted but {e}"));
                    r.verify_hash()
                        .unwrap_or_else(|e| panic!("round {round}: accepted but {e}"));
                }
            }
            Ok(Err(e)) => {
                assert_eq!(e.code(), ErrorCode::Internal, "round {round}: {e}");
                refused += 1;
            }
            Err(_) => panic!("round {round}: panic on {bytes:02x?}"),
        }
    }
    assert!(accepted > 0 && refused > 0, "{accepted} {refused}");
    txn.abort().unwrap();
}

// WS-REQ-0038 (review CR2-4): a table computed from two corners whose 16-bit header names
// collide is written and read: the names of one map are made distinct.
#[test]
fn tables_from_corners_with_colliding_names() {
    let ws = new_workspace("rbin-colliding-corners");
    let mut txn = ws
        .begin(
            ws.resolve_base("rev0").unwrap(),
            &inv("rusteda import --lib a.lib --lib b.lib"),
        )
        .unwrap();
    for (name, corner, h) in [("a", "ss_150C_1v60", 1u64), ("b", "sf_n40C_1v65", 2)] {
        txn.add_library(LibraryEntry {
            name: name.into(),
            corner: corner.into(),
            hash: Hash64(h),
            source: format!("{name}.lib"),
            compiled: format!("cache/{name}-{}.rlib", Hash64(h).hex()),
        })
        .unwrap();
    }
    txn.commit(JsonMap::new()).unwrap();
    let mut txn = ws
        .begin(ws.resolve_base("rev1").unwrap(), &inv("ruststa run"))
        .unwrap();
    let corners = ["libraries.ss_150C_1v60", "libraries.sf_n40C_1v65"];
    let upstream = txn.upstream(&corners).unwrap();
    let names = upstream_names(&upstream).unwrap();
    assert_ne!(names[corners[0]], names[corners[1]]);
    assert_eq!(names[corners[0]][..4], names[corners[1]][..4]);
    txn.add_rbin("sta", "sta.rbin", &words("STA", 4, 1), upstream.clone())
        .unwrap();
    let c = txn.commit(JsonMap::new()).unwrap();
    let sta = ws
        .revision(c.rev)
        .unwrap()
        .open_rbin("sta", "STA", 1..=1)
        .unwrap();
    for corner in corners {
        assert_eq!(sta.upstream_hash(&names[corner]), Some(upstream[corner]));
    }
    assert_eq!(c.rev, Rev(2));
}
