// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Progress events, cancellation and messages of engine runs (`docs/specs/core/`, JOB-REQ).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::diag::{Diag, Location, Severity};

/// A shared cancellation flag (JOB-REQ-0001).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken::default()
    }

    /// Asks every holder of the token to stop at its next checkpoint.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// A run was cancelled at a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// The failure of a job: cancelled, or failed with the engine's error (JOB-REQ-0006).
#[derive(Debug, Clone, PartialEq)]
pub enum JobError<E> {
    Cancelled,
    Failed(E),
}

impl<E> From<Cancelled> for JobError<E> {
    fn from(_: Cancelled) -> JobError<E> {
        JobError::Cancelled
    }
}

impl<E> JobError<E> {
    pub fn map<F>(self, f: impl FnOnce(E) -> F) -> JobError<F> {
        match self {
            JobError::Cancelled => JobError::Cancelled,
            JobError::Failed(e) => JobError::Failed(f(e)),
        }
    }
}

/// What a run reports while it works (JOB-REQ-0003).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A phase begins.
    PhaseStarted { phase: &'static str },
    /// A phase ended after `seconds`, the value the run records in its report.
    PhaseFinished { phase: &'static str, seconds: f64 },
    /// Step `ordinal` (from 1) of `count` (0: unknown) of a phase begins.
    Step {
        phase: &'static str,
        step: &'static str,
        ordinal: u32,
        count: u32,
    },
    /// `done` of `total` units of the current step are finished.
    Progress {
        phase: &'static str,
        done: u64,
        total: u64,
    },
    /// A named figure of the run (for example the node count of a graph).
    Metric { name: &'static str, value: f64 },
    /// A message, in the order in which it arose.
    Diagnostic(Diag),
}

/// Receives the events of a run; called on the thread that runs the phases.
pub trait Observer: Sync {
    fn event(&self, e: &Event);
}

/// The context of a run: an optional cancel token and an optional observer (JOB-REQ-0002).
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    cancel: Option<&'a CancelToken>,
    obs: Option<&'a dyn Observer>,
}

impl std::fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("cancel", &self.cancel)
            .field("observed", &self.obs.is_some())
            .finish()
    }
}

impl<'a> Ctx<'a> {
    /// No token and no observer: the plain library call.
    pub const NONE: Ctx<'static> = Ctx {
        cancel: None,
        obs: None,
    };

    pub fn new(cancel: Option<&'a CancelToken>, obs: Option<&'a dyn Observer>) -> Ctx<'a> {
        Ctx { cancel, obs }
    }

    /// `Err(Cancelled)` when the token is set.
    #[inline]
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        match self.cancel {
            Some(t) if t.is_cancelled() => Err(Cancelled),
            _ => Ok(()),
        }
    }

    #[inline]
    pub fn observed(&self) -> bool {
        self.obs.is_some()
    }

    /// The same cancel token without the observer (JOB-REQ-0002).
    #[inline]
    pub fn unobserved(&self) -> Ctx<'a> {
        Ctx {
            cancel: self.cancel,
            obs: None,
        }
    }

    /// Passes the event built by `f` to the observer; `f` runs only when there is one.
    #[inline]
    pub fn emit(&self, f: impl FnOnce() -> Event) {
        if let Some(o) = self.obs {
            o.event(&f());
        }
    }

    /// Emits `Progress` when `done == total` or `done` is a multiple of
    /// `max(1, ceil(total / 256))` (JOB-REQ-0004).
    #[inline]
    pub fn progress(&self, phase: &'static str, done: u64, total: u64) {
        if let Some(o) = self.obs {
            let stride = total.div_ceil(256).max(1);
            if done == total || done.is_multiple_of(stride) {
                o.event(&Event::Progress { phase, done, total });
            }
        }
    }

    /// A message as a `Diagnostic` event.
    #[inline]
    pub fn diag(&self, d: impl FnOnce() -> Diag) {
        self.emit(|| Event::Diagnostic(d()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Record(Mutex<Vec<Event>>);

    impl Observer for Record {
        fn event(&self, e: &Event) {
            self.0.lock().unwrap().push(e.clone());
        }
    }

    // JOB-REQ-0001, JOB-REQ-0002
    #[test]
    fn checkpoint_follows_the_token() {
        let t = CancelToken::new();
        let ctx = Ctx::new(Some(&t), None);
        assert_eq!(ctx.checkpoint(), Ok(()));
        t.clone().cancel();
        assert_eq!(ctx.checkpoint(), Err(Cancelled));
        assert_eq!(Ctx::NONE.checkpoint(), Ok(()));
    }

    // JOB-REQ-0002: without an observer no event is built.
    #[test]
    fn unobserved_builds_nothing() {
        let built = std::cell::Cell::new(0);
        Ctx::NONE.emit(|| {
            built.set(built.get() + 1);
            Event::Metric {
                name: "x",
                value: 1.0,
            }
        });
        assert_eq!(built.get(), 0);
        assert!(!Ctx::NONE.observed());
    }

    // JOB-REQ-0002: an unobserved copy keeps the cancel token and drops the observer.
    #[test]
    fn unobserved_copy_keeps_cancellation() {
        let t = CancelToken::new();
        let r = Record::default();
        let ctx = Ctx::new(Some(&t), Some(&r));
        let quiet = ctx.unobserved();
        assert!(ctx.observed() && !quiet.observed());
        quiet.emit(|| Event::Metric {
            name: "x",
            value: 1.0,
        });
        assert!(r.0.lock().unwrap().is_empty());
        t.cancel();
        assert_eq!(quiet.checkpoint(), Err(Cancelled));
    }

    // JOB-REQ-0004
    #[test]
    fn progress_is_bounded_and_complete() {
        for total in [0u64, 1, 7, 255, 256, 257, 1000, 10_000_000] {
            let r = Record::default();
            let ctx = Ctx::new(None, Some(&r));
            for done in 1..=total.min(20_000_000) {
                ctx.progress("p", done, total);
            }
            let ev = r.0.into_inner().unwrap();
            assert!(ev.len() <= 257, "{total}: {} events", ev.len());
            if total > 0 {
                assert_eq!(
                    ev.last(),
                    Some(&Event::Progress {
                        phase: "p",
                        done: total,
                        total
                    })
                );
            }
        }
    }

    // JOB-REQ-0006
    #[test]
    fn job_error_maps() {
        let e: JobError<i32> = Cancelled.into();
        assert_eq!(e.map(|x| x + 1), JobError::Cancelled);
        assert_eq!(JobError::Failed(1).map(|x| x + 1), JobError::Failed(2));
    }
}
