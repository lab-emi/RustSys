// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `cargo xtask verify [synth] [--gpu]`: runs the verification suites of
//! `verification/suites/synth/suites.toml` and the command-line contract: integration tests of the
//! command-line crate, and the mapper's unit tests and the job-hook tests of the synthesis engine.
//! `--gpu` builds the command line with every GPU runtime and adds its ignored GPU tests.

use std::path::Path;

/// Options of `verify`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    pub(crate) gpu: bool,
}

/// The integration-test targets of the command-line crate that `verify synth` runs.
pub(crate) const CLI_SUITES: [&str; 12] = [
    "corpus",
    "defects",
    "random",
    "metamorphic",
    "known_answer",
    "synth_frontend",
    "check_obligations",
    "check_seqsim",
    "check_netlists",
    "synth_backends",
    "cli_contract",
    "netlist_sources",
];

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// The cargo invocations of `product`, in order.
pub(crate) fn plan(product: &str, o: Options) -> Result<Vec<Vec<String>>, String> {
    if product != "synth" {
        return Err(format!("unknown product '{product}'"));
    }
    let mut a = args(&["test", "--release", "-p", "rustsys-cli"]);
    if o.gpu {
        a.extend(args(&["--features", "cuda"]));
    }
    for t in CLI_SUITES {
        a.push("--test".into());
        a.push(t.to_string());
    }
    if o.gpu {
        a.extend(args(&["--", "--include-ignored"]));
    }
    Ok(vec![
        a,
        // Suite `mapping-parallel`: unit tests of the mapper.
        args(&["test", "--release", "-p", "rustsys", "--lib", "map::"]),
        // Observation and cancellation of synthesis runs.
        args(&["test", "--release", "-p", "rustsys", "--test", "job_hooks"]),
    ])
}

/// The `verify` command: runs every step of the plan (all of them, also after a failure).
pub(crate) fn run(root: &Path, product: &str, o: Options) -> Result<(), String> {
    let mut failed = Vec::new();
    let guard = crate::guard::Guard::detect();
    println!("verify: {}", guard.describe());
    for a in &plan(product, o)? {
        let ok = guard
            .cargo(env!("CARGO"), a)
            .current_dir(root)
            .status()
            .map_err(|e| format!("cargo {}: {e}", a.join(" ")))?
            .success();
        if !ok {
            failed.push(format!("cargo {}", a.join(" ")));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "verification suites failed:\n  {}",
            failed.join("\n  ")
        ))
    }
}

#[cfg(test)]
mod tests;
