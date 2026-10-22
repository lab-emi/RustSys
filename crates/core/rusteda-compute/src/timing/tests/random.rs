// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Seeded random problems of the timing task: tables with 0 to 3 axes on every argument order,
//! wire, unate, non-unate, single-transition, disabled and latch classes, arcs that share a
//! source, tag maps with every kind of window bound, sparse start rows with absent entries and
//! untagged sources, wire records, and empty levels.

use std::borrow::Cow;

use crate::timing::{
    ArcClass, CompiledLutBank, InArc, LevelizedTimingCsr, LutDesc, NEGATIVE_UNATE, NON_UNATE, NONE,
    POSITIVE_UNATE, StartRows, TagMapRec, TimingProblem, WireRec, absent, out_adjacency,
    transition_bit,
};

/// SplitMix64.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x5DEE_CE66_D1CE_4E5B)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub(crate) fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [lo, hi).
    pub(crate) fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// Uniform in 0..n (0 for n = 0).
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    /// True with probability `p`.
    pub(crate) fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
}

/// The shape of a random problem.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shape {
    pub levels: usize,
    /// Most nodes of a level.
    pub width: usize,
    pub ntags: u32,
    pub max_fanin: usize,
    /// Arcs with tag maps.
    pub maps: bool,
    /// Wire records.
    pub wires: bool,
    /// Tables with three axes.
    pub axes3: bool,
    /// Windows whose opening edge never drops an arrival (single-precision comparisons have no
    /// near-ties of a drop decision then).
    pub continuous: bool,
}

impl Shape {
    /// A shape chosen from the seed: small to medium, every feature on.
    pub(crate) fn from_seed(seed: u64) -> Shape {
        let mut r = Rng::new(seed.wrapping_mul(31).wrapping_add(7));
        Shape {
            levels: 2 + r.below(24),
            width: 1 + r.below(150),
            ntags: 1 + r.below(5) as u32,
            max_fanin: 1 + r.below(5),
            maps: true,
            wires: r.chance(0.5),
            axes3: true,
            continuous: false,
        }
    }
}

/// An owned problem.
#[derive(Debug, Clone)]
pub(crate) struct Owned {
    pub graph: LevelizedTimingCsr<'static>,
    pub luts: CompiledLutBank<'static>,
    pub ntags: u32,
    pub load: Vec<[f64; 4]>,
    pub starts: StartRows,
    pub wires: Vec<WireRec>,
}

impl Owned {
    pub(crate) fn problem(&self) -> TimingProblem<'_> {
        TimingProblem {
            graph: &self.graph,
            luts: &self.luts,
            ntags: self.ntags,
            load: &self.load,
            starts: &self.starts,
            wires: &self.wires,
        }
    }

    pub(crate) fn nodes(&self) -> usize {
        self.graph.nodes()
    }
}

/// Appends sorted axis points: `n` points from `lo` with random steps of about `step`.
fn axis(r: &mut Rng, data: &mut Vec<f64>, n: usize, lo: f64, step: f64) -> u32 {
    let off = data.len() as u32;
    let mut x = lo + r.range(0.0, step);
    for _ in 0..n {
        data.push(x);
        x += step * r.range(0.2, 1.8);
    }
    off
}

/// The value ranges of the canonical arguments: transitions (ns), loads (pF), loads.
const ARG_LO: [f64; 3] = [0.002, 0.0005, 0.0005];
const ARG_STEP: [f64; 3] = [0.12, 0.02, 0.02];

/// A table of `n` axes over arguments `args`, with values `a + b x[0] + c x[1] + e x[2]` plus
/// noise (so that interpolation is no exact linear function).
fn table(
    r: &mut Rng,
    data: &mut Vec<f64>,
    args: &[u32],
    coef: [f64; 4],
    noise: f64,
    continuous: bool,
) -> LutDesc {
    let mut d = LutDesc::default();
    for (k, &arg) in args.iter().enumerate() {
        // One point now and then: constant along that axis.
        let n = if r.chance(0.08) { 1 } else { 2 + r.below(6) };
        d.n[k] = n as u32;
        d.arg[k] = arg;
        d.off[k] = axis(r, data, n, ARG_LO[arg as usize], ARG_STEP[arg as usize]);
    }
    d.voff = data.len() as u32;
    let count = d.values();
    let dims: Vec<usize> = (0..args.len()).map(|k| d.n[k] as usize).collect();
    for flat in 0..count {
        let mut x = [0.0; 3];
        let mut rest = flat;
        for k in (0..args.len()).rev() {
            let i = rest % dims[k];
            rest /= dims[k];
            x[args[k] as usize] = data[d.off[k] as usize + i];
        }
        let v = coef[0] + coef[1] * x[0] + coef[2] * x[1] + coef[3] * x[2];
        // Now and then a negative zero, which no stored value may keep.
        let nz = r.chance(0.01);
        let jitter = noise * r.range(-1.0, 1.0);
        data.push(if nz && !continuous { -0.0 } else { v + jitter });
    }
    d
}

/// Argument orders of tables: every axis count, every order.
const ORDERS: [&[u32]; 13] = [
    &[],
    &[0],
    &[1],
    &[2],
    &[0, 1],
    &[1, 0],
    &[0, 2],
    &[2, 0],
    &[0, 1, 2],
    &[1, 0, 2],
    &[2, 1, 0],
    &[0, 2, 1],
    &[1, 2, 0],
];

/// A random problem of `shape` from `seed`.
pub(crate) fn problem(seed: u64, shape: Shape) -> Owned {
    let mut r = Rng::new(seed);
    let nt = shape.ntags as usize;
    // Tables: delays, transitions (slope below 1 so that transitions stay bounded along paths,
    // some with a negative intercept so that extrapolation goes below 0) and setup times.
    let mut data = Vec::new();
    let mut luts = Vec::new();
    let orders: Vec<&[u32]> = ORDERS
        .iter()
        .copied()
        .filter(|o| shape.axes3 || o.len() < 3)
        .collect();
    let mut kinds = Vec::new();
    for i in 0..36 {
        let order = orders[r.below(orders.len())];
        let kind = i % 3;
        let coef = match kind {
            // Delay.
            0 => [
                r.range(-0.02, 0.15),
                r.range(0.0, 0.6),
                r.range(0.5, 6.0),
                r.range(0.0, 2.0),
            ],
            // Output transition.
            1 => [
                r.range(-0.03, 0.06),
                r.range(0.0, 0.8),
                r.range(0.5, 5.0),
                r.range(0.0, 1.0),
            ],
            // Setup time: related transition, constrained transition, load.
            _ => [
                r.range(-0.05, 0.1),
                r.range(-0.2, 0.3),
                r.range(0.0, 0.4),
                r.range(0.0, 1.0),
            ],
        };
        let noise = if shape.continuous { 0.0005 } else { 0.004 };
        luts.push(table(
            &mut r,
            &mut data,
            order,
            coef,
            noise,
            shape.continuous,
        ));
        kinds.push(kind);
    }
    let pick = |r: &mut Rng, kind: usize| -> u32 {
        if r.chance(0.08) {
            return NONE;
        }
        loop {
            let t = r.below(luts.len());
            if kinds[t] == kind {
                return t as u32;
            }
        }
    };
    let mut classes = vec![ArcClass::wire()];
    for k in 0..20 {
        let bits = match k % 7 {
            0 | 1 => POSITIVE_UNATE,
            2 => NEGATIVE_UNATE,
            3 => NON_UNATE,
            4 => transition_bit(0, 0),
            5 => transition_bit(1, 0) | transition_bit(0, 0),
            _ => {
                if k == 6 {
                    0
                } else {
                    transition_bit(1, 1)
                }
            }
        };
        let mut c = ArcClass::cell(
            bits,
            [pick(&mut r, 0), pick(&mut r, 0)],
            [pick(&mut r, 1), pick(&mut r, 1)],
        );
        if k >= 14 {
            c.setup = [pick(&mut r, 2), pick(&mut r, 2)];
        }
        classes.push(c);
    }
    // Tag maps.
    let mut map_ptr = vec![0u32];
    let mut maps = Vec::new();
    let nmaps = if shape.maps { 1 + r.below(12) } else { 0 };
    for _ in 0..nmaps {
        for _ in 0..1 + r.below(4) {
            let mut rec = TagMapRec::through(r.below(nt) as u32, r.below(nt) as u32);
            rec.shift = if r.chance(0.2) {
                0.0
            } else {
                r.range(-3.0, 3.0)
            };
            for an in 0..2 {
                if !shape.continuous && r.chance(0.6) {
                    rec.open[an] = r.range(-1.0, 6.0);
                }
                if r.chance(0.6) {
                    rec.close[an] = r.range(0.0, 9.0);
                }
                if r.chance(0.4) {
                    rec.limit[an] = r.range(1.0, 8.0);
                }
                rec.close_slew[an] = r.range(0.0, 0.5);
            }
            maps.push(rec);
        }
        map_ptr.push(maps.len() as u32);
    }
    // Levels and arcs.
    let mut level_ptr = vec![0u32];
    let mut in_ptr = vec![0u32];
    let mut in_arcs = Vec::new();
    let mut n = 0usize;
    for l in 0..shape.levels {
        let count = if l == 0 {
            1 + r.below(shape.width)
        } else if r.chance(0.04) {
            0
        } else {
            1 + r.below(shape.width)
        };
        let lo = n;
        for _ in 0..count {
            if l > 0 && lo > 0 {
                let fanin = if r.chance(0.03) {
                    0
                } else {
                    1 + r.below(shape.max_fanin)
                };
                let mut arcs: Vec<InArc> = Vec::new();
                for _ in 0..fanin {
                    // Mostly the previous level, so that paths grow long.
                    let prev = level_ptr[level_ptr.len().saturating_sub(2)] as usize;
                    let src = if r.chance(0.7) && prev < lo {
                        prev + r.below(lo - prev)
                    } else {
                        r.below(lo)
                    };
                    let class = if r.chance(0.25) {
                        0
                    } else {
                        1 + r.below(classes.len() - 1) as u32
                    };
                    let map = if nmaps > 0 && r.chance(0.2) {
                        r.below(nmaps) as u32
                    } else {
                        NONE
                    };
                    arcs.push(InArc {
                        src: src as u32,
                        class,
                        map,
                    });
                    // Now and then a second arc from the same source.
                    if r.chance(0.08) {
                        arcs.push(InArc {
                            src: src as u32,
                            class: 1 + r.below(classes.len() - 1) as u32,
                            map: NONE,
                        });
                    }
                }
                arcs.sort_by_key(|a| a.src);
                in_arcs.extend(arcs);
            }
            n += 1;
            in_ptr.push(in_arcs.len() as u32);
        }
        level_ptr.push(n as u32);
    }
    let (out_ptr, out_dst) = out_adjacency(n, &in_ptr, &in_arcs);
    let load = (0..n)
        .map(|_| {
            let c = r.range(0.001, 0.12);
            [
                c,
                c * r.range(1.0, 1.3),
                c * r.range(0.9, 1.1),
                c * r.range(1.0, 1.4),
            ]
        })
        .collect();
    // Start rows: every source of level 0, and a few other nodes.
    let mut starts = StartRows::default();
    let src_end = level_ptr[1] as usize;
    for v in 0..n {
        if v >= src_end && !r.chance(0.04) {
            continue;
        }
        // Now and then the start row of a level-0 source that reaches this node through a bare
        // wire: the start value ties with the arc's candidate.
        let first = in_ptr[v] as usize;
        if v >= src_end && !shape.wires && in_ptr[v + 1] as usize > first && r.chance(0.5) {
            let a = in_arcs[first];
            if a.class == 0 && a.map == NONE && (a.src as usize) < src_end {
                if let Some(k) = starts.find(a.src) {
                    let (arr, slew) = starts.row(k, shape.ntags);
                    let (arr, slew) = (arr.to_vec(), [slew[0], slew[1], slew[2], slew[3]]);
                    starts.push(v as u32, &arr, &slew);
                    continue;
                }
            }
        }
        let untagged = r.chance(0.1);
        let mut slew = [0.0; 4];
        for (i, s) in slew.iter_mut().enumerate() {
            *s = if r.chance(0.05) {
                absent(i % 2)
            } else {
                r.range(0.005, 0.4)
            };
        }
        let mut arr = vec![0.0; nt * 4];
        for (i, a) in arr.iter_mut().enumerate() {
            let an = i % 2;
            // `i % 4` is the transition and analysis of the arrival.
            let present = !untagged && slew[i % 4] != absent(an) && r.chance(0.7);
            *a = if present {
                // Zeros of both signs: the values store +0.0.
                match r.below(40) {
                    0 => 0.0,
                    1 => -0.0,
                    _ => r.range(-0.5, 4.0),
                }
            } else {
                absent(an)
            };
        }
        starts.push(v as u32, &arr, &slew);
    }
    let wires = if shape.wires {
        in_arcs
            .iter()
            .map(|a| {
                if a.class != 0 || r.chance(0.2) {
                    return WireRec::default();
                }
                let mut w = WireRec::default();
                for i in 0..4 {
                    w.delay[i] = r.range(0.0, 0.05);
                    w.slew[i] = if r.chance(0.3) {
                        0.0
                    } else {
                        r.range(0.0, 0.08)
                    };
                }
                w
            })
            .collect()
    } else {
        Vec::new()
    };
    Owned {
        graph: LevelizedTimingCsr {
            level_ptr: Cow::Owned(level_ptr),
            in_ptr: Cow::Owned(in_ptr),
            in_arcs: Cow::Owned(in_arcs),
            out_ptr: Cow::Owned(out_ptr),
            out_dst: Cow::Owned(out_dst),
            map_ptr: Cow::Owned(map_ptr),
            maps: Cow::Owned(maps),
        },
        luts: CompiledLutBank {
            luts: Cow::Owned(luts),
            data: Cow::Owned(data),
            classes: Cow::Owned(classes),
        },
        ntags: shape.ntags,
        load,
        starts,
        wires,
    }
}
