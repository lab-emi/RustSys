// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Benchmark, ignored by default: the start of a query on a table set of 500k rows in 8
//! sections, measured in a fresh child process with a warm page cache.
//!
//! ```text
//! cargo test -p rusteda-workspace --test bench -- --ignored --nocapture
//! ```
//!
//! The parent writes the table set into a workspace, reads the file twice to warm the page
//! cache, then runs this test binary again as a child (environment flag set) several times.
//! The child measures (a) `Revision::open_rbin` (map + header validation) and one lookup, (b)
//! the whole query path: open the workspace, resolve HEAD, read `meta.json`, open the table,
//! one lookup, and (c) `verify_hash` over the content. The parent also measures the wall time
//! of each child process, which includes process start-up and the test harness.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use rusteda_workspace::{JsonMap, RbinWriter, Rev, Upstream, Workspace};

const ROWS: usize = 500_000;
const CHILD: &str = "RUSTEDA_WS_BENCH_CHILD";
const TEST: &str = "open_validate_lookup_500k_rows";
const RUNS: usize = 11;

#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
#[repr(C)]
struct Rec16 {
    name: u32,
    cell: u32,
    x: i32,
    y: i32,
}

/// Eight sections of `ROWS` rows (about 34 MB), like a netlist table.
fn table_set() -> RbinWriter<'static> {
    let n = ROWS as u32;
    let mut w = RbinWriter::new("BENCH", 1).unwrap();
    w.section_vec(
        1,
        (0..ROWS * 8)
            .map(|i| b'a' + (i % 26) as u8)
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    w.section_vec(2, (0..=n).map(|i| i * 8).collect::<Vec<u32>>())
        .unwrap();
    w.section_vec(3, (0..n).map(|i| [i, i % 97]).collect::<Vec<[u32; 2]>>())
        .unwrap();
    w.section_vec(4, (0..=n).map(|i| i * 3).collect::<Vec<u32>>())
        .unwrap();
    w.section_vec(5, (0..n * 3).map(|i| i / 3).collect::<Vec<u32>>())
        .unwrap();
    let recs = (0..n).map(|i| Rec16 {
        name: i,
        cell: i % 97,
        x: i as i32,
        y: -(i as i32),
    });
    w.section_vec(6, recs.collect::<Vec<Rec16>>()).unwrap();
    w.section_vec(
        7,
        (0..n).map(|i| f64::from(i) * 0.001).collect::<Vec<f64>>(),
    )
    .unwrap();
    w.section_vec(8, (0..n).map(u64::from).collect::<Vec<u64>>())
        .unwrap();
    w
}

#[test]
#[ignore = "benchmark: cargo test --test bench -- --ignored --nocapture"]
fn open_validate_lookup_500k_rows() {
    match std::env::var_os(CHILD) {
        Some(root) => child(Path::new(&root)),
        None => parent(),
    }
}

fn child(root: &Path) {
    let revision = Workspace::open(root).unwrap().revision(Rev(1)).unwrap();
    let t = Instant::now();
    let table = revision.open_rbin("bench", "BENCH", 1..=1).unwrap();
    let instance = table.section::<[u32; 2]>(3).unwrap()[ROWS / 2];
    let raw = t.elapsed();
    drop(table);

    let t = Instant::now();
    let ws = Workspace::open(root).unwrap();
    let head = ws.resolve_query("HEAD").unwrap();
    let revision = ws.revision(head).unwrap();
    let table = revision.open_rbin("bench", "BENCH", 1..=1).unwrap();
    let record = table.section::<Rec16>(6).unwrap()[ROWS - 1];
    let query = t.elapsed();

    let t = Instant::now();
    table.verify_hash().unwrap();
    let verify = t.elapsed();

    assert_eq!(instance, [ROWS as u32 / 2, (ROWS as u32 / 2) % 97]);
    assert_eq!(record.name, ROWS as u32 - 1);
    eprintln!(
        "BENCH raw_us={} query_us={} verify_us={}",
        raw.as_secs_f64() * 1e6,
        query.as_secs_f64() * 1e6,
        verify.as_secs_f64() * 1e6
    );
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn min(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::INFINITY, f64::min)
}

fn parent() {
    let dir = common::temp_dir("bench");
    let root = dir.join("ws");
    let ws = Workspace::init(&root, &common::inv("rusteda init"))
        .unwrap()
        .workspace;

    let t = Instant::now();
    let writer = table_set();
    let build = t.elapsed();
    let t = Instant::now();
    let mut txn = ws
        .begin(
            ws.resolve_base("rev0").unwrap(),
            &common::inv("bench write"),
        )
        .unwrap();
    txn.add_rbin("bench", "bench.rbin", &writer, Upstream::new())
        .unwrap();
    let committed = txn.commit(JsonMap::new()).unwrap();
    let write = t.elapsed();
    let path = committed.dir.join("bench.rbin");
    let size = fs::metadata(&path).unwrap().len();
    for _ in 0..2 {
        fs::read(&path).unwrap();
    }

    let exe = std::env::current_exe().unwrap();
    let (mut raw, mut query, mut verify, mut wall) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for run in 0..=RUNS {
        let t = Instant::now();
        let out = Command::new(&exe)
            .args([
                "--exact",
                TEST,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, &root)
            .output()
            .unwrap();
        let elapsed = t.elapsed();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "child failed:\n{stderr}");
        let line = stderr
            .lines()
            .find(|l| l.starts_with("BENCH "))
            .expect("a BENCH line");
        let value = |key: &str| -> f64 {
            let field = line.split(' ').find_map(|f| f.strip_prefix(key)).unwrap();
            field.parse().unwrap()
        };
        if run == 0 {
            continue; // warm-up: page cache and dynamic loader
        }
        raw.push(value("raw_us="));
        query.push(value("query_us="));
        verify.push(value("verify_us="));
        wall.push(elapsed.as_secs_f64() * 1e6);
    }

    let report = format!(
        "rusteda-workspace benchmark: {ROWS} rows, 8 sections, {size} bytes, warm page cache, \
         {RUNS} child processes\n\
         build table set (in memory)      {:>10.0} us\n\
         commit (write + fsync + publish) {:>10.0} us\n\
         {:<32} {:>10} {:>10}\n\
         {:<32} {:>10.1} {:>10.1}\n\
         {:<32} {:>10.1} {:>10.1}\n\
         {:<32} {:>10.1} {:>10.1}\n\
         {:<32} {:>10.1} {:>10.1}\n",
        build.as_secs_f64() * 1e6,
        write.as_secs_f64() * 1e6,
        "measure (us)",
        "median",
        "min",
        "open + validate + 1 lookup",
        median(raw.clone()),
        min(&raw),
        "query path (ws, HEAD, meta, open)",
        median(query.clone()),
        min(&query),
        "verify_hash (content xxh3)",
        median(verify.clone()),
        min(&verify),
        "child process wall time",
        median(wall.clone()),
        min(&wall),
    );
    fs::write(dir.join("report.txt"), &report).unwrap();
    fs::remove_dir_all(&root).unwrap();
    eprintln!("{report}");
    assert!(
        median(raw) < 50_000.0,
        "opening a mapped table must not read it"
    );
}
