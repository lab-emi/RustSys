// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! A literal, sequential reading of COMPUTE-REQ-0014 to COMPUTE-REQ-0018, structured differently
//! from the backends: every output value of a node gathers its candidates in the specified order
//! and keeps the first best one. It computes one node at a time in node order and evaluates
//! tables recursively, axis by axis.

use std::cell::Cell;

use crate::timing::{NO_PRED, NONE, TimingProblem, TimingValues};

/// How often each rule decided something, to show that random problems exercise every rule.
#[derive(Debug, Default)]
pub(crate) struct Counts {
    pub present_arrivals: Cell<u64>,
    pub mapped_candidates: Cell<u64>,
    pub open_drops: Cell<u64>,
    pub setup_clamps: Cell<u64>,
    pub limit_clamps: Cell<u64>,
    pub degraded: Cell<u64>,
    pub transition_clamps: Cell<u64>,
    pub negative_delays: Cell<u64>,
    pub three_axis_lookups: Cell<u64>,
    pub start_ties: Cell<u64>,
    pub arc_ties: Cell<u64>,
    pub untagged_transitions: Cell<u64>,
}

fn bump(c: &Cell<u64>) {
    c.set(c.get() + 1);
}

/// The interval search of COMPUTE-REQ-0014, written as a search for the first upper point that
/// the argument does not exceed.
fn interval(p: &[f64], x: f64) -> (usize, usize, f64) {
    if p.len() < 2 {
        return (0, 0, 0.0);
    }
    let last = p.len() - 2;
    let i0 = (0..=last)
        .find(|&i| i == last || x <= p[i + 1])
        .unwrap_or(last);
    (i0, i0 + 1, (x - p[i0]) / (p[i0 + 1] - p[i0]))
}

/// Table `t` at canonical arguments `x`: interpolation along the first axis of the values
/// interpolated along the remaining axes.
fn table(p: &TimingProblem, t: u32, x: [f64; 3], c: &Counts) -> f64 {
    if t == NONE {
        return 0.0;
    }
    let d = &p.luts.luts[t as usize];
    let data = &p.luts.data;
    let axes = d.axes();
    if axes == 3 {
        bump(&c.three_axis_lookups);
    }
    let stride = |k: usize| -> usize { (k + 1..axes).map(|j| d.n[j] as usize).product() };
    fn rec(
        data: &[f64],
        d: &crate::timing::LutDesc,
        axes: usize,
        k: usize,
        base: usize,
        x: [f64; 3],
        stride: &dyn Fn(usize) -> usize,
    ) -> f64 {
        if k == axes {
            return data[d.voff as usize + base];
        }
        let pts = &data[d.off[k] as usize..(d.off[k] + d.n[k]) as usize];
        let (i0, i1, w) = interval(pts, x[d.arg[k] as usize]);
        let s = stride(k);
        let f0 = rec(data, d, axes, k + 1, base + i0 * s, x, stride);
        let f1 = rec(data, d, axes, k + 1, base + i1 * s, x, stride);
        f0 + (f1 - f0) * w
    }
    rec(data, d, axes, 0, 0, x, &stride)
}

fn better(an: usize, x: f64, y: f64) -> bool {
    if an == 0 { x < y } else { x > y }
}

fn missing(an: usize) -> f64 {
    if an == 0 {
        f64::INFINITY
    } else {
        f64::NEG_INFINITY
    }
}

/// The values of every node.
pub(crate) fn evaluate(p: &TimingProblem) -> TimingValues {
    evaluate_counted(p, &Counts::default())
}

/// The values of every node, counting the decisions of the rules in `c`.
pub(crate) fn evaluate_counted(p: &TimingProblem, c: &Counts) -> TimingValues {
    let n = p.nodes();
    let nt = p.ntags as usize;
    let g = p.graph;
    let mut arr = vec![0.0; n * nt * 4];
    let mut slew = vec![0.0; n * 4];
    let mut pred = vec![NO_PRED; n * nt * 4];
    let a_at = |v: usize, t: usize, tr: usize, an: usize| ((v * nt + t) * 2 + tr) * 2 + an;
    for v in 0..n {
        let start = p.starts.find(v as u32);
        let arcs = g.in_ptr[v] as usize..g.in_ptr[v + 1] as usize;
        // The delay and transition of an arc for (ti, to, an), or None when the arc does not
        // carry ti to to or the source has no transition.
        let eval = |k: usize, ti: usize, to: usize, an: usize, slew: &[f64]| {
            let a = g.in_arcs[k];
            let cl = p.luts.classes[a.class as usize];
            if cl.flags & (1 << (1 + 2 * ti + to)) == 0 {
                return None;
            }
            let s = slew[(a.src as usize * 2 + ti) * 2 + an];
            if s == missing(an) {
                return None;
            }
            let l = p.load[v][to * 2 + an];
            if cl.flags & 1 == 1 {
                let (d, k2) = match p.wires.get(k) {
                    Some(w) => (w.delay[to * 2 + an], w.slew[to * 2 + an]),
                    None => (0.0, 0.0),
                };
                let q = if k2 == 0.0 {
                    s
                } else {
                    bump(&c.degraded);
                    (s * s + k2 * k2).sqrt()
                };
                return Some((d, q, s, l));
            }
            let d = table(p, cl.delay[to], [s, l, l], c);
            let q = table(p, cl.slew[to], [s, l, l], c);
            if d < 0.0 {
                bump(&c.negative_delays);
            }
            if q < 0.0 {
                bump(&c.transition_clamps);
            }
            Some((d, q.max(0.0), s, l))
        };
        for to in 0..2 {
            for an in 0..2 {
                let mut best = start.map_or(missing(an), |k| p.starts.slew[k * 4 + to * 2 + an]);
                for k in arcs.clone() {
                    for ti in 0..2 {
                        if let Some((_, q, _, _)) = eval(k, ti, to, an, &slew)
                            && better(an, q, best)
                        {
                            best = q;
                            let src = g.in_arcs[k].src as usize;
                            let tagged = (0..nt).any(|t| arr[a_at(src, t, ti, an)] != missing(an));
                            if !tagged {
                                bump(&c.untagged_transitions);
                            }
                        }
                    }
                }
                slew[(v * 2 + to) * 2 + an] = if best == 0.0 { 0.0 } else { best };
            }
        }
        for t in 0..nt {
            for to in 0..2 {
                for an in 0..2 {
                    let mut best = start.map_or(missing(an), |k| {
                        p.starts.arr[(k * nt + t) * 4 + to * 2 + an]
                    });
                    let mut from = NO_PRED;
                    for k in arcs.clone() {
                        let a = g.in_arcs[k];
                        let cl = p.luts.classes[a.class as usize];
                        for ti in 0..2 {
                            let Some((d, _, s, l)) = eval(k, ti, to, an, &slew) else {
                                continue;
                            };
                            let mut cands = Vec::new();
                            if a.map == NONE {
                                let x = arr[a_at(a.src as usize, t, ti, an)];
                                if x != missing(an) {
                                    cands.push(x + d);
                                }
                            } else {
                                for r in g.map(a.map).iter().filter(|r| r.dst_tag as usize == t) {
                                    let x = arr[a_at(a.src as usize, r.src_tag as usize, ti, an)];
                                    if x == missing(an) {
                                        continue;
                                    }
                                    bump(&c.mapped_candidates);
                                    let m = x + r.shift;
                                    if m < r.open[an] {
                                        bump(&c.open_drops);
                                        continue;
                                    }
                                    let mut h = r.limit[an];
                                    let mut by_setup = false;
                                    if r.close[an] != f64::INFINITY {
                                        let st =
                                            table(p, cl.setup[ti], [r.close_slew[an], s, l], c);
                                        by_setup = r.close[an] - st < h;
                                        h = h.min(r.close[an] - st);
                                    }
                                    if m > h {
                                        bump(if by_setup {
                                            &c.setup_clamps
                                        } else {
                                            &c.limit_clamps
                                        });
                                    }
                                    cands.push(m.min(h) + d);
                                }
                            }
                            for cand in cands {
                                if better(an, cand, best) {
                                    best = cand;
                                    from = a.src | ((ti as u32) << 31);
                                } else if cand == best {
                                    bump(if from == NO_PRED {
                                        &c.start_ties
                                    } else {
                                        &c.arc_ties
                                    });
                                }
                            }
                        }
                    }
                    let i = a_at(v, t, to, an);
                    if best != missing(an) {
                        bump(&c.present_arrivals);
                    }
                    arr[i] = if best == 0.0 { 0.0 } else { best };
                    pred[i] = from;
                }
            }
        }
    }
    TimingValues {
        ntags: p.ntags,
        arr,
        slew,
        pred,
    }
}
