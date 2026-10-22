// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The values of a propagation and their layout (COMPUTE-REQ-0016), and the state of the
//! single-call form (COMPUTE-REQ-0015, COMPUTE-REQ-0023).

use super::abi::StartRows;

/// Analysis index of the early analysis (reduced with the minimum).
pub const EARLY: usize = 0;
/// Analysis index of the late analysis (reduced with the maximum).
pub const LATE: usize = 1;
/// Transition index of a rising transition.
pub const RISE: usize = 0;
/// Transition index of a falling transition.
pub const FALL: usize = 1;
/// The predecessor of a start value or an absent arrival.
pub const NO_PRED: u32 = u32::MAX;

/// The absent value of analysis `an`: `+inf` early, `-inf` late.
#[inline]
pub fn absent(an: usize) -> f64 {
    if an == EARLY {
        f64::INFINITY
    } else {
        f64::NEG_INFINITY
    }
}

/// Index of the arrival (and predecessor) of node `v`, tag `t`, transition `tr` and analysis
/// `an` among `ntags` tags.
#[inline]
pub fn arr_index(v: usize, t: usize, tr: usize, an: usize, ntags: usize) -> usize {
    ((v * ntags + t) * 2 + tr) * 2 + an
}

/// Index of the transition of node `v`, transition `tr` and analysis `an`.
#[inline]
pub fn slew_index(v: usize, tr: usize, an: usize) -> usize {
    (v * 2 + tr) * 2 + an
}

/// The source node and source transition of a predecessor, or `None` for [`NO_PRED`].
#[inline]
pub fn pred_source(p: u32) -> Option<(u32, usize)> {
    (p != NO_PRED).then_some((p & 0x7FFF_FFFF, (p >> 31) as usize))
}

/// The canonical form of a value: zeros are `+0.0` (COMPUTE-REQ-0016).
#[inline]
pub(crate) fn canon(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}

/// Arrivals, transitions and predecessors of every node (COMPUTE-REQ-0016).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimingValues {
    pub ntags: u32,
    /// `arr[((v * T + t) * 2 + tr) * 2 + an]`.
    pub arr: Vec<f64>,
    /// `slew[(v * 2 + tr) * 2 + an]`.
    pub slew: Vec<f64>,
    /// Indexed like `arr`: `src | (ti << 31)` or [`NO_PRED`].
    pub pred: Vec<u32>,
}

impl TimingValues {
    /// Every value of `nodes` nodes absent.
    pub fn absent(nodes: usize, ntags: u32) -> TimingValues {
        let w = nodes * ntags as usize * 4;
        TimingValues {
            ntags,
            arr: (0..w).map(|i| absent(i % 2)).collect(),
            slew: (0..nodes * 4).map(|i| absent(i % 2)).collect(),
            pred: vec![NO_PRED; w],
        }
    }

    /// The node count.
    pub fn nodes(&self) -> usize {
        self.slew.len() / 4
    }

    /// True when the arrays have the lengths of `nodes` nodes and `ntags` tags.
    pub fn fits(&self, nodes: usize, ntags: u32) -> bool {
        let w = nodes * ntags as usize * 4;
        self.ntags == ntags
            && self.arr.len() == w
            && self.pred.len() == w
            && self.slew.len() == nodes * 4
    }

    /// The arrival of node `v`, tag `t`, transition `tr` and analysis `an`.
    pub fn arrival(&self, v: usize, t: usize, tr: usize, an: usize) -> f64 {
        self.arr[arr_index(v, t, tr, an, self.ntags as usize)]
    }

    /// The predecessor of that arrival.
    pub fn predecessor(&self, v: usize, t: usize, tr: usize, an: usize) -> u32 {
        self.pred[arr_index(v, t, tr, an, self.ntags as usize)]
    }

    /// The transition of node `v`, transition `tr` and analysis `an`.
    pub fn transition(&self, v: usize, tr: usize, an: usize) -> f64 {
        self.slew[slew_index(v, tr, an)]
    }

    /// The first value whose bits differ from `other`'s, as text, or `None` when every array is
    /// bitwise equal.
    pub fn first_difference(&self, other: &TimingValues) -> Option<String> {
        if self.ntags != other.ntags
            || self.arr.len() != other.arr.len()
            || self.slew.len() != other.slew.len()
            || self.pred.len() != other.pred.len()
        {
            return Some(format!(
                "shapes differ: {} tags, {}/{}/{} values against {} tags, {}/{}/{}",
                self.ntags,
                self.arr.len(),
                self.slew.len(),
                self.pred.len(),
                other.ntags,
                other.arr.len(),
                other.slew.len(),
                other.pred.len()
            ));
        }
        let w = self.ntags as usize * 4;
        let at = |i: usize| {
            let (v, r) = (i / w.max(1), i % w.max(1));
            format!("node {v} tag {} tr {} an {}", r / 4, (r / 2) % 2, r % 2)
        };
        if let Some(i) =
            (0..self.arr.len()).find(|&i| self.arr[i].to_bits() != other.arr[i].to_bits())
        {
            return Some(format!(
                "arrival of {}: {:e} against {:e}",
                at(i),
                self.arr[i],
                other.arr[i]
            ));
        }
        if let Some(i) =
            (0..self.slew.len()).find(|&i| self.slew[i].to_bits() != other.slew[i].to_bits())
        {
            return Some(format!(
                "transition of node {} tr {} an {}: {:e} against {:e}",
                i / 4,
                (i / 2) % 2,
                i % 2,
                self.slew[i],
                other.slew[i]
            ));
        }
        if let Some(i) = (0..self.pred.len()).find(|&i| self.pred[i] != other.pred[i]) {
            return Some(format!(
                "predecessor of {}: {:#x} against {:#x}",
                at(i),
                self.pred[i],
                other.pred[i]
            ));
        }
        None
    }
}

/// The inputs and values of one corner for the single-call form
/// [`propagate_levels`](super::TimingPropagationBackend::propagate_levels) (COMPUTE-REQ-0015,
/// COMPUTE-REQ-0023): the per-node loads and the start rows go in, the arrivals, transitions and
/// predecessors (COMPUTE-REQ-0016) come out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimingStateSoA {
    pub ntags: u32,
    /// `load[v][tr * 2 + an]`.
    pub load: Vec<[f64; 4]>,
    pub starts: StartRows,
    pub arr: Vec<f64>,
    pub slew: Vec<f64>,
    pub pred: Vec<u32>,
}

impl TimingStateSoA {
    /// A state with these inputs and no values yet.
    pub fn new(ntags: u32, load: Vec<[f64; 4]>, starts: StartRows) -> TimingStateSoA {
        TimingStateSoA {
            ntags,
            load,
            starts,
            ..TimingStateSoA::default()
        }
    }

    /// Moves values into the state.
    pub fn set_values(&mut self, v: TimingValues) {
        self.ntags = v.ntags;
        self.arr = v.arr;
        self.slew = v.slew;
        self.pred = v.pred;
    }

    /// A copy of the values.
    pub fn values(&self) -> TimingValues {
        TimingValues {
            ntags: self.ntags,
            arr: self.arr.clone(),
            slew: self.slew.clone(),
            pred: self.pred.clone(),
        }
    }
}
