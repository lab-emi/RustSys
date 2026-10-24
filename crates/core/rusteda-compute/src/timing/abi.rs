// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The interface of the timing task (COMPUTE-REQ-0011 to COMPUTE-REQ-0015): plain records in
//! host byte order and `Cow` arrays, so that an engine can lower into owned arrays or borrow
//! tables mapped from disk.

use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};

/// No table, no tag map (`u32::MAX`).
pub const NONE: u32 = u32::MAX;

/// [`ArcClass::flags`] bit of a wire arc (COMPUTE-REQ-0013).
pub const WIRE: u32 = 1;

/// The [`ArcClass::flags`] bit that lets input transition `ti` produce output transition `to`
/// (0 rise, 1 fall; COMPUTE-REQ-0013).
pub const fn transition_bit(ti: usize, to: usize) -> u32 {
    1 << (1 + 2 * ti + to)
}

/// Rise to rise and fall to fall.
pub const POSITIVE_UNATE: u32 = transition_bit(0, 0) | transition_bit(1, 1);
/// Rise to fall and fall to rise.
pub const NEGATIVE_UNATE: u32 = transition_bit(0, 1) | transition_bit(1, 0);
/// Every input transition to every output transition.
pub const NON_UNATE: u32 = POSITIVE_UNATE | NEGATIVE_UNATE;
/// The defined bits of [`ArcClass::flags`].
pub const FLAG_MASK: u32 = WIRE | NON_UNATE;

/// One incoming arc of a node, in CSR order (12 bytes; COMPUTE-REQ-0011).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct InArc {
    /// The source node, on a lower level than the arc's destination.
    pub src: u32,
    /// Index into [`CompiledLutBank::classes`].
    pub class: u32,
    /// Index of the arc's tag map ([`LevelizedTimingCsr::map_ptr`]), or [`NONE`] for the identity
    /// map.
    pub map: u32,
}

/// One record of a tag map (80 bytes; COMPUTE-REQ-0012): arrivals of `src_tag` at the arc's
/// source enter `dst_tag` at its destination through a time shift and a window. The arrays are
/// indexed by analysis (0 early, 1 late).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct TagMapRec {
    pub src_tag: u32,
    pub dst_tag: u32,
    /// Added to the source arrival: the change of time origin from `src_tag` to `dst_tag`.
    pub shift: f64,
    /// A shifted arrival below `open` is dropped; `-inf`: no lower bound.
    pub open: [f64; 2],
    /// The closing edge: the shifted arrival is limited to `close` minus the setup time of the
    /// arc's class; `+inf`: no closing edge.
    pub close: [f64; 2],
    /// The transition at the closing edge, the related-pin argument of the setup table.
    pub close_slew: [f64; 2],
    /// An upper limit independent of the setup time (for example the largest time a latch may
    /// borrow); `+inf`: none.
    pub limit: [f64; 2],
}

impl TagMapRec {
    /// A record without shift and window: `src_tag` continues as `dst_tag`.
    pub fn through(src_tag: u32, dst_tag: u32) -> TagMapRec {
        TagMapRec {
            src_tag,
            dst_tag,
            shift: 0.0,
            open: [f64::NEG_INFINITY; 2],
            close: [f64::INFINITY; 2],
            close_slew: [0.0; 2],
            limit: [f64::INFINITY; 2],
        }
    }
}

/// A lookup table with up to three axes (40 bytes; COMPUTE-REQ-0013, COMPUTE-REQ-0014).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct LutDesc {
    /// Points per axis; 0 marks an absent axis, and the axes in use come first.
    pub n: [u32; 3],
    /// Offset of each axis' points in [`CompiledLutBank::data`].
    pub off: [u32; 3],
    /// The canonical argument (0, 1 or 2) that indexes each axis.
    pub arg: [u32; 3],
    /// Offset of the values: row-major, the last axis varies fastest.
    pub voff: u32,
}

impl LutDesc {
    /// A table without axes: the single value at `voff`.
    pub fn scalar(voff: u32) -> LutDesc {
        LutDesc {
            voff,
            ..LutDesc::default()
        }
    }

    /// The number of axes in use (the leading axes with points).
    pub fn axes(&self) -> usize {
        self.n.iter().take_while(|&&n| n > 0).count()
    }

    /// The number of values: the product of the point counts of the axes in use.
    pub fn values(&self) -> usize {
        self.n[..self.axes()].iter().map(|&n| n as usize).product()
    }
}

/// What an arc does, shared by all arcs of the same library timing arc (28 bytes;
/// COMPUTE-REQ-0013).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct ArcClass {
    /// Bit 0 [`WIRE`]; bit `1 + 2 ti + to` ([`transition_bit`]): input transition `ti` produces
    /// output transition `to`.
    pub flags: u32,
    /// Delay table per output transition, or [`NONE`].
    pub delay: [u32; 2],
    /// Output transition table per output transition, or [`NONE`].
    pub slew: [u32; 2],
    /// Setup table of a tag-map window per input (data) transition, or [`NONE`].
    pub setup: [u32; 2],
}

impl ArcClass {
    /// The wire class: no tables, rise to rise and fall to fall.
    pub const fn wire() -> ArcClass {
        ArcClass {
            flags: WIRE | POSITIVE_UNATE,
            delay: [NONE; 2],
            slew: [NONE; 2],
            setup: [NONE; 2],
        }
    }

    /// A cell class with the given transition bits and delay and output transition tables per
    /// output transition, without setup tables.
    pub const fn cell(transitions: u32, delay: [u32; 2], slew: [u32; 2]) -> ArcClass {
        ArcClass {
            flags: transitions & NON_UNATE,
            delay,
            slew,
            setup: [NONE; 2],
        }
    }

    /// True for a wire class.
    pub fn is_wire(&self) -> bool {
        self.flags & WIRE != 0
    }

    /// True when input transition `ti` produces output transition `to`.
    pub fn allows(&self, ti: usize, to: usize) -> bool {
        self.flags & transition_bit(ti, to) != 0
    }
}

/// The wire record of an incoming arc (64 bytes; COMPUTE-REQ-0015), indexed `to * 2 + an`: the
/// hook of resistance-capacitance wire models, all zero for ideal wires.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Pod, Zeroable)]
pub struct WireRec {
    /// Added to the arrival.
    pub delay: [f64; 4],
    /// Degradation term `k`: the transition `s` becomes `sqrt(s * s + k * k)`, or stays `s` when
    /// `k` is 0.
    pub slew: [f64; 4],
}

impl WireRec {
    /// True when every term is zero (an ideal wire).
    pub fn is_zero(&self) -> bool {
        self.delay.iter().chain(&self.slew).all(|&x| x == 0.0)
    }
}

/// Evaluated RC arc values, per (input transition, output transition, bound).
/// CPU waveform preparation is tag-independent; all backends consume identical data.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Pod, Zeroable)]
pub struct ArcValues {
    pub delay: [f64; 8],
    pub slew: [f64; 8],
}

/// The levelized timing graph (COMPUTE-REQ-0011) with its tag maps (COMPUTE-REQ-0012).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LevelizedTimingCsr<'a> {
    /// Level `l` holds the nodes `level_ptr[l] .. level_ptr[l + 1]`.
    pub level_ptr: Cow<'a, [u32]>,
    /// The incoming arcs of node `v` are `in_arcs[in_ptr[v] .. in_ptr[v + 1]]`.
    pub in_ptr: Cow<'a, [u32]>,
    pub in_arcs: Cow<'a, [InArc]>,
    /// The distinct destinations of the arcs from node `u`, increasing:
    /// `out_dst[out_ptr[u] .. out_ptr[u + 1]]`.
    pub out_ptr: Cow<'a, [u32]>,
    pub out_dst: Cow<'a, [u32]>,
    /// Tag map `m` is `maps[map_ptr[m] .. map_ptr[m + 1]]`; empty or `[0]` without maps.
    pub map_ptr: Cow<'a, [u32]>,
    pub maps: Cow<'a, [TagMapRec]>,
}

impl LevelizedTimingCsr<'_> {
    /// The node count (0 for an empty `in_ptr`).
    pub fn nodes(&self) -> usize {
        self.in_ptr.len().saturating_sub(1)
    }

    /// The level count.
    pub fn levels(&self) -> usize {
        self.level_ptr.len().saturating_sub(1)
    }

    /// The number of incoming arcs.
    pub fn arcs(&self) -> usize {
        self.in_arcs.len()
    }

    /// The number of tag maps.
    pub fn map_count(&self) -> usize {
        self.map_ptr.len().saturating_sub(1)
    }

    /// The incoming arcs of node `v`.
    pub fn incoming(&self, v: usize) -> &[InArc] {
        &self.in_arcs[self.in_ptr[v] as usize..self.in_ptr[v + 1] as usize]
    }

    /// The records of tag map `m`.
    pub fn map(&self, m: u32) -> &[TagMapRec] {
        let m = m as usize;
        &self.maps[self.map_ptr[m] as usize..self.map_ptr[m + 1] as usize]
    }

    /// The level of every node.
    pub fn level_of(&self) -> Vec<u32> {
        let mut out = vec![0u32; self.nodes()];
        for (l, w) in self.level_ptr.windows(2).enumerate() {
            for x in &mut out[w[0] as usize..w[1] as usize] {
                *x = l as u32;
            }
        }
        out
    }
}

/// The outgoing adjacency `(out_ptr, out_dst)` of the incoming arcs of `n` nodes: for each source,
/// the distinct destinations of its arcs, increasing (COMPUTE-REQ-0011).
pub fn out_adjacency(n: usize, in_ptr: &[u32], in_arcs: &[InArc]) -> (Vec<u32>, Vec<u32>) {
    let mut count = vec![0u32; n + 1];
    let mut last = vec![u32::MAX; n];
    for v in 0..n {
        for a in &in_arcs[in_ptr[v] as usize..in_ptr[v + 1] as usize] {
            let u = a.src as usize;
            if last[u] != v as u32 {
                last[u] = v as u32;
                count[u + 1] += 1;
            }
        }
    }
    for u in 0..n {
        count[u + 1] += count[u];
    }
    let mut cursor = count.clone();
    let mut dst = vec![0u32; count[n] as usize];
    last.fill(u32::MAX);
    for v in 0..n {
        for a in &in_arcs[in_ptr[v] as usize..in_ptr[v + 1] as usize] {
            let u = a.src as usize;
            if last[u] != v as u32 {
                last[u] = v as u32;
                dst[cursor[u] as usize] = v as u32;
                cursor[u] += 1;
            }
        }
    }
    (count, dst)
}

/// The timing tables of one corner (COMPUTE-REQ-0013).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompiledLutBank<'a> {
    pub luts: Cow<'a, [LutDesc]>,
    /// Axis points and values.
    pub data: Cow<'a, [f64]>,
    pub classes: Cow<'a, [ArcClass]>,
}

/// The start rows of a propagation (COMPUTE-REQ-0015): row `k` of `node[k]` holds the arrivals
/// `arr[(k * T + t) * 4 + tr * 2 + an]` and the transitions `slew[k * 4 + tr * 2 + an]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StartRows {
    /// The nodes with start values, strictly increasing.
    pub node: Vec<u32>,
    pub arr: Vec<f64>,
    pub slew: Vec<f64>,
}

impl StartRows {
    /// The number of rows.
    pub fn len(&self) -> usize {
        self.node.len()
    }

    /// True without rows.
    pub fn is_empty(&self) -> bool {
        self.node.is_empty()
    }

    /// Appends the row of `node` (after every row of a smaller node): its `T * 4` arrivals and 4
    /// transitions.
    pub fn push(&mut self, node: u32, arr: &[f64], slew: &[f64; 4]) {
        self.node.push(node);
        self.arr.extend_from_slice(arr);
        self.slew.extend_from_slice(slew);
    }

    /// The arrivals and transitions of row `k` for `ntags` tags.
    pub fn row(&self, k: usize, ntags: u32) -> (&[f64], &[f64]) {
        let w = ntags as usize * 4;
        (
            &self.arr[k * w..(k + 1) * w],
            &self.slew[k * 4..(k + 1) * 4],
        )
    }

    /// The row index of `node`, if it has one.
    pub fn find(&self, node: u32) -> Option<usize> {
        self.node.binary_search(&node).ok()
    }
}

/// Everything one propagation reads (COMPUTE-REQ-0011 to COMPUTE-REQ-0015).
#[derive(Debug, Clone, Copy)]
pub struct TimingProblem<'a> {
    pub graph: &'a LevelizedTimingCsr<'a>,
    pub luts: &'a CompiledLutBank<'a>,
    /// The tag count `T >= 1`.
    pub ntags: u32,
    /// `load[v][tr * 2 + an]`.
    pub load: &'a [[f64; 4]],
    pub starts: &'a StartRows,
    /// Empty (ideal wires) or one record per incoming arc in CSR order.
    pub wires: &'a [WireRec],
    /// Empty, or one prepared RC delay/transition record per incoming arc.
    pub arc_values: &'a [ArcValues],
}

impl TimingProblem<'_> {
    /// The node count.
    pub fn nodes(&self) -> usize {
        self.graph.nodes()
    }

    /// The number of values per node row of arrivals and predecessors: `T * 4`.
    pub fn row_width(&self) -> usize {
        self.ntags as usize * 4
    }
}
