// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Tests of the cut-pass task on random problems: the CPU backend's duplicate and rank phases
//! against the kernel's quadratic loops, independence of the thread count, validation, and
//! word-identical results of every backend (COMPUTE-REQ-0104; the GPU test needs a GPU).

use std::collections::BTreeMap;

use super::cpu::unique_ranked;
use super::pack::SW;
use super::*;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn coin(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// An owned problem.
struct Owned {
    fanins: Vec<[u32; 2]>,
    inputs: Vec<u32>,
    level_ptr: Vec<u32>,
    level_nodes: Vec<u32>,
    init: Vec<[Choice; 2]>,
    key_vars: Vec<u8>,
    key_tt: Vec<u64>,
    key_first: Vec<u32>,
    m_cell: Vec<u32>,
    m_perm: Vec<[u8; 6]>,
    m_phase: Vec<u8>,
    c_inputs: Vec<u8>,
    c_area: Vec<u32>,
    c_delay: Vec<[i32; 6]>,
    max_leaves: u32,
    max_cuts: u32,
    inv_delay: i32,
    inv_area: u32,
}

impl Owned {
    fn view(&self) -> CutPassProblem<'_> {
        CutPassProblem {
            fanins: &self.fanins,
            inputs: &self.inputs,
            level_ptr: &self.level_ptr,
            level_nodes: &self.level_nodes,
            init: &self.init,
            max_leaves: self.max_leaves,
            max_cuts: self.max_cuts,
            inv_delay: self.inv_delay,
            inv_area: self.inv_area,
            lib: MatchTable {
                key_vars: &self.key_vars,
                key_tt: &self.key_tt,
                key_first: &self.key_first,
                m_cell: &self.m_cell,
                m_perm: &self.m_perm,
                m_phase: &self.m_phase,
                c_inputs: &self.c_inputs,
                c_area: &self.c_area,
                c_delay: &self.c_delay,
            },
        }
    }
}

fn permutations(n: usize) -> Vec<Vec<u8>> {
    fn rec(k: usize, cur: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
        if k == cur.len() {
            out.push(cur.clone());
            return;
        }
        for i in k..cur.len() {
            cur.swap(k, i);
            rec(k + 1, cur, out);
            cur.swap(k, i);
        }
    }
    let mut out = Vec::new();
    rec(0, &mut (0..n as u8).collect(), &mut out);
    out
}

/// A random library: common two-input cells and random functions of up to four inputs that
/// depend on every input, expanded into every permutation and input phase.
fn random_lib(rng: &mut Rng, o: &mut Owned) {
    let mut cells: Vec<(u8, u64)> = vec![
        (1, 0b01),
        (1, 0b10),
        (2, 0b1000),
        (2, 0b0111),
        (2, 0b1110),
        (2, 0b0001),
        (2, 0b0110),
        (2, 0b1001),
    ];
    for _ in 0..rng.below(8) {
        let k = 3 + rng.below(2) as u8;
        let mask = (1u64 << (1u32 << k)) - 1;
        let tt = rng.next() & mask;
        let depends_all = (0..k as usize).all(|i| {
            let s = 1u32 << i;
            let vm = [
                0xAAAA_AAAA_AAAA_AAAAu64,
                0xCCCC_CCCC_CCCC_CCCC,
                0xF0F0_F0F0_F0F0_F0F0,
                0xFF00_FF00_FF00_FF00,
            ][i];
            ((tt >> s) ^ tt) & !vm & mask != 0
        });
        if depends_all {
            cells.push((k, tt));
        }
    }
    let mut table: BTreeMap<(u8, u64), Vec<(u32, [u8; 6], u8)>> = BTreeMap::new();
    for (ci, &(k, ctt)) in cells.iter().enumerate() {
        o.c_inputs.push(k);
        o.c_area.push(100 + rng.below(20_000) as u32);
        let mut d = [0i32; 6];
        for x in d.iter_mut().take(k as usize) {
            *x = 1 + rng.below(90_000) as i32;
        }
        o.c_delay.push(d);
        for p in permutations(k as usize) {
            for phase in 0..(1u32 << k) {
                let mut tt = 0u64;
                for m in 0..(1u32 << k) {
                    let mut y = 0u32;
                    for (i, &leaf) in p.iter().enumerate() {
                        y |= (((m >> leaf) & 1) ^ ((phase >> i) & 1)) << i;
                    }
                    if (ctt >> y) & 1 == 1 {
                        tt |= 1 << m;
                    }
                }
                let mut perm = [0u8; 6];
                perm[..k as usize].copy_from_slice(&p);
                table
                    .entry((k, tt))
                    .or_default()
                    .push((ci as u32, perm, phase as u8));
            }
        }
    }
    o.key_first.push(0);
    for ((k, tt), ms) in table {
        o.key_vars.push(k);
        o.key_tt.push(tt);
        // A bounded number of matches per key, in a random but fixed order.
        let take = 1 + rng.below(6) as usize;
        let start = rng.below(ms.len() as u64) as usize;
        for j in 0..take.min(ms.len()) {
            let (c, perm, ph) = ms[(start + j) % ms.len()];
            o.m_cell.push(c);
            o.m_perm.push(perm);
            o.m_phase.push(ph);
        }
        o.key_first.push(o.m_cell.len() as u32);
    }
}

/// A random graph of `ands` AND nodes over `inputs` inputs with random initial choices.
fn random_problem(seed: u64, inputs: usize, ands: usize, max_cuts: u32, k: u32) -> Owned {
    let mut rng = Rng(seed);
    let n = 1 + inputs + ands;
    let mut o = Owned {
        fanins: vec![[0, 0]; n],
        inputs: (1..=inputs as u32).collect(),
        level_ptr: vec![0],
        level_nodes: Vec::new(),
        init: vec![[Choice::NONE; 2]; n],
        key_vars: Vec::new(),
        key_tt: Vec::new(),
        key_first: Vec::new(),
        m_cell: Vec::new(),
        m_perm: Vec::new(),
        m_phase: Vec::new(),
        c_inputs: Vec::new(),
        c_area: Vec::new(),
        c_delay: Vec::new(),
        max_leaves: k,
        max_cuts,
        inv_delay: 1 + rng.below(30_000) as i32,
        inv_area: 50 + rng.below(1_000) as u32,
    };
    random_lib(&mut rng, &mut o);
    let zero = Choice {
        imp: Imp::INPUT,
        arr: 0,
        af: 0,
    };
    o.init[0] = [zero, zero];
    for i in 1..=inputs {
        let arr = rng.below(300_000) as i32 - 50_000;
        let neg = if rng.coin(30) {
            Choice {
                imp: Imp::FREE_NEG,
                arr,
                af: 0,
            }
        } else {
            Choice {
                imp: Imp::INV,
                arr: arr + o.inv_delay,
                af: o.inv_area,
            }
        };
        o.init[i] = [
            Choice {
                imp: Imp::INPUT,
                arr,
                af: 0,
            },
            neg,
        ];
    }
    let mut level = vec![0u32; n];
    let mut levels: Vec<Vec<u32>> = Vec::new();
    for id in inputs + 1..n {
        let near = |rng: &mut Rng| id - 1 - rng.below((id - 1).min(40) as u64) as usize;
        let a = near(&mut rng);
        let mut b = 1 + rng.below(id as u64 - 1) as usize;
        if b == a {
            b = if a > 1 { a - 1 } else { a + 1 };
        }
        o.fanins[id] = [
            2 * a as u32 + u32::from(rng.coin(50)),
            2 * b as u32 + u32::from(rng.coin(50)),
        ];
        let l = 1 + level[a].max(level[b]);
        level[id] = l;
        if levels.len() < l as usize {
            levels.resize(l as usize, Vec::new());
        }
        levels[l as usize - 1].push(id as u32);
    }
    for l in levels {
        o.level_nodes.extend(l);
        o.level_ptr.push(o.level_nodes.len() as u32);
    }
    o
}

/// The costs of the passes of a run: a delay pass, area-flow passes with required times derived
/// from the previous choices (some phases outside the cover, some unconstrained), and a final
/// delay-ranked pass with required times.
fn pass_costs(
    rng: &mut Rng,
    n: usize,
    prev: &[[Choice; 2]],
    i: usize,
) -> (Rank, Vec<u32>, Option<Vec<[i32; 2]>>) {
    let fo2: Vec<u32> = (0..n).map(|_| rng.below(12) as u32).collect();
    if i == 0 {
        return (Rank::Delay, fo2, None);
    }
    let req = (0..n)
        .map(|id| {
            let mut r = [ARR_INF; 2];
            for p in 0..2 {
                let a = prev.get(id).map_or(ARR_INF, |c| c[p].arr);
                r[p] = match rng.below(10) {
                    0..=2 => ARR_INF,
                    3 => ARR_INF - 1,
                    _ if a == ARR_INF => ARR_INF - 1,
                    _ => a.saturating_add(rng.below(40_000) as i32 - 10_000),
                };
            }
            r
        })
        .collect();
    let rank = if i == 4 { Rank::Delay } else { Rank::AreaFlow };
    (rank, fo2, Some(req))
}

/// Outputs of every pass of a run on `b`.
fn run_all(b: &dyn CutPassBackend, o: &Owned, seed: u64, passes: usize) -> Vec<PassOutput> {
    let mut s = b
        .open(&o.view())
        .unwrap_or_else(|e| panic!("{}: {e}", b.name()));
    let mut rng = Rng(seed ^ 0xC0FF_EE00);
    let n = o.fanins.len();
    let mut outs: Vec<PassOutput> = Vec::new();
    for i in 0..passes {
        let prev = outs.last().map_or(o.init.clone(), |x| x.choices.clone());
        let (rank, fo2, req) = pass_costs(&mut rng, n, &prev, i);
        let rb = if i % 2 == 1 {
            Readback::All
        } else {
            Readback::Chosen
        };
        let mut out = PassOutput::default();
        s.run(
            &PassCosts {
                rank,
                fo2: &fo2,
                required: req.as_deref(),
            },
            rb,
            &mut out,
        )
        .unwrap_or_else(|e| panic!("{} pass {i}: {e}", b.name()));
        outs.push(out);
    }
    outs
}

fn same(a: &[PassOutput], b: &[PassOutput]) -> Result<(), String> {
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if x.choices != y.choices {
            let id = (0..x.choices.len()).find(|&j| x.choices[j] != y.choices[j]);
            return Err(format!("pass {i}: choices differ first at node {id:?}"));
        }
        if x.cut_count != y.cut_count {
            return Err(format!("pass {i}: cut counts differ"));
        }
        if x.rows != y.rows {
            let r = (0..x.rows.len()).find(|&j| x.rows[j] != y.rows[j]);
            return Err(format!("pass {i}: rows differ first at row {r:?}"));
        }
    }
    Ok(())
}

const CASES: [(u64, usize, usize, u32, u32); 8] = [
    (1, 8, 300, 1, 4),
    (2, 12, 800, 8, 6),
    (3, 20, 1500, 12, 6),
    (4, 6, 400, 40, 5),
    (5, 30, 2500, 20, 6),
    (6, 10, 600, 126, 6),
    (7, 16, 1200, 3, 2),
    (8, 24, 2000, 40, 6),
];

// COMPUTE-REQ-0105: the hash-and-sort phases equal the kernel's quadratic duplicate and rank
// loops.
#[test]
fn unique_and_rank_equal_quadratic_loops() {
    let mut rng = Rng(77);
    for round in 0..300 {
        let v = 1 + rng.below(400) as usize;
        let cand: Vec<[u32; SW]> = (0..v)
            .map(|p| {
                let mut x = [0u32; SW];
                x[0] = p as u32;
                let n = 1 + rng.below(3) as u32;
                x[1] = n;
                let mut l: Vec<u32> = (0..n).map(|_| rng.below(6) as u32).collect();
                l.sort_unstable();
                l.dedup();
                x[1] = l.len() as u32;
                x[2..2 + l.len()].copy_from_slice(&l);
                x[10] = rng.below(4) as u32;
                x[11] = rng.below(4) as u32;
                x
            })
            .collect();
        for area in [false, true] {
            let (order, _) = unique_ranked(&cand, area);
            // The kernel: flag of an earlier equal leaf set; rank by counting smaller candidates.
            let same_leaves = |a: &[u32; SW], b: &[u32; SW]| {
                a[1] == b[1] && a[2..2 + a[1] as usize] == b[2..2 + b[1] as usize]
            };
            let flag: Vec<bool> = (0..v)
                .map(|i| (0..i).any(|j| same_leaves(&cand[j], &cand[i])))
                .collect();
            let less = |x: &[u32; SW], y: &[u32; SW]| {
                let (xa, ya, xf, yf) = (x[10], y[10], x[11], y[11]);
                if area {
                    if xf != yf {
                        return xf < yf;
                    }
                    if x[1] != y[1] {
                        return x[1] < y[1];
                    }
                    if xa != ya {
                        return xa < ya;
                    }
                } else {
                    if xa != ya {
                        return xa < ya;
                    }
                    if x[1] != y[1] {
                        return x[1] < y[1];
                    }
                    if xf != yf {
                        return xf < yf;
                    }
                }
                x[0] < y[0]
            };
            let uniq: Vec<usize> = (0..v).filter(|&i| !flag[i]).collect();
            assert_eq!(order.len(), uniq.len(), "round {round}");
            for &i in &uniq {
                let r = uniq
                    .iter()
                    .filter(|&&j| j != i && less(&cand[j], &cand[i]))
                    .count();
                assert_eq!(order[r] as usize, i, "round {round} area {area}");
            }
        }
    }
}

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool")
}

// COMPUTE-REQ-0104, COMPUTE-REQ-0105: the CPU backend's results do not depend on the thread
// count.
#[test]
fn cpu_backend_is_thread_independent() {
    for &(seed, inputs, ands, c, k) in &CASES[..5] {
        let o = random_problem(seed, inputs, ands, c, k);
        let cpu = backend(BackendKind::Cpu).expect("cpu");
        let one = pool(1).install(|| run_all(cpu, &o, seed, 5));
        let many = pool(8).install(|| run_all(cpu, &o, seed, 5));
        same(&one, &many).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        // Every node of a level is mapped (the library has every two-input function).
        let last = &one[4];
        for &id in &o.level_nodes {
            assert!(last.choices[id as usize].iter().all(|c| c.arr != ARR_INF));
        }
    }
}

// COMPUTE-REQ-0101: malformed problems are rejected before any backend touches them.
#[test]
fn validation_rejects_malformed_problems() {
    let cpu = backend(BackendKind::Cpu).expect("cpu");
    let base = random_problem(9, 6, 50, 4, 4);
    assert!(cpu.open(&base.view()).is_ok());
    let check = |f: &dyn Fn(&mut Owned), want_too_large: bool| {
        let mut o = random_problem(9, 6, 50, 4, 4);
        f(&mut o);
        match cpu.open(&o.view()) {
            Err(ComputeError::TooLarge(_)) if want_too_large => {}
            Err(ComputeError::Invalid(_)) if !want_too_large => {}
            Err(e) => panic!("unexpected error {e}"),
            Ok(_) => panic!("accepted"),
        }
    };
    check(&|o| o.max_cuts = 127, true);
    check(&|o| o.max_cuts = 0, true);
    check(&|o| o.max_leaves = 7, false);
    check(&|o| o.fanins[20][0] = 2 * 49, false);
    check(&|o| o.fanins[20][1] = 0, false);
    check(&|o| o.level_nodes.swap(0, 40), false);
    check(&|o| o.init[3][0].imp = Imp::cell(0, 0), false);
    check(&|o| o.c_delay[0][0] = -1, false);
    check(&|o| o.m_perm[0][0] = 5, false);
    check(&|o| o.key_tt.swap(0, 1), false);
    let mut o = random_problem(9, 6, 50, 4, 4);
    o.inputs.push(70);
    assert!(matches!(cpu.open(&o.view()), Err(ComputeError::Invalid(_))));
    let mut s = cpu.open(&base.view()).expect("open");
    let fo2 = vec![2u32; 3];
    assert!(matches!(
        s.run(
            &PassCosts {
                rank: Rank::Delay,
                fo2: &fo2,
                required: None
            },
            Readback::Chosen,
            &mut PassOutput::default()
        ),
        Err(ComputeError::Invalid(_))
    ));
}

// COMPUTE-REQ-0004: a backend that the build lacks is an error, never a fallback.
#[test]
fn missing_backend_is_an_error() {
    if !GpuRuntime::Cuda.compiled() {
        assert!(matches!(
            backend(BackendKind::Cuda),
            Err(ComputeError::NotCompiled("cuda"))
        ));
        assert!(matches!(
            backend(BackendKind::Gpu),
            Err(ComputeError::Unavailable(_))
        ));
    }
}

// COMPUTE-REQ-0104, COMPUTE-REQ-0106: the CUDA backend returns the CPU backend's words for every
// pass. Needs a GPU and the NVRTC library; run with `--ignored` on a GPU machine.
#[test]
#[ignore]
fn cuda_equals_cpu_on_random_problems() {
    let cuda = backend(BackendKind::Cuda).unwrap_or_else(|e| panic!("{e}"));
    let cpu = backend(BackendKind::Cpu).expect("cpu");
    for &(seed, inputs, ands, c, k) in &CASES {
        for rep in 0..4u64 {
            let s = seed * 100 + rep;
            let o = random_problem(s, inputs, ands, c, k);
            let a = run_all(cpu, &o, s, 5);
            let b = run_all(cuda, &o, s, 5);
            same(&a, &b).unwrap_or_else(|e| panic!("seed {s} C {c} k {k}: {e}"));
        }
    }
}
