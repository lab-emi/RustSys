// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Resource guard of the verification runs (AGENTS rule 9.3): every `cargo test` that `verify`
//! starts runs with at most four test threads (unless `RUST_TEST_THREADS` says otherwise), under a
//! wall-clock timeout and, where the user's service manager accepts transient scopes, inside one
//! with a memory limit; every synthesis and check step of `verify pdk` runs in such a scope under
//! its own time limit. A runaway allocation or a hang then ends that step instead of the host.
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `RUSTEDA_VERIFY_MEMORY_MAX` | `MemoryMax` of the scope | `16G` |
//! | `RUSTEDA_VERIFY_TIMEOUT_S` | Timeout of one `cargo test` step in seconds | `3600` |
//! | `RUSTEDA_VERIFY_NO_SCOPE` | When set, no scope (timeouts and threads still apply) | unset |

use std::ffi::OsStr;
use std::process::{Command, Stdio};

/// How the verification steps are confined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Guard {
    /// Run each step in a transient scope of the user's service manager.
    pub(crate) scope: bool,
    /// `MemoryMax` of the scope, in the manager's syntax (`16G`).
    pub(crate) memory_max: String,
    /// Wall-clock limit of one `cargo test` step in seconds.
    pub(crate) timeout_s: u64,
}

impl Guard {
    /// The guard of this machine and environment.
    pub(crate) fn detect() -> Guard {
        let memory_max =
            std::env::var("RUSTEDA_VERIFY_MEMORY_MAX").unwrap_or_else(|_| "16G".to_string());
        let timeout_s = std::env::var("RUSTEDA_VERIFY_TIMEOUT_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);
        let scope = std::env::var_os("RUSTEDA_VERIFY_NO_SCOPE").is_none()
            && Command::new("systemd-run")
                .args(["--user", "--scope", "--quiet", "true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
        Guard {
            scope,
            memory_max,
            timeout_s,
        }
    }

    /// The command that runs `program` in the memory scope, when there is one. The scope's
    /// manager runs `program` in its own process, so killing the child kills `program`.
    pub(crate) fn wrap(&self, program: impl AsRef<OsStr>) -> Command {
        if self.scope {
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "-p"])
                .arg(format!("MemoryMax={}", self.memory_max))
                .args(["-p", "MemorySwapMax=4G"])
                .arg(program);
            c
        } else {
            Command::new(program)
        }
    }

    /// The command that runs `cargo` with `args` under this guard: in the scope, under the
    /// timeout, with at most four test threads.
    pub(crate) fn cargo(&self, cargo: &str, args: &[String]) -> Command {
        let mut cmd = self.wrap("timeout");
        cmd.arg(format!("{}s", self.timeout_s))
            .arg(cargo)
            .args(args);
        if std::env::var_os("RUST_TEST_THREADS").is_none() {
            cmd.env("RUST_TEST_THREADS", "4");
        }
        cmd
    }

    /// One line that says how the steps are confined.
    pub(crate) fn describe(&self) -> String {
        if self.scope {
            format!(
                "steps run in a user scope with MemoryMax={} (cargo steps: {} s timeout)",
                self.memory_max, self.timeout_s
            )
        } else {
            format!(
                "steps run with no memory scope (cargo steps: {} s timeout)",
                self.timeout_s
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Guard;

    fn args(cmd: &std::process::Command) -> Vec<String> {
        std::iter::once(cmd.get_program())
            .chain(cmd.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn scoped_and_unscoped_commands() {
        let step = vec!["test".to_string(), "--release".to_string()];
        let scoped = Guard {
            scope: true,
            memory_max: "8G".to_string(),
            timeout_s: 60,
        };
        assert_eq!(
            args(&scoped.cargo("cargo", &step)),
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "-p",
                "MemoryMax=8G",
                "-p",
                "MemorySwapMax=4G",
                "timeout",
                "60s",
                "cargo",
                "test",
                "--release"
            ]
        );
        assert_eq!(
            args(&scoped.wrap("rustsys")),
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "-p",
                "MemoryMax=8G",
                "-p",
                "MemorySwapMax=4G",
                "rustsys"
            ]
        );
        let plain = Guard {
            scope: false,
            ..scoped
        };
        assert_eq!(
            args(&plain.cargo("cargo", &step)),
            ["timeout", "60s", "cargo", "test", "--release"]
        );
        assert_eq!(args(&plain.wrap("rustsys")), ["rustsys"]);
        assert!(plain.describe().contains("60 s"));
    }
}
