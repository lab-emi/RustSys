// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Hand-derived values of small problems. Every table is linear in its arguments and every
//! input is a dyadic fraction, so that each expected value below is exact and derived from the
//! rules alone; the CPU backend and the literal evaluator must both give these bits.

use std::borrow::Cow;

use super::random::Owned;
use super::{assert_same, cpu_values, naive};
use crate::timing::{
    ArcClass, CompiledLutBank, EARLY, FALL, InArc, LATE, LevelizedTimingCsr, LutDesc,
    NEGATIVE_UNATE, NO_PRED, NONE, POSITIVE_UNATE, RISE, StartRows, TagMapRec, TimingValues, WIRE,
    WireRec, absent, arr_index, out_adjacency, pred_source, slew_index,
};

/// A problem built level by level.
struct Build {
    level_ptr: Vec<u32>,
    in_ptr: Vec<u32>,
    arcs: Vec<InArc>,
    map_ptr: Vec<u32>,
    maps: Vec<TagMapRec>,
    luts: Vec<LutDesc>,
    data: Vec<f64>,
    classes: Vec<ArcClass>,
    ntags: u32,
    load: Vec<[f64; 4]>,
    starts: StartRows,
    wires: Vec<WireRec>,
}

impl Build {
    fn new(ntags: u32) -> Build {
        Build {
            level_ptr: vec![0],
            in_ptr: vec![0],
            arcs: Vec::new(),
            map_ptr: Vec::new(),
            maps: Vec::new(),
            luts: Vec::new(),
            data: Vec::new(),
            classes: vec![ArcClass::wire()],
            ntags,
            load: Vec::new(),
            starts: StartRows::default(),
            wires: Vec::new(),
        }
    }

    /// A table on `axes` (argument, points) with row-major `values`.
    fn lut(&mut self, axes: &[(u32, &[f64])], values: &[f64]) -> u32 {
        let mut d = LutDesc::default();
        for (k, (arg, pts)) in axes.iter().enumerate() {
            d.n[k] = pts.len() as u32;
            d.arg[k] = *arg;
            d.off[k] = self.data.len() as u32;
            self.data.extend_from_slice(pts);
        }
        d.voff = self.data.len() as u32;
        assert_eq!(values.len(), d.values());
        self.data.extend_from_slice(values);
        self.luts.push(d);
        (self.luts.len() - 1) as u32
    }

    fn class(&mut self, c: ArcClass) -> u32 {
        self.classes.push(c);
        (self.classes.len() - 1) as u32
    }

    fn map(&mut self, recs: &[TagMapRec]) -> u32 {
        if self.map_ptr.is_empty() {
            self.map_ptr.push(0);
        }
        self.maps.extend_from_slice(recs);
        self.map_ptr.push(self.maps.len() as u32);
        (self.map_ptr.len() - 2) as u32
    }

    /// A level whose nodes have these incoming arcs and loads; returns the first node.
    fn level(&mut self, nodes: &[(&[InArc], [f64; 4])]) -> u32 {
        let first = self.load.len() as u32;
        for (arcs, load) in nodes {
            self.arcs.extend_from_slice(arcs);
            self.in_ptr.push(self.arcs.len() as u32);
            self.load.push(*load);
        }
        self.level_ptr.push(self.load.len() as u32);
        first
    }

    fn owned(self) -> Owned {
        let n = self.load.len();
        let (out_ptr, out_dst) = out_adjacency(n, &self.in_ptr, &self.arcs);
        Owned {
            graph: LevelizedTimingCsr {
                level_ptr: Cow::Owned(self.level_ptr),
                in_ptr: Cow::Owned(self.in_ptr),
                in_arcs: Cow::Owned(self.arcs),
                out_ptr: Cow::Owned(out_ptr),
                out_dst: Cow::Owned(out_dst),
                map_ptr: Cow::Owned(self.map_ptr),
                maps: Cow::Owned(self.maps),
            },
            luts: CompiledLutBank {
                luts: Cow::Owned(self.luts),
                data: Cow::Owned(self.data),
                classes: Cow::Owned(self.classes),
            },
            ntags: self.ntags,
            load: self.load,
            starts: self.starts,
            wires: self.wires,
        }
    }
}

fn arc(src: u32, class: u32) -> InArc {
    InArc {
        src,
        class,
        map: NONE,
    }
}

const NO_ARCS: &[InArc] = &[];
const ABSENT_ROW: [f64; 4] = [
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

/// One tag's arrivals `[rise early, rise late, fall early, fall late]`.
fn row(x: [f64; 4]) -> Vec<f64> {
    x.to_vec()
}

/// Runs both evaluators and checks that they agree.
fn run(o: &Owned) -> TimingValues {
    o.problem().validate().unwrap_or_else(|e| panic!("{e}"));
    let v = cpu_values(o);
    assert_same(&v, &naive::evaluate(&o.problem()), "literal evaluator");
    v
}

fn arr(v: &TimingValues, node: usize, t: usize, tr: usize, an: usize) -> f64 {
    v.arr[arr_index(node, t, tr, an, v.ntags as usize)]
}

fn pred(v: &TimingValues, node: usize, t: usize, tr: usize, an: usize) -> u32 {
    v.pred[arr_index(node, t, tr, an, v.ntags as usize)]
}

fn slew(v: &TimingValues, node: usize, tr: usize, an: usize) -> f64 {
    v.slew[slew_index(node, tr, an)]
}

/// The delay table `0.125 + 0.5 s` and the transition table `0.0625 + 0.25 s + 0.5 L`.
fn tables(b: &mut Build) -> (u32, u32) {
    let d = b.lut(&[(0, &[0.0, 1.0])], &[0.125, 0.625]);
    let s = b.lut(
        &[(0, &[0.0, 1.0]), (1, &[0.0, 2.0])],
        &[0.0625, 1.0625, 0.3125, 1.3125],
    );
    (d, s)
}

// COMPUTE-REQ-0014, COMPUTE-REQ-0016, COMPUTE-REQ-0017: a wire, a positive-unate and a
// negative-unate arc in a chain; delays and transitions are evaluated at the source pin's
// transition and the node's load, per transition and analysis.
#[test]
fn chain_of_unate_arcs() {
    let mut b = Build::new(1);
    let (d, s) = tables(&mut b);
    let pos = b.class(ArcClass::cell(POSITIVE_UNATE, [d, d], [s, s]));
    let neg = b.class(ArcClass::cell(NEGATIVE_UNATE, [d, d], [s, s]));
    b.level(&[(NO_ARCS, [0.0; 4])]);
    b.starts
        .push(0, &row([1.0, 2.0, 1.5, 2.5]), &[0.25, 0.5, 0.5, 0.75]);
    b.level(&[(&[arc(0, 0)], [0.0; 4])]);
    b.level(&[(&[arc(1, pos)], [0.5; 4])]);
    b.level(&[(&[arc(2, neg)], [1.0; 4])]);
    let v = run(&b.owned());
    // Node 1: the wire passes everything; predecessor node 0 with the same transition.
    for (tr, an, a, s) in [
        (RISE, EARLY, 1.0, 0.25),
        (RISE, LATE, 2.0, 0.5),
        (FALL, EARLY, 1.5, 0.5),
        (FALL, LATE, 2.5, 0.75),
    ] {
        assert_eq!(arr(&v, 1, 0, tr, an), a);
        assert_eq!(slew(&v, 1, tr, an), s);
        assert_eq!(pred_source(pred(&v, 1, 0, tr, an)), Some((0, tr)));
    }
    // Node 2: d = 0.125 + 0.5 s, q = 0.0625 + 0.25 s + 0.5 * 0.5.
    for (tr, an, a, s) in [
        (RISE, EARLY, 1.25, 0.375),
        (RISE, LATE, 2.375, 0.4375),
        (FALL, EARLY, 1.875, 0.4375),
        (FALL, LATE, 3.0, 0.5),
    ] {
        assert_eq!((arr(&v, 2, 0, tr, an), slew(&v, 2, tr, an)), (a, s));
        assert_eq!(pred_source(pred(&v, 2, 0, tr, an)), Some((1, tr)));
    }
    // Node 3: the crossed transitions, q = 0.5625 + 0.25 s.
    for (tr, an, a, s) in [
        (RISE, EARLY, 2.21875, 0.671875),
        (RISE, LATE, 3.375, 0.6875),
        (FALL, EARLY, 1.5625, 0.65625),
        (FALL, LATE, 2.71875, 0.671875),
    ] {
        assert_eq!((arr(&v, 3, 0, tr, an), slew(&v, 3, tr, an)), (a, s));
        assert_eq!(pred_source(pred(&v, 3, 0, tr, an)), Some((2, 1 - tr)));
    }
    assert_eq!(pred(&v, 0, 0, RISE, EARLY), NO_PRED);
}

// COMPUTE-REQ-0017: the worst candidate wins per analysis; among equal candidates the start value
// and then the first arc in CSR order win.
#[test]
fn ties_and_start_values() {
    let mut b = Build::new(1);
    let (d, _) = tables(&mut b);
    let id = b.lut(&[(0, &[0.0, 1.0])], &[0.0, 1.0]);
    let one = b.lut(&[], &[1.0]);
    let c = b.class(ArcClass::cell(POSITIVE_UNATE, [d, d], [id, id]));
    let c2 = b.class(ArcClass::cell(POSITIVE_UNATE, [one, one], [id, id]));
    b.level(&[(NO_ARCS, [0.0; 4]), (NO_ARCS, [0.0; 4])]);
    for v in [0, 1] {
        b.starts.push(
            v,
            &row([1.0, 2.0, f64::INFINITY, f64::NEG_INFINITY]),
            &[0.25, 0.25, f64::INFINITY, f64::NEG_INFINITY],
        );
    }
    b.level(&[
        (&[arc(0, c), arc(1, c)], [0.0; 4]),
        (&[arc(0, c)], [0.0; 4]),
        (&[arc(0, c), arc(1, c2)], [0.0; 4]),
    ]);
    b.starts.push(
        3,
        &row([1.25, 2.25, f64::INFINITY, f64::NEG_INFINITY]),
        &[0.25, 0.25, f64::INFINITY, f64::NEG_INFINITY],
    );
    let v = run(&b.owned());
    // Equal candidates from nodes 0 and 1: the first arc.
    assert_eq!(
        (arr(&v, 2, 0, RISE, EARLY), arr(&v, 2, 0, RISE, LATE)),
        (1.25, 2.25)
    );
    assert_eq!(pred(&v, 2, 0, RISE, EARLY), 0);
    assert_eq!(pred(&v, 2, 0, RISE, LATE), 0);
    // A candidate equal to the start value: the start value.
    assert_eq!(
        (arr(&v, 3, 0, RISE, EARLY), arr(&v, 3, 0, RISE, LATE)),
        (1.25, 2.25)
    );
    assert_eq!(pred(&v, 3, 0, RISE, EARLY), NO_PRED);
    assert_eq!(pred(&v, 3, 0, RISE, LATE), NO_PRED);
    // Different candidates: the smaller early, the larger late.
    assert_eq!(
        (arr(&v, 4, 0, RISE, EARLY), pred(&v, 4, 0, RISE, EARLY)),
        (1.25, 0)
    );
    assert_eq!(
        (arr(&v, 4, 0, RISE, LATE), pred(&v, 4, 0, RISE, LATE)),
        (3.0, 1)
    );
    // No falling input: falling values stay absent.
    assert_eq!(arr(&v, 4, 0, FALL, EARLY), absent(EARLY));
    assert_eq!(slew(&v, 4, FALL, LATE), absent(LATE));
    assert_eq!(pred(&v, 4, 0, FALL, LATE), NO_PRED);
}

// COMPUTE-REQ-0012, COMPUTE-REQ-0014, COMPUTE-REQ-0018: tag maps shift arrivals into other tags,
// drop those below the opening edge, limit them to the closing edge minus the setup time of a
// three-axis table (closing-edge transition, source transition, load) and to the limit.
#[test]
fn tag_map_windows() {
    let mut b = Build::new(2);
    let p: &[f64] = &[0.0, 1.0];
    // setup = 0.125 + 0.25 x0 + 0.5 x1 + x2.
    let vals: Vec<f64> = (0..8)
        .map(|i| {
            0.125
                + 0.25 * f64::from((i >> 2) & 1)
                + 0.5 * f64::from((i >> 1) & 1)
                + f64::from(i & 1)
        })
        .collect();
    let setup = b.lut(&[(0, p), (1, p), (2, p)], &vals);
    let mut c = ArcClass::cell(POSITIVE_UNATE, [NONE, NONE], [NONE, NONE]);
    c.setup = [setup, setup];
    let c = b.class(c);
    let inf = f64::INFINITY;
    let r0 = TagMapRec {
        shift: -2.0,
        open: [0.5, 0.5],
        close: [inf, 2.5],
        close_slew: [0.0, 0.5],
        ..TagMapRec::through(0, 1)
    };
    let r1 = TagMapRec {
        limit: [inf, 4.0],
        ..TagMapRec::through(0, 0)
    };
    let r2 = TagMapRec {
        close: [3.0, inf],
        close_slew: [0.25, 0.0],
        ..TagMapRec::through(1, 1)
    };
    let m = b.map(&[r0, r1, r2]);
    b.level(&[(NO_ARCS, [0.0; 4])]);
    let mut a = row([1.0, 5.0, f64::INFINITY, f64::NEG_INFINITY]);
    a.extend(row([0.5, 0.75, f64::INFINITY, f64::NEG_INFINITY]));
    b.starts
        .push(0, &a, &[0.25, 0.5, f64::INFINITY, f64::NEG_INFINITY]);
    b.level(&[(
        &[InArc {
            src: 0,
            class: c,
            map: m,
        }],
        [0.5; 4],
    )]);
    let v = run(&b.owned());
    // Tag 0: only r1; the late arrival 5 is limited to 4.
    assert_eq!(
        (arr(&v, 1, 0, RISE, EARLY), arr(&v, 1, 0, RISE, LATE)),
        (1.0, 4.0)
    );
    // Tag 1 early: r0 drops 1 - 2 < 0.5; r2 keeps 0.5 (below 3 - setup(0.25, 0.25, 0.5) =
    // 2.1875). Tag 1 late: r0 limits 5 - 2 = 3 to 2.5 - setup(0.5, 0.5, 0.5) = 1.5, which beats
    // r2's 0.75.
    assert_eq!(
        (arr(&v, 1, 1, RISE, EARLY), arr(&v, 1, 1, RISE, LATE)),
        (0.5, 1.5)
    );
    for t in 0..2 {
        for an in [EARLY, LATE] {
            assert_eq!(pred_source(pred(&v, 1, t, RISE, an)), Some((0, RISE)));
            assert_eq!(arr(&v, 1, t, FALL, an), absent(an));
        }
    }
    // Without tables the delay and transition are 0.
    assert_eq!(
        (slew(&v, 1, RISE, EARLY), slew(&v, 1, RISE, LATE)),
        (0.0, 0.0)
    );
}

// COMPUTE-REQ-0015, COMPUTE-REQ-0017: a wire record adds its delay and degrades the transition to
// sqrt(s^2 + k^2) (k = 0 keeps it); negative transitions clamp at 0, negative delays do not.
#[test]
fn wires_and_negative_values() {
    let mut b = Build::new(1);
    let neg_d = b.lut(&[], &[-0.5]);
    let neg_s = b.lut(&[], &[-0.25]);
    let c = b.class(ArcClass::cell(
        POSITIVE_UNATE,
        [neg_d, neg_d],
        [neg_s, neg_s],
    ));
    b.level(&[(NO_ARCS, [0.0; 4])]);
    b.starts.push(
        0,
        &row([1.0, 2.0, f64::INFINITY, f64::NEG_INFINITY]),
        &[0.5, 0.5, f64::INFINITY, f64::NEG_INFINITY],
    );
    b.level(&[(&[arc(0, 0)], [0.0; 4])]);
    b.level(&[(&[arc(1, c)], [0.0; 4])]);
    b.wires = vec![
        WireRec {
            delay: [0.125, 0.25, 0.0, 0.0],
            slew: [0.375, 0.0, 0.0, 0.0],
        },
        WireRec::default(),
    ];
    let v = run(&b.owned());
    assert_eq!(
        (arr(&v, 1, 0, RISE, EARLY), slew(&v, 1, RISE, EARLY)),
        (1.125, 0.625)
    );
    assert_eq!(
        (arr(&v, 1, 0, RISE, LATE), slew(&v, 1, RISE, LATE)),
        (2.25, 0.5)
    );
    assert_eq!(
        (arr(&v, 2, 0, RISE, EARLY), slew(&v, 2, RISE, EARLY)),
        (0.625, 0.0)
    );
    assert_eq!(
        (arr(&v, 2, 0, RISE, LATE), slew(&v, 2, RISE, LATE)),
        (1.75, 0.0)
    );
    assert_eq!(WIRE, 1);
}

// COMPUTE-REQ-0016, COMPUTE-REQ-0017: a source without arrivals (untagged) still sets the
// transition; arrival delays use the source pin's own transition; nodes without inputs stay
// absent.
#[test]
fn untagged_sources_set_transitions() {
    let mut b = Build::new(1);
    let (d, _) = tables(&mut b);
    let id = b.lut(&[(0, &[0.0, 1.0])], &[0.0, 1.0]);
    let c = b.class(ArcClass::cell(POSITIVE_UNATE, [d, d], [id, id]));
    b.level(&[(NO_ARCS, [0.0; 4]), (NO_ARCS, [0.0; 4])]);
    b.starts.push(
        0,
        &ABSENT_ROW,
        &[0.25, 0.75, f64::INFINITY, f64::NEG_INFINITY],
    );
    b.starts.push(
        1,
        &row([1.0, 1.0, f64::INFINITY, f64::NEG_INFINITY]),
        &[0.5, 0.5, f64::INFINITY, f64::NEG_INFINITY],
    );
    b.level(&[(&[arc(0, c), arc(1, c)], [0.0; 4]), (NO_ARCS, [0.0; 4])]);
    let v = run(&b.owned());
    assert_eq!(
        (slew(&v, 2, RISE, EARLY), slew(&v, 2, RISE, LATE)),
        (0.25, 0.75)
    );
    assert_eq!(
        (arr(&v, 2, 0, RISE, EARLY), arr(&v, 2, 0, RISE, LATE)),
        (1.375, 1.375)
    );
    assert_eq!(pred(&v, 2, 0, RISE, LATE), 1);
    for an in [EARLY, LATE] {
        assert_eq!(slew(&v, 3, RISE, an), absent(an));
        assert_eq!(arr(&v, 3, 0, FALL, an), absent(an));
        assert_eq!(arr(&v, 0, 0, RISE, an), absent(an));
    }
}

// COMPUTE-REQ-0016: zeros are stored as +0.0, from start values and from tables.
#[test]
fn zeros_are_canonical() {
    let mut b = Build::new(1);
    let nz = b.lut(&[], &[-0.0]);
    let c = b.class(ArcClass::cell(POSITIVE_UNATE, [nz, nz], [nz, nz]));
    b.level(&[(NO_ARCS, [0.0; 4])]);
    b.starts.push(
        0,
        &row([-0.0, -0.0, f64::INFINITY, f64::NEG_INFINITY]),
        &[-0.0, -0.0, f64::INFINITY, f64::NEG_INFINITY],
    );
    b.level(&[(&[arc(0, c)], [0.0; 4])]);
    let v = run(&b.owned());
    for node in [0, 1] {
        for an in [EARLY, LATE] {
            assert_eq!(arr(&v, node, 0, RISE, an).to_bits(), 0.0f64.to_bits());
            assert_eq!(slew(&v, node, RISE, an).to_bits(), 0.0f64.to_bits());
        }
    }
}

// COMPUTE-REQ-0016: index helpers and predecessor encoding.
#[test]
fn layout_helpers() {
    assert_eq!(arr_index(2, 1, 1, 0, 3), ((2 * 3 + 1) * 2 + 1) * 2);
    assert_eq!(slew_index(5, 1, 1), 23);
    assert_eq!(pred_source(7 | (1 << 31)), Some((7, 1)));
    assert_eq!(pred_source(7), Some((7, 0)));
    assert_eq!(pred_source(NO_PRED), None);
    let v = TimingValues::absent(2, 3);
    assert!(v.fits(2, 3) && !v.fits(3, 3) && !v.fits(2, 2));
    assert_eq!(v.arrival(1, 2, 1, 0), f64::INFINITY);
    assert_eq!(v.transition(1, 1, 1), f64::NEG_INFINITY);
    assert_eq!(v.predecessor(0, 0, 0, 0), NO_PRED);
    assert_eq!(v.first_difference(&v.clone()), None);
    let mut w = v.clone();
    w.pred[5] = 3;
    assert!(
        v.first_difference(&w)
            .is_some_and(|d| d.contains("predecessor"))
    );
}
