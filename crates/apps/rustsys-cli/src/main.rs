// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The `rustsys` command (`docs/specs/cli/README.md`).

fn main() -> std::process::ExitCode {
    rustsys_cli::run(std::env::args_os().collect())
}
