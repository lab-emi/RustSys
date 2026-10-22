// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The CUDA runtime (feature `cuda`, COMPUTE-REQ-0002, COMPUTE-REQ-0004): kernels written in
//! CUDA C are compiled at run time by NVRTC for the architecture of the device and launched
//! through the CUDA driver API. The CUDA driver and NVRTC libraries are loaded at run time; builds
//! need no CUDA toolkit.

// Backend glue over the CUDA driver and NVRTC C APIs (AGENTS rules 8.4): every `unsafe` block
// carries its justification.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{Arc, Mutex, OnceLock};

use cudarc::driver::{CudaContext, CudaModule, DriverError};
pub use cudarc::driver::{
    CudaFunction, CudaSlice, CudaStream, DeviceRepr, LaunchConfig, PinnedHostSlice, PushKernelArg,
    ValidAsZeroBits,
};
use cudarc::nvrtc::{Ptx, result as nr, sys as ns};

use crate::GpuError;

impl From<DriverError> for GpuError {
    fn from(e: DriverError) -> GpuError {
        GpuError(format!("CUDA driver: {e}"))
    }
}

/// Releases an NVRTC program when dropped, so that every exit path of compilation destroys its
/// program (COMPUTE-REQ-0004).
struct Program(ns::nvrtcProgram);

impl Drop for Program {
    fn drop(&mut self) {
        // SAFETY: `self.0` was created by `nvrtc::result::create_program` and is destroyed only
        // here.
        let _ = unsafe { nr::destroy_program(self.0) };
    }
}

/// The default CUDA device with its stream and the modules compiled for it.
pub struct Device {
    ctx: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    arch: String,
    name: String,
    /// Compiled modules keyed by their source text and complete NVRTC option list.
    modules: Mutex<HashMap<(&'static str, Vec<String>), Arc<CudaModule>>>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("name", &self.name)
            .field("arch", &self.arch)
            .finish_non_exhaustive()
    }
}

static DEVICE: OnceLock<Result<Device, GpuError>> = OnceLock::new();

/// The default device, opened once per process.
pub fn device() -> Result<&'static Device, GpuError> {
    match DEVICE.get_or_init(open) {
        Ok(d) => Ok(d),
        Err(e) => Err(e.clone()),
    }
}

fn open() -> Result<Device, GpuError> {
    // SAFETY: the probes only load the shared libraries; no CUDA function is called.
    if !unsafe { cudarc::driver::sys::is_culib_present() } {
        return Err(GpuError("CUDA driver library (libcuda) not found".into()));
    }
    // SAFETY: as above.
    if !unsafe { cudarc::nvrtc::sys::is_culib_present() } {
        return Err(GpuError(
            "NVRTC library (libnvrtc) not found; add the library directory of a CUDA toolkit to \
             LD_LIBRARY_PATH"
                .into(),
        ));
    }
    let ctx = CudaContext::new(0)?;
    let (maj, min) = ctx.compute_capability()?;
    let name = ctx.name()?;
    let stream = ctx.new_stream()?;
    let suffix = if maj >= 9 { "a" } else { "" };
    Ok(Device {
        ctx,
        stream,
        arch: format!("sm_{maj}{min}{suffix}"),
        name,
        modules: Mutex::new(HashMap::new()),
    })
}

impl Device {
    /// The device name and architecture (COMPUTE-REQ-0004).
    pub fn describe(&self) -> String {
        format!("{} ({})", self.name, self.arch)
    }

    pub fn stream(&self) -> &Arc<CudaStream> {
        &self.stream
    }

    /// A new stream on this device's context (COMPUTE-REQ-0004).
    pub fn new_stream(&self) -> Result<Arc<CudaStream>, GpuError> {
        Ok(self.ctx.new_stream()?)
    }

    /// Free and total device memory in bytes (COMPUTE-REQ-0004).
    pub fn mem_info(&self) -> Result<(usize, usize), GpuError> {
        self.ctx.bind_to_thread()?;
        Ok(cudarc::driver::result::mem_get_info()?)
    }

    /// Page-locked host memory of at least `len` elements, not write-combined (COMPUTE-REQ-0004),
    /// so that the host reads it at full speed after a device-to-host copy.
    pub fn pinned<T: DeviceRepr>(&self, len: usize) -> Result<PinnedHostSlice<T>, GpuError> {
        self.ctx.bind_to_thread()?;
        // SAFETY: callers read the slice only after a device-to-host copy has written it; flag 0
        // selects cached, non-write-combined page-locked memory.
        Ok(unsafe { self.ctx.alloc_pinned_with_flags::<T>(len.max(1), 0) }?)
    }

    /// Kernel `name` of the CUDA C source `src` with the default options, compiled once per
    /// process.
    pub fn function(&self, src: &'static str, name: &str) -> Result<CudaFunction, GpuError> {
        self.function_with(src, &[], name)
    }

    /// Kernel `name` of the CUDA C source `src` compiled with extra NVRTC `options` (for example
    /// `-DREAL_F64` and `--fmad=false`), which replace the default options they name; cached per
    /// process by source and complete option list (COMPUTE-REQ-0004).
    pub fn function_with(
        &self,
        src: &'static str,
        options: &[&str],
        name: &str,
    ) -> Result<CudaFunction, GpuError> {
        // Module loading does not bind the context to the calling thread by itself.
        self.ctx.bind_to_thread()?;
        // Default code generation settings (COMPUTE-REQ-0002): architecture-specific target from
        // compute capability 9.0 on, NVRTC's default fused multiply-add contraction, correctly
        // rounded division and square root, no flush to zero.
        let mut opts = vec![
            format!("--gpu-architecture={}", self.arch),
            "--std=c++17".into(),
            "--fmad=true".into(),
            "--prec-div=true".into(),
            "--prec-sqrt=true".into(),
            "--ftz=false".into(),
        ];
        for &opt in options {
            if let Some((k, _)) = opt.split_once('=')
                && opt.starts_with("--")
                && let Some(slot) = opts
                    .iter_mut()
                    .find(|o| o.split_once('=').is_some_and(|(ok, _)| ok == k))
            {
                *slot = opt.into();
            } else {
                opts.push(opt.into());
            }
        }
        let key = (src, opts);
        let module = {
            let mut m = self
                .modules
                .lock()
                .map_err(|_| GpuError("module cache poisoned".into()))?;
            match m.get(&key) {
                Some(x) => x.clone(),
                None => {
                    let x = self.compile(src, &key.1)?;
                    m.insert(key, x.clone());
                    x
                }
            }
        };
        Ok(module.load_function(name)?)
    }

    /// Compiles CUDA C to a CUBIN for this device's architecture with `opts` and loads it.
    fn compile(&self, src: &str, opts: &[String]) -> Result<Arc<CudaModule>, GpuError> {
        let err = |what: &str, e: &dyn std::fmt::Debug| GpuError(format!("NVRTC {what}: {e:?}"));
        let csrc = CString::new(src).map_err(|e| err("source", &e))?;
        let prog = Program(nr::create_program(&csrc, None).map_err(|e| err("create", &e))?);
        // SAFETY: `prog.0` was created above and `Program`'s `Drop` destroys it on every return
        // path below.
        let compiled = unsafe { nr::compile_program(prog.0, opts) };
        if let Err(e) = compiled {
            // SAFETY: `prog.0` is valid until `prog` drops at return.
            let log = unsafe { nr::get_program_log(prog.0) }
                .map(|l| l.iter().map(|c| *c as u8 as char).collect::<String>())
                .unwrap_or_default();
            return Err(GpuError(format!(
                "NVRTC compile: {e:?}: {}",
                log.trim_end_matches('\0')
            )));
        }
        let mut n = 0usize;
        // SAFETY: `prog.0` holds a successful compilation; `n` receives the CUBIN size.
        unsafe { ns::nvrtcGetCUBINSize(prog.0, &mut n) }
            .result()
            .map_err(|e| err("CUBIN size", &e))?;
        let mut cubin = vec![0u8; n];
        // SAFETY: `cubin` has exactly the size NVRTC reported.
        unsafe { ns::nvrtcGetCUBIN(prog.0, cubin.as_mut_ptr().cast()) }
            .result()
            .map_err(|e| err("CUBIN", &e))?;
        drop(prog);
        Ok(self.ctx.load_module(Ptx::from_binary(cubin))?)
    }

    /// Copies `data` to a new device array.
    pub fn upload<T: DeviceRepr>(&self, data: &[T]) -> Result<CudaSlice<T>, GpuError> {
        Ok(self.stream.clone_htod(data)?)
    }

    /// A zero-filled device array (at least one element: some kernels bind every argument).
    pub fn zeros<T: DeviceRepr + ValidAsZeroBits>(
        &self,
        len: usize,
    ) -> Result<CudaSlice<T>, GpuError> {
        Ok(self.stream.alloc_zeros(len.max(1))?)
    }

    /// Copies a device array to the host after all queued work has finished; execution errors
    /// of earlier launches are reported here.
    pub fn read<T: DeviceRepr>(&self, s: &CudaSlice<T>) -> Result<Vec<T>, GpuError> {
        let v = self.stream.clone_dtoh(s)?;
        self.stream.synchronize()?;
        Ok(v)
    }

    /// Waits for all work queued on the stream.
    pub fn sync(&self) -> Result<(), GpuError> {
        Ok(self.stream.synchronize()?)
    }
}

/// One-dimensional launch shape for `units` work items in blocks of `block` threads.
pub fn launch_1d(units: usize, block: u32) -> LaunchConfig {
    LaunchConfig {
        grid_dim: ((units as u32).div_ceil(block).max(1), 1, 1),
        block_dim: (block, 1, 1),
        shared_mem_bytes: 0,
    }
}

/// Source of the device self-test kernel: `y[i] = 2 x[i] + 1` for `i < n`.
const SELF_TEST_SRC: &str = r#"
extern "C" __global__ void self_test(const float* x, float* y, unsigned n) {
    unsigned i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < n) { y[i] = 2.0f * x[i] + 1.0f; }
}
"#;

/// Runs the self-test kernel and compares every result exactly (COMPUTE-REQ-0001).
pub fn self_test(dev: &Device) -> Result<(), GpuError> {
    let n = 1000usize;
    let x: Vec<f32> = (0..n).map(|i| i as f32 * 0.25 - 17.0).collect();
    let xd = dev.upload(&x)?;
    let mut yd = dev.zeros::<f32>(n)?;
    let f = dev.function(SELF_TEST_SRC, "self_test")?;
    let n32 = n as u32;
    let mut b = dev.stream.launch_builder(&f);
    b.arg(&xd).arg(&mut yd).arg(&n32);
    // SAFETY: the kernel reads `n` floats from `xd` and writes `n` floats to `yd`, both of which
    // hold at least `n` elements; `n32` matches the kernel's `unsigned` parameter.
    unsafe { b.launch(launch_1d(n, 128)) }?;
    let y = dev.read(&yd)?;
    for (i, (a, b)) in x.iter().zip(&y).enumerate() {
        if *b != 2.0 * a + 1.0 {
            return Err(GpuError(format!(
                "device self-test failed at element {i}: {b} instead of {}",
                2.0 * a + 1.0
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // COMPUTE-REQ-0004: every W0 device item keeps its signature and every additive item is
    // callable with its specified signature.
    #[test]
    fn device_api_is_additive() {
        fn _assert_signatures(dev: &Device) -> Result<(), GpuError> {
            let _d: &'static Device = device()?;
            let _st: &Arc<CudaStream> = dev.stream();
            let _f1: CudaFunction = dev.function("extern \"C\" __global__ void k() {}", "k")?;
            let _f2: CudaFunction = dev.function_with(
                "extern \"C\" __global__ void k() {}",
                &["--fmad=false", "-DREAL_F64"],
                "k",
            )?;
            let slice: CudaSlice<f32> = dev.upload(&[1.0f32, 2.0f32])?;
            let _z: CudaSlice<u32> = dev.zeros::<u32>(4)?;
            let _host: Vec<f32> = dev.read(&slice)?;
            dev.sync()?;
            let _cfg: LaunchConfig = launch_1d(10, 64);
            self_test(dev)?;
            let _pin: PinnedHostSlice<f64> = dev.pinned::<f64>(8)?;
            let _new_st: Arc<CudaStream> = dev.new_stream()?;
            let (_free, _total): (usize, usize) = dev.mem_info()?;
            let _desc: String = dev.describe();
            Ok(())
        }
        let cfg = launch_1d(257, 128);
        assert_eq!(cfg.grid_dim, (3, 1, 1));
        assert_eq!(cfg.block_dim, (128, 1, 1));
    }
}
