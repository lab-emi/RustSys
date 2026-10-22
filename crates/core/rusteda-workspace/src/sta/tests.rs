// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Unit tests of the timing table reader and builder (WS-REQ-0126 to WS-REQ-0136).

use std::collections::BTreeMap;

use super::layout::{CHAIN_HEADER_WORDS, section};
use super::*;
use crate::error::ErrorCode;
use crate::rbin::{Rbin, RbinWriter};
use crate::schema::Upstream;

/// A small sequential design: `u1` launches, `u2` combines, `u3` captures; `y` is an output.
/// Paths: setup `u1/CLK -> u3/D` (-0.2 ns), setup `a -> y` (0.1 ns), hold `u1/CLK -> u3/D`
/// (0.05 ns).
fn sample(mode: StoreMode) -> StaTableBuilder {
    let mut b = StaTableBuilder::new(mode);
    b.top("demo");
    let tt = b.corner("tt");
    b.clock("clk", 0.5, 0.02);
    let _clk = b.port("clk");
    let a = b.port("a");
    let y = b.port("y");
    b.instance("u1", "dff_1")
        .instance("u2", "nand2_1")
        .instance("u3", "dff_1");
    let u1_ck = b.pin("u1", "CLK");
    let u1_q = b.pin("u1", "Q");
    let u2_a = b.pin("u2", "A");
    let u2_b = b.pin("u2", "B");
    let u2_y = b.pin("u2", "Y");
    let u3_d = b.pin("u3", "D");
    b.endpoint(u3_d, EndpointKind::FlipFlop)
        .endpoint(y, EndpointKind::OutputPort)
        .endpoint_slack(u3_d, tt, Some(-0.2), Some(0.05))
        .endpoint_slack(y, tt, Some(0.1), None);
    let reg = |slack: f64, kind: CheckKind| PathSpec {
        corner: tt,
        kind,
        slack_ns: slack,
        required_ns: 0.4,
        points: vec![
            (u1_ck, Transition::Rise, PointKind::ClockLaunch, 0.0),
            (u1_q, Transition::Rise, PointKind::Cell, 0.25),
            (u2_a, Transition::Rise, PointKind::Net, 0.26),
            (u2_y, Transition::Fall, PointKind::Cell, 0.55),
            (u3_d, Transition::Fall, PointKind::Net, 0.6),
        ],
    };
    b.path(reg(0.05, CheckKind::Hold));
    b.path(PathSpec {
        corner: tt,
        kind: CheckKind::Setup,
        slack_ns: 0.1,
        required_ns: 0.5,
        points: vec![
            (a, Transition::Rise, PointKind::Port, 0.0),
            (u2_b, Transition::Rise, PointKind::Net, 0.01),
            (u2_y, Transition::Fall, PointKind::Cell, 0.3),
            (y, Transition::Fall, PointKind::Net, 0.4),
        ],
    });
    b.path(reg(-0.2, CheckKind::Setup));
    b
}

fn bytes_of(b: &StaTableBuilder) -> Vec<u8> {
    b.to_bytes(&Upstream::new()).unwrap()
}

fn open(b: &StaTableBuilder) -> StaTable {
    StaTable::from_bytes(bytes_of(b)).unwrap()
}

type Sections = BTreeMap<u32, (u32, Vec<u8>)>;

/// The sections of valid table bytes.
fn sections_of(bytes: &[u8]) -> Sections {
    let r = Rbin::from_bytes(bytes.to_vec(), KIND, VERSIONS).unwrap();
    r.sections()
        .iter()
        .map(|s| (s.id, (s.elem_size, r.section_bytes(s.id).unwrap().to_vec())))
        .collect()
}

/// A valid rbin container of `kind` and `version` holding `secs`.
fn assemble(kind: &str, version: u32, secs: &Sections) -> Vec<u8> {
    let mut w = RbinWriter::new(kind, version).unwrap();
    for (id, (elem, b)) in secs {
        w.section_bytes(*id, *elem, b.clone()).unwrap();
    }
    w.to_bytes()
}

/// The table of `b` with its sections changed by `f`, in a valid container.
fn mutated(b: &StaTableBuilder, f: impl FnOnce(&mut Sections)) -> Vec<u8> {
    let mut secs = sections_of(&bytes_of(b));
    f(&mut secs);
    assemble(KIND, FORMAT_VERSION, &secs)
}

fn bytes(secs: &mut Sections, id: u32) -> &mut Vec<u8> {
    &mut secs.get_mut(&id).unwrap().1
}

fn get_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn set_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn open_code(bytes: Vec<u8>) -> ErrorCode {
    StaTable::from_bytes(bytes).unwrap_err().code()
}

// WS-REQ-0126, WS-REQ-0135, WS-REQ-0136: the builder writes a table of 24 sections that the
// reader opens in both store modes, with the counts of META.
#[test]
fn builder_tables_open_in_both_modes() {
    for mode in [StoreMode::Full, StoreMode::Summary] {
        let raw = bytes_of(&sample(mode));
        let rbin = Rbin::from_bytes(raw.clone(), KIND, VERSIONS).unwrap();
        assert_eq!(rbin.kind(), "STA");
        assert_eq!(rbin.format_version(), 1);
        let ids: Vec<u32> = rbin.sections().iter().map(|s| s.id).collect();
        assert_eq!(ids, (1..=24).collect::<Vec<_>>());
        for s in rbin.sections() {
            assert_eq!(Some(s.elem_size), section::elem_size(s.id), "{}", s.id);
        }
        rbin.verify_hash().unwrap();
        let t = StaTable::from_bytes(raw).unwrap();
        t.verify_hash().unwrap();
        assert_eq!(t.store_mode(), mode);
        let c = t.counts();
        assert_eq!(
            (c.nodes, c.port_bits, c.cells, c.corners, c.clocks),
            (9, 3, 3, 1, 1)
        );
        assert_eq!(
            (c.endpoints, c.ranked_paths, c.top_k_paths, c.path_groups),
            (2, 3, 3, 1)
        );
        assert_eq!(t.corner_names().unwrap(), ["tt"]);
        let clocks = t.clocks().unwrap();
        assert_eq!(clocks[0].name, "clk");
        assert_eq!(clocks[0].period_ns, 0.5);
        assert!((clocks[0].setup_uncertainty_ns - 0.02).abs() < 1e-7);
        let scope = t.scope().unwrap();
        assert_eq!(scope["top"], "demo");
        assert_eq!(scope["store_mode"], mode.as_str());
        assert_eq!(scope["clock_model"], "ideal");
    }
}

// WS-REQ-0136: equal inputs give equal bytes, and -0.0 is written as +0.0.
#[test]
fn builder_is_deterministic() {
    assert_eq!(
        bytes_of(&sample(StoreMode::Full)),
        bytes_of(&sample(StoreMode::Full))
    );
    let mut a = sample(StoreMode::Summary);
    let mut b = sample(StoreMode::Summary);
    let y = a.port("y");
    a.endpoint_slack(y, 0, Some(-0.0), None);
    let y = b.port("y");
    b.endpoint_slack(y, 0, Some(0.0), None);
    assert_eq!(bytes_of(&a), bytes_of(&b));
}

// WS-REQ-0133: path<N> is PATH_RANK row N-1; setup-family paths rank first, by slack; a row
// outside the ranked paths is UNKNOWN_HANDLE naming the handle.
#[test]
fn path_handles_are_rank_rows() {
    let t = open(&sample(StoreMode::Summary));
    let ranks = t.path_ranks().unwrap();
    let kinds: Vec<(CheckKind, f64)> = ranks.iter().map(|r| (r.kind, r.slack_ns)).collect();
    assert_eq!(kinds[0].0, CheckKind::Setup);
    assert!((kinds[0].1 + 0.2).abs() < 1e-6);
    assert_eq!(kinds[1].0, CheckKind::Setup);
    assert!((kinds[1].1 - 0.1).abs() < 1e-6);
    assert_eq!(kinds[2].0, CheckKind::Hold);
    let p1 = t.path(0).unwrap();
    assert_eq!(p1.handle(), "path1");
    assert_eq!(p1.kind, CheckKind::Setup);
    assert_eq!(p1.corner_name, "tt");
    assert_eq!(p1.endpoint_name, "u3/D");
    assert_eq!(p1.slack_ns, -0.2);
    assert!(p1.slack_exact && p1.materialized);
    assert_eq!(p1.instances(), ["u1", "u2", "u3"]);
    let names: Vec<&str> = p1.points.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["u1/CLK", "u1/Q", "u2/A", "u2/Y", "u3/D"]);
    assert_eq!(p1.points[3].instance.as_deref(), Some("u2"));
    assert_eq!(p1.points[3].pin, "Y");
    assert_eq!(p1.points[3].kind, PointKind::Cell);
    assert_eq!(p1.points[3].transition, Transition::Fall);
    assert_eq!(p1.points[3].arrival_ns, Some(0.55));
    let p2 = t.path(1).unwrap();
    assert_eq!(p2.endpoint_name, "y");
    assert_eq!(p2.points[0].instance, None);
    assert_eq!(p2.instances(), ["u2"]);
    assert_eq!(t.path_required(0).unwrap(), (0.4, 0.0));
    let e = t.path(3).unwrap_err();
    assert_eq!(e.code(), ErrorCode::UnknownHandle);
    assert!(e.to_string().contains("path4"), "{e}");
}

// WS-REQ-0134: paths beyond the materialized ones come from their chains, with the same names,
// the f32 slack of the rank record and no arrivals, in both store modes.
#[test]
fn chain_paths_beyond_top_k() {
    for mode in [StoreMode::Full, StoreMode::Summary] {
        let all = open(&sample(mode));
        let mut b = sample(mode);
        b.top_k(1);
        let t = open(&b);
        assert_eq!(t.counts().top_k_paths, 1);
        let first = t.path(0).unwrap();
        assert!(first.materialized && first.slack_exact);
        for row in 1..3 {
            let chained = t.path(row).unwrap();
            let full = all.path(row).unwrap();
            assert!(!chained.materialized && !chained.slack_exact);
            assert!(chained.points.iter().all(|p| p.arrival_ns.is_none()));
            assert!((chained.slack_ns - full.slack_ns).abs() < 1e-6);
            let names = |p: &RankedPath| -> Vec<(String, Option<String>, PointKind, Transition)> {
                p.points
                    .iter()
                    .map(|x| (x.name.clone(), x.instance.clone(), x.kind, x.transition))
                    .collect()
            };
            assert_eq!(names(&chained), names(&full));
            assert_eq!(chained.endpoint_name, full.endpoint_name);
        }
    }
}

// WS-REQ-0130: names are found through NAME_HASH; pins carry their instance as the prefix.
#[test]
fn names_resolve_through_the_name_hash() {
    for mode in [StoreMode::Full, StoreMode::Summary] {
        let t = open(&sample(mode));
        let mut seen = std::collections::BTreeSet::new();
        for name in [
            "clk", "a", "y", "u1/CLK", "u1/Q", "u2/A", "u2/B", "u2/Y", "u3/D",
        ] {
            let node = t.node_by_name(name).unwrap().unwrap();
            assert!(seen.insert(node), "{name}");
            assert_eq!(node < 3, !name.contains('/'), "{name}: ports come first");
        }
        assert_eq!(t.node_by_name("u2").unwrap(), None);
        assert_eq!(t.node_by_name("u9/A").unwrap(), None);
    }
}

// WS-REQ-0131: the summary row of a corner aggregates its endpoints; a WNS without endpoints
// is absent, not zero.
#[test]
fn summary_rows() {
    let t = open(&sample(StoreMode::Summary));
    let rows = t.summary().unwrap();
    assert_eq!(rows.len(), 1);
    let s = t.corner_summary(0).unwrap();
    assert_eq!(s, rows[0]);
    assert_eq!(s.group, OVERALL_GROUP);
    assert_eq!(s.setup_wns_ns, Some(-0.2));
    assert_eq!(s.setup_tns_ns, -0.2);
    assert_eq!((s.setup_violating, s.setup_endpoints), (1, 2));
    assert_eq!(s.hold_wns_ns, Some(0.05));
    assert_eq!((s.hold_violating, s.hold_endpoints), (0, 1));
    assert_eq!(s.total_endpoints, 2);
    assert_eq!(
        t.corner_summary(1).unwrap_err().code(),
        ErrorCode::UnknownHandle
    );
    let mut b = StaTableBuilder::new(StoreMode::Summary);
    let c = b.corner("ss");
    let y = b.port("y");
    b.endpoint(y, EndpointKind::OutputPort)
        .endpoint_slack(y, c, None, Some(0.3));
    let s = open(&b).corner_summary(0).unwrap();
    assert_eq!((s.setup_wns_ns, s.setup_endpoints), (None, 0));
    assert_eq!(s.hold_wns_ns, Some(0.3));
}

// WS-REQ-0132: endpoints with their slacks, kinds and worst paths; EP_ORDER sorts by slack.
#[test]
fn endpoints_and_their_order() {
    let t = open(&sample(StoreMode::Full));
    let eps = t.endpoints(0).unwrap();
    assert_eq!(eps.len(), 2);
    let y = eps.iter().find(|e| e.name == "y").unwrap();
    assert_eq!(y.kind, EndpointKind::OutputPort);
    assert_eq!(y.instance, None);
    assert_eq!((y.setup_slack_ns, y.hold_slack_ns), (Some(0.1), None));
    assert_eq!((y.worst_setup_path, y.worst_hold_path), (Some(1), None));
    let d = eps.iter().find(|e| e.name == "u3/D").unwrap();
    assert_eq!(d.kind, EndpointKind::FlipFlop);
    assert_eq!(d.instance.as_deref(), Some("u3"));
    assert_eq!((d.worst_setup_path, d.worst_hold_path), (Some(0), Some(2)));
    let setup = t.endpoint_order(0, false).unwrap();
    assert_eq!(eps[setup[0]].name, "u3/D");
    assert_eq!(eps[setup[1]].name, "y");
    let hold = t.endpoint_order(0, true).unwrap();
    assert_eq!(eps[hold[0]].name, "u3/D", "NaN slacks sort last");
    assert_eq!(t.endpoint(d.index, 0).unwrap(), *d);
    assert_eq!(
        t.endpoint(5, 0).unwrap_err().code(),
        ErrorCode::UnknownHandle
    );
    assert_eq!(t.endpoints(3).unwrap_err().code(), ErrorCode::UnknownHandle);
}

// WS-REQ-0127, WS-REQ-0128: the structure is validated when the table is opened; the changes
// below address the record fields at the byte offsets of the specification.
#[test]
fn malformed_structure_is_corrupt() {
    use ErrorCode::{CorruptTable, SchemaTooNew};
    let s = sample(StoreMode::Summary);
    let f = sample(StoreMode::Full);
    // The container: kind, version, section directory, element sizes, file length.
    let secs = sections_of(&bytes_of(&s));
    assert_eq!(open_code(assemble("NETLIST", 1, &secs)), CorruptTable);
    assert_eq!(open_code(assemble(KIND, 2, &secs)), SchemaTooNew);
    let mut fewer = secs.clone();
    fewer.remove(&section::NOTES);
    assert_eq!(open_code(assemble(KIND, 1, &fewer)), CorruptTable);
    let mut more = secs.clone();
    more.insert(25, (1, vec![0]));
    assert_eq!(open_code(assemble(KIND, 1, &more)), CorruptTable);
    let mut resized = secs.clone();
    resized.get_mut(&section::ENDPOINTS).unwrap().0 = 16;
    assert_eq!(open_code(assemble(KIND, 1, &resized)), CorruptTable);
    let mut tail = bytes_of(&s);
    tail.extend_from_slice(&[0; 8]);
    assert_eq!(open_code(tail), CorruptTable);
    // META values and counts.
    let meta_cases: [(usize, u32); 5] = [
        (52, 2), // store mode
        (60, 7), // precision
        (4, 99), // more port bits than nodes
        (44, 9), // more materialized than ranked paths
        (24, 2), // corners disagree with CORNERS
    ];
    for (at, v) in meta_cases {
        let raw = mutated(&s, |x| set_u32(bytes(x, section::META), at, v));
        assert_eq!(open_code(raw), CorruptTable, "META offset {at}");
    }
    let raw = mutated(&s, |x| {
        let m = bytes(x, section::META).clone();
        bytes(x, section::META).extend(m);
    });
    assert_eq!(open_code(raw), CorruptTable, "two META records");
    // Mode-dependent node sections.
    let raw = mutated(&s, |x| *bytes(x, section::NODE_REC) = vec![0; 152]);
    assert_eq!(open_code(raw), CorruptTable, "NODE_REC in the summary mode");
    let raw = mutated(&f, |x| bytes(x, section::NODE_REC).truncate(152));
    assert_eq!(
        open_code(raw),
        CorruptTable,
        "short NODE_REC in the full mode"
    );
    let raw = mutated(&f, |x| {
        let b = bytes(x, section::TAG_EXTRA_PTR);
        let last = b.len() - 4;
        set_u32(b, last, 1);
    });
    assert_eq!(open_code(raw), CorruptTable, "TAG_EXTRA_PTR past TAG_EXTRA");
    // Row pointers, hash tables, strings and scope.
    let raw = mutated(&s, |x| {
        let b = bytes(x, section::PATH_CHAIN_PTR);
        let last = b.len() - 4;
        let v = get_u32(b, last);
        set_u32(b, last, v - 2);
    });
    assert_eq!(open_code(raw), CorruptTable, "PATH_CHAIN_PTR end");
    let raw = mutated(&s, |x| bytes(x, section::NAME_HASH).truncate(48));
    assert_eq!(open_code(raw), CorruptTable, "three NAME_HASH buckets");
    let raw = mutated(&s, |x| bytes(x, section::INST_HASH).truncate(4));
    assert_eq!(open_code(raw), CorruptTable, "a partial INST_HASH bucket");
    let raw = mutated(&s, |x| set_u32(bytes(x, section::STR_OFFS), 0, 1));
    assert_eq!(open_code(raw), CorruptTable, "STR_OFFS start");
    let raw = mutated(&s, |x| *bytes(x, section::SCOPE) = b"{".to_vec());
    assert_eq!(open_code(raw), CorruptTable, "SCOPE length");
    let raw = mutated(&s, |x| bytes(x, section::CORNERS).extend([0u8; 16]));
    assert_eq!(open_code(raw), CorruptTable, "CORNERS count");
}

// WS-REQ-0127, WS-REQ-0129: every index read from the table is checked before it is used.
#[test]
fn malformed_indices_are_corrupt() {
    use ErrorCode::CorruptTable;
    let s = sample(StoreMode::Summary);
    let mut chained = sample(StoreMode::Summary);
    chained.top_k(0);
    // A rank record naming an endpoint that does not exist.
    let raw = mutated(&s, |x| {
        set_u32(bytes(x, section::PATH_RANK), 4, 0x0000_0007)
    });
    let t = StaTable::from_bytes(raw).unwrap();
    assert_eq!(t.path(0).unwrap_err().code(), CorruptTable);
    assert_eq!(t.path_ranks().unwrap_err().code(), CorruptTable);
    // A rank record whose early-check bit disagrees with its check.
    let raw = mutated(&s, |x| bytes(x, section::PATH_RANK)[7] ^= 1);
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .path(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    // A chain step naming a bucket out of range, and one naming another node.
    let step = (CHAIN_HEADER_WORDS + 1) * 4;
    let raw = mutated(&chained, |x| {
        set_u32(bytes(x, section::PATH_CHAIN), step, 999)
    });
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .path(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    let raw = mutated(&chained, |x| {
        let b = bytes(x, section::PATH_CHAIN);
        let other = get_u32(b, step + 8);
        set_u32(b, step, other);
    });
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .path(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    // A chain row pointer past its neighbour.
    let raw = mutated(&s, |x| set_u32(bytes(x, section::PATH_CHAIN_PTR), 4, 9999));
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .path(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    // A materialized path recording another rank, or a point range past PATH_POINTS.
    let raw = mutated(&s, |x| set_u32(bytes(x, section::PATHS), 32, 2));
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .path(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    let raw = mutated(&s, |x| set_u32(bytes(x, section::PATHS), 52, 9999));
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .path(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    // Name buckets: a leaf string out of range, and a pin without its instance prefix.
    let t = open(&s);
    let node = t.node_by_name("u2/Y").unwrap().unwrap();
    let table = sections_of(&bytes_of(&s));
    let hash = &table[&section::NAME_HASH].1;
    let bucket = (0..hash.len() / 16)
        .find(|&b| get_u32(hash, b * 16) != 0 && get_u32(hash, b * 16 + 4) == node)
        .unwrap();
    let raw = mutated(&s, |x| {
        set_u32(bytes(x, section::NAME_HASH), bucket * 16 + 12, 4000)
    });
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .path(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    let raw = mutated(&s, |x| {
        set_u32(bytes(x, section::NAME_HASH), bucket * 16 + 8, NO_INDEX)
    });
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .path(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    // EP_ORDER that repeats an endpoint; an endpoint kind outside the codes.
    let raw = mutated(&s, |x| {
        let b = bytes(x, section::EP_ORDER);
        let second = get_u32(b, 4);
        set_u32(b, 0, second);
    });
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .endpoint_order(0, false)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    let raw = mutated(&s, |x| bytes(x, section::ENDPOINTS)[20] = 9);
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .endpoints(0)
            .unwrap_err()
            .code(),
        CorruptTable
    );
    // A summary row of another corner; SCOPE that is not an object.
    let raw = mutated(&s, |x| set_u32(bytes(x, section::SUMMARY), 32, 3));
    let t = StaTable::from_bytes(raw).unwrap();
    assert_eq!(t.corner_summary(0).unwrap_err().code(), CorruptTable);
    assert_eq!(t.summary().unwrap_err().code(), CorruptTable);
    let raw = mutated(&s, |x| *bytes(x, section::SCOPE) = b"[1]".to_vec());
    assert_eq!(
        StaTable::from_bytes(raw)
            .unwrap()
            .scope()
            .unwrap_err()
            .code(),
        CorruptTable
    );
}

/// Deterministic xorshift64*.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Every query of `t`; errors are fine, panics are not.
fn every_query(t: &StaTable) {
    let c = t.counts();
    let _ = t.corner_names();
    let _ = t.clocks();
    let _ = t.summary();
    let _ = t.scope();
    for corner in 0..c.corners as usize + 1 {
        let _ = t.corner_summary(corner);
        let _ = t.endpoints(corner);
        let _ = t.endpoint_order(corner, false);
        let _ = t.endpoint_order(corner, true);
        let _ = t.endpoint(0, corner);
    }
    let _ = t.path_ranks();
    for row in 0..c.ranked_paths as usize + 1 {
        let _ = t.path(row);
        let _ = t.path_required(row);
    }
    for name in ["clk", "u2/Y", "u3/D", "zz"] {
        let _ = t.node_by_name(name);
    }
}

// WS-REQ-0129: random single-word changes of every section never panic a query.
#[test]
fn mutated_tables_never_panic() {
    let mut rng = Rng(0x5eed_1234_abcd_9876);
    let mut opened = 0;
    for mode in [StoreMode::Full, StoreMode::Summary] {
        for top_k in [None, Some(1)] {
            let mut b = sample(mode);
            if let Some(k) = top_k {
                b.top_k(k);
            }
            let base = sections_of(&bytes_of(&b));
            let ids: Vec<u32> = base
                .iter()
                .filter(|(_, (_, v))| v.len() >= 4)
                .map(|(id, _)| *id)
                .collect();
            for _ in 0..600 {
                let mut secs = base.clone();
                let id = ids[rng.below(ids.len())];
                let v = &mut secs.get_mut(&id).unwrap().1;
                let at = rng.below(v.len() / 4) * 4;
                let word = match rng.below(4) {
                    0 => 0,
                    1 => u32::MAX,
                    2 => rng.next() as u32 & 0xFF,
                    _ => rng.next() as u32,
                };
                v[at..at + 4].copy_from_slice(&word.to_le_bytes());
                if let Ok(t) = StaTable::from_bytes(assemble(KIND, FORMAT_VERSION, &secs)) {
                    opened += 1;
                    every_query(&t);
                }
            }
        }
    }
    assert!(opened > 100, "most changes keep the structure: {opened}");
}

// WS-REQ-0135: the builder rejects tables the format cannot hold or calls naming nothing.
#[test]
fn builder_rejects_misuse() {
    let mut b = StaTableBuilder::new(StoreMode::Summary);
    assert_eq!(
        b.to_bytes(&Upstream::new()).unwrap_err().code(),
        ErrorCode::Internal,
        "no corner"
    );
    let c = b.corner("tt");
    let y = b.port("y");
    b.endpoint_slack(y, c, Some(1.0), None);
    assert_eq!(
        b.to_bytes(&Upstream::new()).unwrap_err().code(),
        ErrorCode::Internal,
        "not an endpoint"
    );
    let mut b = StaTableBuilder::new(StoreMode::Summary);
    let c = b.corner("tt");
    let y = b.port("y");
    let a = b.port("a");
    b.endpoint(y, EndpointKind::OutputPort);
    b.path(PathSpec {
        corner: c,
        kind: CheckKind::Setup,
        slack_ns: 0.0,
        required_ns: 1.0,
        points: vec![(a, Transition::Rise, PointKind::Port, 0.0)],
    });
    assert_eq!(
        b.to_bytes(&Upstream::new()).unwrap_err().code(),
        ErrorCode::Internal,
        "a path that does not end at an endpoint"
    );
}
