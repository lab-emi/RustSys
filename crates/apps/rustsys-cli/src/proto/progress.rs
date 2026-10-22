// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The progress of a running job as the progress line shows it (CLI-REQ-0008), and an observer
//! combinator.

use rusteda_core::job::{Event, Observer};

/// What the progress line shows of a running job.
#[derive(Debug, Clone, Default)]
pub struct ProgressState {
    /// The running phase.
    pub current: Option<&'static str>,
    /// Step name, ordinal and count within the current phase.
    pub step: Option<(&'static str, u32, u32)>,
    /// Done and total units of the current step.
    pub done: Option<(u64, u64)>,
}

impl ProgressState {
    /// Folds one event.
    pub fn apply(&mut self, e: &Event) {
        match e {
            Event::PhaseStarted { phase } => {
                self.current = Some(phase);
                self.step = None;
                self.done = None;
            }
            Event::PhaseFinished { phase, .. } => {
                if self.current == Some(*phase) {
                    self.current = None;
                    self.step = None;
                    self.done = None;
                }
            }
            Event::Step {
                step,
                ordinal,
                count,
                ..
            } => {
                self.step = Some((step, *ordinal, *count));
                self.done = None;
            }
            Event::Progress { done, total, .. } => self.done = Some((*done, *total)),
            _ => {}
        }
    }

    /// Fraction done of the current step, clamped to [0, 1].
    pub fn fraction(&self) -> Option<f64> {
        self.done
            .filter(|d| d.1 > 0)
            .map(|(d, t)| (d as f64 / t as f64).clamp(0.0, 1.0))
    }

    /// One ASCII line: `<phase>  [<step> i/n]  [<done>/<total> <p>%]` (CLI-REQ-0008).
    pub fn line(&self) -> String {
        let mut s = String::new();
        if let Some(p) = self.current {
            s.push_str(p);
        }
        if let Some((step, i, n)) = self.step {
            if n > 0 {
                s.push_str(&format!("  {step} {i}/{n}"));
            } else {
                s.push_str(&format!("  {step}"));
            }
        }
        if let (Some((d, t)), Some(f)) = (self.done, self.fraction()) {
            s.push_str(&format!("  {d}/{t} {:.0}%", f * 100.0));
        }
        s
    }
}

/// Passes every event to each observer in turn.
#[derive(Clone, Copy)]
pub struct Tee<'a>(pub &'a [&'a dyn Observer]);

impl std::fmt::Debug for Tee<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tee({} observers)", self.0.len())
    }
}

impl Observer for Tee<'_> {
    fn event(&self, e: &Event) {
        for o in self.0 {
            o.event(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusteda_core::job::Diag;

    // CLI-REQ-0008: the line follows the phase, the step and the progress of the step; a finished
    // phase clears it.
    #[test]
    fn folds_events() {
        let mut s = ProgressState::default();
        assert_eq!(s.line(), "");
        s.apply(&Event::PhaseStarted { phase: "map" });
        s.apply(&Event::Step {
            phase: "map",
            step: "area_pass",
            ordinal: 2,
            count: 4,
        });
        s.apply(&Event::Progress {
            phase: "map",
            done: 50,
            total: 200,
        });
        s.apply(&Event::Diagnostic(Diag::warning("S0501", "x")));
        assert_eq!(s.line(), "map  area_pass 2/4  50/200 25%");
        s.apply(&Event::Step {
            phase: "map",
            step: "exact_pass",
            ordinal: 0,
            count: 0,
        });
        assert_eq!(s.line(), "map  exact_pass");
        s.apply(&Event::PhaseFinished {
            phase: "map",
            seconds: 1.5,
        });
        assert_eq!(s.current, None);
        assert_eq!(s.line(), "");
    }
}
