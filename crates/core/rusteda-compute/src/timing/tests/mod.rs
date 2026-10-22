// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Tests of the timing task on random and hand-made problems; no engine is needed. The GPU tests
//! (module `gpu`) need a GPU and are ignored by default: run them with
//! `cargo test -p rusteda-compute --features cuda -- --include-ignored`.

mod checks;
mod cpu;
#[cfg(feature = "cuda")]
mod gpu;
mod known;
pub(crate) mod naive;
pub(crate) mod random;

use crate::timing::{CPU, Request, TimingPropagationBackend, TimingValues};
use random::Owned;

/// The values of a full propagation on the CPU backend.
pub(crate) fn cpu_values(o: &Owned) -> TimingValues {
    let p = o.problem();
    let mut s = CPU.open(&p).unwrap_or_else(|e| panic!("open: {e}"));
    s.propagate(&Request::all())
        .unwrap_or_else(|e| panic!("propagate: {e}"));
    s.into_values()
}

/// Panics with the first difference between two value sets.
pub(crate) fn assert_same(a: &TimingValues, b: &TimingValues, what: &str) {
    if let Some(d) = a.first_difference(b) {
        panic!("{what}: {d}");
    }
}

/// A rayon pool of `threads` threads.
pub(crate) fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap_or_else(|e| panic!("thread pool: {e}"))
}
