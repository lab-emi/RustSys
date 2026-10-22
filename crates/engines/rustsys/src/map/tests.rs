// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The level-parallel mapping steps against one-by-one references on random graphs, under a
//! thread pool: the required-time and cover sweep and the cover extraction (SYNTH-REQ-0515), the
//! exact-local-area pass (SYNTH-REQ-0517), and whole mappings for several thread counts.

use rustc_hash::FxHashSet;
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::random::Rng;

use super::library::{ARR_INF, CellLib, DELAY_SCALE};
use super::mapper::{Impl, MapBackend, MapOptions, Mapped, MappedGate, Mapper, map, relaxed};
use crate::aig::{Aig, Lit};
use rusteda_core::job::Ctx;

fn cell_lib(v: Variant) -> CellLib {
    let l = rusteda_formats::liberty::read_text(&library(v), v.name()).expect("fixture library");
    CellLib::build(&[l], &[]).expect("cell library")
}

/// A random graph: `ands` AND nodes over `inputs` inputs, fanins drawn mostly from recent nodes
/// (for depth) with random polarity; the last `outputs` nodes are the roots.
fn random_aig(seed: u64, inputs: usize, ands: usize, outputs: usize) -> (Aig, Vec<Lit>) {
    let mut rng = Rng(seed);
    let mut aig = Aig::new();
    let mut lits: Vec<Lit> = (0..inputs).map(|_| aig.input()).collect();
    for _ in 0..ands {
        let n = lits.len() as u64;
        let near = n - 1 - rng.below(n.min(48));
        let a = lits[near as usize].xor(rng.coin(50));
        let b = lits[rng.below(n) as usize].xor(rng.coin(50));
        let y = aig.and(a, b);
        if !y.is_const() {
            lits.push(y.regular());
        }
    }
    let roots = lits[lits.len() - outputs..]
        .iter()
        .map(|l| l.xor(rng.coin(50)))
        .collect();
    (aig, roots)
}

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool")
}

fn same_gates(a: &Mapped, b: &Mapped) -> bool {
    let key = |g: &MappedGate| (g.cell, g.out, g.ins.clone());
    a.depth == b.depth
        && a.free_neg_used == b.free_neg_used
        && a.gates.len() == b.gates.len()
        && a.gates.iter().zip(&b.gates).all(|(x, y)| key(x) == key(y))
}

fn same_choices(a: &Mapper, b: &Mapper) -> bool {
    a.best.iter().zip(&b.best).all(|(x, y)| {
        (0..2).all(|p| x[p].imp == y[p].imp && x[p].arr == y[p].arr && x[p].af == y[p].af)
    })
}

const OPTS: MapOptions = MapOptions {
    max_cuts: 12,
    cut_memory_budget: 4 * 1024 * 1024 * 1024,
    area_passes: 2,
    exact_passes: 0,
    exact_limits: super::mapper::ExactLimits::DEFAULT,
    delay_relax: Some(0.1),
    backend: MapBackend::Cpu,
    fanout_recovery: None,
    primary_inputs: 0,
};

// SYNTH-REQ-0515: the parallel sweep equals the one-by-one reverse topological order.
#[test]
fn cover_sweep_equals_one_by_one() {
    let lib = cell_lib(Variant::Rich);
    let free = FxHashSet::default();
    let p = pool(8);
    for seed in 1..=4 {
        let (aig, roots) = random_aig(seed, 40, 2500, 48);
        let mut m = Mapper::new(&aig, &lib, &roots, &free);
        let (mapped, limit) = p.install(|| m.cut_passes_cpu(OPTS, Ctx::NONE).unwrap());
        for lim in [None, limit] {
            let req = p.install(|| m.required(lim));
            assert_eq!(
                req,
                required_one_by_one(&m, lim),
                "seed {seed} required times"
            );
        }
        let ext = p.install(|| m.extract());
        assert!(
            same_gates(&ext, &extract_one_by_one(&m)),
            "seed {seed} cover"
        );
        assert!(same_gates(&ext, &mapped), "seed {seed} cover of the passes");
    }
}

// <3000 nodes, two compact tables and two maps (<32MiB). Full allocated covers remain the
// independent reference for intermediate count views, including constant roots and polarity.
#[test]
fn compact_intermediate_covers_match_full_gate_covers() {
    let lib = cell_lib(Variant::Rich);
    let free = FxHashSet::default();
    let p = pool(16);
    for seed in [73, 81] {
        let (aig, mut roots) = random_aig(seed, 48, 2500, 96);
        roots.extend([Lit::FALSE, Lit::TRUE, Lit::TRUE]);
        let mut a = Mapper::new(&aig, &lib, &roots, &free);
        let mut b = Mapper::new(&aig, &lib, &roots, &free);
        a.compact_cover = true;
        let (mut ma, limit) = p.install(|| a.cut_passes_cpu(OPTS, Ctx::NONE).unwrap());
        let (mut mb, _) = b.cut_passes_cpu(OPTS, Ctx::NONE).unwrap();
        assert!(ma.gates.is_empty());
        for _ in 0..3 {
            assert!(same_choices(&a, &b));
            assert_eq!(a.fanout_estimate(&ma), b.fanout_estimate(&mb));
            assert!(same_gates(&a.extract(), &mb));
            let req = a.required(limit);
            p.install(|| a.exact_area(&ma, &req, Ctx::NONE).unwrap());
            b.exact_area_one_by_one(&mb, &req);
            ma = a.extract_current();
            mb = b.extract();
        }
        assert!(same_choices(&a, &b));
        assert!(same_gates(&a.extract(), &mb));
    }
}

// SYNTH-REQ-0517: the speculative level-parallel exact-local-area pass decides every node as the
// one-by-one level order does, pass after pass.
#[test]
fn exact_area_equals_one_by_one() {
    let lib = cell_lib(Variant::Rich);
    let free = FxHashSet::default();
    let p = pool(8);
    for seed in 11..=14 {
        let (aig, roots) = random_aig(seed, 48, 3000, 64);
        let mut a = Mapper::new(&aig, &lib, &roots, &free);
        let mut b = Mapper::new(&aig, &lib, &roots, &free);
        let (mut ma, limit) = p.install(|| a.cut_passes_cpu(OPTS, Ctx::NONE).unwrap());
        let (mut mb, _) = b.cut_passes_cpu(OPTS, Ctx::NONE).unwrap();
        assert!(same_choices(&a, &b), "seed {seed}: cut passes differ");
        let wide = a.levels.iter().filter(|l| l.len() >= 16).count();
        assert!(wide > 4, "seed {seed}: only {wide} levels run in parallel");
        for pass in 0..3 {
            let req = a.required(limit);
            p.install(|| a.exact_area(&ma, &req, Ctx::NONE).unwrap());
            b.exact_area_one_by_one(&mb, &req);
            assert!(same_choices(&a, &b), "seed {seed} pass {pass}");
            ma = a.extract();
            mb = b.extract();
        }
    }
}

// <3500 nodes and <16MiB scratch. Exhausted speculation must replay the reference order;
// electrical cost buckets must not alter a choice at 0/1 or a buffer-price boundary.
#[test]
fn exact_area_priced_and_spilled_plans_equal_sequential() {
    let lib = cell_lib(Variant::Rich);
    let free = FxHashSet::default();
    let p = pool(16);
    for budget in [0, 4096, 1024 * 1024] {
        let (aig, roots) = random_aig(511, 48, 3000, 128);
        let mut a = Mapper::new(&aig, &lib, &roots, &free);
        let mut b = Mapper::new(&aig, &lib, &roots, &free);
        let (mut ma, limit) = p.install(|| a.cut_passes_cpu(OPTS, Ctx::NONE).unwrap());
        let (mut mb, _) = b.cut_passes_cpu(OPTS, Ctx::NONE).unwrap();
        a.repair_price = 137;
        b.repair_price = 137;
        a.repair_fanout = 3;
        b.repair_fanout = 3;
        a.exact_limits.max_plan_bytes = budget;
        a.exact_limits.batch_nodes = 127;
        for pass in 0..3 {
            let req = a.required(limit);
            let stats = p.install(|| a.exact_area(&ma, &req, Ctx::NONE).unwrap());
            b.exact_area_one_by_one(&mb, &req);
            assert!(same_choices(&a, &b), "budget {budget}, pass {pass}");
            if budget == 0 {
                assert!(stats.spilled > 0);
            }
            ma = a.extract();
            mb = b.extract();
        }
    }
}

// SYNTH-REQ-0515, SYNTH-REQ-0517: the mapping does not depend on the thread count.
#[test]
fn mapping_independent_of_threads() {
    let lib = cell_lib(Variant::Full);
    let free = FxHashSet::default();
    let (aig, roots) = random_aig(99, 64, 4000, 64);
    let opts = MapOptions {
        max_cuts: 40,
        area_passes: 4,
        exact_passes: 4,
        ..OPTS
    };
    let base = pool(1)
        .install(|| map(&aig, &lib, &roots, &free, opts))
        .expect("map");
    for threads in [3, 8] {
        let m = pool(threads)
            .install(|| map(&aig, &lib, &roots, &free, opts))
            .expect("map");
        assert!(same_gates(&base, &m), "{threads} threads");
    }
}

// SYNTH-REQ-0512: area recovery keeps the arrival of every output within the delay bound
// `floor(target * (1 + r))` of the delay-oriented cover.
#[test]
fn area_recovery_keeps_delay_bound() {
    let free = FxHashSet::default();
    let mut worst = Vec::new();
    for v in [Variant::Rich, Variant::Full, Variant::Minimal] {
        let lib = cell_lib(v);
        for seed in 200..230 {
            let (aig, roots) = random_aig(seed, 24, 600, 16);
            let delay_only = MapOptions {
                area_passes: 0,
                exact_passes: 0,
                ..OPTS
            };
            let d = map(&aig, &lib, &roots, &free, delay_only).expect("map");
            let target = (d.max_arrival * DELAY_SCALE).round() as i32;
            for (area_passes, exact_passes) in [(2, 0), (2, 2), (4, 6)] {
                let o = MapOptions {
                    area_passes,
                    exact_passes,
                    ..OPTS
                };
                let m = map(&aig, &lib, &roots, &free, o).expect("map");
                let got = (m.max_arrival * DELAY_SCALE).round() as i32;
                if got > relaxed(target, 0.1) {
                    worst.push(format!(
                        "{v:?} seed {seed} passes ({area_passes}, {exact_passes}): {got} > {}",
                        relaxed(target, 0.1)
                    ));
                }
            }
        }
    }
    assert!(
        worst.is_empty(),
        "delay bound exceeded:\n{}",
        worst.join("\n")
    );
}

/// The sequential required-time computation (reverse node order).
fn required_one_by_one(mp: &Mapper, limit: Option<i32>) -> Vec<[i32; 2]> {
    let n = mp.aig.num_nodes();
    let mut req = vec![[ARR_INF; 2]; n];
    let lim = limit.unwrap_or(ARR_INF);
    let mut used = vec![[false; 2]; n];
    let sub = |r: i32, d: i32| {
        if r == ARR_INF {
            ARR_INF
        } else {
            r.saturating_sub(d)
        }
    };
    for r in mp.roots {
        if !r.is_const() {
            let p = usize::from(r.is_compl());
            req[r.node() as usize][p] = req[r.node() as usize][p].min(lim);
            used[r.node() as usize][p] = true;
        }
    }
    for id in (1..n).rev() {
        // Process the inverter phase first so that its input phase is updated in time.
        for p in [0usize, 1] {
            for q in [p, 1 - p] {
                if used[id][q] && matches!(mp.best[id][q].imp, Impl::Inv) {
                    let r = sub(req[id][q], mp.d_inv);
                    req[id][1 - q] = req[id][1 - q].min(r);
                    used[id][1 - q] = true;
                }
            }
        }
        for q in 0..2usize {
            if !used[id][q] {
                continue;
            }
            if let Impl::Cell { cut, m } = mp.best[id][q].imp {
                let c = &mp.cuts[id][cut as usize];
                let cell = &mp.lib.comb[m.cell as usize];
                for i in 0..cell.inputs.len() {
                    let leaf = c.leaves[m.perm[i] as usize] as usize;
                    let ph = usize::from((m.phase >> i) & 1 == 1);
                    let r = sub(req[id][q], cell.delay_q[i]);
                    req[leaf][ph] = req[leaf][ph].min(r);
                    used[leaf][ph] = true;
                }
            }
        }
    }
    if limit.is_none() {
        for (i, u) in used.iter().enumerate() {
            for p in 0..2 {
                if u[p] {
                    req[i][p] = ARR_INF - 1;
                }
            }
        }
    }
    req
}

/// The sequential cover extraction (depth-first from the roots).
fn extract_one_by_one(mp: &Mapper) -> Mapped {
    let mut gates = Vec::new();
    let mut done: FxHashSet<(u32, bool)> = FxHashSet::default();
    let mut free_neg_used = FxHashSet::default();
    let mut stack: Vec<(u32, bool)> = mp
        .roots
        .iter()
        .filter(|r| !r.is_const())
        .map(|r| (r.node(), r.is_compl()))
        .collect();
    while let Some((id, ph)) = stack.pop() {
        if !done.insert((id, ph)) {
            continue;
        }
        match mp.best[id as usize][usize::from(ph)].imp {
            Impl::Input => {}
            Impl::FreeNeg => {
                free_neg_used.insert(id);
            }
            Impl::Inv => {
                gates.push(MappedGate {
                    cell: mp.lib.inverter,
                    out: (id, ph),
                    ins: vec![(id, !ph)],
                });
                stack.push((id, !ph));
            }
            Impl::Cell { cut, m } => {
                let c = &mp.cuts[id as usize][cut as usize];
                let cell = &mp.lib.comb[m.cell as usize];
                let ins: Vec<(u32, bool)> = (0..cell.inputs.len())
                    .map(|i| (c.leaves[m.perm[i] as usize], (m.phase >> i) & 1 == 1))
                    .collect();
                stack.extend(ins.iter().copied());
                gates.push(MappedGate {
                    cell: m.cell as usize,
                    out: (id, ph),
                    ins,
                });
            }
            Impl::None => {
                // Every AND node has a two-input decomposition through the inverter and a
                // NAND/AND/NOR-like cell (checked when the library is classified).
                unreachable!("node {id} phase {ph} has no implementation");
            }
        }
    }
    // Logic depth: nodes are topologically numbered; within a node the inverter of the other
    // phase comes after the phase it reads.
    gates.sort_by_key(|g| (g.out.0, g.out.1));
    let mut order = gates.clone();
    order.sort_by_key(|g| (g.out.0, matches!(g.ins.first(), Some(x) if x.0 == g.out.0)));
    let mut level: rustc_hash::FxHashMap<(u32, bool), u32> = rustc_hash::FxHashMap::default();
    let mut depth = 0u32;
    for g in &order {
        let d = 1 + g
            .ins
            .iter()
            .map(|x| level.get(x).copied().unwrap_or(0))
            .max()
            .unwrap_or(0);
        level.insert(g.out, d);
        depth = depth.max(d);
    }
    Mapped {
        gates,
        free_neg_used,
        max_arrival: 0.0,
        depth,
        backend: "cpu",
        phase_s: Vec::new(),
    }
}

// SYNTH-REQ-0524, SYNTH-REQ-0525: load-priced area-flow passes and cached integer area-flow shares.
#[test]
fn load_priced_cover_is_thread_independent() {
    let lib = cell_lib(Variant::Rich);
    for seed in 101..104 {
        let (aig, roots) = random_aig(seed, 32, 2500, 96);
        let opts = MapOptions {
            fanout_recovery: Some(3),
            primary_inputs: 8,
            delay_relax: None,
            exact_passes: 4,
            ..OPTS
        };
        let a = pool(1).install(|| map(&aig, &lib, &roots, &FxHashSet::default(), opts).unwrap());
        let b = pool(8).install(|| map(&aig, &lib, &roots, &FxHashSet::default(), opts).unwrap());
        assert!(same_gates(&a, &b), "load-priced seed {seed}");
    }
}

// SYNTH-REQ-0544: constrained mapping delay proxy uses specified input slew and non-excluded drives.
#[test]
fn constrained_characterization_ignores_excluded_drive_inventory() {
    let mut libs =
        vec![rusteda_formats::liberty::read_text(&library(Variant::Rich), "profile").unwrap()];
    let before = CellLib::build_profile(&libs, &[], true, Some(0.1)).unwrap();
    let mut big = libs[0]
        .cells
        .iter()
        .find(|c| c.name == "wt_inv_x1")
        .unwrap()
        .clone();
    big.name = "excluded_big_driver".into();
    big.area = 10000.0;
    for p in &mut big.pins {
        p.capacitance = 10000.0;
    }
    libs[0].cells.push(big);
    let after =
        CellLib::build_profile(&libs, &["excluded_big_driver".into()], true, Some(0.1)).unwrap();
    assert_eq!(before.comb.len(), after.comb.len());
    for (a, b) in before.comb.iter().zip(&after.comb) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.delay_q, b.delay_q);
    }
    let faster =
        CellLib::build_profile(&libs, &["excluded_big_driver".into()], true, Some(0.01)).unwrap();
    assert!(
        faster
            .comb
            .iter()
            .zip(&after.comb)
            .any(|(a, b)| a.delay_q != b.delay_q)
    );
}

// SYNTH-REQ-0537, SYNTH-REQ-0538: unconstrained required-time annotations and complemented leaf matches.
#[test]
fn explicit_boundary_launches_and_requirements_survive_recovery() {
    use super::mapper::{TimingBoundary, map_with_boundary};
    let lib = cell_lib(Variant::Rich);
    let mut aig = Aig::new();
    let a = aig.input();
    let b = aig.input();
    let out = aig.and(a, b);
    let roots = [out, !a];
    let boundary = TimingBoundary {
        launches: vec![(a.node(), 100000), (b.node(), 20000)],
        required: vec![180000, ARR_INF],
    };
    let opts = MapOptions {
        exact_passes: 4,
        delay_relax: None,
        ..OPTS
    };
    let one = pool(1)
        .install(|| {
            map_with_boundary(
                &aig,
                &lib,
                &roots,
                &FxHashSet::default(),
                opts,
                Some(&boundary),
                Ctx::NONE,
            )
        })
        .unwrap();
    let many = pool(8)
        .install(|| {
            map_with_boundary(
                &aig,
                &lib,
                &roots,
                &FxHashSet::default(),
                opts,
                Some(&boundary),
                Ctx::NONE,
            )
        })
        .unwrap();
    assert!(same_gates(&one, &many));
    assert!(one.max_arrival >= 1.0);
    let mut m = Mapper::new(&aig, &lib, &roots, &FxHashSet::default());
    m.root_required = Some(boundary.required.clone());
    let (mapped, _) = m.cut_passes_cpu(opts, Ctx::NONE).unwrap();
    let req = m.required(None);
    assert_eq!(req[out.node() as usize][0], 180000);
    assert_ne!(req[a.node() as usize][1], ARR_INF);
    assert!(!mapped.gates.is_empty());
    let bad = TimingBoundary {
        launches: vec![(u32::MAX, 0)],
        required: boundary.required,
    };
    assert!(
        map_with_boundary(
            &aig,
            &lib,
            &roots,
            &FxHashSet::default(),
            opts,
            Some(&bad),
            Ctx::NONE
        )
        .is_err()
    );
}

// SYNTH-REQ-0537: area recovery relaxes only already infeasible root bounds.
#[test]
fn area_recovery_relaxes_only_already_infeasible_root_bounds() {
    let lib = cell_lib(Variant::Rich);
    let mut aig = Aig::new();
    let a = aig.input();
    let b = aig.input();
    let out = aig.and(a, b);
    let roots = [out, !a];
    let free = FxHashSet::default();
    let mut m = Mapper::new(&aig, &lib, &roots, &free);
    let bounds = vec![-100, ARR_INF];
    m.root_required = Some(bounds);
    m.cut_passes_cpu(
        MapOptions {
            delay_relax: None,
            ..OPTS
        },
        Ctx::NONE,
    )
    .unwrap();
    let bounds = m.root_required.as_ref().unwrap();
    assert!(bounds[0] > -100);
    assert_eq!(bounds[1], ARR_INF);
    let mut delay = Mapper::new(&aig, &lib, &roots, &free);
    delay.root_required = Some(vec![-100, ARR_INF]);
    delay.cut_passes_cpu(OPTS, Ctx::NONE).unwrap();
    assert_eq!(delay.root_required.as_ref().unwrap()[0], -100);
}

// ---- WP2 prototype: the cut-pass task against the mapper's own passes ----

use super::compute::TaskPasses;
use super::mapper::Rank;
use rusteda_compute::BackendKind;

fn same_state(a: &Mapper, b: &Mapper) -> Result<(), String> {
    for &id in a.levels.iter().flatten() {
        let i = id as usize;
        if a.cuts[i] != b.cuts[i] {
            return Err(format!("cuts of node {id} differ"));
        }
        for p in 0..2 {
            let (x, y) = (a.best[i][p], b.best[i][p]);
            if x.imp != y.imp || x.arr != y.arr || x.af != y.af {
                return Err(format!(
                    "choice of node {id} phase {p} differs: {x:?} vs {y:?}"
                ));
            }
        }
    }
    Ok(())
}

/// The mapper's passes and the task's passes on the same inputs, compared after every pass.
fn task_equals_mapper(kind: BackendKind, cases: &[(Variant, u64, usize, usize, usize, usize)]) {
    let free = FxHashSet::default();
    for &(v, seed, inputs, ands, outputs, c) in cases {
        let lib = cell_lib(v);
        let (aig, roots) = random_aig(seed, inputs, ands, outputs);
        let mut a = Mapper::new(&aig, &lib, &roots, &free);
        let mut b = Mapper::new(&aig, &lib, &roots, &free);
        let mut t = TaskPasses::new(&b, c, kind);
        let fo2: Vec<u32> = a.fanout.iter().map(|f| (*f).max(1) * 2).collect();
        a.pass(Rank::Delay, &fo2, None, c, Ctx::NONE).unwrap();
        t.pass(&mut b, Rank::Delay, &fo2, None, true);
        same_state(&a, &b).unwrap_or_else(|e| panic!("{v:?} seed {seed} C {c} delay pass: {e}"));
        let limit = Some(relaxed(a.target(), 0.1));
        let mut mapped = a.extract();
        for i in 0..3 {
            let req = a.required(if i == 2 { None } else { limit });
            let est = a.fanout_estimate(&mapped);
            a.pass(Rank::Area, &est, Some(&req), c, Ctx::NONE).unwrap();
            t.pass(&mut b, Rank::Area, &est, Some(&req), true);
            same_state(&a, &b)
                .unwrap_or_else(|e| panic!("{v:?} seed {seed} C {c} area pass {i}: {e}"));
            mapped = a.extract();
        }
    }
}

const TASK_CASES: [(Variant, u64, usize, usize, usize, usize); 6] = [
    (Variant::Rich, 1, 40, 2500, 48, 12),
    (Variant::Full, 2, 64, 3000, 64, 40),
    (Variant::Minimal, 3, 24, 1500, 16, 8),
    (Variant::Rich, 4, 32, 2000, 32, 100),
    (Variant::Full, 5, 16, 800, 8, 1),
    (Variant::Rich, 6, 48, 3000, 64, 126),
];

// SYNTH-REQ-0515, SYNTH-REQ-0516: after every pass, the task's CPU backend leaves the mapper in
// the state of the mapper's own passes.
#[test]
fn task_cpu_passes_equal_mapper_passes() {
    task_equals_mapper(BackendKind::Cpu, &TASK_CASES);
}

// SYNTH-REQ-0516: the same for the CUDA backend; needs a GPU.
#[test]
#[ignore]
fn task_cuda_passes_equal_mapper_passes() {
    task_equals_mapper(BackendKind::Cuda, &TASK_CASES);
}

// SYNTH-REQ-0516: whole mappings with technology passes and no exact passes (the whole cut table
// must be read back before a technology pass) and with exact passes.
#[test]
fn task_cpu_mapping_equals_cpu_mapping() {
    let free = FxHashSet::default();
    for (v, seed) in [
        (Variant::Rich, 31u64),
        (Variant::Full, 32),
        (Variant::Minimal, 33),
    ] {
        let lib = cell_lib(v);
        let (aig, roots) = random_aig(seed, 32, 2500, 64);
        for (fanout_recovery, exact_passes, area_passes) in
            [(None, 0, 2), (Some(3), 0, 2), (Some(3), 2, 4), (None, 4, 4)]
        {
            let o = MapOptions {
                max_cuts: 20,
                area_passes,
                exact_passes,
                fanout_recovery,
                primary_inputs: 8,
                ..OPTS
            };
            let cpu = map(&aig, &lib, &roots, &free, o).expect("map");
            let task = map(
                &aig,
                &lib,
                &roots,
                &free,
                MapOptions {
                    backend: MapBackend::CpuTask,
                    ..o
                },
            )
            .expect("map");
            assert_eq!(task.backend, "cpu");
            assert!(
                same_gates(&cpu, &task),
                "{v:?} seed {seed} recovery {fanout_recovery:?} exact {exact_passes}"
            );
        }
    }
}
