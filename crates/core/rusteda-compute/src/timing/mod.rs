// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The timing propagation task (`docs/specs/compute/timing.md`, COMPUTE-REQ-0010 to
//! COMPUTE-REQ-0029): arrival times per tag, transition times per pin and predecessors,
//! propagated level by level over a levelized timing graph, reduced with the minimum (early) and
//! the maximum (late).
//!
//! An engine lowers its graph, library tables, start values and tag maps into the interface of
//! [`abi`] ([`TimingProblem`]), selects a backend with [`timing_backend`] and either calls the
//! single-call form [`TimingPropagationBackend::propagate_levels`] or opens a
//! [`TimingSession`] for repeated and incremental propagation. The `cpu` backend is
//! authoritative; the `cuda` backend agrees with it bit for bit in `f64` (COMPUTE-REQ-0019).

pub mod abi;
mod cpu;
#[cfg(feature = "cuda")]
mod cuda;
pub mod lut;
mod state;
#[cfg(test)]
mod tests;
mod validate;

pub use abi::{
    ArcClass, CompiledLutBank, FLAG_MASK, InArc, LevelizedTimingCsr, LutDesc, NEGATIVE_UNATE,
    NON_UNATE, NONE, POSITIVE_UNATE, StartRows, TagMapRec, TimingProblem, WIRE, WireRec,
    out_adjacency, transition_bit,
};
pub use state::{
    EARLY, FALL, LATE, NO_PRED, RISE, TimingStateSoA, TimingValues, absent, arr_index, pred_source,
    slew_index,
};
pub use validate::MAX_NODES;

use rusteda_core::job::Ctx;

use crate::{BackendKind, ComputeError, Precision};

/// The nodes a propagation computes (COMPUTE-REQ-0021).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Every node.
    All,
    /// Every node from this level on (and from the lowest patched level, when lower), on the
    /// session's resident values below it.
    FromLevel(u32),
    /// The forward cone of these nodes and of every node patched since the last propagation,
    /// with bitwise change detection.
    Cone(&'a [u32]),
}

/// The values a propagation or [`TimingSession::read`] copies to the host view
/// (COMPUTE-REQ-0022). The CPU backend computes in place, so its host view is always complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readback<'a> {
    /// Every row.
    All,
    /// Nothing: the values stay resident on the device.
    None,
    /// The rows of these nodes.
    Rows(&'a [u32]),
    /// The rows the last propagation replaced.
    Changed,
}

/// One propagation (COMPUTE-REQ-0021, COMPUTE-REQ-0022, COMPUTE-REQ-0026).
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub scope: Scope<'a>,
    pub readback: Readback<'a>,
    /// Only order-independent reductions. Every backend of this crate reduces deterministically
    /// either way (COMPUTE-REQ-0024).
    pub deterministic: bool,
    /// Cancel token and progress observer.
    pub ctx: Ctx<'a>,
}

impl<'a> Request<'a> {
    /// Every node, every row read back, deterministic, without a context.
    pub fn all() -> Request<'a> {
        Request {
            scope: Scope::All,
            readback: Readback::All,
            deterministic: true,
            ctx: Ctx::NONE,
        }
    }

    /// The cone of `seeds` with the changed rows read back.
    pub fn cone(seeds: &'a [u32]) -> Request<'a> {
        Request {
            scope: Scope::Cone(seeds),
            readback: Readback::Changed,
            ..Request::all()
        }
    }

    /// The same request with another scope.
    pub fn with_scope(self, scope: Scope<'a>) -> Request<'a> {
        Request { scope, ..self }
    }

    /// The same request with another readback.
    pub fn with_readback(self, readback: Readback<'a>) -> Request<'a> {
        Request { readback, ..self }
    }

    /// The same request with a context.
    pub fn with_ctx(self, ctx: Ctx<'a>) -> Request<'a> {
        Request { ctx, ..self }
    }
}

/// What a propagation did and what it cost (COMPUTE-REQ-0024; wall-clock seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct PropagationStats {
    /// `Cpu` or `Cuda`: the backend that computed.
    pub backend: BackendKind,
    /// The device: the CPU thread count, or the GPU name and architecture.
    pub device: String,
    pub precision: Precision,
    /// Whether every reduction was order-independent.
    pub deterministic: bool,
    /// Levels visited.
    pub levels: u32,
    /// Kernel launches (0 on the CPU).
    pub launches: u32,
    pub nodes_evaluated: u64,
    /// Nodes whose values were replaced (every evaluated node for `All` and `FromLevel`).
    pub nodes_changed: u64,
    /// The largest magnitude of a present value among the evaluated rows.
    pub max_abs_ns: f64,
    pub upload_s: f64,
    pub kernel_s: f64,
    pub readback_s: f64,
    pub total_s: f64,
}

impl PropagationStats {
    /// Empty statistics of a backend.
    pub fn new(backend: BackendKind, device: String, precision: Precision) -> PropagationStats {
        PropagationStats {
            backend,
            device,
            precision,
            deterministic: true,
            levels: 0,
            launches: 0,
            nodes_evaluated: 0,
            nodes_changed: 0,
            max_abs_ns: 0.0,
            upload_s: 0.0,
            kernel_s: 0.0,
            readback_s: 0.0,
            total_s: 0.0,
        }
    }
}

/// A backend of the timing task (COMPUTE-REQ-0010).
pub trait TimingPropagationBackend: Send + Sync + std::fmt::Debug {
    /// `Cpu` or `Cuda`.
    fn kind(&self) -> BackendKind;

    /// The precision of the backend's arithmetic.
    fn precision(&self) -> Precision;

    /// Validates `problem` and makes it resident (COMPUTE-REQ-0020, COMPUTE-REQ-0021).
    fn open<'p>(
        &self,
        problem: &TimingProblem<'p>,
    ) -> Result<Box<dyn TimingSession + 'p>, ComputeError>;

    /// The single-call form of the development plan (COMPUTE-REQ-0023): propagates every level
    /// of one corner, with the loads and start rows that `state` carries and ideal wires, and
    /// stores the arrivals, transitions and predecessors in `state`.
    fn propagate_levels(
        &self,
        graph: &LevelizedTimingCsr,
        lut_bank: &CompiledLutBank,
        state: &mut TimingStateSoA,
        deterministic: bool,
    ) -> Result<PropagationStats, ComputeError> {
        let (stats, values) = {
            let p = TimingProblem {
                graph,
                luts: lut_bank,
                ntags: state.ntags,
                load: &state.load,
                starts: &state.starts,
                wires: &[],
            };
            let mut s = self.open(&p)?;
            let req = Request {
                deterministic,
                ..Request::all()
            };
            let stats = s.propagate(&req)?;
            (stats, s.into_values())
        };
        state.set_values(values);
        Ok(stats)
    }
}

/// A problem resident on a backend (COMPUTE-REQ-0021, COMPUTE-REQ-0022).
pub trait TimingSession: Send {
    /// `Cpu` or `Cuda`.
    fn kind(&self) -> BackendKind;

    /// The precision of the session's arithmetic.
    fn precision(&self) -> Precision;

    /// Computes the values of `req.scope` and copies `req.readback` to the host view. On an
    /// error the session has no resident values until the next complete propagation.
    fn propagate(&mut self, req: &Request<'_>) -> Result<PropagationStats, ComputeError>;

    /// Copies resident values to the host view (a no-op on the CPU, which computes in place).
    fn read(&mut self, readback: Readback<'_>) -> Result<(), ComputeError>;

    /// The host view: complete after a propagation or read of every row, otherwise current for
    /// the rows read.
    fn values(&self) -> &TimingValues;

    /// After a `Cone` propagation, the nodes whose values it replaced, increasing; `None` after
    /// any other scope (every evaluated node).
    fn changed(&self) -> Option<&[u32]>;

    /// Replaces loads: `(node, load[tr * 2 + an])`.
    fn set_loads(&mut self, edits: &[(u32, [f64; 4])]) -> Result<(), ComputeError>;

    /// Replaces arc classes: `(index of the arc in CSR order, class)`.
    fn set_arc_classes(&mut self, edits: &[(u32, u32)]) -> Result<(), ComputeError>;

    /// Replaces the start rows.
    fn set_starts(&mut self, starts: &StartRows) -> Result<(), ComputeError>;

    /// Makes `values` the resident values, as if a complete propagation had computed them, for
    /// example values carried over from an earlier analysis before a `Cone` propagation.
    fn write_values(&mut self, values: &TimingValues) -> Result<(), ComputeError>;

    /// The host view, consuming the session.
    fn into_values(self: Box<Self>) -> TimingValues;
}

/// The CPU backend (COMPUTE-REQ-0027).
pub static CPU: cpu::CpuBackend = cpu::CpuBackend;

/// The backend of `kind` in `precision` (COMPUTE-REQ-0010). `Gpu` resolves to the first compiled
/// GPU backend whose device passes the per-process self-test. An uncompiled or unusable backend
/// is an error, never another backend.
pub fn timing_backend(
    kind: BackendKind,
    precision: Precision,
) -> Result<&'static dyn TimingPropagationBackend, ComputeError> {
    match kind {
        BackendKind::Cpu => match precision {
            Precision::F64 => Ok(&CPU),
            Precision::F32 => Err(ComputeError::Unavailable(
                "the cpu backend computes in f64 only".into(),
            )),
        },
        BackendKind::Cuda | BackendKind::Gpu => gpu_backend(kind, precision),
    }
}

#[cfg(feature = "cuda")]
fn gpu_backend(
    kind: BackendKind,
    precision: Precision,
) -> Result<&'static dyn TimingPropagationBackend, ComputeError> {
    crate::prepare(crate::GpuRuntime::Cuda).map_err(|e| {
        ComputeError::Unavailable(match kind {
            BackendKind::Gpu => format!("no GPU backend passes its self-test (cuda: {e})"),
            _ => e.0,
        })
    })?;
    Ok(cuda::backend(precision))
}

#[cfg(not(feature = "cuda"))]
fn gpu_backend(
    kind: BackendKind,
    _precision: Precision,
) -> Result<&'static dyn TimingPropagationBackend, ComputeError> {
    Err(ComputeError::NotCompiled(kind.name()))
}

/// Selects the backend and compiles its kernels for `precision` (COMPUTE-REQ-0029), so that an
/// engine can overlap the preparation with other work on a second thread (COMPUTE-REQ-0003).
pub fn prewarm(kind: BackendKind, precision: Precision) -> Result<(), ComputeError> {
    let b = timing_backend(kind, precision)?;
    #[cfg(feature = "cuda")]
    if b.kind() == BackendKind::Cuda {
        cuda::compile(precision)?;
    }
    let _ = b;
    Ok(())
}
