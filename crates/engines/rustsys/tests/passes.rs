// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! SYNTH-REQ-0410: each optimization pass, applied alone and in the full script order, keeps the
//! function of every output, next-state, clock and asynchronous-control root. Designs with at most
//! 16 graph inputs are compared exhaustively, others on 32 768 random patterns (the synthesized
//! corpus is additionally proven equivalent by the independent checker).

mod common;

use rustsys::aig::Aig;
use rustsys::elab::Design;
use rustsys::opt;
use rustsys_fixtures::rtl::CORPUS;

/// Values of every root for the given input words (one word of 64 patterns per graph input).
fn roots(d: &Design, words: &[u64]) -> Vec<u64> {
    let sim = d.simulate(words);
    opt::design_roots(d)
        .iter()
        .map(|l| Aig::lit_value(&sim, *l))
        .collect()
}

/// Input words for round `r`: exhaustive enumeration when the inputs fit, else pseudo-random.
fn words(n: usize, r: u64) -> Vec<u64> {
    let mut x = 0x9e37_79b9_7f4a_7c15u64 ^ r.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    (0..n)
        .map(|i| {
            if n <= 16 {
                // Pattern p of round r is the input vector r * 64 + p.
                let mut w = 0u64;
                for p in 0..64u64 {
                    if ((r * 64 + p) >> i) & 1 == 1 {
                        w |= 1 << p;
                    }
                }
                w
            } else {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            }
        })
        .collect()
}

fn rounds(n: usize) -> u64 {
    if n <= 16 {
        (1u64 << n).div_ceil(64)
    } else {
        512
    }
}

fn same(name: &str, pass: &str, before: &Design, after: &Design) {
    let n = before.aig.num_inputs();
    assert_eq!(n, after.aig.num_inputs(), "{name} {pass}: inputs changed");
    for r in 0..rounds(n) {
        let w = words(n, r);
        assert_eq!(
            roots(before, &w),
            roots(after, &w),
            "{name} {pass}: round {r}"
        );
    }
}

type Pass = fn(&mut Design);

const PASSES: [(&str, Pass); 9] = [
    ("balance", opt::balance::balance),
    ("rewrite", opt::rewrite::rewrite),
    ("refactor", opt::refactor::refactor),
    ("rewrite-zero", opt::rewrite::rewrite_zero),
    ("resub", opt::resub::resub),
    ("merge", opt::merge::merge),
    ("merge-local", opt::merge::merge_local),
    ("mux-merge", opt::muxmerge::mux_merge),
    ("mux-decode", opt::muxdecode::mux_decode),
];

// SYNTH-REQ-0411, SYNTH-REQ-0414, SYNTH-REQ-0418: <5000-node reconvergent
// forest, <128 MiB across two design copies and 16 bounded worker caches.
// Exhaustive/simulation checks here supplement the external ABC pass checks.
#[test]
fn parallel_planning_keeps_stable_graph_and_roots() {
    let base = common::design(
        r#"
module m(input clk,s,t,input [127:0] a,b,c,d,output reg [127:0] q,output [127:0] y);
  wire [127:0] f = ((a & b) | (a & c)) ^ ((s ? b : c) & (~d | a));
  always @(posedge clk) q <= t ? f : q;
  assign y = f ^ q;
endmodule
"#,
    );
    assert!(base.aig.num_nodes() < 5000);
    for (name, pass) in PASSES
        .iter()
        .filter(|(name, _)| matches!(*name, "rewrite" | "rewrite-zero" | "refactor" | "resub"))
    {
        let run = |threads| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let mut d = base.clone();
            pool.install(|| pass(&mut d));
            d
        };
        let serial = run(1);
        same("parallel_forest", name, &base, &serial);
        for threads in [2, 16] {
            let parallel = run(threads);
            assert_eq!(
                format!("{serial:?}"),
                format!("{parallel:?}"),
                "{name}, {threads} threads"
            );
        }
    }
}

// SYNTH-REQ-0414: optional storage/node exhaustion is a recorded no-op.
#[test]
fn rewrite_storage_and_node_budgets_preserve_design() {
    use opt::parallel::WorkLimits;
    let base = common::design("module m(input a,b,c,output y);assign y=(a&b)|(a&c);endmodule");
    for limits in [
        WorkLimits {
            max_nodes: 0,
            ..WorkLimits::default()
        },
        WorkLimits {
            max_cut_bytes: 0,
            ..WorkLimits::default()
        },
        WorkLimits {
            max_cut_bytes: base.aig.num_nodes() * 16,
            ..WorkLimits::default()
        },
    ] {
        let mut d = base.clone();
        let stats = opt::rewrite::rewrite_with_limits(&mut d, false, limits);
        assert!(stats.node_budget_reached || stats.storage_budget_reached);
        assert_eq!(format!("{base:?}"), format!("{d:?}"));
    }
}

// SYNTH-REQ-0411, SYNTH-REQ-0414, SYNTH-REQ-0418: dependent rewrites in a
// <5000-node reconvergent forest must replay stale speculative candidates.
#[test]
fn resub_replays_conflicts_and_bounds_read_storage() {
    let base = common::design(
        r#"
module m(input [63:0] a,b,c,d, output [63:0] bc_o,z);
  wire [63:0] bc=b|c;
  wire [63:0] y=(a&b)|(a&c);
  assign bc_o=bc;
  assign z=(y&d)|(y&~d);
endmodule
"#,
    );
    assert!(base.aig.num_nodes() < 5000);
    assert!(
        base.aig.num_nodes() < 1024,
        "dependencies must share one commit batch"
    );
    let run = |threads, limits| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let mut d = base.clone();
        let stats = pool.install(|| opt::resub::resub_with_limits(&mut d, limits));
        (d, stats)
    };
    let (serial, _) = run(1, opt::resub::ResubLimits::default());
    let (parallel, stats) = run(16, opt::resub::ResubLimits::default());
    assert!(
        stats.speculative_replays > 0,
        "fixture must exercise ordered conflict replay"
    );
    assert_eq!(format!("{serial:?}"), format!("{parallel:?}"));
    same("speculative_resub", "resub", &base, &parallel);
    let (spill, stats) = run(
        16,
        opt::resub::ResubLimits {
            max_proposal_bytes: 0,
            ..Default::default()
        },
    );
    assert!(stats.speculative_replays > 0 && stats.bounded_windows > 0);
    assert_eq!(
        format!("{serial:?}"),
        format!("{spill:?}"),
        "read-storage spill must replay, not skip optimization"
    );
    let limits = opt::resub::ResubLimits {
        max_read_records: 0,
        ..Default::default()
    };
    for threads in [1, 16] {
        let (unchanged, stats) = run(threads, limits);
        assert!(stats.bounded_windows > 0);
        assert_eq!(format!("{base:?}"), format!("{unchanged:?}"));
    }
}

// SYNTH-REQ-0414: a bounded refactoring window is an explicit no-op.
#[test]
fn refactor_window_budget_preserves_design() {
    let base = common::design("module m(input a,b,c,output y);assign y=(a&b)|(a&c);endmodule");
    let mut d = base.clone();
    let stats = opt::refactor::refactor_cached_with_limits(
        &mut d,
        &mut opt::refactor::FormCache::default(),
        opt::parallel::WorkLimits {
            max_window_nodes: 0,
            ..Default::default()
        },
    );
    assert!(stats.bounded_windows > 0);
    assert_eq!(format!("{base:?}"), format!("{d:?}"));
    let mut d = base.clone();
    let stats = opt::refactor::refactor_cached_with_limits(
        &mut d,
        &mut opt::refactor::FormCache::default(),
        opt::parallel::WorkLimits {
            max_plan_bytes: 0,
            ..Default::default()
        },
    );
    assert!(stats.storage_budget_reached);
    assert_eq!(format!("{base:?}"), format!("{d:?}"));
}

#[test]
fn every_pass_keeps_every_root() {
    for d in CORPUS {
        let srcs: Vec<&str> = d.files.iter().map(|(_, t)| *t).collect();
        let base = common::elab(&srcs, Some(d.top)).unwrap_or_else(|e| panic!("{}: {e}", d.name));
        // Alone.
        for (pass, f) in PASSES {
            let mut x = base.clone();
            f(&mut x);
            same(d.name, pass, &base, &x);
        }
        // Chained in the order of the high-effort script (without sweep, which may remove
        // unobservable state and so changes the root list).
        let mut x = base.clone();
        for (pass, f) in PASSES.iter().chain(PASSES.iter()) {
            f(&mut x);
            same(d.name, pass, &base, &x);
        }
    }
}

// SYNTH-REQ-0406: an enabled decoded write needs one select term per word, shared by its bits,
// and one multiplexer per bit.
#[test]
fn mux_merge_shares_word_selects() {
    let src = r#"
module m (input clk, we, input [1:0] wa, input [7:0] wd, output [31:0] q);
  reg [7:0] r [0:3];
  always @(posedge clk) if (we) r[wa] <= wd;
  assign q = {r[3], r[2], r[1], r[0]};
endmodule"#;
    let mut d = common::design(src);
    opt::sweep(&mut d);
    let before = d.clone();
    opt::muxmerge::mux_merge(&mut d);
    same("rf", "mux-merge", &before, &d);
    assert!(before.aig.num_ands() > 150, "{}", before.aig.num_ands());
    assert!(d.aig.num_ands() <= 32 * 3 + 12, "{}", d.aig.num_ands());
}

// SYNTH-REQ-0406: a selection between the same two words in either order needs one multiplexer
// per bit.
#[test]
fn mux_merge_shares_swapped_selections() {
    let src = r#"
module m (input [7:0] a, b, input lt, mx, output reg [7:0] y);
  always @* if (mx) y = lt ? b : a; else y = lt ? a : b;
endmodule"#;
    let mut d = common::design(src);
    opt::sweep(&mut d);
    let before = d.clone();
    opt::muxmerge::mux_merge(&mut d);
    same("swap", "mux-merge", &before, &d);
    assert!(d.aig.num_ands() <= 8 * 3 + 3, "{}", d.aig.num_ands());
}

// SYNTH-REQ-0408: nested selections whose selects repeat across the bits of a word become sums of
// products over select products shared by all bits, in the phase whose leaves are regular.
#[test]
fn mux_decode_shares_select_products() {
    let src = r#"
module m (input s, t, input [15:0] a, b, c, output [15:0] y, output [15:0] z);
  assign y = s ? (t ? a : b) : c;
  assign z = ~(s ? (t ? a : b) : c);
endmodule"#;
    let mut d = common::design(src);
    opt::sweep(&mut d);
    let before = d.clone();
    opt::muxdecode::mux_decode(&mut d);
    same("mux3", "mux-decode", &before, &d);
    // Two shared products and five AND nodes per bit (three terms, two ORs) instead of six.
    assert!(before.aig.num_ands() >= 16 * 6, "{}", before.aig.num_ands());
    assert!(d.aig.num_ands() <= 16 * 5 + 2, "{}", d.aig.num_ands());
}

// SYNTH-REQ-0414: optional-pass budgets must roll back without changing even
// state names or graph identities. Tiny fixture, <32 MiB including copies.
#[test]
fn mux_decode_budget_fallback_keeps_original_design() {
    use opt::muxdecode::{DecodeLimits, DecodeStatus, mux_decode_with_limits};
    let src = r#"
module m(input s, t, input [15:0] a, b, c, output [15:0] y);
  assign y = s ? (t ? a : b) : c;
endmodule"#;
    let base = common::design(src);
    let mut limits = vec![
        DecodeLimits {
            max_nodes: base.aig.num_nodes() - 1,
            ..DecodeLimits::default()
        },
        DecodeLimits {
            max_signatures: 0,
            ..DecodeLimits::default()
        },
    ];
    limits.extend([0, 16, 128, 512].map(|max_expansions| DecodeLimits {
        max_expansions,
        ..DecodeLimits::default()
    }));
    for limit in limits {
        let mut design = base.clone();
        assert_eq!(
            mux_decode_with_limits(&mut design, limit),
            DecodeStatus::BudgetReached
        );
        assert_eq!(format!("{design:?}"), format!("{base:?}"));
    }
}

// SYNTH-REQ-0414: bounded broad forest (<5,000 nodes), <16 MiB scratch with
// one worker. The independent external CEC fixture separately checks the pass.
#[test]
fn mux_decode_broad_forest_uses_shared_signatures() {
    let src = r#"
module m(input s, t, input [511:0] a, b, c, output [511:0] y);
  assign y = s ? (t ? a : b) : c;
endmodule"#;
    let before = common::design(src);
    assert!(before.aig.num_nodes() < 5000);
    let mut design = before.clone();
    let status = opt::muxdecode::mux_decode_with_limits(
        &mut design,
        opt::muxdecode::DecodeLimits::default(),
    );
    assert_eq!(status, opt::muxdecode::DecodeStatus::Rewritten);
    assert!(design.aig.num_ands() < before.aig.num_ands());
    same("broad_forest", "mux-decode", &before, &design);
}

// SYNTH-REQ-0411: unsuccessful fanout scans need a work bound independent of
// the number of usable divisors. <5,000 nodes and <32 MiB for both graph copies.
#[test]
fn resub_bounds_high_fanout_exploration() {
    let src = r#"
module m(input s, t, input [511:0] a, b, c, output [511:0] y);
  assign y = s ? (t ? a : b) : c;
endmodule"#;
    let before = common::design(src);
    let mut design = before.clone();
    let stats = opt::resub::resub_with_limits(
        &mut design,
        opt::resub::ResubLimits {
            max_fanout_visits_per_window: 8,
            ..opt::resub::ResubLimits::default()
        },
    );
    assert!(stats.bounded_windows > 0);
    assert!(stats.fanout_visits <= stats.windows * 8);
    same("high_fanout", "resub", &before, &design);
    let mut untouched = before.clone();
    let stats = opt::resub::resub_with_limits(
        &mut untouched,
        opt::resub::ResubLimits {
            max_nodes: 0,
            ..opt::resub::ResubLimits::default()
        },
    );
    assert!(stats.node_budget_reached);
    assert_eq!(format!("{untouched:?}"), format!("{before:?}"));
}

// SYNTH-REQ-0408: a select that guards a single bit is not decoded, and a hold term under a
// shared enable and reset becomes one product per word.
#[test]
fn mux_decode_needs_repeated_selects() {
    let single = r#"
module m (input s, t, a, b, c, output y);
  assign y = s ? (t ? a : b) : c;
endmodule"#;
    let mut d = common::design(single);
    opt::sweep(&mut d);
    let before = d.aig.num_ands();
    opt::muxdecode::mux_decode(&mut d);
    assert_eq!(d.aig.num_ands(), before);
    let word = r#"
module m (input clk, rstn, w, input [7:0] a, output reg [7:0] q);
  always @(posedge clk) if (!rstn) ; else if (w) q <= a; else q <= {1'b0, q[7:1]};
endmodule"#;
    let mut d = common::design(word);
    opt::sweep(&mut d);
    let before = d.clone();
    opt::muxdecode::mux_decode(&mut d);
    same("shift", "mux-decode", &before, &d);
    assert!(d.aig.num_ands() < before.aig.num_ands());
}
