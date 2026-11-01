// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The dual-PDK suite (`docs/verification/pdk.md`): the fixture designs synthesized and checked
//! with pinned standard-cell libraries of two process design kits.
//!
//! - `pdk fetch [--dir DIR]`: downloads and verifies the libraries of `verification/pdks.toml`
//!   ([`mod@fetch`]);
//! - `verify pdk [--dir DIR] [--require] [--timeout SECS]`: runs the suite ([`run`]). Without the
//!   libraries it prints a SKIPPED banner and succeeds, unless `--require` is given.
//!
//! The libraries live in the library directory: `--dir`, else `$RUSTEDA_PDK_DIR`, else
//! `target/pdk` of the repository ([`fetch::library_dir`]). They are never tracked: design data
//! is tracked only as synthetic fixtures (AGENTS 5.6), and the files exceed the size limit.
//!
//! `fetch.rs`, `manifest.rs`, `json.rs` and `verification/pdks.toml` are shared verbatim with the
//! other RustEDA tool repositories; the command line (this file), the fixture index
//! ([`designs`]), the runner ([`run`]) and the hazard scan ([`scan`]) belong to this repository.

mod designs;
mod fetch;
pub(crate) mod json;
mod manifest;
mod run;
mod scan;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default time limit of each `rustsys` invocation of `verify pdk`.
const TIMEOUT_S: u64 = 600;

/// Options of `pdk fetch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FetchOptions {
    pub(crate) dir: Option<PathBuf>,
}

/// Options of `verify pdk`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifyOptions {
    pub(crate) dir: Option<PathBuf>,
    /// Missing libraries fail the suite instead of skipping it.
    pub(crate) require: bool,
    /// Time limit of each `rustsys` invocation.
    pub(crate) timeout: Duration,
}

impl Default for VerifyOptions {
    fn default() -> VerifyOptions {
        VerifyOptions {
            dir: None,
            require: false,
            timeout: Duration::from_secs(TIMEOUT_S),
        }
    }
}

/// The value of option `name` at `args[*i]` (`--name VALUE` or `--name=VALUE`), advancing `*i`
/// past a separate value; `None` when `args[*i]` is not that option.
fn option_value(args: &[String], i: &mut usize, name: &str) -> Option<Result<String, String>> {
    let a = args.get(*i)?;
    if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
        return Some(if v.is_empty() {
            Err(format!("{name} needs a value"))
        } else {
            Ok(v.to_string())
        });
    }
    if a != name {
        return None;
    }
    *i += 1;
    Some(match args.get(*i) {
        Some(v) if !v.is_empty() && !v.starts_with("--") => Ok(v.clone()),
        _ => Err(format!("{name} needs a value")),
    })
}

/// `pdk fetch [--dir DIR]`: the arguments after `pdk`.
pub(crate) fn fetch_args(args: &[String]) -> Result<FetchOptions, String> {
    match args.first().map(String::as_str) {
        Some("fetch") => {}
        Some(other) => return Err(format!("unknown pdk command '{other}' (expected 'fetch')")),
        None => return Err("usage: cargo xtask pdk fetch [--dir DIR]".into()),
    }
    let mut o = FetchOptions::default();
    let mut i = 1;
    while let Some(a) = args.get(i) {
        match option_value(args, &mut i, "--dir") {
            Some(v) => o.dir = Some(PathBuf::from(v?)),
            None if a.starts_with('-') => return Err(format!("unknown option '{a}'")),
            None => return Err(format!("unexpected argument '{a}'")),
        }
        i += 1;
    }
    Ok(o)
}

/// `verify pdk [--dir DIR] [--require] [--timeout SECS]`: the arguments after `pdk`.
pub(crate) fn verify_args(args: &[String]) -> Result<VerifyOptions, String> {
    let mut o = VerifyOptions::default();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if let Some(v) = option_value(args, &mut i, "--dir") {
            o.dir = Some(PathBuf::from(v?));
        } else if let Some(v) = option_value(args, &mut i, "--timeout") {
            let v = v?;
            o.timeout = match v.parse::<u64>() {
                Ok(s) if s > 0 => Duration::from_secs(s),
                _ => {
                    return Err(format!(
                        "--timeout needs a positive number of seconds, not '{v}'"
                    ));
                }
            };
        } else if a == "--require" {
            o.require = true;
        } else if a.starts_with('-') {
            return Err(format!("unknown option '{a}'"));
        } else {
            return Err(format!("unexpected argument '{a}'"));
        }
        i += 1;
    }
    Ok(o)
}

/// The library directory of `--dir` or the environment ([`fetch::library_dir`]).
fn library_dir(root: &Path, dir: Option<&Path>) -> PathBuf {
    fetch::library_dir(root, dir, std::env::var_os(fetch::DIR_VAR))
}

/// The `pdk fetch` command. The run outputs of `verify pdk` are no place for libraries: each run
/// deletes them.
pub(crate) fn fetch(root: &Path, o: &FetchOptions) -> Result<(), String> {
    let libs = manifest::load(root)?;
    let dir = library_dir(root, o.dir.as_deref());
    fetch::run(root, &libs, &dir, &[root.join(run::OUT)])
}

/// The `verify pdk` command.
pub(crate) fn verify(root: &Path, o: &VerifyOptions) -> Result<(), String> {
    let libs = manifest::load(root)?;
    let dir = library_dir(root, o.dir.as_deref());
    run::run(root, &libs, &dir, o)
}
