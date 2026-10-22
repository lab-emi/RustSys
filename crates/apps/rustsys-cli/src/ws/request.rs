// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! `--request-id` of the mutating post-mapping commands (CLI-REQ-0212): an agent retrying a
//! command after a lost answer gets the revision that the first attempt committed, instead of
//! a second revision or a revision conflict.
//!
//! The id is recorded through the argument vector that `history.jsonl` already keeps for every
//! command (WS-REQ-0042), so the shared history schema is unchanged.

use rusteda_workspace::{Error, HistoryEntry, Rev, Workspace};

/// The longest request id.
pub(crate) const MAX_LEN: usize = 64;

/// Checks that `id` is a shell-safe handle of `[A-Za-z0-9_-]`, 1 to [`MAX_LEN`] characters
/// (Rule 4.1); anything else is `USAGE`.
pub(crate) fn check(id: &str) -> Result<(), Error> {
    let ok = !id.is_empty()
        && id.len() <= MAX_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(Error::usage(format!(
            "--request-id '{id}' is not 1 to {MAX_LEN} characters of [A-Za-z0-9_-]"
        )))
    }
}

/// The request id of a recorded argument vector: the value of `--request-id ID` or
/// `--request-id=ID`.
pub(crate) fn of_argv(argv: &[String]) -> Option<&str> {
    let mut words = argv.iter();
    while let Some(w) = words.next() {
        if w == "--" {
            return None;
        }
        if w == "--request-id" {
            return words.next().map(String::as_str);
        }
        if let Some(v) = w.strip_prefix("--request-id=") {
            return Some(v);
        }
    }
    None
}

/// The last successful commit of `command` with request id `id` in `history.jsonl`, if any.
/// It must have the same base revision: an id reused for another base is `INPUT_REJECTED`.
pub(crate) fn replay(
    ws: &Workspace,
    command: &str,
    id: &str,
    base: Option<Rev>,
) -> Result<Option<HistoryEntry>, Error> {
    let found = ws.history()?.into_iter().rev().find(|h| {
        h.ok && h.command == command && h.new_rev.is_some() && of_argv(&h.argv) == Some(id)
    });
    match found {
        Some(h) if base.is_some() && h.base_rev != base => Err(Error::input_rejected(format!(
            "request id '{id}' already committed {} from base {}, not from {}; use a new \
             request id for another base revision",
            h.new_rev.map_or_else(|| "?".into(), |r| r.to_string()),
            h.base_rev.map_or_else(|| "none".into(), |r| r.to_string()),
            base.map_or_else(|| "none".into(), |r| r.to_string()),
        ))),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CLI-REQ-0212: shell-safe ids only; the id is read from either spelling of the option.
    #[test]
    fn ids_and_argv() {
        for ok in ["req1", "a-b_C9", &"x".repeat(MAX_LEN)] {
            check(ok).unwrap();
        }
        for bad in ["", "a b", "#p1", "$x", "a/b", "é", &"x".repeat(MAX_LEN + 1)] {
            assert!(check(bad).is_err(), "{bad:?}");
        }
        let argv = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            of_argv(&argv(&["rustsys", "resize", "--request-id", "r1"])),
            Some("r1")
        );
        assert_eq!(of_argv(&argv(&["rustsys", "--request-id=r2"])), Some("r2"));
        assert_eq!(of_argv(&argv(&["rustsys", "resize"])), None);
        assert_eq!(
            of_argv(&argv(&["rustsys", "--", "--request-id", "r3"])),
            None
        );
    }
}
