// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The CPU backend (COMPUTE-REQ-0027): `f64`, the nodes of a level on the threads of the calling
//! thread's rayon pool, each node reduced sequentially in CSR order (COMPUTE-REQ-0017), so that
//! the values do not depend on the number of threads (COMPUTE-REQ-0019). Sessions compute in the
//! host view and borrow the problem's arrays until a patch copies the patched array.

use std::borrow::Cow;
use std::time::Instant;

use rayon::prelude::*;
use rusteda_core::job::Ctx;

use super::abi::{
    ArcClass, ArcValues, InArc, LevelizedTimingCsr, LutDesc, NONE, StartRows, TagMapRec,
    TimingProblem, WireRec,
};
use super::lut::lookup;
use super::state::{NO_PRED, TimingValues, absent, arr_index, canon, slew_index};
use super::validate::{check_load, validate_starts};
use super::{PropagationStats, Readback, Request, Scope, TimingPropagationBackend, TimingSession};
use crate::{BackendKind, ComputeError, Precision};

/// Levels with fewer nodes run on the calling thread.
const PAR_MIN: usize = 64;

/// The CPU backend.
#[derive(Debug)]
pub struct CpuBackend;

impl TimingPropagationBackend for CpuBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Cpu
    }

    fn precision(&self) -> Precision {
        Precision::F64
    }

    fn open<'p>(
        &self,
        problem: &TimingProblem<'p>,
    ) -> Result<Box<dyn TimingSession + 'p>, ComputeError> {
        problem.validate()?;
        Ok(Box::new(CpuSession::new(problem)))
    }
}

/// True when `x` is better than `y` in analysis `an`: smaller early, larger late.
#[inline]
fn better(an: usize, x: f64, y: f64) -> bool {
    if an == 0 { x < y } else { x > y }
}

/// The degraded transition of a wire record term `k` (COMPUTE-REQ-0015).
#[inline]
fn degrade(s: f64, k: f64) -> f64 {
    if k == 0.0 { s } else { (s * s + k * k).sqrt() }
}

/// The start row of every node, or [`NONE`].
fn start_index(n: usize, starts: &StartRows) -> Vec<u32> {
    let mut out = vec![NONE; n];
    for (k, &v) in starts.node.iter().enumerate() {
        out[v as usize] = k as u32;
    }
    out
}

/// The inputs that one node evaluation reads.
struct Inputs<'a> {
    in_ptr: &'a [u32],
    in_arcs: &'a [InArc],
    map_ptr: &'a [u32],
    maps: &'a [TagMapRec],
    classes: &'a [ArcClass],
    luts: &'a [LutDesc],
    data: &'a [f64],
    load: &'a [[f64; 4]],
    wires: &'a [WireRec],
    arc_values: &'a [ArcValues],
    start_row: &'a [u32],
    starts: &'a StartRows,
    ntags: usize,
}

impl Inputs<'_> {
    /// Computes the row of node `v` (COMPUTE-REQ-0017, COMPUTE-REQ-0018) into `arr` (`T * 4`
    /// arrivals), `slew` (4 transitions) and `pred` (`T * 4` predecessors) from the values of the
    /// lower levels, `low_arr` and `low_slew`. Returns the largest magnitude of a present value.
    fn node(
        &self,
        v: usize,
        low_arr: &[f64],
        low_slew: &[f64],
        arr: &mut [f64],
        slew: &mut [f64],
        pred: &mut [u32],
    ) -> f64 {
        let nt = self.ntags;
        match self.start_row[v] {
            NONE => {
                for (i, x) in arr.iter_mut().enumerate() {
                    *x = absent(i % 2);
                }
                for (i, x) in slew.iter_mut().enumerate() {
                    *x = absent(i % 2);
                }
            }
            r => {
                let (a, s) = self.starts.row(r as usize, nt as u32);
                arr.copy_from_slice(a);
                slew.copy_from_slice(s);
            }
        }
        pred.fill(NO_PRED);
        for k in self.in_ptr[v] as usize..self.in_ptr[v + 1] as usize {
            let a = self.in_arcs[k];
            let c = &self.classes[a.class as usize];
            let u = a.src as usize;
            let recs: &[TagMapRec] = if a.map == NONE {
                &[]
            } else {
                let m = a.map as usize;
                &self.maps[self.map_ptr[m] as usize..self.map_ptr[m + 1] as usize]
            };
            for ti in 0..2 {
                for to in 0..2 {
                    if !c.allows(ti, to) {
                        continue;
                    }
                    for an in 0..2 {
                        let s = low_slew[slew_index(u, ti, an)];
                        if s == absent(an) {
                            continue;
                        }
                        let l = self.load[v][to * 2 + an];
                        let (d, q) = if let Some(rec) = self.arc_values.get(k) {
                            let j = (ti * 2 + to) * 2 + an;
                            (rec.delay[j], rec.slew[j])
                        } else if c.is_wire() {
                            match self.wires.get(k) {
                                Some(w) => (w.delay[to * 2 + an], degrade(s, w.slew[to * 2 + an])),
                                None => (0.0, s),
                            }
                        } else {
                            let x = [s, l, l];
                            let d = lookup(self.luts, self.data, c.delay[to], x);
                            let q = lookup(self.luts, self.data, c.slew[to], x);
                            (d, if q < 0.0 { 0.0 } else { q })
                        };
                        let si = to * 2 + an;
                        if better(an, q, slew[si]) {
                            slew[si] = q;
                        }
                        let p = u as u32 | ((ti as u32) << 31);
                        if a.map == NONE {
                            for t in 0..nt {
                                let x = low_arr[arr_index(u, t, ti, an, nt)];
                                if x == absent(an) {
                                    continue;
                                }
                                let cand = x + d;
                                let i = (t * 2 + to) * 2 + an;
                                if better(an, cand, arr[i]) {
                                    arr[i] = cand;
                                    pred[i] = p;
                                }
                            }
                            continue;
                        }
                        for r in recs {
                            let x = low_arr[arr_index(u, r.src_tag as usize, ti, an, nt)];
                            if x == absent(an) {
                                continue;
                            }
                            let mut m = x + r.shift;
                            if m < r.open[an] {
                                continue;
                            }
                            let mut h = r.limit[an];
                            if r.close[an] < f64::INFINITY {
                                let x = [r.close_slew[an], s, l];
                                let e = r.close[an] - lookup(self.luts, self.data, c.setup[ti], x);
                                if e < h {
                                    h = e;
                                }
                            }
                            if m > h {
                                m = h;
                            }
                            let cand = m + d;
                            let i = (r.dst_tag as usize * 2 + to) * 2 + an;
                            if better(an, cand, arr[i]) {
                                arr[i] = cand;
                                pred[i] = p;
                            }
                        }
                    }
                }
            }
        }
        let mut mx = 0.0f64;
        for x in arr.iter_mut().chain(slew.iter_mut()) {
            *x = canon(*x);
            if x.is_finite() && x.abs() > mx {
                mx = x.abs();
            }
        }
        mx
    }
}

/// The larger of two magnitudes.
fn max2(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// A session of the CPU backend.
struct CpuSession<'p> {
    graph: &'p LevelizedTimingCsr<'p>,
    luts: &'p crate::timing::CompiledLutBank<'p>,
    ntags: u32,
    in_arcs: Cow<'p, [InArc]>,
    load: Cow<'p, [[f64; 4]]>,
    starts: Cow<'p, StartRows>,
    wires: &'p [WireRec],
    arc_values: &'p [ArcValues],
    start_row: Vec<u32>,
    /// The level of every node, built for the first cone.
    level_of: Vec<u32>,
    values: TimingValues,
    /// Whether `values` are complete for the inputs before the pending patches.
    resident: bool,
    /// Nodes patched since the last propagation.
    patched: Vec<u32>,
    changed: Option<Vec<u32>>,
}

impl<'p> CpuSession<'p> {
    fn new(p: &TimingProblem<'p>) -> CpuSession<'p> {
        CpuSession {
            graph: p.graph,
            luts: p.luts,
            ntags: p.ntags,
            in_arcs: Cow::Borrowed(&p.graph.in_arcs),
            load: Cow::Borrowed(p.load),
            starts: Cow::Borrowed(p.starts),
            wires: p.wires,
            arc_values: p.arc_values,
            start_row: start_index(p.graph.nodes(), p.starts),
            level_of: Vec::new(),
            values: TimingValues {
                ntags: p.ntags,
                ..TimingValues::default()
            },
            resident: false,
            patched: Vec::new(),
            changed: None,
        }
    }

    fn nodes(&self) -> usize {
        self.graph.nodes()
    }

    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            in_ptr: &self.graph.in_ptr,
            in_arcs: &self.in_arcs,
            map_ptr: &self.graph.map_ptr,
            maps: &self.graph.maps,
            classes: &self.luts.classes,
            luts: &self.luts.luts,
            data: &self.luts.data,
            load: &self.load,
            wires: self.wires,
            arc_values: self.arc_values,
            start_row: &self.start_row,
            starts: &self.starts,
            ntags: self.ntags as usize,
        }
    }

    /// Recomputes every node from level `first` on.
    fn run_levels(
        &mut self,
        first: usize,
        ctx: Ctx<'_>,
        st: &mut PropagationStats,
    ) -> Result<(), ComputeError> {
        let (n, nt) = (self.nodes(), self.ntags);
        if !self.values.fits(n, nt) {
            let w = n * nt as usize * 4;
            self.values = TimingValues {
                ntags: nt,
                arr: vec![0.0; w],
                slew: vec![0.0; n * 4],
                pred: vec![NO_PRED; w],
            };
        }
        let mut values = std::mem::take(&mut self.values);
        let r = levels(
            &self.inputs(),
            &self.graph.level_ptr,
            first,
            &mut values,
            ctx,
            st,
        );
        self.values = values;
        r
    }

    /// Recomputes the cone of `seeds` and the patched nodes.
    fn run_cone(
        &mut self,
        seeds: &[u32],
        ctx: Ctx<'_>,
        st: &mut PropagationStats,
    ) -> Result<Vec<u32>, ComputeError> {
        if self.level_of.is_empty() {
            self.level_of = self.graph.level_of();
        }
        let mut values = std::mem::take(&mut self.values);
        let start: Vec<u32> = seeds.iter().chain(&self.patched).copied().collect();
        let r = cone(
            &self.inputs(),
            self.graph,
            &self.level_of,
            &start,
            &mut values,
            ctx,
            st,
        );
        self.values = values;
        r
    }

    /// The lowest level of a patched node, if any.
    fn lowest_patched_level(&mut self) -> Option<usize> {
        if self.patched.is_empty() {
            return None;
        }
        if self.level_of.is_empty() {
            self.level_of = self.graph.level_of();
        }
        self.patched
            .iter()
            .map(|&v| self.level_of[v as usize] as usize)
            .min()
    }

    fn check_rows(&self, rb: Readback<'_>) -> Result<(), ComputeError> {
        if let Readback::Rows(rows) = rb {
            let n = self.nodes();
            if let Some(&r) = rows.iter().find(|&&r| r as usize >= n) {
                return Err(ComputeError::Invalid(format!("readback row {r} of {n}")));
            }
        }
        Ok(())
    }
}

/// Propagates the levels from `first` on into `values` (COMPUTE-REQ-0017, COMPUTE-REQ-0026).
fn levels(
    inp: &Inputs<'_>,
    level_ptr: &[u32],
    first: usize,
    values: &mut TimingValues,
    ctx: Ctx<'_>,
    st: &mut PropagationStats,
) -> Result<(), ComputeError> {
    let w = inp.ntags * 4;
    let nlev = level_ptr.len() - 1;
    let mut mx = 0.0f64;
    for l in first..nlev {
        ctx.checkpoint()?;
        let (b, e) = (level_ptr[l] as usize, level_ptr[l + 1] as usize);
        let count = e - b;
        let (low_arr, high_arr) = values.arr.split_at_mut(b * w);
        let (low_slew, high_slew) = values.slew.split_at_mut(b * 4);
        let high_pred = &mut values.pred[b * w..];
        let (low_arr, low_slew) = (&*low_arr, &*low_slew);
        let a = &mut high_arr[..count * w];
        let s = &mut high_slew[..count * 4];
        let p = &mut high_pred[..count * w];
        let m = if count >= PAR_MIN {
            a.par_chunks_mut(w)
                .zip(s.par_chunks_mut(4))
                .zip(p.par_chunks_mut(w))
                .enumerate()
                .with_min_len(32)
                .map(|(i, ((a, s), p))| inp.node(b + i, low_arr, low_slew, a, s, p))
                .reduce(|| 0.0, max2)
        } else {
            a.chunks_mut(w)
                .zip(s.chunks_mut(4))
                .zip(p.chunks_mut(w))
                .enumerate()
                .map(|(i, ((a, s), p))| inp.node(b + i, low_arr, low_slew, a, s, p))
                .fold(0.0, max2)
        };
        mx = max2(mx, m);
        st.levels += 1;
        st.nodes_evaluated += count as u64;
        ctx.progress("propagate", (l + 1 - first) as u64, (nlev - first) as u64);
    }
    st.nodes_changed = st.nodes_evaluated;
    st.max_abs_ns = mx;
    Ok(())
}

/// Propagates the forward cone of `start` with bitwise change detection (COMPUTE-REQ-0021);
/// returns the nodes whose values changed, increasing.
fn cone(
    inp: &Inputs<'_>,
    g: &LevelizedTimingCsr,
    level_of: &[u32],
    start: &[u32],
    values: &mut TimingValues,
    ctx: Ctx<'_>,
    st: &mut PropagationStats,
) -> Result<Vec<u32>, ComputeError> {
    let n = g.nodes();
    let w = inp.ntags * 4;
    let nlev = g.levels();
    let mut dirty = vec![false; n];
    let mut bucket: Vec<Vec<u32>> = vec![Vec::new(); nlev];
    for &v in start {
        if !dirty[v as usize] {
            dirty[v as usize] = true;
            bucket[level_of[v as usize] as usize].push(v);
        }
    }
    let first = bucket.iter().position(|b| !b.is_empty()).unwrap_or(nlev);
    let mut changed = Vec::new();
    let (mut ta, mut ts, mut tp) = (Vec::new(), Vec::new(), Vec::new());
    let mut mx = 0.0f64;
    for l in first..nlev {
        if bucket[l].is_empty() {
            continue;
        }
        ctx.checkpoint()?;
        let mut list = std::mem::take(&mut bucket[l]);
        list.sort_unstable();
        let b = g.level_ptr[l] as usize;
        ta.resize(list.len() * w, 0.0);
        ts.resize(list.len() * 4, 0.0);
        tp.resize(list.len() * w, NO_PRED);
        let (low_arr, low_slew) = (&values.arr[..b * w], &values.slew[..b * 4]);
        let rows = ta
            .par_chunks_mut(w)
            .zip(ts.par_chunks_mut(4))
            .zip(tp.par_chunks_mut(w))
            .zip(list.par_iter())
            .with_min_len(32);
        let m = rows
            .map(|(((a, s), p), &v)| inp.node(v as usize, low_arr, low_slew, a, s, p))
            .reduce(|| 0.0, max2);
        mx = max2(mx, m);
        for (i, &v) in list.iter().enumerate() {
            let v = v as usize;
            let (na, ns, np) = (
                &ta[i * w..(i + 1) * w],
                &ts[i * 4..(i + 1) * 4],
                &tp[i * w..(i + 1) * w],
            );
            let (oa, os, op) = (
                &mut values.arr[v * w..(v + 1) * w],
                &mut values.slew[v * 4..(v + 1) * 4],
                &mut values.pred[v * w..(v + 1) * w],
            );
            let same = na
                .iter()
                .zip(oa.iter())
                .all(|(x, y)| x.to_bits() == y.to_bits())
                && ns
                    .iter()
                    .zip(os.iter())
                    .all(|(x, y)| x.to_bits() == y.to_bits())
                && np == &op[..];
            if same {
                continue;
            }
            oa.copy_from_slice(na);
            os.copy_from_slice(ns);
            op.copy_from_slice(np);
            changed.push(v as u32);
            for &d in &g.out_dst[g.out_ptr[v] as usize..g.out_ptr[v + 1] as usize] {
                if !dirty[d as usize] {
                    dirty[d as usize] = true;
                    bucket[level_of[d as usize] as usize].push(d);
                }
            }
        }
        st.levels += 1;
        st.nodes_evaluated += list.len() as u64;
        ctx.progress("propagate", (l + 1 - first) as u64, (nlev - first) as u64);
    }
    st.nodes_changed = changed.len() as u64;
    st.max_abs_ns = mx;
    Ok(changed)
}

impl TimingSession for CpuSession<'_> {
    fn kind(&self) -> BackendKind {
        BackendKind::Cpu
    }

    fn precision(&self) -> Precision {
        Precision::F64
    }

    fn propagate(&mut self, req: &Request<'_>) -> Result<PropagationStats, ComputeError> {
        let t0 = Instant::now();
        let device = format!("{} threads", rayon::current_num_threads());
        let mut st = PropagationStats::new(BackendKind::Cpu, device, Precision::F64);
        self.check_rows(req.readback)?;
        let needs_resident = !matches!(req.scope, Scope::All);
        if needs_resident && !self.resident {
            return Err(ComputeError::Invalid(
                "no resident values: propagate every node first".into(),
            ));
        }
        let n = self.nodes();
        let r = match req.scope {
            Scope::All => self.run_levels(0, req.ctx, &mut st).map(|()| None),
            Scope::FromLevel(l) => {
                let first = (l as usize).min(self.graph.levels());
                let first = self.lowest_patched_level().map_or(first, |p| p.min(first));
                self.run_levels(first, req.ctx, &mut st).map(|()| None)
            }
            Scope::Cone(seeds) => {
                if let Some(&s) = seeds.iter().find(|&&s| s as usize >= n) {
                    return Err(ComputeError::Invalid(format!("cone seed {s} of {n}")));
                }
                self.run_cone(seeds, req.ctx, &mut st).map(Some)
            }
        };
        match r {
            Ok(changed) => {
                self.changed = changed;
                self.patched.clear();
                self.resident = true;
            }
            Err(e) => {
                self.changed = None;
                self.resident = false;
                return Err(e);
            }
        }
        st.kernel_s = t0.elapsed().as_secs_f64();
        st.total_s = st.kernel_s;
        Ok(st)
    }

    fn read(&mut self, readback: Readback<'_>) -> Result<(), ComputeError> {
        if !self.resident {
            return Err(ComputeError::Invalid("no values to read".into()));
        }
        self.check_rows(readback)
    }

    fn values(&self) -> &TimingValues {
        &self.values
    }

    fn changed(&self) -> Option<&[u32]> {
        self.changed.as_deref()
    }

    fn set_loads(&mut self, edits: &[(u32, [f64; 4])]) -> Result<(), ComputeError> {
        if !self.arc_values.is_empty() {
            return Err(ComputeError::Invalid(
                "re-prepare RC arc values before patching inputs".into(),
            ));
        }
        let n = self.nodes();
        for &(v, l) in edits {
            if v as usize >= n {
                return Err(ComputeError::Invalid(format!("load of node {v} of {n}")));
            }
            check_load(v as usize, &l)?;
        }
        let load = self.load.to_mut();
        for &(v, l) in edits {
            load[v as usize] = l;
            self.patched.push(v);
        }
        Ok(())
    }

    fn set_arc_classes(&mut self, edits: &[(u32, u32)]) -> Result<(), ComputeError> {
        if !self.arc_values.is_empty() {
            return Err(ComputeError::Invalid(
                "re-prepare RC arc values before patching inputs".into(),
            ));
        }
        let (na, nc) = (self.in_arcs.len(), self.luts.classes.len());
        for &(k, c) in edits {
            if k as usize >= na || c as usize >= nc {
                return Err(ComputeError::Invalid(format!(
                    "class {c} of {nc} for arc {k} of {na}"
                )));
            }
            if let Some(w) = self.wires.get(k as usize)
                && !self.luts.classes[c as usize].is_wire()
                && !w.is_zero()
            {
                return Err(ComputeError::Invalid(format!(
                    "arc {k} has a wire record but class {c} is no wire class"
                )));
            }
        }
        let in_ptr = &self.graph.in_ptr;
        let arcs = self.in_arcs.to_mut();
        for &(k, c) in edits {
            arcs[k as usize].class = c;
            let v = in_ptr.partition_point(|&p| p <= k) - 1;
            self.patched.push(v as u32);
        }
        Ok(())
    }

    fn set_starts(&mut self, starts: &StartRows) -> Result<(), ComputeError> {
        if !self.arc_values.is_empty() {
            return Err(ComputeError::Invalid(
                "re-prepare RC arc values before patching inputs".into(),
            ));
        }
        let n = self.nodes();
        validate_starts(starts, n, self.ntags)?;
        diff_starts(&self.starts, starts, self.ntags, &mut self.patched);
        self.start_row = start_index(n, starts);
        self.starts = Cow::Owned(starts.clone());
        Ok(())
    }

    fn write_values(&mut self, values: &TimingValues) -> Result<(), ComputeError> {
        let (n, nt) = (self.nodes(), self.ntags);
        if !values.fits(n, nt) {
            return Err(ComputeError::Invalid(format!(
                "values do not fit {n} nodes and {nt} tags"
            )));
        }
        if values.arr.iter().chain(&values.slew).any(|x| x.is_nan()) {
            return Err(ComputeError::Invalid("values hold NaN".into()));
        }
        self.values = TimingValues {
            ntags: nt,
            arr: values.arr.iter().map(|&x| canon(x)).collect(),
            slew: values.slew.iter().map(|&x| canon(x)).collect(),
            pred: values.pred.clone(),
        };
        self.resident = true;
        self.changed = None;
        Ok(())
    }

    fn into_values(self: Box<Self>) -> TimingValues {
        self.values
    }
}

/// Appends to `out` the nodes whose start row differs in any bit between `old` and `new`
/// (including rows only one of them has).
pub(super) fn diff_starts(old: &StartRows, new: &StartRows, ntags: u32, out: &mut Vec<u32>) {
    let (mut i, mut j) = (0, 0);
    let same = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits());
    while i < old.len() || j < new.len() {
        if j == new.len() || (i < old.len() && old.node[i] < new.node[j]) {
            out.push(old.node[i]);
            i += 1;
        } else if i == old.len() || new.node[j] < old.node[i] {
            out.push(new.node[j]);
            j += 1;
        } else {
            let ((oa, os), (na, ns)) = (old.row(i, ntags), new.row(j, ntags));
            if !same(oa, na) || !same(os, ns) {
                out.push(old.node[i]);
            }
            i += 1;
            j += 1;
        }
    }
}
