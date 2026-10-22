// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Tests of the command line of the dual-PDK suite. The shared modules test themselves
//! (`fetch.rs`, `manifest.rs`, `json.rs`), as do the fixture index, the runner and the scan.

use super::*;

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// `pdk fetch [--dir DIR]`: the subcommand is required, `--dir` needs a value and takes either
// spelling; anything else fails.
#[test]
fn fetch_arguments() {
    let a = |s: &[&str]| fetch_args(&strings(s));
    assert_eq!(a(&["fetch"]), Ok(FetchOptions::default()));
    let dir = Ok(FetchOptions {
        dir: Some("libs".into()),
    });
    assert_eq!(a(&["fetch", "--dir", "libs"]), dir);
    assert_eq!(a(&["fetch", "--dir=libs"]), dir);
    for bad in [
        &[][..],
        &["get"],
        &["fetch", "--dir"],
        &["fetch", "--dir", "--require"],
        &["fetch", "--dir="],
        &["fetch", "libs"],
        &["fetch", "--require"],
    ] {
        assert!(a(bad).is_err(), "{bad:?}");
    }
}

// `verify pdk [--dir DIR] [--require] [--timeout SECS]`, in any order; the time limit defaults
// to 600 s and must be a positive number of seconds.
#[test]
fn verify_arguments() {
    let a = |s: &[&str]| verify_args(&strings(s));
    assert_eq!(a(&[]), Ok(VerifyOptions::default()));
    assert_eq!(VerifyOptions::default().timeout, Duration::from_secs(600));
    assert_eq!(
        a(&["--timeout", "30", "--require", "--dir=/x"]),
        Ok(VerifyOptions {
            dir: Some("/x".into()),
            require: true,
            timeout: Duration::from_secs(30),
        })
    );
    assert_eq!(
        a(&["--dir", "d"]).map(|o| (o.dir, o.require)),
        Ok((Some("d".into()), false))
    );
    for bad in [
        &["--timeout", "0"][..],
        &["--timeout", "x"],
        &["--timeout"],
        &["--gpu"],
        &["synth"],
        &["--dir"],
    ] {
        assert!(a(bad).is_err(), "{bad:?}");
    }
}
