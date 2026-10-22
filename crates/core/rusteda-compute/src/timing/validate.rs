// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Validation of problems and patches (COMPUTE-REQ-0020): every statement of the interface that
//! the backends rely on, so that no kernel reads out of bounds and the level-parallel reductions
//! never read a value of their own level.

use super::abi::{
    CompiledLutBank, FLAG_MASK, LevelizedTimingCsr, NEGATIVE_UNATE, NONE, StartRows, TimingProblem,
    WireRec,
};
use crate::ComputeError;

/// The largest node count: predecessors keep the source transition in bit 31.
pub const MAX_NODES: usize = (1 << 31) - 1;

fn bad<T>(m: String) -> Result<T, ComputeError> {
    Err(ComputeError::Invalid(m))
}

/// `ptr` starts at 0, never decreases and ends at `end`.
fn pointers(what: &str, ptr: &[u32], end: usize) -> Result<(), ComputeError> {
    if ptr.first() != Some(&0) {
        return bad(format!("{what} does not start at 0"));
    }
    if let Some(i) = ptr.windows(2).position(|w| w[1] < w[0]) {
        return bad(format!("{what}[{}] decreases", i + 1));
    }
    if ptr.last().map(|&x| x as usize) != Some(end) {
        return bad(format!("{what} does not end at {end}"));
    }
    Ok(())
}

/// An early value is finite or `+inf` (absent), a late one finite or `-inf`.
fn value_ok(x: f64, an: usize) -> bool {
    x.is_finite() || (an == 0 && x == f64::INFINITY) || (an == 1 && x == f64::NEG_INFINITY)
}

/// True when `x` is the absent value of analysis `an`.
fn absent(x: f64, an: usize) -> bool {
    if an == 0 {
        x == f64::INFINITY
    } else {
        x == f64::NEG_INFINITY
    }
}

impl TimingProblem<'_> {
    /// Checks the problem against COMPUTE-REQ-0011 to COMPUTE-REQ-0015 (COMPUTE-REQ-0020).
    pub fn validate(&self) -> Result<(), ComputeError> {
        if self.ntags == 0 {
            return bad("ntags is 0".into());
        }
        let g = self.graph;
        if g.in_ptr.is_empty() {
            return bad("in_ptr is empty (it holds nodes + 1 entries)".into());
        }
        let n = g.nodes();
        if n > MAX_NODES {
            return bad(format!("{n} nodes (at most {MAX_NODES})"));
        }
        let rows = n.checked_mul(self.row_width());
        if rows.is_none_or(|x| x > isize::MAX as usize / 8) {
            return bad(format!("{n} nodes with {} tags overflow", self.ntags));
        }
        validate_graph(g, self.ntags, self.luts.classes.len())?;
        validate_bank(self.luts)?;
        if self.load.len() != n {
            return bad(format!("{} loads for {n} nodes", self.load.len()));
        }
        for (v, l) in self.load.iter().enumerate() {
            check_load(v, l)?;
        }
        validate_starts(self.starts, n, self.ntags)?;
        if !self.wires.is_empty() {
            if self.wires.len() != g.arcs() {
                return bad(format!(
                    "{} wire records for {} arcs",
                    self.wires.len(),
                    g.arcs()
                ));
            }
            for (k, (w, a)) in self.wires.iter().zip(g.in_arcs.iter()).enumerate() {
                check_wire(k, w, self.luts.classes[a.class as usize].is_wire())?;
            }
        }
        Ok(())
    }
}

/// The graph and its tag maps.
fn validate_graph(g: &LevelizedTimingCsr, ntags: u32, classes: usize) -> Result<(), ComputeError> {
    let n = g.nodes();
    if g.level_ptr.is_empty() {
        return bad("level_ptr is empty".into());
    }
    pointers("level_ptr", &g.level_ptr, n)?;
    pointers("in_ptr", &g.in_ptr, g.in_arcs.len())?;
    let nmaps = if g.map_ptr.is_empty() {
        if !g.maps.is_empty() {
            return bad("tag map records without map_ptr".into());
        }
        0
    } else {
        pointers("map_ptr", &g.map_ptr, g.maps.len())?;
        g.map_count()
    };
    for (l, w) in g.level_ptr.windows(2).enumerate() {
        let lo = w[0];
        for v in w[0] as usize..w[1] as usize {
            let mut prev = 0;
            for (i, a) in g.incoming(v).iter().enumerate() {
                let k = g.in_ptr[v] as usize + i;
                if a.src >= lo {
                    return bad(format!(
                        "in_arcs[{k}] into node {v} (level {l}): source {} is not on a lower level",
                        a.src
                    ));
                }
                if a.src < prev {
                    return bad(format!(
                        "in_arcs[{k}] into node {v}: sources are not sorted"
                    ));
                }
                prev = a.src;
                if a.class as usize >= classes {
                    return bad(format!("in_arcs[{k}]: class {} of {classes}", a.class));
                }
                if a.map != NONE && a.map as usize >= nmaps {
                    return bad(format!("in_arcs[{k}]: tag map {} of {nmaps}", a.map));
                }
            }
        }
    }
    if g.out_ptr.len() != n + 1 {
        return bad(format!(
            "out_ptr has {} entries for {n} nodes",
            g.out_ptr.len()
        ));
    }
    pointers("out_ptr", &g.out_ptr, g.out_dst.len())?;
    let (ptr, dst) = super::abi::out_adjacency(n, &g.in_ptr, &g.in_arcs);
    if let Some(u) = (0..n).find(|&u| ptr[u + 1] != g.out_ptr[u + 1]) {
        return bad(format!(
            "out_ptr of node {u} does not match the incoming arcs"
        ));
    }
    if let Some(i) = (0..dst.len()).find(|&i| dst[i] != g.out_dst[i]) {
        return bad(format!(
            "out_dst[{i}] is {}, the incoming arcs give {}",
            g.out_dst[i], dst[i]
        ));
    }
    for (i, r) in g.maps.iter().enumerate() {
        if r.src_tag >= ntags || r.dst_tag >= ntags {
            return bad(format!(
                "maps[{i}]: tags {} -> {} of {ntags}",
                r.src_tag, r.dst_tag
            ));
        }
        let ok = r.shift.is_finite()
            && r.close_slew.iter().all(|x| x.is_finite())
            && r.open.iter().all(|&x| !x.is_nan() && x < f64::INFINITY)
            && r.close
                .iter()
                .all(|&x| !x.is_nan() && x > f64::NEG_INFINITY)
            && r.limit
                .iter()
                .all(|&x| !x.is_nan() && x > f64::NEG_INFINITY);
        if !ok {
            return bad(format!(
                "maps[{i}]: shift and closing-edge transitions must be finite, open below \
                 +inf, close and limit above -inf"
            ));
        }
    }
    Ok(())
}

/// The tables and classes of a bank.
fn validate_bank(b: &CompiledLutBank) -> Result<(), ComputeError> {
    let d = &b.data;
    if let Some(i) = d.iter().position(|x| !x.is_finite()) {
        return bad(format!("data[{i}] is not finite"));
    }
    for (t, l) in b.luts.iter().enumerate() {
        let axes = l.axes();
        if l.n[axes..].iter().any(|&n| n != 0) {
            return bad(format!("luts[{t}]: an absent axis precedes an axis in use"));
        }
        for k in 0..axes {
            let (off, n) = (l.off[k] as usize, l.n[k] as usize);
            if l.arg[k] > 2 {
                return bad(format!("luts[{t}]: axis {k} argument {}", l.arg[k]));
            }
            let Some(p) = d.get(off..off + n) else {
                return bad(format!("luts[{t}]: axis {k} out of range"));
            };
            if p.windows(2).any(|w| w[1] <= w[0]) {
                return bad(format!("luts[{t}]: axis {k} is not strictly increasing"));
            }
        }
        if d.get(l.voff as usize..l.voff as usize + l.values())
            .is_none()
        {
            return bad(format!("luts[{t}]: values out of range"));
        }
    }
    let nl = b.luts.len();
    for (i, c) in b.classes.iter().enumerate() {
        if c.flags & !FLAG_MASK != 0 {
            return bad(format!("classes[{i}]: undefined flag bits {:#x}", c.flags));
        }
        let tables = c.delay.iter().chain(&c.slew).chain(&c.setup);
        if let Some(t) = tables.clone().find(|&&t| t != NONE && t as usize >= nl) {
            return bad(format!("classes[{i}]: table {t} of {nl}"));
        }
        if c.is_wire() && (tables.into_iter().any(|&t| t != NONE) || c.flags & NEGATIVE_UNATE != 0)
        {
            return bad(format!(
                "classes[{i}]: a wire class has no tables and does not invert"
            ));
        }
    }
    Ok(())
}

/// One node's loads: finite.
pub(crate) fn check_load(v: usize, l: &[f64; 4]) -> Result<(), ComputeError> {
    if l.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        bad(format!("load of node {v} is not finite"))
    }
}

/// One wire record: finite, and zero unless its arc is a wire arc.
pub(crate) fn check_wire(k: usize, w: &WireRec, wire_arc: bool) -> Result<(), ComputeError> {
    if !w.delay.iter().chain(&w.slew).all(|x| x.is_finite()) {
        return bad(format!("wires[{k}] is not finite"));
    }
    if !wire_arc && !w.is_zero() {
        return bad(format!("wires[{k}] is not zero but its arc is no wire arc"));
    }
    Ok(())
}

/// Start rows for `n` nodes and `ntags` tags (COMPUTE-REQ-0015).
pub(crate) fn validate_starts(s: &StartRows, n: usize, ntags: u32) -> Result<(), ComputeError> {
    let w = ntags as usize * 4;
    if s.arr.len() != s.node.len() * w || s.slew.len() != s.node.len() * 4 {
        return bad(format!(
            "start rows: {} nodes, {} arrivals and {} transitions for {ntags} tags",
            s.node.len(),
            s.arr.len(),
            s.slew.len()
        ));
    }
    if let Some(i) = s.node.windows(2).position(|p| p[1] <= p[0]) {
        return bad(format!("start rows: node[{}] does not increase", i + 1));
    }
    if let Some(&v) = s.node.last()
        && v as usize >= n
    {
        return bad(format!("start rows: node {v} of {n}"));
    }
    for (k, &v) in s.node.iter().enumerate() {
        let (arr, slew) = s.row(k, ntags);
        for (i, &x) in slew.iter().enumerate() {
            if !value_ok(x, i % 2) {
                return bad(format!("start row of node {v}: transition {i} is {x}"));
            }
        }
        for (i, &x) in arr.iter().enumerate() {
            let (tr_an, an) = (i % 4, i % 2);
            if !value_ok(x, an) {
                return bad(format!("start row of node {v}: arrival {i} is {x}"));
            }
            if !absent(x, an) && absent(slew[tr_an], an) {
                return bad(format!(
                    "start row of node {v}: arrival {i} is present without its transition"
                ));
            }
        }
    }
    Ok(())
}
