// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! COMPUTE-REQ-0030: bounded three-node prepared-arc ABI and frozen-input checks.
//! Additional state <4 KiB/case, two test workers; no path enumeration.
use crate::timing::*;
use crate::{BackendKind, Precision};
use std::borrow::Cow;

fn run(kind: BackendKind) -> TimingValues {
    let in_ptr = vec![0, 0, 1, 2];
    let arcs = vec![
        InArc {
            src: 0,
            class: 0,
            map: NONE,
        },
        InArc {
            src: 1,
            class: 1,
            map: NONE,
        },
    ];
    let (out_ptr, out_dst) = out_adjacency(3, &in_ptr, &arcs);
    let graph = LevelizedTimingCsr {
        level_ptr: Cow::Owned(vec![0, 1, 2, 3]),
        in_ptr: Cow::Owned(in_ptr),
        in_arcs: Cow::Owned(arcs),
        out_ptr: Cow::Owned(out_ptr),
        out_dst: Cow::Owned(out_dst),
        map_ptr: Cow::Borrowed(&[0]),
        maps: Cow::Borrowed(&[]),
    };
    let bank = CompiledLutBank {
        luts: Cow::Owned(vec![LutDesc::scalar(0), LutDesc::scalar(1)]),
        data: Cow::Owned(vec![100.0, 100.0]),
        classes: Cow::Owned(vec![
            ArcClass::cell(POSITIVE_UNATE, [0; 2], [1; 2]),
            ArcClass::wire(),
        ]),
    };
    let mut starts = StartRows::default();
    starts.push(0, &[0.0; 4], &[0.1; 4]);
    let values = [
        ArcValues {
            delay: [0.2; 8],
            slew: [0.15; 8],
        },
        ArcValues {
            delay: [0.03; 8],
            slew: [0.17; 8],
        },
    ];
    let loads = [[0.0; 4]; 3];
    let problem = TimingProblem {
        graph: &graph,
        luts: &bank,
        ntags: 1,
        load: &loads,
        starts: &starts,
        wires: &[],
        arc_values: &values,
    };
    problem.validate().unwrap();
    let backend = timing_backend(kind, Precision::F64).unwrap();
    let mut session = backend.open(&problem).unwrap();
    session.propagate(&Request::all()).unwrap();
    assert!(session.set_loads(&[(1, [1.0; 4])]).is_err());
    assert!(session.set_starts(&starts).is_err());
    assert!(session.set_arc_classes(&[(0, 0)]).is_err());
    let result = session.into_values();
    assert!((result.arrival(2, 0, 0, 1) - 0.23).abs() < 1e-15);
    assert_eq!(result.transition(2, 0, 1), 0.17);
    assert!(
        TimingProblem {
            arc_values: &values[..1],
            ..problem
        }
        .validate()
        .is_err()
    );
    let mut invalid = values;
    invalid[0].slew[0] = f64::NAN;
    assert!(
        TimingProblem {
            arc_values: &invalid,
            ..problem
        }
        .validate()
        .is_err()
    );
    result
}

#[test]
fn prepared_values_and_frozen_inputs() {
    run(BackendKind::Cpu);
}

#[test]
#[cfg(feature = "cuda")]
#[ignore = "requires native CUDA, execute in a physical-memory cgroup"]
fn prepared_cpu_cuda_agree() {
    super::assert_same(
        &run(BackendKind::Cpu),
        &run(BackendKind::Cuda),
        "prepared RC arc f64",
    );
}
