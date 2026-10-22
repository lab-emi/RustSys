// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

use super::*;
use crate::error::ErrorCode;

fn sample() -> RbinWriter<'static> {
    let mut w = RbinWriter::new("TEST", 2).unwrap();
    w.upstream("netlist", Hash64(0x11)).unwrap();
    w.upstream("CONSTR", Hash64(0x22)).unwrap();
    w.section_vec(5, vec![1u8, 2, 3]).unwrap();
    w.section_vec(2, vec![10u32, 20, 30]).unwrap();
    w.section_vec(9, Vec::<u64>::new()).unwrap();
    w
}

// WS-REQ-0030, WS-REQ-0031
#[test]
fn layout() {
    let w = sample();
    let b = w.to_bytes();
    assert_eq!(&b[..8], b"RUSTEDA\0");
    assert_eq!(&b[8..16], b"TEST\0\0\0\0");
    assert_eq!(read_u32(&b, 16), 2);
    assert_eq!(read_u32(&b, 20), 0);
    assert_eq!(read_u32(&b, 32), 3);
    assert_eq!(read_u32(&b, 36), 2);
    let hl = 48 + 2 * 16 + 3 * 24;
    assert_eq!(read_u64(&b, 40), hl);
    assert_eq!(read_u64(&b, 24), Hash64::of(&b[hl as usize..]).0);
    // Upstream entries sorted by name bytes: "CONSTR" < "netlist".
    assert_eq!(&b[48..56], b"CONSTR\0\0");
    assert_eq!(read_u64(&b, 56), 0x22);
    assert_eq!(&b[64..72], b"netlist\0");
    // Sections by id: 2 (12 bytes, padded to 16), 5 (3 bytes, padded to 8), 9 (empty).
    let sec = |i: u64| (80 + 24 * i) as usize;
    assert_eq!((read_u32(&b, sec(0)), read_u32(&b, sec(0) + 4)), (2, 4));
    assert_eq!(
        (read_u64(&b, sec(0) + 8), read_u64(&b, sec(0) + 16)),
        (hl, 12)
    );
    assert_eq!(
        (read_u32(&b, sec(1)), read_u64(&b, sec(1) + 8)),
        (5, hl + 16)
    );
    assert_eq!(
        (read_u32(&b, sec(2)), read_u64(&b, sec(2) + 8)),
        (9, hl + 24)
    );
    assert_eq!(b.len() as u64, hl + 24);
    assert_eq!(w.file_len(), b.len() as u64);
    assert_eq!(&b[hl as usize + 12..hl as usize + 16], &[0, 0, 0, 0]);
    assert_eq!(&b[hl as usize + 16..], &[1, 2, 3, 0, 0, 0, 0, 0]);
}

// WS-REQ-0031, WS-REQ-0100
#[test]
fn deterministic_bytes() {
    let a = sample().to_bytes();
    let mut w = RbinWriter::new("TEST", 2).unwrap();
    w.section_vec(9, Vec::<u64>::new()).unwrap();
    w.section_bytes(
        2,
        4,
        [10u32, 20, 30]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    w.upstream("CONSTR", Hash64(0x22)).unwrap();
    w.section_bytes(5, 1, &[1u8, 2, 3][..]).unwrap();
    w.upstream("netlist", Hash64(0x11)).unwrap();
    assert_eq!(w.to_bytes(), a, "insertion order does not matter");
    assert_eq!(sample().to_bytes(), a);
}

// WS-REQ-0033
#[test]
fn round_trip_in_memory() {
    let r = Rbin::from_bytes(sample().to_bytes(), "TEST", 1..=2).unwrap();
    assert_eq!(r.kind(), "TEST");
    assert_eq!(r.format_version(), 2);
    assert_eq!(r.section::<u32>(2).unwrap(), &[10, 20, 30]);
    assert_eq!(r.section::<u8>(5).unwrap(), &[1, 2, 3]);
    assert!(r.section::<u64>(9).unwrap().is_empty());
    assert_eq!(r.upstream_hash("netlist"), Some(Hash64(0x11)));
    assert_eq!(r.upstream_hash("x"), None);
    assert_eq!(
        r.sections().iter().map(|s| s.id).collect::<Vec<_>>(),
        [2, 5, 9]
    );
    assert_eq!(r.section_info(2).unwrap().count(), 3);
    r.verify_hash().unwrap();
    let err = r.section::<u32>(3).unwrap_err();
    assert!(matches!(
        err,
        Error::Corrupt {
            what: Corruption::MissingSection { id: 3 },
            ..
        }
    ));
    let err = r.section::<u16>(2).unwrap_err();
    assert!(matches!(
        err,
        Error::Corrupt {
            what: Corruption::TypeMismatch { .. },
            ..
        }
    ));
    assert_eq!(err.code(), ErrorCode::CorruptTable);
}

#[test]
fn writer_misuse() {
    assert!(RbinWriter::new("", 1).is_err());
    assert!(RbinWriter::new("TOOLONGKD", 1).is_err());
    assert!(RbinWriter::new("A B", 1).is_err());
    let mut w = RbinWriter::new("K", 1).unwrap();
    w.section_vec(1, vec![1u32]).unwrap();
    assert_eq!(
        w.section_vec(1, vec![1u32]).unwrap_err().code(),
        ErrorCode::Internal
    );
    assert!(w.section_bytes(2, 4, vec![0u8; 3]).is_err());
    assert!(w.section_bytes(2, 0, vec![]).is_err());
    assert!(w.section_vec(3, vec![(); 2]).is_err());
    w.upstream("a", Hash64(1)).unwrap();
    assert!(w.upstream("a", Hash64(2)).is_err());
    assert!(w.upstream("ninechars", Hash64(2)).is_err());
}

/// The header name of `table` in the upstream map of `tables`.
fn name_in(tables: &[&str], table: &str) -> String {
    let upstream: Upstream = tables.iter().map(|t| (t.to_string(), Hash64(0))).collect();
    upstream_names(&upstream).unwrap()[table].clone()
}

// WS-REQ-0038 (review CW-3): every upstream table, the contract's long names included, has
// an 8-byte header name; short names stay as they are.
#[test]
fn upstream_names_of_tables() {
    for short in ["rtl", "netlist", "synth", "check", "sta", "a-b_1234"] {
        assert_eq!(name_in(&[short], short), short);
    }
    let longs = [
        "constraints",
        "libraries",
        "libraries.ss",
        "libraries.tt",
        "netlist_a",
    ];
    let mut seen = BTreeSet::new();
    for long in longs {
        let name = name_in(&[long], long);
        assert_eq!(name.len(), 8, "{name}");
        assert_eq!((&name[..3], &name[3..4]), (&long[..3], "."), "{name}");
        assert_eq!(name, name_in(&longs, long), "no collision, the same name");
        encode_name(&name, "upstream name").unwrap();
        assert!(seen.insert(name), "{long}");
    }
    for bad in ["", "a b"] {
        let upstream = Upstream::from([(bad.to_string(), Hash64(0))]);
        assert_eq!(
            upstream_names(&upstream).unwrap_err().code(),
            ErrorCode::Internal
        );
    }

    let upstream = Upstream::from([
        ("netlist".to_string(), Hash64(1)),
        ("constraints".to_string(), Hash64(2)),
        ("libraries".to_string(), Hash64(3)),
    ]);
    let mut w = RbinWriter::new("STA", 1).unwrap();
    w.upstream_tables(&upstream).unwrap();
    assert!(w.upstream_tables(&upstream).is_err(), "entries given twice");
    let r = Rbin::from_bytes(w.to_bytes(), "STA", 1..=1).unwrap();
    let names = upstream_names(&upstream).unwrap();
    for (table, hash) in &upstream {
        assert_eq!(r.upstream_hash(&names[table]), Some(*hash), "{table}");
    }
    assert_eq!(r.upstream_hash("rtl"), None);
    let expected = header_upstream(&upstream).unwrap();
    assert!(r.has_upstream(&expected));
    assert!(!r.has_upstream(&header_upstream(&Upstream::new()).unwrap()));
}

// WS-REQ-0038 (review CR2-4): names whose 16-bit hashes collide get distinct header names in
// one map, deterministically, so a table computed from both can be written and read.
#[test]
fn colliding_upstream_names() {
    let mut by_name = BTreeMap::new();
    let (a, b) = (0..100_000)
        .find_map(|i| {
            let table = format!("libraries.c{i}");
            let name = name_in(&[&table], &table);
            by_name
                .insert(name, table.clone())
                .map(|other| (other, table))
        })
        .expect("16-bit names collide within a few hundred names");
    let (first, second) = if a < b { (a, b) } else { (b, a) };
    let alone = name_in(&[&first], &first);
    assert_eq!(alone, name_in(&[&second], &second), "they collide alone");
    let upstream = Upstream::from([(first.clone(), Hash64(1)), (second.clone(), Hash64(2))]);
    let names = upstream_names(&upstream).unwrap();
    assert_eq!(
        names[&first], alone,
        "the first in byte order keeps its name"
    );
    let bumped = u16::from_str_radix(&alone[4..], 16)
        .unwrap()
        .wrapping_add(1);
    assert_eq!(names[&second], format!("{}.{bumped:04x}", &alone[..3]));
    let entries = header_upstream(&upstream).unwrap();
    assert_eq!(entries.len(), 2);
    let mut w = RbinWriter::new("STA", 1).unwrap();
    w.upstream_tables(&upstream).unwrap();
    let r = Rbin::from_bytes(w.to_bytes(), "STA", 1..=1).unwrap();
    assert!(r.has_upstream(&entries));
    assert_eq!(r.upstream_hash(&names[&second]), Some(Hash64(2)));

    // A short name that looks like a shortened one is taken first.
    let short = alone.clone();
    let upstream = Upstream::from([(first.clone(), Hash64(1)), (short.clone(), Hash64(2))]);
    let names = upstream_names(&upstream).unwrap();
    assert_eq!(names[&short], short);
    assert_ne!(names[&first], short);

    // Many corners of a PDK: every map of all of them has distinct names.
    let mut corners = Upstream::new();
    for p in ["tt", "ss", "ff", "sf", "fs"] {
        for t in ["025C", "100C", "n40C", "125C", "150C", "085C", "000C"] {
            for v in [
                "1v80", "1v60", "1v40", "1v28", "1v35", "1v56", "1v62", "1v65",
            ] {
                corners.insert(format!("libraries.{p}_{t}_{v}"), Hash64(0));
            }
        }
    }
    let names = upstream_names(&corners).unwrap();
    let distinct: BTreeSet<&String> = names.values().collect();
    assert_eq!(distinct.len(), corners.len());
}

// WS-REQ-0078 (review R2-C4, CR2-3): the import check reads a table once and gives its file
// hash; an rbin file must have a valid structure and content hash, whatever its kind.
#[test]
fn import_inspection() {
    let mut w = RbinWriter::new("ANY", 7).unwrap();
    w.upstream("x", Hash64(3)).unwrap();
    w.section_vec(1, vec![5u32; 8]).unwrap();
    let good = w.to_bytes();
    let inspect_bytes = |b: &[u8]| inspect(b, b.len() as u64).unwrap();
    match inspect_bytes(&good) {
        Inspected::Rbin {
            file_hash,
            upstream,
        } => {
            assert_eq!(file_hash, Hash64::of(&good));
            assert_eq!(upstream.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    for plain in [&b""[..], b"{}", b"RUSTEDA", b"module top; endmodule\n"] {
        assert!(
            matches!(inspect_bytes(plain), Inspected::Plain(h) if h == Hash64::of(plain)),
            "{plain:?}"
        );
    }
    let header_len = read_u64(&good, 40) as usize;
    let mut flipped = good.clone();
    flipped[header_len] ^= 0xff;
    assert!(matches!(
        inspect_bytes(&flipped),
        Inspected::Corrupt(Corruption::HashMismatch { .. })
    ));
    let mut oob = good.clone();
    oob[64 + 16..64 + 24].copy_from_slice(&4096u64.to_le_bytes());
    assert!(matches!(
        inspect_bytes(&oob),
        Inspected::Corrupt(Corruption::SectionBounds { len: 4096, .. })
    ));
    for cut in [8, 20, 47, 48, 60, header_len - 1, good.len() - 8] {
        assert!(
            matches!(inspect_bytes(&good[..cut]), Inspected::Corrupt(_)),
            "cut at {cut}"
        );
    }
    let mut count = good.clone();
    count[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(
        inspect_bytes(&count),
        Inspected::Corrupt(Corruption::HeaderLen { .. })
    ));
    // A reader that delivers fewer bytes than the length it announced.
    let err = inspect(&good[..], good.len() as u64 + 8).unwrap_err();
    assert!(err.to_string().contains("changed"), "{err}");
}

// WS-REQ-0039 (review CW-5): a newer format version is SCHEMA_TOO_NEW (exit 1), an older
// unsupported one CORRUPT_TABLE.
#[test]
fn newer_and_older_format_versions() {
    let w = RbinWriter::new("K", 2).unwrap();
    let err = Rbin::from_bytes(w.to_bytes(), "K", 1..=1).unwrap_err();
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
    assert!(err.help().unwrap().contains("upgrade the tools"));
    let err = Rbin::from_bytes(w.to_bytes(), "K", 3..=4).unwrap_err();
    assert_eq!((err.code(), err.exit_code()), (ErrorCode::CorruptTable, 2));
}
