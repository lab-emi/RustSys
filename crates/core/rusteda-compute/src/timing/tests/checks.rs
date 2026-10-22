// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Validation (COMPUTE-REQ-0020): every corruption of a valid problem is rejected with
//! `Invalid`, by `validate()` and by every backend's `open`.

use super::random::{self, Owned, Shape};
use crate::ComputeError;
use crate::timing::{
    CPU, NEGATIVE_UNATE, NONE, StartRows, TimingPropagationBackend, WIRE, WireRec,
};

/// A valid problem with every feature: maps, wires, three axes, several tags.
fn valid() -> Owned {
    let mut shape = Shape::from_seed(3);
    shape.levels = 6;
    shape.width = 12;
    shape.ntags = 3;
    shape.wires = true;
    let o = random::problem(3, shape);
    o.problem()
        .validate()
        .unwrap_or_else(|e| panic!("valid problem rejected: {e}"));
    assert!(!o.graph.maps.is_empty() && !o.wires.is_empty());
    o
}

/// The first arc whose destination lies on level 2 or above, with its destination.
fn deep_arc(o: &Owned) -> (usize, usize) {
    let lp = &o.graph.level_ptr;
    let first = lp[2] as usize;
    let k = o.graph.in_ptr[first] as usize;
    let v = o.graph.in_ptr.partition_point(|&p| p as usize <= k) - 1;
    (k, v)
}

type Corruption = (&'static str, fn(&mut Owned));

const CORRUPTIONS: &[Corruption] = &[
    ("ntags is 0", |o| o.ntags = 0),
    ("in_ptr is empty", |o| o.graph.in_ptr.to_mut().clear()),
    ("level_ptr is empty", |o| o.graph.level_ptr.to_mut().clear()),
    ("level_ptr does not end", |o| {
        o.graph.level_ptr.to_mut().pop();
    }),
    ("decreases", |o| {
        let lp = o.graph.level_ptr.to_mut();
        lp[1] = lp[2] + 1;
    }),
    ("in_ptr does not end", |o| {
        let p = o.graph.in_ptr.to_mut();
        let last = p.len() - 1;
        p[last] -= 1;
    }),
    ("not on a lower level", |o| {
        let (k, v) = deep_arc(o);
        o.graph.in_arcs.to_mut()[k].src = v as u32;
    }),
    ("sources are not sorted", |o| {
        let (k, v) = deep_arc(o);
        let arcs = o.graph.in_arcs.to_mut();
        arcs[k].src = o.graph.level_ptr[1] - 1;
        let end = o.graph.in_ptr[v + 1] as usize;
        assert!(end > k);
        arcs.insert(k + 1, arcs[k]);
        arcs[k + 1].src = 0;
        for p in o.graph.in_ptr.to_mut()[v + 1..].iter_mut() {
            *p += 1;
        }
        let n = o.load.len();
        let (op, od) = crate::timing::out_adjacency(n, &o.graph.in_ptr, arcs);
        *o.graph.out_ptr.to_mut() = op;
        *o.graph.out_dst.to_mut() = od;
        o.wires.insert(k + 1, WireRec::default());
    }),
    ("class", |o| {
        let n = o.luts.classes.len() as u32;
        o.graph.in_arcs.to_mut()[0].class = n;
    }),
    ("tag map", |o| {
        let n = o.graph.map_count() as u32;
        o.graph.in_arcs.to_mut()[0].map = n;
    }),
    ("out_dst", |o| {
        let d = o.graph.out_dst.to_mut();
        d[0] = d[0].wrapping_add(1);
    }),
    ("out_ptr has", |o| {
        o.graph.out_ptr.to_mut().pop();
    }),
    ("out_ptr of node", |o| {
        let n = o.load.len();
        let p = o.graph.out_ptr.to_mut();
        let u = (0..n).find(|&u| p[u + 1] > p[u]).unwrap_or(0);
        p[u + 1] -= 1;
        o.graph.out_dst.to_mut().remove(p[u + 1] as usize);
        for x in &mut p[u + 2..] {
            *x -= 1;
        }
    }),
    ("map_ptr", |o| {
        o.graph.map_ptr.to_mut().pop();
    }),
    ("without map_ptr", |o| o.graph.map_ptr.to_mut().clear()),
    ("tags", |o| o.graph.maps.to_mut()[0].dst_tag = 3),
    ("shift", |o| o.graph.maps.to_mut()[0].shift = f64::NAN),
    ("shift", |o| o.graph.maps.to_mut()[0].shift = f64::INFINITY),
    ("shift", |o| {
        o.graph.maps.to_mut()[0].open[1] = f64::INFINITY
    }),
    ("shift", |o| {
        o.graph.maps.to_mut()[0].close[0] = f64::NEG_INFINITY
    }),
    ("shift", |o| o.graph.maps.to_mut()[0].limit[1] = f64::NAN),
    ("shift", |o| {
        o.graph.maps.to_mut()[0].close_slew[0] = f64::INFINITY
    }),
    ("undefined flag bits", |o| {
        o.luts.classes.to_mut()[1].flags |= 1 << 7
    }),
    ("classes[", |o| {
        let n = o.luts.luts.len() as u32;
        o.luts.classes.to_mut()[1].delay[1] = n;
    }),
    ("a wire class has no tables", |o| {
        o.luts.classes.to_mut()[0].slew[0] = 0;
    }),
    ("a wire class has no tables", |o| {
        o.luts.classes.to_mut()[0].flags = WIRE | NEGATIVE_UNATE;
    }),
    ("is not finite", |o| o.luts.data.to_mut()[0] = f64::NAN),
    ("is not finite", |o| {
        o.luts.data.to_mut()[0] = f64::NEG_INFINITY
    }),
    ("axis 0 out of range", |o| {
        let n = o.luts.data.len() as u32;
        let t = first_axis_lut(o);
        o.luts.luts.to_mut()[t].off[0] = n;
    }),
    ("not strictly increasing", |o| {
        let t = first_axis_lut(o);
        let (off, n) = {
            let l = &o.luts.luts[t];
            (l.off[0] as usize, l.n[0] as usize)
        };
        assert!(n >= 2);
        let d = o.luts.data.to_mut();
        d[off + 1] = d[off];
    }),
    ("argument", |o| {
        let t = first_axis_lut(o);
        o.luts.luts.to_mut()[t].arg[0] = 3;
    }),
    ("absent axis precedes", |o| {
        let t = first_axis_lut(o);
        let l = &mut o.luts.luts.to_mut()[t];
        l.n[2] = l.n[0];
        l.n[1] = 0;
    }),
    ("values out of range", |o| {
        let n = o.luts.data.len() as u32;
        o.luts.luts.to_mut()[0].voff = n;
    }),
    ("loads for", |o| {
        o.load.pop();
    }),
    ("load of node", |o| o.load[1][2] = f64::NAN),
    ("load of node", |o| o.load[0][0] = f64::INFINITY),
    ("does not increase", |o| {
        let s = &mut o.starts;
        s.node[1] = s.node[0];
    }),
    ("start rows: node", |o| {
        let n = o.load.len() as u32;
        let s = &mut o.starts;
        let last = s.node.len() - 1;
        s.node[last] = n;
    }),
    ("start rows:", |o| {
        o.starts.arr.pop();
    }),
    ("without its transition", |o| {
        let s = &mut o.starts;
        // Arrival tag 0 rise early present, its transition absent.
        s.arr[0] = 1.0;
        s.slew[0] = f64::INFINITY;
    }),
    ("arrival", |o| o.starts.arr[0] = f64::NEG_INFINITY),
    ("arrival", |o| o.starts.arr[1] = f64::NAN),
    ("transition", |o| o.starts.slew[1] = f64::INFINITY),
    ("wire records for", |o| {
        o.wires.pop();
    }),
    ("is not zero but its arc is no wire arc", |o| {
        let k = o
            .graph
            .in_arcs
            .iter()
            .position(|a| a.class != 0)
            .unwrap_or(0);
        o.wires[k].delay[0] = 0.5;
    }),
    ("wires[", |o| {
        let k = o
            .graph
            .in_arcs
            .iter()
            .position(|a| a.class == 0)
            .unwrap_or(0);
        o.wires[k].slew[3] = f64::INFINITY;
    }),
];

/// A table with at least one axis of two or more points.
fn first_axis_lut(o: &Owned) -> usize {
    o.luts
        .luts
        .iter()
        .position(|l| l.n[0] >= 2 && l.n[1] == 0)
        .unwrap_or(0)
}

// COMPUTE-REQ-0020: each corruption is rejected by `validate()` and by `open`, naming the item.
#[test]
fn corruptions_are_rejected() {
    let base = valid();
    for (i, (want, corrupt)) in CORRUPTIONS.iter().enumerate() {
        let mut o = base.clone();
        corrupt(&mut o);
        let p = o.problem();
        match p.validate() {
            Err(ComputeError::Invalid(m)) => {
                assert!(m.contains(want), "corruption {i}: '{m}' lacks '{want}'");
            }
            other => panic!("corruption {i} ({want}) accepted: {other:?}"),
        }
        assert!(
            matches!(CPU.open(&p).map(|_| ()), Err(ComputeError::Invalid(_))),
            "corruption {i}"
        );
    }
}

// COMPUTE-REQ-0020: the boundary cases that are valid: no nodes, no maps (`map_ptr` empty or
// `[0]`), empty levels, nodes without arcs, no start rows, and no wire records.
#[test]
fn boundary_cases_are_valid() {
    let mut o = valid();
    *o.graph.map_ptr.to_mut() = vec![0];
    o.graph.maps.to_mut().clear();
    for a in o.graph.in_arcs.to_mut() {
        a.map = NONE;
    }
    o.problem().validate().unwrap_or_else(|e| panic!("{e}"));
    o.graph.map_ptr.to_mut().clear();
    o.starts = StartRows::default();
    o.wires.clear();
    o.problem().validate().unwrap_or_else(|e| panic!("{e}"));
    let lp = o.graph.level_ptr.to_mut();
    let last = *lp.last().unwrap_or(&0);
    lp.push(last);
    o.problem().validate().unwrap_or_else(|e| panic!("{e}"));
    let mut empty = o.clone();
    *empty.graph.level_ptr.to_mut() = vec![0];
    *empty.graph.in_ptr.to_mut() = vec![0];
    empty.graph.in_arcs.to_mut().clear();
    *empty.graph.out_ptr.to_mut() = vec![0];
    empty.graph.out_dst.to_mut().clear();
    empty.load.clear();
    empty.problem().validate().unwrap_or_else(|e| panic!("{e}"));
    let v = super::cpu_values(&empty);
    assert_eq!((v.arr.len(), v.slew.len(), v.pred.len()), (0, 0, 0));
}
