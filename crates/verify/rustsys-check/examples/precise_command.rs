// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Benchmark-only monotonic timer. The child includes the existing Python
//! pipeline startup and all four CLI processes; resource-guard startup is
//! outside this interval. No timing overhead is subtracted. <4 MiB argv/state,
//! <=4096 arguments; parent provides CPU, RAM, file and wall guards.

use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).take(4098).collect();
    if !(2..=4097).contains(&args.len()) {
        eprintln!("usage: precise_command CLOCK_JSON PROGRAM [ARG ...]");
        return ExitCode::from(2);
    }
    let started = Instant::now();
    let status = Command::new(&args[1])
        .args(&args[2..])
        .stdin(Stdio::null())
        .status();
    let wall_ns = started.elapsed().as_nanos();
    let code = status.map_or(125, |status| status.code().unwrap_or(125));
    let record = format!(
        "{{\"wall_ns\":{wall_ns},\"exit_code\":{code},\"clock\":\"rust-std-Instant-monotonic\"}}\n"
    );
    if let Err(error) = std::fs::write(&args[0], record) {
        eprintln!("BENCH_CLOCK_WRITE: {error}");
        return ExitCode::from(125);
    }
    ExitCode::from(u8::try_from(code).unwrap_or(125))
}
