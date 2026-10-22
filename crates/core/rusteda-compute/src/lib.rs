// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Compute tasks of the RustEDA tools (`docs/specs/compute/`): task traits with a pure Rust CPU
//! backend, always built and authoritative, and native GPU backends that are checked for
//! consistency with it (RustEDA shared-crate contract, section 1; development plan, rule 5).
//!
//! - [`cut`]: the priority-cut enumeration and selection task (COMPUTE-REQ-0101 to
//!   COMPUTE-REQ-0106);
//! - [`timing`]: the timing propagation task (COMPUTE-REQ-0010 to COMPUTE-REQ-0029);
//! - [`BackendKind`], [`Precision`] and [`ComputeError`]: the kinds shared by every task
//!   (COMPUTE-REQ-0005);
//! - [`GpuRuntime`], [`GpuTask`], [`dispatch`] and [`prepare`]: selection of the GPU runtime that
//!   executes an engine's own kernels, with a per-process device self-test (COMPUTE-REQ-0001,
//!   COMPUTE-REQ-0003);
//! - [`cuda`] (feature `cuda`): the CUDA device layer (COMPUTE-REQ-0002, COMPUTE-REQ-0004).

#[cfg(feature = "cuda")]
pub mod cuda;
pub mod cut;
pub mod timing;

use std::panic::{AssertUnwindSafe, catch_unwind};

use rusteda_core::diag::Diag;
use rusteda_core::job::Cancelled;

/// A backend a caller may ask a task for (COMPUTE-REQ-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// Pure Rust, always compiled; the authoritative implementation of every task.
    Cpu,
    /// NVIDIA GPUs: CUDA C kernels compiled by NVRTC (feature `cuda`).
    Cuda,
    /// The first compiled GPU backend whose device passes the per-process self-test.
    Gpu,
}

impl BackendKind {
    /// Every kind, in the order of their names in user-facing lists.
    pub const ALL: [BackendKind; 3] = [BackendKind::Cpu, BackendKind::Cuda, BackendKind::Gpu];

    /// The name of the kind: `cpu`, `cuda` or `gpu`.
    pub fn name(self) -> &'static str {
        match self {
            BackendKind::Cpu => "cpu",
            BackendKind::Cuda => "cuda",
            BackendKind::Gpu => "gpu",
        }
    }

    /// The kind with the given name.
    pub fn parse(name: &str) -> Option<BackendKind> {
        BackendKind::ALL.into_iter().find(|k| k.name() == name)
    }
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The floating-point precision of a backend's arithmetic (COMPUTE-REQ-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Precision {
    /// IEEE 754 double precision: the authoritative precision.
    #[default]
    F64,
    /// IEEE 754 single precision, on request.
    F32,
}

impl Precision {
    /// Every precision.
    pub const ALL: [Precision; 2] = [Precision::F64, Precision::F32];

    /// The name of the precision: `f64` or `f32`.
    pub fn name(self) -> &'static str {
        match self {
            Precision::F64 => "f64",
            Precision::F32 => "f32",
        }
    }

    /// The precision with the given name.
    pub fn parse(name: &str) -> Option<Precision> {
        Precision::ALL.into_iter().find(|p| p.name() == name)
    }
}

impl std::fmt::Display for Precision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Where the inputs of a task came from, which decides the envelope code of
/// [`ComputeError::Invalid`] (COMPUTE-REQ-0025).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOrigin {
    /// Built by the engine in memory: an invalid input is an internal error.
    Lowered,
    /// Borrowed from a table mapped from disk: an invalid input is a corrupt table.
    Mapped,
}

/// The failure of a compute task (COMPUTE-REQ-0005, COMPUTE-REQ-0025). A failed task never
/// returns values.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComputeError {
    /// The build lacks the named backend (its Cargo feature).
    NotCompiled(&'static str),
    /// The backend is compiled but unusable: no driver or device, or a failed self-test.
    Unavailable(String),
    /// The problem exceeds the device memory or the index range of the backend.
    TooLarge(String),
    /// A kernel did not compile.
    Compile(String),
    /// A driver call, a copy, a launch or a kernel execution failed.
    Device(String),
    /// The problem or a patch violates the task's interface.
    Invalid(String),
    /// The job was cancelled at a checkpoint.
    Cancelled,
}

impl ComputeError {
    /// The envelope code of the error (`rusteda.v1`, contract section 7) for inputs of `origin`.
    pub fn envelope_code(&self, origin: InputOrigin) -> &'static str {
        match self {
            ComputeError::NotCompiled(_) | ComputeError::Unavailable(_) => "BACKEND_UNAVAILABLE",
            ComputeError::TooLarge(_) => "INPUT_REJECTED",
            ComputeError::Compile(_) | ComputeError::Device(_) => "INTERNAL",
            ComputeError::Invalid(_) => match origin {
                InputOrigin::Lowered => "INTERNAL",
                InputOrigin::Mapped => "CORRUPT_TABLE",
            },
            ComputeError::Cancelled => "CANCELLED",
        }
    }

    /// The diagnostic code of the error, in the range A0500 to A0599 of this crate.
    pub fn diag_code(&self) -> &'static str {
        match self {
            ComputeError::NotCompiled(_) | ComputeError::Unavailable(_) => "A0501",
            ComputeError::TooLarge(_) => "A0502",
            ComputeError::Compile(_) => "A0503",
            ComputeError::Device(_) => "A0504",
            ComputeError::Invalid(_) => "A0505",
            ComputeError::Cancelled => "A0506",
        }
    }

    /// The error as a coded message: an error, or a note for a cancellation.
    pub fn diag(&self) -> Diag {
        let d = match self {
            ComputeError::Cancelled => Diag::note(self.diag_code(), self.to_string()),
            _ => Diag::error(self.diag_code(), self.to_string()),
        };
        match self {
            ComputeError::NotCompiled(_) => {
                d.with_help("build with the Cargo feature `cuda`, or choose the backend `cpu`")
            }
            ComputeError::Unavailable(_) => d.with_help(
                "a GPU backend needs an NVIDIA GPU, its driver and the NVRTC library; or choose \
                 the backend `cpu`",
            ),
            ComputeError::TooLarge(_) => d.with_help("choose the backend `cpu`"),
            _ => d,
        }
    }
}

impl std::fmt::Display for ComputeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComputeError::NotCompiled(b) => write!(f, "this build has no {b} backend"),
            ComputeError::Unavailable(m) => write!(f, "backend unavailable: {m}"),
            ComputeError::TooLarge(m) => write!(f, "problem too large for the backend: {m}"),
            ComputeError::Compile(m) => write!(f, "kernel compilation failed: {m}"),
            ComputeError::Device(m) => write!(f, "device failure: {m}"),
            ComputeError::Invalid(m) => write!(f, "invalid problem: {m}"),
            ComputeError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for ComputeError {}

impl From<Cancelled> for ComputeError {
    fn from(_: Cancelled) -> ComputeError {
        ComputeError::Cancelled
    }
}

impl From<GpuError> for ComputeError {
    /// A failure of the device layer after the device was opened.
    fn from(e: GpuError) -> ComputeError {
        ComputeError::Device(e.0)
    }
}

/// A GPU runtime a build may contain (COMPUTE-REQ-0001).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuRuntime {
    /// CUDA C kernels compiled by NVRTC (feature `cuda`, COMPUTE-REQ-0002).
    Cuda,
}

impl GpuRuntime {
    /// Every runtime, in the order automatic selection tries them.
    pub const ALL: [GpuRuntime; 1] = [GpuRuntime::Cuda];

    pub fn name(self) -> &'static str {
        match self {
            GpuRuntime::Cuda => "cuda",
        }
    }

    /// True when this build contains the runtime.
    pub fn compiled(self) -> bool {
        match self {
            GpuRuntime::Cuda => cfg!(feature = "cuda"),
        }
    }
}

/// A failure to set up or run GPU work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuError(pub String);

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GpuError {}

/// GPU work of an engine: one implementation per runtime; a task without an implementation for
/// the runtime it is run on fails (COMPUTE-REQ-0001).
pub trait GpuTask {
    type Output;
    /// The CUDA C implementation for the `cuda` runtime.
    #[cfg(feature = "cuda")]
    fn run_cuda(&self, _dev: &cuda::Device) -> Result<Self::Output, GpuError> {
        Err(GpuError(
            "this task has no implementation for the cuda runtime".into(),
        ))
    }
}

/// Runs `task` on runtime `want`, or with `None` on the first compiled runtime (in
/// [`GpuRuntime::ALL`] order) whose default device passes the self-test. Returns the output and
/// the runtime that produced it.
pub fn dispatch<T: GpuTask>(
    want: Option<GpuRuntime>,
    task: &T,
) -> Result<(T::Output, GpuRuntime), GpuError> {
    let candidates: Vec<GpuRuntime> = match want {
        Some(r) if !r.compiled() => {
            return Err(GpuError(format!(
                "this build has no {} runtime (enable the `{}` feature)",
                r.name(),
                r.name()
            )));
        }
        Some(r) => vec![r],
        None => GpuRuntime::ALL
            .into_iter()
            .filter(|r| r.compiled())
            .collect(),
    };
    if candidates.is_empty() {
        return Err(GpuError(
            "this build has no GPU runtime (enable the `cuda` feature)".into(),
        ));
    }
    let mut failures = Vec::new();
    for rt in candidates {
        match prepare(rt) {
            Ok(()) => return run_on(rt, task).map(|o| (o, rt)),
            Err(e) => failures.push(format!("{}: {}", rt.name(), e.0)),
        }
    }
    Err(GpuError(format!(
        "no usable GPU runtime ({})",
        failures.join("; ")
    )))
}

/// Checks the preconditions of a runtime and runs the self-test on its default device
/// (COMPUTE-REQ-0001), once per process and runtime; later calls return the first result (and
/// wait for it while another thread computes it, COMPUTE-REQ-0003).
pub fn prepare(rt: GpuRuntime) -> Result<(), GpuError> {
    static PREPARED: [std::sync::OnceLock<Result<(), GpuError>>; GpuRuntime::ALL.len()] =
        [const { std::sync::OnceLock::new() }; GpuRuntime::ALL.len()];
    let slot = GpuRuntime::ALL
        .iter()
        .position(|r| *r == rt)
        .unwrap_or_default();
    PREPARED[slot].get_or_init(|| run_on(rt, &SelfTest)).clone()
}

fn run_on<T: GpuTask>(rt: GpuRuntime, task: &T) -> Result<T::Output, GpuError> {
    let out = catch_unwind(AssertUnwindSafe(|| match rt {
        #[cfg(feature = "cuda")]
        GpuRuntime::Cuda => task.run_cuda(cuda::device()?),
        #[allow(unreachable_patterns)]
        _ => {
            // In a build without a GPU runtime this is the only arm and `task` is otherwise unused.
            let _ = task;
            Err(GpuError(format!("this build has no {} runtime", rt.name())))
        }
    }));
    match out {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| p.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            Err(GpuError(format!(
                "{} runtime failed: {}",
                rt.name(),
                msg.trim()
            )))
        }
    }
}

/// Runs the self-test kernel of a runtime, which compares every element with the exact expected
/// value, so that a runtime that silently fails to execute kernels is rejected.
struct SelfTest;

impl GpuTask for SelfTest {
    type Output = ();
    #[cfg(feature = "cuda")]
    fn run_cuda(&self, dev: &cuda::Device) -> Result<(), GpuError> {
        cuda::self_test(dev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // COMPUTE-REQ-0001: a requested runtime that the build lacks, and a build without any GPU
    // runtime, are failures. The assertions need a build without the runtime features, such as
    // `cargo test -p rusteda-compute --lib`; a workspace build may unify them in.
    #[test]
    fn requested_runtime_must_be_compiled() {
        for rt in GpuRuntime::ALL.into_iter().filter(|r| !r.compiled()) {
            assert_eq!(
                dispatch(Some(rt), &SelfTest).map(|_| ()),
                Err(GpuError(format!(
                    "this build has no {0} runtime (enable the `{0}` feature)",
                    rt.name()
                )))
            );
        }
        if !GpuRuntime::ALL.iter().any(|r| r.compiled()) {
            assert_eq!(
                dispatch(None, &SelfTest).map(|_| ()),
                Err(GpuError(
                    "this build has no GPU runtime (enable the `cuda` feature)".into()
                ))
            );
        }
    }

    // Needs a GPU and the NVRTC library; run with `--ignored` on a GPU machine.
    #[test]
    #[ignore]
    fn self_test_on_every_compiled_runtime() {
        for rt in GpuRuntime::ALL.into_iter().filter(|r| r.compiled()) {
            prepare(rt).unwrap_or_else(|e| panic!("{}: {e}", rt.name()));
        }
    }

    // COMPUTE-REQ-0005: the shared kinds have stable names that parse back.
    #[test]
    fn kind_and_precision_names() {
        for k in BackendKind::ALL {
            assert_eq!(BackendKind::parse(k.name()), Some(k));
            assert_eq!(k.to_string(), k.name());
        }
        for p in Precision::ALL {
            assert_eq!(Precision::parse(p.name()), Some(p));
        }
        assert_eq!(
            BackendKind::ALL.map(BackendKind::name),
            ["cpu", "cuda", "gpu"]
        );
        assert_eq!(Precision::ALL.map(Precision::name), ["f64", "f32"]);
        assert_eq!(BackendKind::parse("metal"), None);
        assert_eq!(Precision::parse("f16"), None);
    }

    // COMPUTE-REQ-0025, COMPUTE-REQ-0005: every error has its envelope code and diagnostic; an
    // uncompiled or unusable backend is BACKEND_UNAVAILABLE with A0501.
    #[test]
    fn error_envelope_codes_and_diagnostics() {
        use InputOrigin::{Lowered, Mapped};
        let cases = [
            (
                ComputeError::NotCompiled("cuda"),
                "BACKEND_UNAVAILABLE",
                "BACKEND_UNAVAILABLE",
                "A0501",
            ),
            (
                ComputeError::Unavailable("no device".into()),
                "BACKEND_UNAVAILABLE",
                "BACKEND_UNAVAILABLE",
                "A0501",
            ),
            (
                ComputeError::TooLarge("x".into()),
                "INPUT_REJECTED",
                "INPUT_REJECTED",
                "A0502",
            ),
            (
                ComputeError::Compile("x".into()),
                "INTERNAL",
                "INTERNAL",
                "A0503",
            ),
            (
                ComputeError::Device("x".into()),
                "INTERNAL",
                "INTERNAL",
                "A0504",
            ),
            (
                ComputeError::Invalid("x".into()),
                "INTERNAL",
                "CORRUPT_TABLE",
                "A0505",
            ),
            (ComputeError::Cancelled, "CANCELLED", "CANCELLED", "A0506"),
        ];
        for (e, lowered, mapped, code) in cases {
            assert_eq!(e.envelope_code(Lowered), lowered, "{e}");
            assert_eq!(e.envelope_code(Mapped), mapped, "{e}");
            assert_eq!(e.diag_code(), code);
            let d = e.diag();
            assert_eq!(d.code, code);
            assert_eq!(d.message, e.to_string());
            let range = d.code[1..].parse::<u32>().unwrap_or(0);
            assert!(d.code.starts_with('A') && (500..600).contains(&range));
        }
        assert_eq!(
            ComputeError::NotCompiled("cuda").to_string(),
            "this build has no cuda backend"
        );
        assert!(ComputeError::NotCompiled("cuda").diag().help.is_some());
        assert_eq!(ComputeError::from(Cancelled), ComputeError::Cancelled);
        assert_eq!(
            ComputeError::from(GpuError("launch".into())),
            ComputeError::Device("launch".into())
        );
    }
}
