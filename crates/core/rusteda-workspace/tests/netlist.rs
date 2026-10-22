// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Integration tests for `rusteda_workspace::netlist` (`NETLIST` v1 zero-copy codec and
//! workspace netlist table helpers, `WS-REQ-0120` to `WS-REQ-0125`).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rusteda_core::names;
use rusteda_core::netlist::{
    AssignRec, Conn, FrozenNetlist, GateNetlist, InstId, InstRec, NetId, NetRec, PinDir, PinId,
    PortDir, PortRec, module_entry, section,
};
use rusteda_workspace::netlist::{self, FORMAT_VERSION, KIND, TABLE_NAME, TABLE_PATH, VERSIONS};
use rusteda_workspace::{ErrorCode, Invocation, Rbin, RbinWriter, Rev, Upstream, Workspace};

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rusteda-ws-netlist-{label}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn inv(cmd: &str) -> Invocation {
    Invocation::new("rusteda", "0.1.0", cmd, cmd.split_whitespace())
}

/// Deterministic xorshift64* PRNG.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    fn pick<T: Copy>(&mut self, v: &[T]) -> Option<T> {
        (!v.is_empty()).then(|| v[self.below(v.len())])
    }
}

fn random_hier_name(rng: &mut Rng) -> String {
    const ROOTS: [&str; 6] = ["a", "b", "n", "u", "y", "zz"];
    let seg = |rng: &mut Rng| {
        let mut s = format!("{}{}", ROOTS[rng.below(ROOTS.len())], rng.below(24));
        if rng.one_in(5) {
            s.insert(rng.below(s.len() + 1), ['/', '\\'][rng.below(2)]);
        }
        s
    };
    let first = seg(rng);
    if rng.one_in(3) {
        names::join([first, seg(rng)])
    } else {
        names::escape_segment(&first)
    }
}

/// Builds a `GateNetlist` via `steps` randomized edits including insertions, removals,
/// disconnects, pin moves, buffer insertion/removal, and arena slot reuse.
fn build_random_netlist(seed: u64, steps: usize) -> GateNetlist {
    let mut g = GateNetlist::new("top_mod").unwrap();
    let inv_c = g
        .add_cell_type("INV_X1", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .unwrap();
    let buf_c = g
        .add_cell_type("BUF_X1", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .unwrap();
    let nand_c = g
        .add_cell_type(
            "NAND2_X1",
            &[
                ("A", PinDir::Input),
                ("B", PinDir::Input),
                ("Y", PinDir::Output),
            ],
        )
        .unwrap();
    let dff_c = g
        .add_cell_type(
            "DFF_X1",
            &[
                ("D", PinDir::Input),
                ("CK", PinDir::Input),
                ("Q", PinDir::Output),
                ("QN", PinDir::Output),
            ],
        )
        .unwrap();
    let bidir_c = g
        .add_cell_type(
            "PAD_IO",
            &[
                ("IO", PinDir::Inout),
                ("EN", PinDir::Input),
                ("BIAS", PinDir::Other),
            ],
        )
        .unwrap();
    let cells = [inv_c, buf_c, nand_c, dff_c, bidir_c];

    let mut rng = Rng::new(seed);
    let mut removed_slots = 0usize;

    for _ in 0..steps {
        let nets: Vec<NetId> = g.nets().map(|(id, _)| id).collect();
        let insts: Vec<InstId> = g.instances().map(|(id, _)| id).collect();
        let pins: Vec<PinId> = g.pins().map(|(id, _)| id).collect();

        match rng.below(12) {
            0 | 1 => {
                let n = (!rng.one_in(4)).then(|| random_hier_name(&mut rng));
                let idx = rng.one_in(2).then(|| rng.below(6) as i32 - 2);
                let _ = g.add_net(n.as_deref(), idx);
            }
            2 | 3 => {
                let c = rng.pick(&cells).unwrap();
                let _ = g.add_instance(&random_hier_name(&mut rng), c);
            }
            4 => {
                if let Some(i) = rng.pick(&insts) {
                    if g.remove_instance(i).is_ok() {
                        removed_slots += 1;
                    }
                }
            }
            5..=7 => {
                if let Some(i) = rng.pick(&insts) {
                    let conn = if rng.one_in(5) {
                        Conn::Const(rng.one_in(2))
                    } else if let Some(n) = rng.pick(&nets) {
                        Conn::Net(n)
                    } else {
                        continue;
                    };
                    let pin_idx = rng.below(4) as u32;
                    let _ = g.connect(i, pin_idx, conn);
                }
            }
            8 => {
                if let Some(p) = rng.pick(&pins) {
                    let _ = g.disconnect(p);
                }
            }
            9 => {
                if let (Some(lhs), Some(rhs_net)) = (rng.pick(&nets), rng.pick(&nets)) {
                    let rhs = if rng.one_in(4) {
                        Conn::Const(rng.one_in(2))
                    } else {
                        Conn::Net(rhs_net)
                    };
                    let _ = g.add_assign(lhs, rhs);
                }
            }
            10 => {
                let dir = [PortDir::Input, PortDir::Output, PortDir::Inout][rng.below(3)];
                let range = rng
                    .one_in(2)
                    .then(|| (rng.below(4) as i32 - 1, rng.below(4) as i32 - 1));
                let _ = g.add_port(&random_hier_name(&mut rng), dir, range);
            }
            _ => {
                if let Some(n) = rng.pick(&nets) {
                    if g.remove_net(n).is_ok() {
                        removed_slots += 1;
                    }
                }
                if g.assign_count() > 0 && rng.one_in(2) {
                    let _ = g.remove_assign(rng.below(g.assign_count()));
                }
            }
        }
    }
    assert!(removed_slots > 0, "expected arena removals and slot reuse");
    g
}

fn assert_all_borrowed_in_range(frozen: &FrozenNetlist<'_>, rbin: &Rbin) {
    let base = rbin.as_bytes().as_ptr() as usize;
    let end = base + rbin.as_bytes().len();
    let check_ptr = |ptr: *const u8, byte_len: usize, name: &str| {
        if byte_len > 0 {
            let p = ptr as usize;
            assert!(
                p >= base && p + byte_len <= end,
                "section {name} slice [{p:#x}..{:#x}) is not inside rbin [{base:#x}..{end:#x})",
                p + byte_len
            );
        }
    };

    macro_rules! check_cow {
        ($field:ident) => {
            match &frozen.$field {
                Cow::Borrowed(slice) => {
                    check_ptr(
                        slice.as_ptr() as *const u8,
                        std::mem::size_of_val(*slice),
                        stringify!($field),
                    );
                }
                Cow::Owned(_) => panic!(
                    "expected Cow::Borrowed for {}, got Cow::Owned",
                    stringify!($field)
                ),
            }
        };
    }

    check_cow!(str_bytes);
    check_cow!(str_offs);
    check_cow!(module);
    check_cow!(cells);
    check_cow!(cell_pin_start);
    check_cow!(cell_pin_name);
    check_cow!(cell_pin_dir);
    check_cow!(ports);
    check_cow!(port_bit_start);
    check_cow!(port_bit_net);
    check_cow!(nets);
    check_cow!(instances);
    check_cow!(inst_pin_start);
    check_cow!(pin_cellpin);
    check_cow!(pin_net);
    check_cow!(assigns);
    check_cow!(net_pin_start);
    check_cow!(net_pins);
}

/// WS-REQ-0120, WS-REQ-0121, WS-REQ-0122: round-trips on generated `GateNetlist`s with random
/// edits and arena slot reuse, byte-identical encodes, and zero-copy pointers into the buffer.
#[test]
fn round_trip_determinism_and_zero_copy_on_random_netlists() {
    for seed in 1..=6 {
        let g = build_random_netlist(seed, 800);
        let frozen = g.freeze();
        frozen.validate().unwrap();

        let bytes1 = netlist::encode(&frozen).unwrap();
        let bytes2 = netlist::encode(&frozen).unwrap();
        assert_eq!(bytes1, bytes2, "seed {seed}: encode must be deterministic");

        let rbin = netlist::open_bytes(bytes1.clone()).unwrap();
        assert_eq!(rbin.kind(), KIND);
        assert_eq!(rbin.format_version(), FORMAT_VERSION);
        assert_eq!(rbin.sections().len(), section::COUNT);

        let decoded = netlist::decode(&rbin).unwrap();
        assert_all_borrowed_in_range(&decoded, &rbin);
        assert_eq!(decoded, frozen, "seed {seed}: decoded != frozen");

        // Thaw -> freeze round-trip equals the original FrozenNetlist and encodes identically.
        let thawed = decoded.thaw().unwrap();
        let refrozen = thawed.freeze();
        assert_eq!(refrozen, frozen, "seed {seed}: freeze(thaw(f)) != f");
        let bytes3 = netlist::encode(&refrozen).unwrap();
        assert_eq!(bytes3, bytes1, "seed {seed}: re-encoded bytes differ");
    }
}

/// WS-REQ-0124, WS-REQ-0125: workspace `add_to_txn` with upstream map, `open_rev`, `open_head`,
/// and zero-copy pointers into the memory-mapped revision file.
#[test]
fn workspace_commit_open_zero_copy_mmap_and_upstream() {
    let dir = temp_dir("ws-netlist");
    let ws = Workspace::init(&dir, &inv("rusteda init"))
        .unwrap()
        .workspace;

    let g = build_random_netlist(42, 600);
    let frozen = g.freeze();

    // Commit rev1: imported netlist (empty upstream).
    let base0 = ws.resolve_base("rev0").unwrap();
    let mut txn1 = ws.begin(base0, &inv("rusteda import")).unwrap();
    let hash1 = netlist::add_to_txn(&mut txn1, &frozen, Upstream::new()).unwrap();
    let c1 = txn1.commit(BTreeMap::new()).unwrap();
    assert_eq!(c1.rev, Rev(1));
    assert_eq!(c1.meta.tables[TABLE_NAME].path, TABLE_PATH);
    assert_eq!(c1.meta.tables[TABLE_NAME].hash, hash1);

    let (head_rev, mapped_rbin) = netlist::open_head(&ws).unwrap();
    assert_eq!(head_rev.rev(), Rev(1));
    let mapped_frozen = netlist::decode(&mapped_rbin).unwrap();
    assert_all_borrowed_in_range(&mapped_frozen, &mapped_rbin);
    assert_eq!(mapped_frozen, frozen);

    let thawed = netlist::thaw_rev(&head_rev).unwrap();
    assert_eq!(thawed.freeze(), frozen);

    // Commit rev2: derived netlist with upstream map referencing a table in rev2.
    let base1 = ws.resolve_base("rev1").unwrap();
    let mut txn2 = ws.begin(base1, &inv("rusteda eco")).unwrap();
    txn2.add_bytes("eco_log", "eco_log.json", b"{}", Upstream::new())
        .unwrap();
    let up = txn2.upstream(&["libraries", "eco_log"]).unwrap();
    let _hash2 = netlist::add_to_txn(&mut txn2, &frozen, up.clone()).unwrap();
    let c2 = txn2.commit(BTreeMap::new()).unwrap();
    assert_eq!(c2.rev, Rev(2));

    let rev2 = ws.revision(Rev(2)).unwrap();
    let rbin2 = netlist::open_rev(&rev2).unwrap();
    assert_eq!(rbin2.upstream().len(), 2);
    let decoded2 = netlist::decode(&rbin2).unwrap();
    assert_all_borrowed_in_range(&decoded2, &rbin2);
    assert_eq!(decoded2, frozen);

    let _ = fs::remove_dir_all(&dir);
}

/// WS-REQ-0123: `FrozenNetlist::validate()` failures are mapped to `ErrorCode::CorruptTable`
/// and newer format versions map to `ErrorCode::SchemaTooNew`.
#[test]
fn validate_failures_and_schema_version_are_mapped() {
    let g = build_random_netlist(7, 200);
    let valid = g.freeze();

    // 1. Corrupt MODULE section length (5 u32s instead of 6) in a well-hashed Rbin -> CORRUPT_TABLE.
    {
        let mut bad = valid.clone();
        bad.module.to_mut().pop();
        let bytes = netlist::encode(&bad).unwrap();
        let rbin = netlist::open_bytes(bytes).unwrap();
        let err = netlist::decode(&rbin).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
        assert!(
            err.to_string().contains("MODULE"),
            "expected MODULE section error, got: {err}"
        );
    }

    // 2. Corrupt STR_OFFS (Does not start at 0) -> CORRUPT_TABLE.
    {
        let mut bad = valid.clone();
        bad.str_offs.to_mut()[0] = 1;
        let bytes = netlist::encode(&bad).unwrap();
        let rbin = netlist::open_bytes(bytes).unwrap();
        let err = netlist::decode(&rbin).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
        assert!(err.to_string().contains("STR_OFFS"));
    }

    // 3. Corrupt CELL_PIN_DIR (invalid direction code 99) -> CORRUPT_TABLE.
    {
        let mut bad = valid.clone();
        bad.cell_pin_dir.to_mut()[0] = 99;
        let bytes = netlist::encode(&bad).unwrap();
        let rbin = netlist::open_bytes(bytes).unwrap();
        let err = netlist::decode(&rbin).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
        assert!(err.to_string().contains("CELL_PIN_DIR"));
    }

    // 4. Corrupt INSTANCES (out-of-range cell index) -> CORRUPT_TABLE.
    {
        let mut bad = valid.clone();
        bad.instances.to_mut()[0].cell = 999_999;
        let bytes = netlist::encode(&bad).unwrap();
        let rbin = netlist::open_bytes(bytes).unwrap();
        let err = netlist::decode(&rbin).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
        assert!(err.to_string().contains("INSTANCES"));
    }

    // 5. Missing section 18 -> CORRUPT_TABLE.
    {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION).unwrap();
        for (id, elem_size, bytes) in valid.sections() {
            if id != section::NET_PINS {
                w.section_bytes(id, elem_size, bytes).unwrap();
            }
        }
        let rbin = netlist::open_bytes(w.to_bytes()).unwrap();
        let err = netlist::decode(&rbin).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
    }

    // 6. Extra section 19 -> CORRUPT_TABLE.
    {
        let mut w = netlist::writer(&valid).unwrap();
        w.section_bytes(19, 1, &[0u8; 4][..]).unwrap();
        let rbin = netlist::open_bytes(w.to_bytes()).unwrap();
        let err = netlist::decode(&rbin).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
    }

    // 7. Newer format_version -> SCHEMA_TOO_NEW.
    {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION + 1).unwrap();
        for (id, elem_size, bytes) in valid.sections() {
            w.section_bytes(id, elem_size, bytes).unwrap();
        }
        let err = Rbin::from_bytes(w.to_bytes(), KIND, VERSIONS).unwrap_err();
        assert_eq!(err.code(), ErrorCode::SchemaTooNew);
    }
}

/// WS-REQ-0123: 10,000 random mutations (raw byte flips, truncations, and re-hashed section
/// payload mutations) return `CORRUPT_TABLE` (or `SCHEMA_TOO_NEW` if the header version was
/// bumped) without ever panicking.
#[test]
fn fuzz_10k_mutations_never_panic() {
    let g = build_random_netlist(99, 300);
    let frozen = g.freeze();
    let base_bytes = netlist::encode(&frozen).unwrap();
    let sections = frozen.sections();

    let mut rng = Rng::new(0xC0FF_EE12_3456_789A);

    for iter in 0..10_000 {
        let mode = iter % 5;
        let mutated_bytes = match mode {
            // Mode 0: random truncation of the file.
            0 => {
                let len = rng.below(base_bytes.len());
                base_bytes[..len].to_vec()
            }
            // Mode 1 & 2: flip 1..=4 random bytes in the raw `.rbin` buffer.
            1 | 2 => {
                let mut b = base_bytes.clone();
                let flips = 1 + rng.below(4);
                for _ in 0..flips {
                    let pos = rng.below(b.len());
                    let delta = 1u8 << rng.below(8);
                    b[pos] ^= delta;
                }
                b
            }
            // Mode 3 & 4: mutate one of the 18 sections and re-encode with a valid RbinWriter
            // header + content_hash so `FrozenNetlist::validate()` is directly fuzzed!
            _ => {
                let target_sec = 1 + rng.below(section::COUNT) as u32;
                let mut w = RbinWriter::new(KIND, FORMAT_VERSION).unwrap();
                for &(id, elem_size, sec_bytes) in &sections {
                    let mut owned = sec_bytes.to_vec();
                    if id == target_sec {
                        if owned.is_empty() || rng.one_in(4) {
                            // Append or truncate a whole element.
                            let elem = elem_size as usize;
                            if owned.len() >= elem && rng.one_in(2) {
                                owned.truncate(owned.len() - elem);
                            } else {
                                owned.extend(std::iter::repeat_n(0xFFu8, elem));
                            }
                        } else {
                            let pos = rng.below(owned.len());
                            owned[pos] ^= 1u8 << rng.below(8);
                        }
                    }
                    w.section_bytes(id, elem_size, owned).unwrap();
                }
                w.to_bytes()
            }
        };

        if let Ok(rbin) = Rbin::from_bytes(mutated_bytes, KIND, VERSIONS) {
            if let Ok(decoded) = netlist::decode(&rbin) {
                // If a random mutation in a re-hashed section happened to produce a valid
                // netlist (e.g., flipping a valid ASCII char in an unused string or flipping
                // constant 0 to constant 1), `thaw()` must also succeed without panicking.
                let _ = decoded.thaw().unwrap();
            }
        }
    }
}

fn peak_rss_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let num = rest.split_whitespace().next()?;
            return num.parse::<u64>().ok();
        }
    }
    None
}

/// Ignored benchmark: encode and zero-copy decode of a generated 1M-cell netlist, reporting
/// wall-clock times and peak RSS (`cargo test -p rusteda-workspace --release --test netlist
/// bench_1m_cell_netlist -- --ignored --nocapture`).
#[test]
#[ignore = "1M-cell benchmark; run with --release -- --ignored --nocapture"]
fn bench_1m_cell_netlist() {
    const N_CELLS: usize = 1_000_000;
    let t_build = Instant::now();

    // Construct a valid 1M-cell FrozenNetlist directly to avoid test setup overhead, then
    // verify `FrozenNetlist::validate()`, `netlist::encode`, mmap `open_rev`, and zero-copy
    // `netlist::decode`.
    // Strings: "top" (0), "INV_X1" (1), "A" (2), "Y" (3), "clk" (4),
    // nets "n0".."n1000000" (5..1000006), instances "u0".."u999999" (1000006..2000006).
    let mut str_bytes = Vec::with_capacity(20 * 1024 * 1024);
    let mut str_offs = Vec::with_capacity(2 * N_CELLS + 8);
    str_offs.push(0u32);
    let mut push_str = |s: &[u8]| -> u32 {
        let id = (str_offs.len() - 1) as u32;
        str_bytes.extend_from_slice(s);
        str_offs.push(str_bytes.len() as u32);
        id
    };
    let s_top = push_str(b"top");
    let s_inv = push_str(b"INV_X1");
    let s_a = push_str(b"A");
    let s_y = push_str(b"Y");
    let s_in = push_str(b"in0");
    let s_out = push_str(b"out0");

    let n_nets = N_CELLS + 1;
    let mut nets = Vec::with_capacity(n_nets);
    nets.push(NetRec {
        name: s_in,
        index: netlist::SCALAR,
    });
    for k in 1..n_nets - 1 {
        let sid = push_str(format!("n{k}").as_bytes());
        nets.push(NetRec {
            name: sid,
            index: netlist::SCALAR,
        });
    }
    nets.push(NetRec {
        name: s_out,
        index: netlist::SCALAR,
    });

    let mut instances = Vec::with_capacity(N_CELLS);
    let mut inst_pin_start = Vec::with_capacity(N_CELLS + 1);
    inst_pin_start.push(0u32);
    let mut pin_cellpin = Vec::with_capacity(2 * N_CELLS);
    let mut pin_net = Vec::with_capacity(2 * N_CELLS);

    for k in 0..N_CELLS {
        let sid = push_str(format!("u{k}").as_bytes());
        instances.push(InstRec { name: sid, cell: 0 });
        pin_cellpin.push(0); // A
        pin_net.push(k as u32);
        pin_cellpin.push(1); // Y
        pin_net.push((k + 1) as u32);
        inst_pin_start.push(pin_net.len() as u32);
    }

    // Net-to-pin CSR: net 0 has pin 0; net k (1..N_CELLS) has pins 2k-1 and 2k; net N_CELLS has pin 2*N_CELLS - 1.
    let mut net_pin_start = Vec::with_capacity(n_nets + 1);
    let mut net_pins = Vec::with_capacity(2 * N_CELLS);
    net_pin_start.push(0);
    net_pins.push(0);
    net_pin_start.push(1);
    for k in 1..N_CELLS {
        let p_drv = (2 * k - 1) as u32;
        let p_load = (2 * k) as u32;
        net_pins.push(p_drv);
        net_pins.push(p_load);
        net_pin_start.push(net_pins.len() as u32);
    }
    net_pins.push((2 * N_CELLS - 1) as u32);
    net_pin_start.push(net_pins.len() as u32);

    let ports = vec![
        PortRec {
            name: s_in,
            dir: 0,
            is_bus: 0,
            _pad: 0,
            msb: 0,
            lsb: 0,
        },
        PortRec {
            name: s_out,
            dir: 1,
            is_bus: 0,
            _pad: 0,
            msb: 0,
            lsb: 0,
        },
    ];
    let port_bit_start = vec![0, 1, 2];
    let port_bit_net = vec![0, (n_nets - 1) as u32];

    let mut module = vec![0u32; module_entry::COUNT];
    module[module_entry::NAME] = s_top;
    module[module_entry::N_PORTS] = 2;
    module[module_entry::N_NETS] = n_nets as u32;
    module[module_entry::N_INSTS] = N_CELLS as u32;
    module[module_entry::N_PINS] = (2 * N_CELLS) as u32;
    module[module_entry::N_ASSIGNS] = 0;

    let frozen = FrozenNetlist {
        str_bytes: Cow::Owned(str_bytes),
        str_offs: Cow::Owned(str_offs),
        module: Cow::Owned(module),
        cells: Cow::Owned(vec![s_inv]),
        cell_pin_start: Cow::Owned(vec![0, 2]),
        cell_pin_name: Cow::Owned(vec![s_a, s_y]),
        cell_pin_dir: Cow::Owned(vec![0, 1]),
        ports: Cow::Owned(ports),
        port_bit_start: Cow::Owned(port_bit_start),
        port_bit_net: Cow::Owned(port_bit_net),
        nets: Cow::Owned(nets),
        instances: Cow::Owned(instances),
        inst_pin_start: Cow::Owned(inst_pin_start),
        pin_cellpin: Cow::Owned(pin_cellpin),
        pin_net: Cow::Owned(pin_net),
        assigns: Cow::Owned(Vec::<AssignRec>::new()),
        net_pin_start: Cow::Owned(net_pin_start),
        net_pins: Cow::Owned(net_pins),
    };
    let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;

    let dir = temp_dir("bench-1m");
    let ws = Workspace::init(&dir, &inv("rusteda init"))
        .unwrap()
        .workspace;
    let base = ws.resolve_base("rev0").unwrap();
    let mut txn = ws.begin(base, &inv("rusteda import")).unwrap();

    let t_encode = Instant::now();
    let _hash = netlist::add_to_txn(&mut txn, &frozen, Upstream::new()).unwrap();
    let encode_ms = t_encode.elapsed().as_secs_f64() * 1000.0;
    txn.commit(BTreeMap::new()).unwrap();

    let t_decode = Instant::now();
    let (_rev, rbin) = netlist::open_head(&ws).unwrap();
    let decoded = netlist::decode(&rbin).unwrap();
    let decode_ms = t_decode.elapsed().as_secs_f64() * 1000.0;

    assert_all_borrowed_in_range(&decoded, &rbin);
    assert_eq!(decoded.instances.len(), N_CELLS);
    let rss_kib = peak_rss_kib().unwrap_or(0);
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!(
            "BENCH 1M-cell netlist: build={build_ms:.1}ms, encode+write={encode_ms:.1}ms ({:.1} MiB), mmap+decode+validate={decode_ms:.1}ms, peak_rss={:.1} MiB\n",
            rbin.as_bytes().len() as f64 / (1024.0 * 1024.0),
            rss_kib as f64 / 1024.0
        ),
    );

    let _ = fs::remove_dir_all(&dir);
}
