// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The CUDA backend of the timing task (feature `cuda`, COMPUTE-REQ-0028): `f64` (compiled with
//! `-DREAL_F64` and `--fmad=false`, bit-identical to the CPU backend) and `f32` (default NVRTC
//! options), one stream per session, inputs and values resident on the device across levels and
//! across propagations, and full readback through page-locked host memory that is not
//! write-combined.

// Backend glue: kernel launches and device-to-pinned copies through the CUDA driver API (AGENTS
// rules 8.4); every `unsafe` block carries its justification.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use rusteda_core::job::Ctx;

use super::abi::{InArc, NONE, StartRows, TagMapRec, TimingProblem};
use super::cpu::diff_starts;
use super::state::{NO_PRED, TimingValues, canon};
use super::validate::{check_load, validate_starts};
use super::{PropagationStats, Readback, Request, Scope, TimingPropagationBackend, TimingSession};
use crate::cuda::{
    CudaFunction, CudaSlice, CudaStream, Device, DeviceRepr, PinnedHostSlice, PushKernelArg,
    ValidAsZeroBits, device, launch_1d,
};
use crate::{BackendKind, ComputeError, Precision};

const SRC: &str = include_str!("kernels/timing.cu");
const BLOCK: u32 = 128;

trait Real: DeviceRepr + ValidAsZeroBits + Copy + Send + Sync + Default + 'static {
    const OPTIONS: &'static [&'static str];
    const PREC: Precision;
    fn from_f64(x: f64) -> Self;
    fn to_f64(self) -> f64;
}

impl Real for f64 {
    // No fused multiply-add contraction (COMPUTE-REQ-0019, COMPUTE-REQ-0028): every operation
    // rounds once in IEEE 754 double precision, matching the CPU backend bit for bit.
    const OPTIONS: &'static [&'static str] = &["--fmad=false", "-DREAL_F64"];
    const PREC: Precision = Precision::F64;
    fn from_f64(x: f64) -> f64 {
        x
    }
    fn to_f64(self) -> f64 {
        self
    }
}

impl Real for f32 {
    const OPTIONS: &'static [&'static str] = &[];
    const PREC: Precision = Precision::F32;
    fn from_f64(x: f64) -> f32 {
        x as f32
    }
    fn to_f64(self) -> f64 {
        self as f64
    }
}

/// The CUDA backend for one precision.
#[derive(Debug)]
pub struct CudaBackend {
    precision: Precision,
}

pub(super) static CUDA_F64: CudaBackend = CudaBackend {
    precision: Precision::F64,
};
pub(super) static CUDA_F32: CudaBackend = CudaBackend {
    precision: Precision::F32,
};

pub(super) fn backend(precision: Precision) -> &'static dyn TimingPropagationBackend {
    match precision {
        Precision::F64 => &CUDA_F64,
        Precision::F32 => &CUDA_F32,
    }
}

/// Compiles the timing kernels for `precision` on the default device (COMPUTE-REQ-0029).
pub(super) fn compile(precision: Precision) -> Result<(), ComputeError> {
    let dev = device().map_err(|e| ComputeError::Unavailable(e.0))?;
    let opts = match precision {
        Precision::F64 => f64::OPTIONS,
        Precision::F32 => f32::OPTIONS,
    };
    for name in ["propagate_level", "propagate_cone", "gather_rows"] {
        dev.function_with(SRC, opts, name)
            .map_err(|e| ComputeError::Compile(e.0))?;
    }
    Ok(())
}

impl TimingPropagationBackend for CudaBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Cuda
    }

    fn precision(&self) -> Precision {
        self.precision
    }

    fn open<'p>(
        &self,
        problem: &TimingProblem<'p>,
    ) -> Result<Box<dyn TimingSession + 'p>, ComputeError> {
        problem.validate()?;
        let dev = device().map_err(|e| ComputeError::Unavailable(e.0))?;
        match self.precision {
            Precision::F64 => Ok(Box::new(CudaSession::<f64>::open(problem, dev)?)),
            Precision::F32 => Ok(Box::new(CudaSession::<f32>::open(problem, dev)?)),
        }
    }
}

fn conv<R: Real>(x: &[f64]) -> Vec<R> {
    if x.len() >= 4096 {
        x.par_iter().map(|&v| R::from_f64(v)).collect()
    } else {
        x.iter().map(|&v| R::from_f64(v)).collect()
    }
}

fn start_index(n: usize, starts: &StartRows) -> Vec<u32> {
    let mut out = vec![NONE; n];
    for (k, &v) in starts.node.iter().enumerate() {
        out[v as usize] = k as u32;
    }
    out
}

fn pack_maps<R: Real>(maps: &[TagMapRec]) -> (Vec<u32>, Vec<R>) {
    let mut tags = Vec::with_capacity(maps.len() * 2);
    let mut vals = Vec::with_capacity(maps.len() * 9);
    for r in maps {
        tags.push(r.src_tag);
        tags.push(r.dst_tag);
        vals.push(R::from_f64(r.shift));
        vals.push(R::from_f64(r.open[0]));
        vals.push(R::from_f64(r.open[1]));
        vals.push(R::from_f64(r.close[0]));
        vals.push(R::from_f64(r.close[1]));
        vals.push(R::from_f64(r.close_slew[0]));
        vals.push(R::from_f64(r.close_slew[1]));
        vals.push(R::from_f64(r.limit[0]));
        vals.push(R::from_f64(r.limit[1]));
    }
    (tags, vals)
}

fn upload_slice<T: DeviceRepr + ValidAsZeroBits>(
    stream: &Arc<CudaStream>,
    data: &[T],
) -> Result<CudaSlice<T>, ComputeError> {
    if data.is_empty() {
        stream
            .alloc_zeros::<T>(1)
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))
    } else {
        stream
            .clone_htod(data)
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))
    }
}

fn alloc_zeros<T: DeviceRepr + ValidAsZeroBits>(
    stream: &Arc<CudaStream>,
    len: usize,
) -> Result<CudaSlice<T>, ComputeError> {
    stream
        .alloc_zeros::<T>(len.max(1))
        .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))
}

fn read_slice<T: DeviceRepr>(
    stream: &Arc<CudaStream>,
    s: &CudaSlice<T>,
) -> Result<Vec<T>, ComputeError> {
    let v = stream
        .clone_dtoh(s)
        .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
    stream
        .synchronize()
        .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
    Ok(v)
}

/// Which rows the last propagation replaced, for `Readback::Changed` (COMPUTE-REQ-0022).
enum Replaced {
    All,
    FromNode(usize),
    Cone,
}

/// Page-locked host staging buffers for full readback (COMPUTE-REQ-0004, COMPUTE-REQ-0028).
struct PinnedStaging<R: Real> {
    arr: PinnedHostSlice<R>,
    slew: PinnedHostSlice<R>,
    pred: PinnedHostSlice<u32>,
}

/// Device buffers allocated on first `Cone` propagation.
struct ConeBuffers<R: Real> {
    out_ptr: CudaSlice<u32>,
    out_dst: CudaSlice<u32>,
    dirty: CudaSlice<u8>,
    changed_flags: CudaSlice<u8>,
    stats: CudaSlice<u64>,
    scratch_arr: CudaSlice<R>,
    scratch_pred: CudaSlice<u32>,
    level_of: Vec<u32>,
}

struct CudaSession<'p, R: Real> {
    problem: TimingProblem<'p>,
    dev: &'static Device,
    stream: Arc<CudaStream>,
    fn_level: CudaFunction,
    fn_cone: CudaFunction,
    fn_gather: CudaFunction,
    // Host mirrors of patchable inputs.
    in_arcs_h: Vec<InArc>,
    load_h: Vec<[f64; 4]>,
    starts_h: StartRows,
    start_row_h: Vec<u32>,
    // Resident device inputs and values.
    d_in_ptr: CudaSlice<u32>,
    d_in_arcs: CudaSlice<u32>,
    d_map_ptr: CudaSlice<u32>,
    d_map_tags: CudaSlice<u32>,
    d_map_vals: CudaSlice<R>,
    d_classes: CudaSlice<u32>,
    d_luts: CudaSlice<u32>,
    d_data: CudaSlice<R>,
    d_load: CudaSlice<R>,
    d_wires: CudaSlice<R>,
    has_wires: u32,
    d_start_row: CudaSlice<u32>,
    d_start_arr: CudaSlice<R>,
    d_start_slew: CudaSlice<R>,
    d_arr: CudaSlice<R>,
    d_slew: CudaSlice<R>,
    d_pred: CudaSlice<u32>,
    d_max_bits: CudaSlice<u64>,
    cone_bufs: Option<ConeBuffers<R>>,
    pinned: Option<PinnedStaging<R>>,
    values: TimingValues,
    resident: bool,
    patched: Vec<u32>,
    changed: Option<Vec<u32>>,
    last_replaced: Replaced,
    pending_upload_s: f64,
}

/// Estimates the device memory in bytes that a session of `p` needs (COMPUTE-REQ-0028).
fn session_bytes<R: Real>(p: &TimingProblem<'_>) -> usize {
    let r = size_of::<R>();
    let n = p.nodes();
    let w = p.row_width();
    let g = p.graph;
    let max_lvl = g
        .level_ptr
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as usize)
        .max()
        .unwrap_or(0);
    let fixed = (g.in_ptr.len() + g.map_ptr.len() + g.out_ptr.len() + g.out_dst.len()) * 4
        + g.in_arcs.len() * 12
        + g.maps.len() * (8 + 9 * r)
        + p.luts.classes.len() * 28
        + p.luts.luts.len() * 40
        + p.luts.data.len() * r
        + p.wires.len() * 8 * r
        + p.starts.arr.len() * r
        + p.starts.slew.len() * r;
    let per_node = n * (4 + 4 * r + w * r + 4 * r + w * 4 + 2);
    let cone_scratch = max_lvl * w * (r + 4);
    fixed + per_node + cone_scratch + 4096
}

impl<'p, R: Real> CudaSession<'p, R> {
    fn open(p: &TimingProblem<'p>, dev: &'static Device) -> Result<Self, ComputeError> {
        // Compile kernels before measuring upload or checking free device memory.
        let fn_level = dev
            .function_with(SRC, R::OPTIONS, "propagate_level")
            .map_err(|e| ComputeError::Compile(e.0))?;
        let fn_cone = dev
            .function_with(SRC, R::OPTIONS, "propagate_cone")
            .map_err(|e| ComputeError::Compile(e.0))?;
        let fn_gather = dev
            .function_with(SRC, R::OPTIONS, "gather_rows")
            .map_err(|e| ComputeError::Compile(e.0))?;
        let needed = session_bytes::<R>(p);
        let (free, _) = dev.mem_info()?;
        if needed > free {
            return Err(ComputeError::TooLarge(format!(
                "session needs {needed} bytes, {free} bytes free on device"
            )));
        }
        let t0 = Instant::now();
        let stream = dev.new_stream()?;
        let g = p.graph;
        let n = p.nodes();
        let w = p.row_width();
        let in_arcs_h = g.in_arcs.to_vec();
        let load_h = p.load.to_vec();
        let starts_h = p.starts.clone();
        let start_row_h = start_index(n, &starts_h);
        let (map_tags, map_vals) = pack_maps::<R>(&g.maps);
        let load_flat: &[f64] = bytemuck::cast_slice(&load_h);
        let wires_flat: &[f64] = bytemuck::cast_slice(p.wires);
        let d_in_ptr = upload_slice(&stream, &g.in_ptr)?;
        let d_in_arcs = upload_slice(&stream, bytemuck::cast_slice(&in_arcs_h))?;
        let d_map_ptr = upload_slice(&stream, &g.map_ptr)?;
        let d_map_tags = upload_slice(&stream, &map_tags)?;
        let d_map_vals = upload_slice(&stream, &map_vals)?;
        let d_classes = upload_slice(&stream, bytemuck::cast_slice(&p.luts.classes))?;
        let d_luts = upload_slice(&stream, bytemuck::cast_slice(&p.luts.luts))?;
        let d_data = upload_slice(&stream, &conv::<R>(&p.luts.data))?;
        let d_load = upload_slice(&stream, &conv::<R>(load_flat))?;
        let d_wires = upload_slice(&stream, &conv::<R>(wires_flat))?;
        let has_wires = u32::from(!p.wires.is_empty());
        let d_start_row = upload_slice(&stream, &start_row_h)?;
        let d_start_arr = upload_slice(&stream, &conv::<R>(&starts_h.arr))?;
        let d_start_slew = upload_slice(&stream, &conv::<R>(&starts_h.slew))?;
        let d_arr = alloc_zeros::<R>(&stream, n * w)?;
        let d_slew = alloc_zeros::<R>(&stream, n * 4)?;
        let d_pred = alloc_zeros::<u32>(&stream, n * w)?;
        let d_max_bits = alloc_zeros::<u64>(&stream, 1)?;
        stream
            .synchronize()
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        let pending_upload_s = t0.elapsed().as_secs_f64();
        Ok(CudaSession {
            problem: *p,
            dev,
            stream,
            fn_level,
            fn_cone,
            fn_gather,
            in_arcs_h,
            load_h,
            starts_h,
            start_row_h,
            d_in_ptr,
            d_in_arcs,
            d_map_ptr,
            d_map_tags,
            d_map_vals,
            d_classes,
            d_luts,
            d_data,
            d_load,
            d_wires,
            has_wires,
            d_start_row,
            d_start_arr,
            d_start_slew,
            d_arr,
            d_slew,
            d_pred,
            d_max_bits,
            cone_bufs: None,
            pinned: None,
            values: TimingValues {
                ntags: p.ntags,
                ..TimingValues::default()
            },
            resident: false,
            patched: Vec::new(),
            changed: None,
            last_replaced: Replaced::All,
            pending_upload_s,
        })
    }

    fn nodes(&self) -> usize {
        self.problem.nodes()
    }

    fn ntags(&self) -> u32 {
        self.problem.ntags
    }

    fn row_width(&self) -> usize {
        self.problem.row_width()
    }

    fn ensure_host_values(&mut self) {
        let (n, nt, w) = (self.nodes(), self.ntags(), self.row_width());
        if !self.values.fits(n, nt) {
            self.values = TimingValues {
                ntags: nt,
                arr: vec![0.0; n * w],
                slew: vec![0.0; n * 4],
                pred: vec![NO_PRED; n * w],
            };
        }
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

    fn ensure_cone_buffers(&mut self) -> Result<(), ComputeError> {
        if self.cone_bufs.is_none() {
            let g = self.problem.graph;
            let n = self.nodes();
            let w = self.row_width();
            let max_lvl = g
                .level_ptr
                .windows(2)
                .map(|pair| (pair[1] - pair[0]) as usize)
                .max()
                .unwrap_or(0);
            self.cone_bufs = Some(ConeBuffers {
                out_ptr: upload_slice(&self.stream, &g.out_ptr)?,
                out_dst: upload_slice(&self.stream, &g.out_dst)?,
                dirty: alloc_zeros::<u8>(&self.stream, n)?,
                changed_flags: alloc_zeros::<u8>(&self.stream, n)?,
                stats: alloc_zeros::<u64>(&self.stream, 3)?,
                scratch_arr: alloc_zeros::<R>(&self.stream, max_lvl * w)?,
                scratch_pred: alloc_zeros::<u32>(&self.stream, max_lvl * w)?,
                level_of: g.level_of(),
            });
        }
        Ok(())
    }

    fn lowest_patched_level(&mut self) -> Result<Option<usize>, ComputeError> {
        if self.patched.is_empty() {
            return Ok(None);
        }
        self.ensure_cone_buffers()?;
        let level_of = &self.cone_bufs.as_ref().expect("just built").level_of;
        Ok(self
            .patched
            .iter()
            .map(|&v| level_of[v as usize] as usize)
            .min())
    }

    fn run_levels(
        &mut self,
        first: usize,
        ctx: Ctx<'_>,
        st: &mut PropagationStats,
    ) -> Result<(), ComputeError> {
        let lp = &self.problem.graph.level_ptr;
        let nlev = self.problem.graph.levels();
        let nt = self.ntags();
        self.d_max_bits = upload_slice(&self.stream, &[0u64])?;
        for l in first..nlev {
            ctx.checkpoint()?;
            let (base, end) = (lp[l], lp[l + 1]);
            let count = end - base;
            st.levels += 1;
            if count > 0 {
                let mut b = self.stream.launch_builder(&self.fn_level);
                b.arg(&mut self.d_arr)
                    .arg(&mut self.d_slew)
                    .arg(&mut self.d_pred)
                    .arg(&self.d_in_ptr)
                    .arg(&self.d_in_arcs)
                    .arg(&self.d_map_ptr)
                    .arg(&self.d_map_tags)
                    .arg(&self.d_map_vals)
                    .arg(&self.d_classes)
                    .arg(&self.d_luts)
                    .arg(&self.d_data)
                    .arg(&self.d_load)
                    .arg(&self.d_wires)
                    .arg(&self.has_wires)
                    .arg(&self.d_start_row)
                    .arg(&self.d_start_arr)
                    .arg(&self.d_start_slew)
                    .arg(&mut self.d_max_bits)
                    .arg(&base)
                    .arg(&count)
                    .arg(&nt);
                // SAFETY: the problem and all patches passed `validate`: every index, pointer
                // range, table offset and start row is in bounds; every incoming arc of level `l`
                // has `src < base`, so threads of this launch read only levels `< l` and write
                // only their own node `v in base .. base + count`.
                unsafe { b.launch(launch_1d(count as usize, BLOCK)) }
                    .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
                st.launches += 1;
                st.nodes_evaluated += u64::from(count);
            }
            ctx.progress("propagate", (l + 1 - first) as u64, (nlev - first) as u64);
        }
        st.nodes_changed = st.nodes_evaluated;
        let max_bits = read_slice(&self.stream, &self.d_max_bits)?[0];
        st.max_abs_ns = f64::from_bits(max_bits);
        Ok(())
    }

    fn run_cone(
        &mut self,
        seeds: &[u32],
        ctx: Ctx<'_>,
        st: &mut PropagationStats,
    ) -> Result<Vec<u32>, ComputeError> {
        let n = self.nodes();
        if n == 0 || (seeds.is_empty() && self.patched.is_empty()) {
            return Ok(Vec::new());
        }
        self.ensure_cone_buffers()?;
        let nlev = self.problem.graph.levels();
        let nt = self.ntags();
        let mut dirty_h = vec![0u8; n];
        let mut first = nlev;
        {
            let level_of = &self.cone_bufs.as_ref().expect("just built").level_of;
            for &v in seeds.iter().chain(&self.patched) {
                dirty_h[v as usize] = 1;
                let l = level_of[v as usize] as usize;
                if l < first {
                    first = l;
                }
            }
        }
        if first >= nlev {
            return Ok(Vec::new());
        }
        let cb = self.cone_bufs.as_mut().expect("just built");
        cb.dirty = upload_slice(&self.stream, &dirty_h)?;
        cb.changed_flags = alloc_zeros::<u8>(&self.stream, n)?;
        cb.stats = upload_slice(&self.stream, &[0u64; 3])?;
        let lp = &self.problem.graph.level_ptr;
        for l in first..nlev {
            ctx.checkpoint()?;
            let (base, end) = (lp[l], lp[l + 1]);
            let count = end - base;
            st.levels += 1;
            if count > 0 {
                let mut b = self.stream.launch_builder(&self.fn_cone);
                b.arg(&mut self.d_arr)
                    .arg(&mut self.d_slew)
                    .arg(&mut self.d_pred)
                    .arg(&mut cb.scratch_arr)
                    .arg(&mut cb.scratch_pred)
                    .arg(&self.d_in_ptr)
                    .arg(&self.d_in_arcs)
                    .arg(&self.d_map_ptr)
                    .arg(&self.d_map_tags)
                    .arg(&self.d_map_vals)
                    .arg(&self.d_classes)
                    .arg(&self.d_luts)
                    .arg(&self.d_data)
                    .arg(&self.d_load)
                    .arg(&self.d_wires)
                    .arg(&self.has_wires)
                    .arg(&self.d_start_row)
                    .arg(&self.d_start_arr)
                    .arg(&self.d_start_slew)
                    .arg(&cb.out_ptr)
                    .arg(&cb.out_dst)
                    .arg(&mut cb.dirty)
                    .arg(&mut cb.changed_flags)
                    .arg(&mut cb.stats)
                    .arg(&base)
                    .arg(&count)
                    .arg(&nt);
                // SAFETY: `scratch_arr` and `scratch_pred` hold `max_level_width * w` elements;
                // each thread `pos < count` uses its own slice `pos * w .. (pos + 1) * w`, reads
                // only levels `< l`, and updates only node `v = base + pos` and `dirty` entries of
                // outgoing neighbors on levels `> l`.
                unsafe { b.launch(launch_1d(count as usize, BLOCK)) }
                    .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
                st.launches += 1;
            }
            ctx.progress("propagate", (l + 1 - first) as u64, (nlev - first) as u64);
        }
        let stats = read_slice(&self.stream, &cb.stats)?;
        st.nodes_evaluated = stats[0];
        st.nodes_changed = stats[1];
        st.max_abs_ns = f64::from_bits(stats[2]);
        if stats[1] == 0 {
            return Ok(Vec::new());
        }
        let flags = read_slice(&self.stream, &cb.changed_flags)?;
        let changed: Vec<u32> = flags
            .iter()
            .enumerate()
            .filter_map(|(v, &f)| (f != 0).then_some(v as u32))
            .collect();
        Ok(changed)
    }

    /// Copies every row from the device to `self.values` through page-locked staging memory
    /// (COMPUTE-REQ-0022, COMPUTE-REQ-0028).
    fn read_all(&mut self) -> Result<(), ComputeError> {
        self.ensure_host_values();
        let n = self.nodes();
        if n == 0 {
            return Ok(());
        }
        let w = self.row_width();
        if self.pinned.is_none() {
            self.pinned = Some(PinnedStaging {
                arr: self.dev.pinned::<R>(n * w)?,
                slew: self.dev.pinned::<R>(n * 4)?,
                pred: self.dev.pinned::<u32>(n * w)?,
            });
        }
        let pin = self.pinned.as_mut().expect("just allocated");
        self.stream
            .memcpy_dtoh(&self.d_arr, &mut pin.arr)
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        self.stream
            .memcpy_dtoh(&self.d_slew, &mut pin.slew)
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        self.stream
            .memcpy_dtoh(&self.d_pred, &mut pin.pred)
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        self.stream
            .synchronize()
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        let arr_s = pin
            .arr
            .as_slice()
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        let slew_s = pin
            .slew
            .as_slice()
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        let pred_s = pin
            .pred
            .as_slice()
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        for (dst, &src) in self.values.arr.iter_mut().zip(arr_s) {
            *dst = canon(src.to_f64());
        }
        for (dst, &src) in self.values.slew.iter_mut().zip(slew_s) {
            *dst = canon(src.to_f64());
        }
        self.values.pred.copy_from_slice(&pred_s[..n * w]);
        Ok(())
    }

    /// Copies the rows of `rows` from the device to `self.values` (COMPUTE-REQ-0022).
    fn read_rows(&mut self, rows: &[u32]) -> Result<(), ComputeError> {
        self.ensure_host_values();
        if rows.is_empty() || self.nodes() == 0 {
            return Ok(());
        }
        let k = rows.len();
        let w = self.row_width();
        let d_rows = upload_slice(&self.stream, rows)?;
        let mut g_arr = alloc_zeros::<R>(&self.stream, k * w)?;
        let mut g_slew = alloc_zeros::<R>(&self.stream, k * 4)?;
        let mut g_pred = alloc_zeros::<u32>(&self.stream, k * w)?;
        let (k32, w32) = (k as u32, w as u32);
        let mut b = self.stream.launch_builder(&self.fn_gather);
        b.arg(&self.d_arr)
            .arg(&self.d_slew)
            .arg(&self.d_pred)
            .arg(&d_rows)
            .arg(&mut g_arr)
            .arg(&mut g_slew)
            .arg(&mut g_pred)
            .arg(&k32)
            .arg(&w32);
        // SAFETY: every entry of `rows` was checked to be `< nodes`; `g_arr`, `g_slew` and
        // `g_pred` hold `k * w`, `k * 4` and `k * w` elements.
        unsafe { b.launch(launch_1d(k, BLOCK)) }
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        let h_arr = read_slice(&self.stream, &g_arr)?;
        let h_slew = read_slice(&self.stream, &g_slew)?;
        let h_pred = read_slice(&self.stream, &g_pred)?;
        for (i, &v) in rows.iter().enumerate() {
            let v = v as usize;
            for j in 0..w {
                self.values.arr[v * w + j] = canon(h_arr[i * w + j].to_f64());
                self.values.pred[v * w + j] = h_pred[i * w + j];
            }
            for j in 0..4 {
                self.values.slew[v * 4 + j] = canon(h_slew[i * 4 + j].to_f64());
            }
        }
        Ok(())
    }

    fn read_into(&mut self, readback: Readback<'_>) -> Result<(), ComputeError> {
        match readback {
            Readback::None => {
                self.ensure_host_values();
                Ok(())
            }
            Readback::All => self.read_all(),
            Readback::Rows(rows) => self.read_rows(rows),
            Readback::Changed => match self.last_replaced {
                Replaced::All => self.read_all(),
                Replaced::FromNode(first_node) => {
                    let rows: Vec<u32> = (first_node as u32..self.nodes() as u32).collect();
                    self.read_rows(&rows)
                }
                Replaced::Cone => {
                    let rows = self.changed.clone().unwrap_or_default();
                    self.read_rows(&rows)
                }
            },
        }
    }
}

impl<R: Real> TimingSession for CudaSession<'_, R> {
    fn kind(&self) -> BackendKind {
        BackendKind::Cuda
    }

    fn precision(&self) -> Precision {
        R::PREC
    }

    fn propagate(&mut self, req: &Request<'_>) -> Result<PropagationStats, ComputeError> {
        let t_total = Instant::now();
        let mut st = PropagationStats::new(BackendKind::Cuda, self.dev.describe(), R::PREC);
        st.upload_s = std::mem::take(&mut self.pending_upload_s);
        self.check_rows(req.readback)?;
        let needs_resident = !matches!(req.scope, Scope::All);
        if needs_resident && !self.resident {
            return Err(ComputeError::Invalid(
                "no resident values: propagate every node first".into(),
            ));
        }
        let n = self.nodes();
        if let Scope::Cone(seeds) = req.scope
            && let Some(&s) = seeds.iter().find(|&&s| s as usize >= n)
        {
            return Err(ComputeError::Invalid(format!("cone seed {s} of {n}")));
        }
        let t_kern = Instant::now();
        let r = match req.scope {
            Scope::All => self
                .run_levels(0, req.ctx, &mut st)
                .map(|()| (None, Replaced::All)),
            Scope::FromLevel(l) => {
                let first = (l as usize).min(self.problem.graph.levels());
                let first = self.lowest_patched_level()?.map_or(first, |p| p.min(first));
                let first_node = self.problem.graph.level_ptr[first] as usize;
                self.run_levels(first, req.ctx, &mut st)
                    .map(|()| (None, Replaced::FromNode(first_node)))
            }
            Scope::Cone(seeds) => self
                .run_cone(seeds, req.ctx, &mut st)
                .map(|ch| (Some(ch), Replaced::Cone)),
        };
        st.kernel_s = t_kern.elapsed().as_secs_f64();
        match r {
            Ok((changed, replaced)) => {
                self.changed = changed;
                self.last_replaced = replaced;
                self.patched.clear();
                self.resident = true;
            }
            Err(e) => {
                self.changed = None;
                self.resident = false;
                return Err(e);
            }
        }
        let t_rb = Instant::now();
        self.read_into(req.readback)?;
        st.readback_s = t_rb.elapsed().as_secs_f64();
        st.total_s = st.upload_s + t_total.elapsed().as_secs_f64();
        Ok(st)
    }

    fn read(&mut self, readback: Readback<'_>) -> Result<(), ComputeError> {
        if !self.resident {
            return Err(ComputeError::Invalid("no values to read".into()));
        }
        self.check_rows(readback)?;
        self.read_into(readback)
    }

    fn values(&self) -> &TimingValues {
        &self.values
    }

    fn changed(&self) -> Option<&[u32]> {
        self.changed.as_deref()
    }

    fn set_loads(&mut self, edits: &[(u32, [f64; 4])]) -> Result<(), ComputeError> {
        let n = self.nodes();
        for &(v, l) in edits {
            if v as usize >= n {
                return Err(ComputeError::Invalid(format!("load of node {v} of {n}")));
            }
            check_load(v as usize, &l)?;
        }
        let t0 = Instant::now();
        for &(v, l) in edits {
            self.load_h[v as usize] = l;
            self.patched.push(v);
        }
        let flat: &[f64] = bytemuck::cast_slice(&self.load_h);
        self.d_load = upload_slice(&self.stream, &conv::<R>(flat))?;
        self.pending_upload_s += t0.elapsed().as_secs_f64();
        Ok(())
    }

    fn set_arc_classes(&mut self, edits: &[(u32, u32)]) -> Result<(), ComputeError> {
        let (na, nc) = (self.in_arcs_h.len(), self.problem.luts.classes.len());
        for &(k, c) in edits {
            if k as usize >= na || c as usize >= nc {
                return Err(ComputeError::Invalid(format!(
                    "class {c} of {nc} for arc {k} of {na}"
                )));
            }
            if let Some(w) = self.problem.wires.get(k as usize)
                && !self.problem.luts.classes[c as usize].is_wire()
                && !w.is_zero()
            {
                return Err(ComputeError::Invalid(format!(
                    "arc {k} has a wire record but class {c} is no wire class"
                )));
            }
        }
        let t0 = Instant::now();
        let in_ptr = &self.problem.graph.in_ptr;
        for &(k, c) in edits {
            self.in_arcs_h[k as usize].class = c;
            let v = in_ptr.partition_point(|&p| p <= k) - 1;
            self.patched.push(v as u32);
        }
        self.d_in_arcs = upload_slice(&self.stream, bytemuck::cast_slice(&self.in_arcs_h))?;
        self.pending_upload_s += t0.elapsed().as_secs_f64();
        Ok(())
    }

    fn set_starts(&mut self, starts: &StartRows) -> Result<(), ComputeError> {
        let n = self.nodes();
        validate_starts(starts, n, self.ntags())?;
        let t0 = Instant::now();
        diff_starts(&self.starts_h, starts, self.ntags(), &mut self.patched);
        self.start_row_h = start_index(n, starts);
        self.starts_h = starts.clone();
        self.d_start_row = upload_slice(&self.stream, &self.start_row_h)?;
        self.d_start_arr = upload_slice(&self.stream, &conv::<R>(&self.starts_h.arr))?;
        self.d_start_slew = upload_slice(&self.stream, &conv::<R>(&self.starts_h.slew))?;
        self.pending_upload_s += t0.elapsed().as_secs_f64();
        Ok(())
    }

    fn write_values(&mut self, values: &TimingValues) -> Result<(), ComputeError> {
        let (n, nt) = (self.nodes(), self.ntags());
        if !values.fits(n, nt) {
            return Err(ComputeError::Invalid(format!(
                "values do not fit {n} nodes and {nt} tags"
            )));
        }
        if values.arr.iter().chain(&values.slew).any(|x| x.is_nan()) {
            return Err(ComputeError::Invalid("values hold NaN".into()));
        }
        let t0 = Instant::now();
        let arr_r: Vec<R> = values.arr.iter().map(|&x| R::from_f64(canon(x))).collect();
        let slew_r: Vec<R> = values.slew.iter().map(|&x| R::from_f64(canon(x))).collect();
        self.d_arr = upload_slice(&self.stream, &arr_r)?;
        self.d_slew = upload_slice(&self.stream, &slew_r)?;
        self.d_pred = upload_slice(&self.stream, &values.pred)?;
        self.stream
            .synchronize()
            .map_err(|e| ComputeError::Device(format!("CUDA driver: {e}")))?;
        self.values = TimingValues {
            ntags: nt,
            arr: arr_r.iter().map(|&x| canon(x.to_f64())).collect(),
            slew: slew_r.iter().map(|&x| canon(x.to_f64())).collect(),
            pred: values.pred.clone(),
        };
        self.resident = true;
        self.changed = None;
        self.last_replaced = Replaced::All;
        self.pending_upload_s += t0.elapsed().as_secs_f64();
        Ok(())
    }

    fn into_values(self: Box<Self>) -> TimingValues {
        self.values
    }
}
