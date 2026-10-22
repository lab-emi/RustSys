// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The CPU backend on random problems: the literal evaluator, thread counts, sessions, patches,
//! cones, readback, the single-call form, cancellation, progress and statistics.

use std::borrow::Cow;
use std::sync::Mutex;

use rusteda_core::job::{CancelToken, Ctx, Event, Observer};

use super::random::{self, Owned, Rng, Shape};
use super::{assert_same, cpu_values, naive, pool};
use crate::timing::{
    CPU, NONE, Readback, Request, Scope, StartRows, TimingPropagationBackend, TimingSession,
    TimingStateSoA, TimingValues, absent, timing_backend,
};
use crate::{BackendKind, ComputeError, Precision};

// COMPUTE-REQ-0014, COMPUTE-REQ-0016, COMPUTE-REQ-0017, COMPUTE-REQ-0018: the CPU backend equals
// a literal sequential reading of the rules bit for bit (arrivals, transitions, predecessors) on
// random problems with tag maps, three-axis tables, wire records and start rows.
#[test]
fn cpu_equals_the_literal_evaluator() {
    for seed in 0..80 {
        let o = random::problem(seed, Shape::from_seed(seed));
        let want = naive::evaluate(&o.problem());
        assert_same(&cpu_values(&o), &want, &format!("seed {seed}"));
    }
}

// COMPUTE-REQ-0019, COMPUTE-REQ-0027: 1, 3, 8 and 32 threads give the same bits.
#[test]
fn thread_counts_give_the_same_bits() {
    let pools: Vec<(usize, rayon::ThreadPool)> =
        [1, 3, 8, 32].into_iter().map(|t| (t, pool(t))).collect();
    for seed in 200..230 {
        let mut shape = Shape::from_seed(seed);
        shape.width = 120 + (seed as usize % 7) * 60;
        let o = random::problem(seed, shape);
        let base = pools[0].1.install(|| cpu_values(&o));
        for (t, p) in &pools[1..] {
            let v = p.install(|| cpu_values(&o));
            assert_same(&v, &base, &format!("seed {seed}, {t} threads"));
        }
    }
}

/// Changes a few loads, arc classes and start rows of `cur` and of the session `s`; returns the
/// patched nodes.
pub(super) fn patch(r: &mut Rng, cur: &mut Owned, s: &mut dyn TimingSession) -> Vec<u32> {
    let n = cur.nodes();
    let mut nodes = Vec::new();
    let mut loads = Vec::new();
    for _ in 0..r.below(4) {
        let v = r.below(n) as u32;
        let c = r.range(0.001, 0.12);
        let l = if r.chance(0.2) {
            cur.load[v as usize]
        } else {
            [c, c * 1.1, c, c * 1.2]
        };
        cur.load[v as usize] = l;
        loads.push((v, l));
        nodes.push(v);
    }
    s.set_loads(&loads).unwrap_or_else(|e| panic!("{e}"));
    let na = cur.graph.arcs();
    let nc = cur.luts.classes.len();
    let mut classes = Vec::new();
    if na > 0 {
        for _ in 0..r.below(4) {
            let k = r.below(na);
            let keep_wire = cur.wires.get(k).is_some_and(|w| !w.is_zero());
            let c = if keep_wire { 0 } else { r.below(nc) as u32 };
            cur.graph.in_arcs.to_mut()[k].class = c;
            classes.push((k as u32, c));
            let v = cur.graph.in_ptr.partition_point(|&p| p as usize <= k) - 1;
            nodes.push(v as u32);
        }
    }
    s.set_arc_classes(&classes)
        .unwrap_or_else(|e| panic!("{e}"));
    if r.chance(0.6) {
        let nt = cur.ntags as usize;
        let old = cur.starts.clone();
        let mut new = StartRows::default();
        let extra = r.below(n) as u32;
        for k in 0..old.len() {
            let v = old.node[k];
            if extra < v && new.node.last().is_none_or(|&l| l < extra) && old.find(extra).is_none()
            {
                new.push(extra, &vec![1.5; nt * 4], &[0.25; 4]);
            }
            if r.chance(0.1) {
                continue;
            }
            let (a, sl) = old.row(k, cur.ntags);
            let mut a = a.to_vec();
            if r.chance(0.2) {
                let i = r.below(a.len());
                if a[i] != absent(i % 2) {
                    a[i] += 0.25;
                }
            }
            new.push(v, &a, &[sl[0], sl[1], sl[2], sl[3]]);
        }
        let mut changed = Vec::new();
        crate::timing::cpu::diff_starts(&old, &new, cur.ntags, &mut changed);
        nodes.extend(changed);
        s.set_starts(&new).unwrap_or_else(|e| panic!("{e}"));
        cur.starts = new;
    }
    nodes
}

/// The nodes whose rows differ between two value sets.
pub(super) fn differing_rows(a: &TimingValues, b: &TimingValues) -> Vec<u32> {
    let w = a.ntags as usize * 4;
    (0..a.nodes())
        .filter(|&v| {
            let r = v * w..(v + 1) * w;
            a.arr[r.clone()]
                .iter()
                .zip(&b.arr[r.clone()])
                .any(|(x, y)| x.to_bits() != y.to_bits())
                || a.pred[r.clone()] != b.pred[r]
                || a.slew[v * 4..v * 4 + 4]
                    .iter()
                    .zip(&b.slew[v * 4..v * 4 + 4])
                    .any(|(x, y)| x.to_bits() != y.to_bits())
        })
        .map(|v| v as u32)
        .collect()
}

// COMPUTE-REQ-0021: after patches of loads, arc classes and start rows, a cone propagation from
// random seeds equals a fresh full propagation of the patched problem bit for bit, and lists
// exactly the rows that changed; FromLevel does the same.
#[test]
fn cone_and_from_level_equal_a_fresh_run() {
    for seed in 300..360 {
        let base = random::problem(seed, Shape::from_seed(seed));
        let mut cur = base.clone();
        let mut r = Rng::new(seed ^ 0xC0DE);
        let p = base.problem();
        let mut s = CPU.open(&p).unwrap_or_else(|e| panic!("{e}"));
        s.propagate(&Request::all())
            .unwrap_or_else(|e| panic!("{e}"));
        for round in 0..6 {
            let before = s.values().clone();
            patch(&mut r, &mut cur, s.as_mut());
            let seeds: Vec<u32> = (0..r.below(3))
                .map(|_| r.below(cur.nodes()) as u32)
                .collect();
            let from_level = round % 3 == 2;
            let st = if from_level {
                let l = r.below(cur.graph.levels() + 1) as u32;
                s.propagate(&Request::all().with_scope(Scope::FromLevel(l)))
            } else {
                s.propagate(&Request::cone(&seeds))
            }
            .unwrap_or_else(|e| panic!("seed {seed} round {round}: {e}"));
            let fresh = cpu_values(&cur);
            assert_same(s.values(), &fresh, &format!("seed {seed} round {round}"));
            if !from_level {
                let changed = differing_rows(&before, &fresh);
                assert_eq!(s.changed(), Some(&changed[..]), "seed {seed} round {round}");
                assert_eq!(st.nodes_changed, changed.len() as u64);
                assert!(st.nodes_evaluated >= st.nodes_changed);
            } else {
                assert_eq!(s.changed(), None);
            }
        }
    }
}

// COMPUTE-REQ-0021: values written into a session on a patched problem, followed by a cone of the
// patched nodes, equal a fresh run; a cone with nothing patched changes nothing.
#[test]
fn written_values_restart_a_cone() {
    for seed in 400..440 {
        let base = random::problem(seed, Shape::from_seed(seed));
        let old = cpu_values(&base);
        let mut cur = base.clone();
        let mut r = Rng::new(seed);
        // Patch a scratch session to learn the patched nodes, then open one on the patched
        // problem.
        let p = base.problem();
        let mut scratch = CPU.open(&p).unwrap_or_else(|e| panic!("{e}"));
        let mut nodes = patch(&mut r, &mut cur, scratch.as_mut());
        nodes.sort_unstable();
        nodes.dedup();
        let q = cur.problem();
        let mut s = CPU.open(&q).unwrap_or_else(|e| panic!("{e}"));
        s.write_values(&old).unwrap_or_else(|e| panic!("{e}"));
        s.propagate(&Request::cone(&nodes))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_same(s.values(), &cpu_values(&cur), &format!("seed {seed}"));
        let st = s
            .propagate(&Request::cone(&[]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((st.nodes_evaluated, st.nodes_changed), (0, 0));
        assert_eq!(s.changed(), Some(&[][..]));
    }
}

// COMPUTE-REQ-0022, COMPUTE-REQ-0023: the single-call form fills the state with the values of a
// session; the CPU host view is complete, reads of listed rows succeed and rows out of range are
// rejected.
#[test]
fn single_call_and_readback() {
    for seed in 500..510 {
        let o = random::problem(seed, Shape::from_seed(seed));
        let mut state = TimingStateSoA::new(o.ntags, o.load.clone(), o.starts.clone());
        let mut ideal = o.clone();
        ideal.wires.clear();
        let st = CPU
            .propagate_levels(&o.graph, &o.luts, &mut state, true)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_same(
            &state.values(),
            &cpu_values(&ideal),
            &format!("seed {seed}"),
        );
        assert_eq!(st.backend, BackendKind::Cpu);
        assert_eq!(st.precision, Precision::F64);
        let p = o.problem();
        let mut s = CPU.open(&p).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            s.read(Readback::All),
            Err(ComputeError::Invalid(_))
        ));
        let rows = [0u32, (o.nodes() - 1) as u32];
        s.propagate(&Request::all().with_readback(Readback::Rows(&rows)))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_same(s.values(), &cpu_values(&o), "rows");
        assert!(s.read(Readback::Rows(&rows)).is_ok());
        assert!(s.read(Readback::Changed).is_ok());
        let out = [o.nodes() as u32];
        assert!(matches!(
            s.read(Readback::Rows(&out)),
            Err(ComputeError::Invalid(_))
        ));
        assert!(matches!(
            s.propagate(&Request::cone(&out)),
            Err(ComputeError::Invalid(_))
        ));
    }
}

// COMPUTE-REQ-0024: the statistics of the CPU backend.
#[test]
fn statistics() {
    let o = random::problem(7, Shape::from_seed(7));
    let p = o.problem();
    let mut s = CPU.open(&p).unwrap_or_else(|e| panic!("{e}"));
    let st = s
        .propagate(&Request::all())
        .unwrap_or_else(|e| panic!("{e}"));
    let v = s.values();
    let mx = v
        .arr
        .iter()
        .chain(&v.slew)
        .filter(|x| x.is_finite())
        .fold(0.0f64, |m, x| m.max(x.abs()));
    assert_eq!(st.max_abs_ns, mx);
    assert!(mx > 0.0);
    assert_eq!(st.levels as usize, o.graph.levels());
    assert_eq!(st.nodes_evaluated as usize, o.nodes());
    assert_eq!(st.nodes_changed, st.nodes_evaluated);
    assert_eq!(st.launches, 0);
    assert!(st.deterministic);
    assert!(st.total_s >= st.kernel_s && st.kernel_s >= 0.0);
}

/// Records the progress events of a run.
#[derive(Default)]
struct Progress(Mutex<Vec<(u64, u64)>>);

impl Observer for Progress {
    fn event(&self, e: &Event) {
        if let Event::Progress {
            phase: "propagate",
            done,
            total,
        } = e
            && let Ok(mut v) = self.0.lock()
        {
            v.push((*done, *total));
        }
    }
}

// COMPUTE-REQ-0026: a cancelled propagation stops with Cancelled and leaves no resident values,
// so that a cone is rejected until a full propagation; progress is reported per level.
#[test]
fn cancellation_and_progress() {
    let mut shape = Shape::from_seed(11);
    shape.levels = 12;
    let o = random::problem(11, shape);
    let p = o.problem();
    let mut s = CPU.open(&p).unwrap_or_else(|e| panic!("{e}"));
    let obs = Progress::default();
    let ctx = Ctx::new(None, Some(&obs));
    s.propagate(&Request::all().with_ctx(ctx))
        .unwrap_or_else(|e| panic!("{e}"));
    let seen = obs.0.lock().map(|v| v.clone()).unwrap_or_default();
    let levels = o.graph.levels() as u64;
    assert_eq!(seen.last(), Some(&(levels, levels)));
    let token = CancelToken::new();
    token.cancel();
    let ctx = Ctx::new(Some(&token), None);
    assert_eq!(
        s.propagate(&Request::all().with_ctx(ctx)).map(|_| ()),
        Err(ComputeError::Cancelled)
    );
    assert!(matches!(
        s.propagate(&Request::cone(&[0])),
        Err(ComputeError::Invalid(_))
    ));
    assert!(matches!(
        s.propagate(&Request::all().with_scope(Scope::FromLevel(1))),
        Err(ComputeError::Invalid(_))
    ));
    s.propagate(&Request::all())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_same(s.values(), &cpu_values(&o), "after the cancellation");
}

// COMPUTE-REQ-0020, COMPUTE-REQ-0021: patches are validated before they apply, and a rejected
// patch changes nothing.
#[test]
fn patches_are_validated() {
    let mut shape = Shape::from_seed(13);
    shape.wires = true;
    let o = random::problem(13, shape);
    let p = o.problem();
    let mut s = CPU.open(&p).unwrap_or_else(|e| panic!("{e}"));
    s.propagate(&Request::all())
        .unwrap_or_else(|e| panic!("{e}"));
    let n = o.nodes() as u32;
    let inv = |r: Result<(), ComputeError>| matches!(r, Err(ComputeError::Invalid(_)));
    assert!(inv(s.set_loads(&[(0, [0.1; 4]), (n, [0.1; 4])])));
    assert!(inv(s.set_loads(&[(0, [f64::NAN, 0.0, 0.0, 0.0])])));
    assert!(inv(s.set_loads(&[(0, [f64::INFINITY, 0.0, 0.0, 0.0])])));
    let na = o.graph.arcs() as u32;
    let nc = o.luts.classes.len() as u32;
    assert!(inv(s.set_arc_classes(&[(na, 0)])));
    assert!(inv(s.set_arc_classes(&[(0, nc)])));
    if let Some(k) = o.wires.iter().position(|w| !w.is_zero()) {
        assert!(inv(s.set_arc_classes(&[(k as u32, 1)])));
    }
    let mut bad = o.starts.clone();
    bad.node.reverse();
    if bad.len() > 1 {
        assert!(inv(s.set_starts(&bad)));
    }
    assert!(inv(s.write_values(&TimingValues::absent(1, o.ntags))));
    let mut nan = cpu_values(&o);
    nan.arr[0] = f64::NAN;
    assert!(inv(s.write_values(&nan)));
    // Nothing applied: a cone changes nothing.
    s.propagate(&Request::cone(&[]))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(s.changed(), Some(&[][..]));
}

// COMPUTE-REQ-0010: the CPU backend is f64 only; GPU backends of a build without them are not
// compiled; selection never falls back.
#[test]
fn backend_selection() {
    let b = timing_backend(BackendKind::Cpu, Precision::F64).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        (b.kind(), b.precision()),
        (BackendKind::Cpu, Precision::F64)
    );
    assert!(matches!(
        timing_backend(BackendKind::Cpu, Precision::F32),
        Err(ComputeError::Unavailable(_))
    ));
    if !cfg!(feature = "cuda") {
        for k in [BackendKind::Cuda, BackendKind::Gpu] {
            for p in Precision::ALL {
                assert_eq!(
                    timing_backend(k, p).map(|b| b.kind()),
                    Err(ComputeError::NotCompiled(k.name()))
                );
                assert_eq!(
                    crate::timing::prewarm(k, p),
                    Err(ComputeError::NotCompiled(k.name()))
                );
            }
        }
    }
    assert_eq!(
        crate::timing::prewarm(BackendKind::Cpu, Precision::F64),
        Ok(())
    );
}

// COMPUTE-REQ-0011, COMPUTE-REQ-0021: sessions borrow the problem and copy only patched arrays,
// so that the problem a session was opened on is never changed.
#[test]
fn sessions_leave_the_problem_unchanged() {
    let o = random::problem(17, Shape::from_seed(17));
    let copy = o.clone();
    {
        let p = o.problem();
        let mut s = CPU.open(&p).unwrap_or_else(|e| panic!("{e}"));
        let k = (0..o.graph.arcs())
            .find(|&k| o.graph.in_arcs[k].class != 0 && o.wires.get(k).is_none_or(|w| w.is_zero()));
        if let Some(k) = k {
            s.set_arc_classes(&[(k as u32, 1)])
                .unwrap_or_else(|e| panic!("{e}"));
        }
        s.set_loads(&[(0, [0.5; 4])])
            .unwrap_or_else(|e| panic!("{e}"));
        s.set_starts(&StartRows::default())
            .unwrap_or_else(|e| panic!("{e}"));
        s.propagate(&Request::all())
            .unwrap_or_else(|e| panic!("{e}"));
    }
    assert!(matches!(o.graph.in_arcs, Cow::Owned(_)));
    assert_eq!(o.graph, copy.graph);
    assert_eq!(o.load, copy.load);
    assert_eq!(o.starts, copy.starts);
    assert_eq!(NONE, u32::MAX);
}

// COMPUTE-REQ-0017, COMPUTE-REQ-0018: the random problems of these tests exercise every rule:
// mapped arrivals dropped below the opening edge and limited by the setup time and by the limit,
// degraded wire transitions, clamped transitions, negative delays, three-axis tables, ties with
// start values and between arcs, and transitions set by untagged sources.
#[test]
fn random_problems_exercise_every_rule() {
    let c = naive::Counts::default();
    for seed in 0..80 {
        let o = random::problem(seed, Shape::from_seed(seed));
        naive::evaluate_counted(&o.problem(), &c);
    }
    let counts = [
        ("present arrivals", c.present_arrivals.get()),
        ("mapped candidates", c.mapped_candidates.get()),
        ("drops below the opening edge", c.open_drops.get()),
        ("setup-time limits", c.setup_clamps.get()),
        ("limits", c.limit_clamps.get()),
        ("degraded transitions", c.degraded.get()),
        ("clamped transitions", c.transition_clamps.get()),
        ("negative delays", c.negative_delays.get()),
        ("three-axis lookups", c.three_axis_lookups.get()),
        ("ties with a start value", c.start_ties.get()),
        ("ties between arcs", c.arc_ties.get()),
        (
            "transitions of untagged sources",
            c.untagged_transitions.get(),
        ),
    ];
    for (what, n) in counts {
        assert!(n > 0, "no {what}: {counts:?}");
    }
}
