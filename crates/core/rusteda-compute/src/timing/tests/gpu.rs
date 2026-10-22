// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Tests of the CUDA timing backend (`--features cuda -- --include-ignored`).

use super::cpu::{differing_rows, patch};
use super::random::{self, Rng, Shape};
use super::{assert_same, cpu_values};
use crate::timing::cuda::{CUDA_F32, CUDA_F64};
use crate::timing::{
    Readback, Request, Scope, TimingPropagationBackend, TimingStateSoA, TimingValues, absent,
    prewarm, timing_backend,
};
use crate::{BackendKind, ComputeError, Precision};

// COMPUTE-REQ-0019, COMPUTE-REQ-0028: CUDA f64 (--fmad=false) equals the CPU backend bit for bit
// (arrivals, transitions, predecessors) on >= 100 random problems with tag maps and three-axis
// tables, and repeated runs give identical bits.
#[test]
#[ignore = "needs a CUDA GPU"]
fn cuda_f64_equals_cpu_on_100_random_problems() {
    for seed in 0..105 {
        let mut shape = Shape::from_seed(seed);
        shape.maps = true;
        shape.axes3 = true;
        let o = random::problem(seed, shape);
        let want = cpu_values(&o);
        let p = o.problem();
        let mut s = CUDA_F64
            .open(&p)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let st = s
            .propagate(&Request::all())
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_same(s.values(), &want, &format!("seed {seed}"));
        assert_eq!(st.backend, BackendKind::Cuda);
        assert_eq!(st.precision, Precision::F64);
        assert!(st.deterministic);
        if seed < 15 {
            let first = s.values().clone();
            s.propagate(&Request::all())
                .unwrap_or_else(|e| panic!("seed {seed} rerun: {e}"));
            assert_same(s.values(), &first, &format!("seed {seed} rerun"));
        }
    }
}

/// Checks that `got` (from `f32`) agrees with `want` (from `f64`) on absent entries and lies
/// within `max(1e-4, 2e-5 * |v|)` for every finite entry with `|v| <= 100.0`.
fn assert_f32_envelope(got: &TimingValues, want: &TimingValues, what: &str) {
    assert_eq!(got.nodes(), want.nodes(), "{what}: node count");
    assert_eq!(got.ntags, want.ntags, "{what}: tag count");
    for (k, (&g, &w)) in got.arr.iter().zip(&want.arr).enumerate() {
        let an = k % 2;
        if w == absent(an) || g == absent(an) {
            assert_eq!(
                g.to_bits(),
                w.to_bits(),
                "{what}: arrival {k} absent mismatch ({g} vs {w})"
            );
            continue;
        }
        assert!(
            g.is_finite() && w.is_finite(),
            "{what}: non-finite arrival {k}"
        );
        if w.abs() <= 100.0 {
            let tol = 1e-4_f64.max(2e-5 * w.abs());
            let diff = (g - w).abs();
            assert!(
                diff <= tol,
                "{what}: arrival {k}: got {g}, want {w}, diff {diff} > tol {tol}"
            );
        }
    }
    for (k, (&g, &w)) in got.slew.iter().zip(&want.slew).enumerate() {
        let an = k % 2;
        if w == absent(an) || g == absent(an) {
            assert_eq!(
                g.to_bits(),
                w.to_bits(),
                "{what}: transition {k} absent mismatch ({g} vs {w})"
            );
            continue;
        }
        assert!(
            g.is_finite() && w.is_finite(),
            "{what}: non-finite transition {k}"
        );
        if w.abs() <= 100.0 {
            let tol = 1e-4_f64.max(2e-5 * w.abs());
            let diff = (g - w).abs();
            assert!(
                diff <= tol,
                "{what}: transition {k}: got {g}, want {w}, diff {diff} > tol {tol}"
            );
        }
    }
}

// COMPUTE-REQ-0019, COMPUTE-REQ-0028: CUDA f32 agrees with f64 on absent entries, stays within
// max(1e-4, 2e-5 * |v|) for |v| <= 100 ns on problems without window-drop near-ties, and gives
// bit-identical results across repeated runs.
#[test]
#[ignore = "needs a CUDA GPU"]
fn cuda_f32_within_envelope_and_deterministic() {
    for seed in 0..40 {
        let mut shape = Shape::from_seed(seed);
        shape.continuous = true;
        let o = random::problem(seed, shape);
        let want = cpu_values(&o);
        let p = o.problem();
        let mut s = CUDA_F32
            .open(&p)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let st = s
            .propagate(&Request::all())
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(st.backend, BackendKind::Cuda);
        assert_eq!(st.precision, Precision::F32);
        assert!(st.deterministic);
        let first = s.values().clone();
        assert_f32_envelope(&first, &want, &format!("seed {seed}"));
        s.propagate(&Request::all())
            .unwrap_or_else(|e| panic!("seed {seed} rerun: {e}"));
        assert_same(s.values(), &first, &format!("seed {seed} f32 rerun"));
    }
}

// COMPUTE-REQ-0021, COMPUTE-REQ-0028: on CUDA f64, after patches of loads, arc classes and start
// rows (and after write_values), Cone and FromLevel equal a fresh full propagation bit for bit,
// and Cone lists the exact rows that changed.
#[test]
#[ignore = "needs a CUDA GPU"]
fn cuda_cone_and_from_level_equal_fresh_run() {
    for seed in 300..325 {
        let base = random::problem(seed, Shape::from_seed(seed));
        let mut cur = base.clone();
        let mut r = Rng::new(seed ^ 0xC0DE);
        let p = base.problem();
        let mut s = CUDA_F64.open(&p).unwrap_or_else(|e| panic!("{e}"));
        s.propagate(&Request::all())
            .unwrap_or_else(|e| panic!("{e}"));
        for round in 0..4 {
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
    for seed in 400..410 {
        let base = random::problem(seed, Shape::from_seed(seed));
        let old = cpu_values(&base);
        let mut cur = base.clone();
        let mut r = Rng::new(seed);
        let p = base.problem();
        let mut scratch = CUDA_F64.open(&p).unwrap_or_else(|e| panic!("{e}"));
        let mut nodes = patch(&mut r, &mut cur, scratch.as_mut());
        nodes.sort_unstable();
        nodes.dedup();
        let q = cur.problem();
        let mut s = CUDA_F64.open(&q).unwrap_or_else(|e| panic!("{e}"));
        s.write_values(&old).unwrap_or_else(|e| panic!("{e}"));
        s.propagate(&Request::cone(&nodes))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_same(s.values(), &cpu_values(&cur), &format!("seed {seed}"));
    }
}

// COMPUTE-REQ-0022, COMPUTE-REQ-0023, COMPUTE-REQ-0024, COMPUTE-REQ-0028, COMPUTE-REQ-0029:
// prewarm, backend selection, propagate_levels, Readback::None, Readback::Rows and
// Readback::Changed on CUDA, and PropagationStats fields.
#[test]
#[ignore = "needs a CUDA GPU"]
fn cuda_readback_and_single_call() {
    prewarm(BackendKind::Cuda, Precision::F64).unwrap_or_else(|e| panic!("{e}"));
    prewarm(BackendKind::Gpu, Precision::F32).unwrap_or_else(|e| panic!("{e}"));
    let b = timing_backend(BackendKind::Gpu, Precision::F64).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        (b.kind(), b.precision()),
        (BackendKind::Cuda, Precision::F64)
    );

    for seed in 500..508 {
        let o = random::problem(seed, Shape::from_seed(seed));
        let mut state = TimingStateSoA::new(o.ntags, o.load.clone(), o.starts.clone());
        let mut ideal = o.clone();
        ideal.wires.clear();
        let st = CUDA_F64
            .propagate_levels(&o.graph, &o.luts, &mut state, true)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_same(
            &state.values(),
            &cpu_values(&ideal),
            &format!("seed {seed}"),
        );
        assert_eq!(st.backend, BackendKind::Cuda);
        assert_eq!(st.precision, Precision::F64);
        assert!(st.device.contains("sm_"), "device: {}", st.device);
        let non_empty_levels = o.graph.level_ptr.windows(2).filter(|w| w[1] > w[0]).count() as u32;
        assert_eq!(st.launches, non_empty_levels);
        assert!(st.total_s >= st.kernel_s && st.kernel_s >= 0.0);

        let want = cpu_values(&o);
        let p = o.problem();
        let mut s = CUDA_F64.open(&p).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            s.read(Readback::All),
            Err(ComputeError::Invalid(_))
        ));
        // Propagate with Readback::None, then read subsets of rows and finally All.
        s.propagate(&Request::all().with_readback(Readback::None))
            .unwrap_or_else(|e| panic!("{e}"));
        let n = o.nodes();
        let rows: Vec<u32> = (0..n).step_by(2).map(|v| v as u32).collect();
        s.read(Readback::Rows(&rows))
            .unwrap_or_else(|e| panic!("{e}"));
        let w = o.ntags as usize * 4;
        for &v in &rows {
            let v = v as usize;
            assert_eq!(
                &s.values().arr[v * w..(v + 1) * w],
                &want.arr[v * w..(v + 1) * w]
            );
            assert_eq!(
                &s.values().pred[v * w..(v + 1) * w],
                &want.pred[v * w..(v + 1) * w]
            );
            assert_eq!(
                &s.values().slew[v * 4..v * 4 + 4],
                &want.slew[v * 4..v * 4 + 4]
            );
        }
        s.read(Readback::All).unwrap_or_else(|e| panic!("{e}"));
        assert_same(s.values(), &want, &format!("seed {seed} read all"));

        // Propagate with Readback::Changed on a cone after a load patch.
        let mut cur = o.clone();
        let c = [0.09, 0.10, 0.09, 0.11];
        cur.load[0] = c;
        s.set_loads(&[(0, c)]).unwrap_or_else(|e| panic!("{e}"));
        s.propagate(&Request::cone(&[0]).with_readback(Readback::Changed))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_same(
            s.values(),
            &cpu_values(&cur),
            &format!("seed {seed} read changed"),
        );
    }
}
