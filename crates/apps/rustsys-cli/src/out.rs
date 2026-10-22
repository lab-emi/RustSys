// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Standard error: coded messages, colour and progress (CLI-REQ-0004, CLI-REQ-0007,
//! CLI-REQ-0008). Standard output carries results only (CLI-REQ-0002).

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::proto::progress::ProgressState;
use crate::proto::{Diag, Event, Observer, Severity};

/// How progress is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    Live,
    Log,
    Off,
}

/// Standard error of a command: messages and progress from any thread.
pub struct Err {
    color: bool,
    quiet: bool,
    mode: ProgressMode,
    product: &'static str,
    state: Mutex<ProgressState>,
    /// The live line is on screen.
    line: AtomicBool,
    lock: Mutex<()>,
    t0: Instant,
    width: usize,
}

impl std::fmt::Debug for Err {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Err")
            .field("mode", &self.mode)
            .field("quiet", &self.quiet)
            .finish()
    }
}

fn paint(color: bool, sev: Severity) -> (&'static str, &'static str) {
    if !color {
        return ("", "");
    }
    match sev {
        Severity::Error => ("\x1b[1;31m", "\x1b[0m"),
        Severity::Warning => ("\x1b[1;33m", "\x1b[0m"),
        Severity::Note => ("\x1b[1;36m", "\x1b[0m"),
    }
}

/// The lines of a message on standard error (CLI-REQ-0004, CLI-REQ-0007): the first line with
/// the file location, only the severity word coloured, then the help line when the message or its
/// code has one.
pub fn message_text(color: bool, d: &Diag) -> String {
    let (a, b) = paint(color, d.severity);
    let first = d.render_first_line();
    let word = d.severity.as_str();
    let rest = first.strip_prefix(word).unwrap_or(&first);
    let mut out = format!("{a}{word}{b}{rest}\n");
    let help = d
        .help
        .clone()
        .or_else(|| crate::proto::codes::help_for(&d.code).map(String::from));
    if let Some(h) = help.filter(|h| !h.is_empty()) {
        out.push_str(&format!("  help: {h}\n"));
    }
    out
}

impl Err {
    pub fn new(color: bool, quiet: bool, mode: ProgressMode, product: &'static str) -> Err {
        // The terminal's width, else $COLUMNS, else 80 (CLI-REQ-0008).
        let width = crossterm::terminal::size()
            .ok()
            .map(|(w, _)| usize::from(w))
            .filter(|w| *w >= 20)
            .or_else(|| {
                std::env::var("COLUMNS")
                    .ok()
                    .and_then(|c| c.parse().ok())
                    .filter(|w: &usize| *w >= 20)
            })
            .unwrap_or(80);
        Err {
            color,
            quiet,
            mode,
            product,
            state: Mutex::new(ProgressState::default()),
            line: AtomicBool::new(false),
            lock: Mutex::new(()),
            t0: Instant::now(),
            width,
        }
    }

    fn clear_line(&self, e: &mut dyn Write) {
        if self.line.swap(false, Ordering::Relaxed) {
            let _ = e.write_all(b"\r\x1b[2K");
        }
    }

    /// Prints a message with its help line (CLI-REQ-0004); `--quiet` keeps errors and warnings.
    pub fn diag(&self, d: &Diag) {
        if self.quiet && d.severity == Severity::Note {
            return;
        }
        let _g = self.lock.lock();
        let mut e = std::io::stderr().lock();
        self.clear_line(&mut e);
        let _ = e.write_all(message_text(self.color, d).as_bytes());
    }

    /// Draws the live line (CLI-REQ-0008): first after 300 ms.
    pub fn tick(&self) {
        if self.mode != ProgressMode::Live || self.t0.elapsed() < Duration::from_millis(300) {
            return;
        }
        let line = {
            let Ok(s) = self.state.lock() else {
                return;
            };
            format!(
                "{}  {}  {}",
                self.product,
                s.line(),
                crate::proto::format::clock(self.t0.elapsed().as_secs_f64())
            )
        };
        let line: String = line
            .chars()
            .filter(char::is_ascii)
            .take(self.width.saturating_sub(1))
            .collect();
        let _g = self.lock.lock();
        let mut e = std::io::stderr().lock();
        let _ = write!(e, "\r{line}\x1b[K");
        let _ = e.flush();
        self.line.store(true, Ordering::Relaxed);
    }

    /// Clears the live line at the end.
    pub fn finish(&self) {
        let _g = self.lock.lock();
        let mut e = std::io::stderr().lock();
        self.clear_line(&mut e);
        let _ = e.flush();
    }

    pub fn mode(&self) -> ProgressMode {
        self.mode
    }
}

impl Observer for Err {
    fn event(&self, ev: &Event) {
        if let Ok(mut s) = self.state.lock() {
            s.apply(ev);
        }
        match ev {
            Event::Diagnostic(d) => self.diag(d),
            Event::Step {
                phase,
                step,
                ordinal,
                count,
            } if self.mode == ProgressMode::Log => {
                let _g = self.lock.lock();
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "progress: {:.3} s {} {phase} {step} ({ordinal}/{count})",
                    self.t0.elapsed().as_secs_f64(),
                    self.product
                );
            }
            Event::Metric { name, value } if self.mode == ProgressMode::Log => {
                let _g = self.lock.lock();
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "progress: {:.3} s {} {name}={value}",
                    self.t0.elapsed().as_secs_f64(),
                    self.product
                );
            }
            Event::PhaseFinished { phase, seconds } if self.mode == ProgressMode::Log => {
                let _g = self.lock.lock();
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "progress: {:.3} s {} {phase} done in {seconds:.3} s",
                    self.t0.elapsed().as_secs_f64(),
                    self.product
                );
            }
            _ => {}
        }
    }
}

/// Writes results to standard output; a broken pipe ends output silently (CLI-REQ-0011).
pub fn stdout(bytes: &[u8]) -> Result<(), Diag> {
    let mut o = std::io::stdout().lock();
    match o.write_all(bytes).and_then(|()| o.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(Diag::error("A0004", format!("standard output: {e}"))),
    }
}

/// Runs `f` while a ticker thread redraws the live line every 100 ms.
pub fn with_ticker<T>(err: &Err, f: impl FnOnce() -> T) -> T {
    if err.mode() != ProgressMode::Live {
        return f();
    }
    let done = AtomicBool::new(false);
    let out = std::thread::scope(|s| {
        let ticker = std::thread::Builder::new()
            .name("rustsys-progress".into())
            .spawn_scoped(s, || {
                while !done.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                    if !done.load(Ordering::Relaxed) {
                        err.tick();
                    }
                }
            })
            .ok();
        let out = f();
        done.store(true, Ordering::Relaxed);
        if let Some(t) = ticker {
            let _ = t.join();
        }
        out
    });
    err.finish();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // CLI-REQ-0004, CLI-REQ-0007: the file location after the code; only the severity word is
    // coloured; codes without an entry in the code table pass unchanged, without a help line.
    #[test]
    fn messages() {
        let plain = Diag::warning("S0532", "final cell area 12 exceeds the requested limit 10");
        assert_eq!(
            message_text(false, &plain),
            "warning[S0532]: final cell area 12 exceeds the requested limit 10\n  help: the \
             produced netlist exceeds the area target; inspect the electrical report\n"
        );
        let both = Diag::error("V0100", "expected ';'")
            .at("top.sv", 7, 1)
            .with_help("end the statement");
        assert_eq!(
            message_text(true, &both),
            "\x1b[1;31merror\x1b[0m[V0100]: top.sv:7:1: expected ';'\n  help: end the statement\n"
        );
        for code in ["V0101", "C0001", "S0702"] {
            let d = Diag::note(code, "x").at("f", 1, 1);
            assert_eq!(message_text(false, &d), format!("note[{code}]: f:1:1: x\n"));
        }
        // Codes of the application keep their help lines from the code table.
        assert_eq!(
            message_text(false, &Diag::error("A0008", "no runs root")),
            "error[A0008]: no runs root\n  help: give --runs-root DIR or set RUSTSYS_RUNS_ROOT\n"
        );
    }
}
