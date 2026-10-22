// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Revisions and object handles.
//!
//! - [`Rev`] is a revision `rev<N>`: printed unpadded, read with or without zero padding,
//!   stored in the directory `rev<N zero-padded to at least 4 digits>` (WS-REQ-0020).
//! - [`RevSpec`] is what a query names (`--rev`, `--from`, `--to`): `HEAD` or `rev<N>`.
//!   [`BaseRev`] is what a mutation names (`--base-rev`): only `rev<N>`, never `HEAD`
//!   (WS-REQ-0021). They are different types, so a revision resolved for a query cannot be
//!   passed where a base revision is required.
//! - [`Handle`] is a revision-scoped object handle `<prefix><N>`, such as `inst12`, `net3`,
//!   `pin7`, `path1`, `sel4` or `mm2`. Every handle matches `[A-Za-z0-9_-]+`, and `N` starts at
//!   1: `inst<N>` names row `N - 1` of its table, `path<N>` the path of rank `N` (contract §2,
//!   CA-HANDLES; WS-REQ-0022).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A revision or handle text that does not have the required form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("'{text}' {reason}")]
pub struct BadHandle {
    /// The rejected text.
    pub text: String,
    /// Why it was rejected, phrased to follow the quoted text.
    pub reason: String,
}

impl BadHandle {
    fn new(text: &str, reason: impl Into<String>) -> BadHandle {
        BadHandle {
            text: text.to_string(),
            reason: reason.into(),
        }
    }
}

/// A revision number, printed `rev<N>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Rev(pub u32);

impl Rev {
    /// `rev0`, the empty revision that `init` creates.
    pub const ZERO: Rev = Rev(0);

    /// Reads `rev<N>`, with or without zero padding (`rev3`, `rev0003`). `HEAD` is rejected:
    /// queries read it with [`RevSpec::parse`].
    pub fn parse(text: &str) -> Result<Rev, BadHandle> {
        if text == "HEAD" {
            return Err(BadHandle::new(
                text,
                "is not accepted here: name an explicit revision such as rev3 \
                 (HEAD is accepted only by --rev, --from and --to)",
            ));
        }
        let Some(digits) = text.strip_prefix("rev") else {
            return Err(BadHandle::new(
                text,
                "is not a revision: expected rev<N>, e.g. rev3",
            ));
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(BadHandle::new(
                text,
                "is not a revision: expected rev<N>, e.g. rev3",
            ));
        }
        parse_decimal(digits)
            .map(Rev)
            .ok_or_else(|| BadHandle::new(text, "is out of range"))
    }

    /// The revision number.
    pub const fn number(self) -> u32 {
        self.0
    }

    /// The directory name: `rev` and the number zero-padded to at least 4 digits.
    pub fn dir_name(self) -> String {
        format!("rev{:04}", self.0)
    }

    /// Reads a directory name; only the canonical form of [`Rev::dir_name`] is accepted.
    pub fn from_dir_name(name: &str) -> Option<Rev> {
        let rev = Rev(parse_decimal(name.strip_prefix("rev")?)?);
        (rev.dir_name() == name).then_some(rev)
    }

    /// The next revision number, or `None` at `u32::MAX`.
    pub fn next(self) -> Option<Rev> {
        self.0.checked_add(1).map(Rev)
    }
}

impl fmt::Display for Rev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rev{}", self.0)
    }
}

impl FromStr for Rev {
    type Err = BadHandle;

    fn from_str(text: &str) -> Result<Rev, BadHandle> {
        Rev::parse(text)
    }
}

impl Serialize for Rev {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rev {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Rev, D::Error> {
        let text = String::deserialize(deserializer)?;
        Rev::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// The revision a query names: `HEAD` or `rev<N>` (`--rev`, `--from`, `--to`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RevSpec {
    /// The workspace HEAD at the time the query reads the manifest.
    Head,
    /// An explicit revision.
    Rev(Rev),
}

impl RevSpec {
    /// Reads `HEAD` or `rev<N>`.
    pub fn parse(text: &str) -> Result<RevSpec, BadHandle> {
        if text == "HEAD" {
            Ok(RevSpec::Head)
        } else {
            Rev::parse(text).map(RevSpec::Rev)
        }
    }
}

impl fmt::Display for RevSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RevSpec::Head => f.write_str("HEAD"),
            RevSpec::Rev(rev) => rev.fmt(f),
        }
    }
}

impl FromStr for RevSpec {
    type Err = BadHandle;

    fn from_str(text: &str) -> Result<RevSpec, BadHandle> {
        RevSpec::parse(text)
    }
}

/// The base revision of a mutation (`--base-rev`): always an explicit `rev<N>`, never `HEAD`.
///
/// [`Workspace::resolve_base`](crate::Workspace::resolve_base) also checks that it exists;
/// [`Workspace::begin`](crate::Workspace::begin) and
/// [`Workspace::revert`](crate::Workspace::revert) take only this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BaseRev(Rev);

impl BaseRev {
    /// Reads `rev<N>`; `HEAD` is rejected.
    pub fn parse(text: &str) -> Result<BaseRev, BadHandle> {
        Rev::parse(text).map(BaseRev)
    }

    /// The revision.
    pub const fn rev(self) -> Rev {
        self.0
    }
}

impl fmt::Display for BaseRev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A revision-scoped object handle `<prefix><N>`, such as `inst12` or `sel4`.
///
/// The prefix starts with an ASCII letter, contains only `[A-Za-z0-9_-]` and does not end in a
/// digit; `N` is decimal, read with or without zero padding and printed unpadded. `N` starts at
/// 1 (contract §2, CA-HANDLES): `inst<N>`, `net<N>` and `port<N>` are `1 + row` of the netlist,
/// `pin<N>` is `1 + row` of its pin table, and `path<N>` and `mm<N>` are 1-based ranks, so
/// `N = 0` is never a handle; [`Handle::from_row`] and [`Handle::row`] convert (WS-REQ-0022).
/// Handles are always paired with their revision in JSON.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Handle {
    prefix: String,
    index: u32,
}

impl Handle {
    /// A handle from its parts; `index` is `N`, at least 1.
    pub fn new(prefix: &str, index: u32) -> Result<Handle, BadHandle> {
        check_prefix(prefix)?;
        if index == 0 {
            return Err(BadHandle::new(&format!("{prefix}0"), ZERO));
        }
        Ok(Handle {
            prefix: prefix.to_string(),
            index,
        })
    }

    /// The handle of 0-based row (or rank minus one) `row`: `<prefix><row + 1>`
    /// (CA-HANDLES). Row `u32::MAX` has no handle.
    pub fn from_row(prefix: &str, row: u32) -> Result<Handle, BadHandle> {
        let index = row.checked_add(1).ok_or_else(|| {
            BadHandle::new(
                &format!("{prefix}{row}"),
                "is out of range: row has no handle",
            )
        })?;
        Handle::new(prefix, index)
    }

    /// Reads `<prefix><N>`.
    pub fn parse(text: &str) -> Result<Handle, BadHandle> {
        check_handle(text)?;
        let split = text.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let (prefix, digits) = text.split_at(split);
        if digits.is_empty() {
            return Err(BadHandle::new(
                text,
                "has no number: expected <prefix><N>, e.g. inst12",
            ));
        }
        if prefix.is_empty() {
            return Err(BadHandle::new(
                text,
                "has no prefix: expected <prefix><N>, e.g. inst12",
            ));
        }
        check_prefix(prefix).map_err(|e| BadHandle::new(text, e.reason))?;
        let index = parse_decimal(digits).ok_or_else(|| BadHandle::new(text, "is out of range"))?;
        if index == 0 {
            return Err(BadHandle::new(text, ZERO));
        }
        Ok(Handle {
            prefix: prefix.to_string(),
            index,
        })
    }

    /// Reads `<prefix><N>` for the given prefix and returns `N`.
    pub fn parse_as(text: &str, prefix: &str) -> Result<u32, BadHandle> {
        let handle = Handle::parse(text)?;
        if handle.prefix != prefix {
            return Err(BadHandle::new(
                text,
                format!("is not a {prefix} handle: expected {prefix}<N>, e.g. {prefix}1"),
            ));
        }
        Ok(handle.index)
    }

    /// The prefix, e.g. `inst`.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The number `N`, at least 1.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// The 0-based row (or rank minus one) the handle names: `N - 1` (CA-HANDLES). A handle
    /// that does not resolve to a row of its table is `UNKNOWN_HANDLE` for the tool.
    pub fn row(&self) -> u32 {
        // `N >= 1` for every handle: `new` and `parse` refuse 0.
        self.index - 1
    }
}

/// Why `N = 0` is not a handle.
const ZERO: &str = "is not a handle: N starts at 1 (contract §2, CA-HANDLES), e.g. inst1";

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.prefix, self.index)
    }
}

impl FromStr for Handle {
    type Err = BadHandle;

    fn from_str(text: &str) -> Result<Handle, BadHandle> {
        Handle::parse(text)
    }
}

impl Serialize for Handle {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Handle {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Handle, D::Error> {
        let text = String::deserialize(deserializer)?;
        Handle::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Whether `text` is shell-safe: non-empty and only `[A-Za-z0-9_-]`.
pub fn is_handle(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Checks [`is_handle`] and explains a failure.
pub fn check_handle(text: &str) -> Result<(), BadHandle> {
    if text.is_empty() {
        return Err(BadHandle::new(text, "is empty"));
    }
    if !is_handle(text) {
        return Err(BadHandle::new(
            text,
            "is not a handle: handles match [A-Za-z0-9_-]+",
        ));
    }
    Ok(())
}

fn check_prefix(prefix: &str) -> Result<(), BadHandle> {
    let bytes = prefix.as_bytes();
    let valid = bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && is_handle(prefix)
        && bytes.last().is_some_and(|b| !b.is_ascii_digit());
    if !valid {
        return Err(BadHandle::new(
            prefix,
            "is not a handle prefix: it starts with a letter, uses [A-Za-z0-9_-] and does not \
             end in a digit",
        ));
    }
    Ok(())
}

/// Decimal digits (zero padding allowed) to `u32`; no sign, no other characters.
fn parse_decimal(digits: &str) -> Option<u32> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // WS-REQ-0020
    #[test]
    fn revisions() {
        assert_eq!(Rev::parse("rev3"), Ok(Rev(3)));
        assert_eq!(Rev::parse("rev0003"), Ok(Rev(3)));
        assert_eq!(Rev::parse("rev0"), Ok(Rev(0)));
        assert_eq!(Rev::parse("rev4294967295"), Ok(Rev(u32::MAX)));
        for bad in [
            "",
            "rev",
            "3",
            "Rev3",
            "rev-1",
            "rev+1",
            "rev 1",
            "rev1a",
            "rev4294967296",
        ] {
            assert!(Rev::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Rev(3).to_string(), "rev3");
        assert_eq!(Rev(3).dir_name(), "rev0003");
        assert_eq!(Rev(0).dir_name(), "rev0000");
        assert_eq!(Rev(12345).dir_name(), "rev12345");
        assert_eq!(Rev::from_dir_name("rev0003"), Some(Rev(3)));
        assert_eq!(Rev::from_dir_name("rev12345"), Some(Rev(12345)));
        assert_eq!(Rev::from_dir_name("rev3"), None);
        assert_eq!(Rev::from_dir_name("rev01234"), None);
        assert_eq!(Rev::from_dir_name(".staging-1-2"), None);
        assert_eq!(serde_json::to_string(&Rev(7)).unwrap(), "\"rev7\"");
        assert_eq!(serde_json::from_str::<Rev>("\"rev0007\"").unwrap(), Rev(7));
        assert_eq!(Rev(u32::MAX).next(), None);
    }

    // WS-REQ-0021
    #[test]
    fn head_only_for_queries() {
        assert_eq!(RevSpec::parse("HEAD"), Ok(RevSpec::Head));
        assert_eq!(RevSpec::parse("rev2"), Ok(RevSpec::Rev(Rev(2))));
        assert!(RevSpec::parse("head").is_err());
        let err = BaseRev::parse("HEAD").unwrap_err();
        assert!(
            err.to_string().contains("HEAD is accepted only by"),
            "{err}"
        );
        assert_eq!(BaseRev::parse("rev0002").unwrap().rev(), Rev(2));
        assert_eq!(BaseRev::parse("rev2").unwrap().to_string(), "rev2");
        assert_eq!(RevSpec::Head.to_string(), "HEAD");
    }

    // WS-REQ-0022
    #[test]
    fn handles() {
        let h = Handle::parse("inst12").unwrap();
        assert_eq!((h.prefix(), h.index()), ("inst", 12));
        assert_eq!(h.to_string(), "inst12");
        assert_eq!(Handle::parse("inst_007").unwrap().to_string(), "inst_7");
        assert_eq!(Handle::parse("my-path3").unwrap().prefix(), "my-path");
        assert_eq!(Handle::parse_as("sel4", "sel"), Ok(4));
        assert!(
            Handle::parse_as("path4", "sel")
                .unwrap_err()
                .to_string()
                .contains("sel handle")
        );
        for bad in [
            "",
            "12",
            "inst",
            "inst#1",
            "inst 1",
            "$x1",
            "_x1",
            "inst99999999999",
            "é1",
        ] {
            assert!(Handle::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Handle::new("path", 3).unwrap().to_string(), "path3");
        assert!(Handle::new("p2", 3).is_err());
        assert!(Handle::new("", 3).is_err());
        assert!(is_handle("rev3") && is_handle("a-b_C9") && !is_handle("a.b") && !is_handle(""));
        assert_eq!(serde_json::to_string(&h).unwrap(), "\"inst12\"");
        assert_eq!(
            serde_json::from_str::<Handle>("\"mm2\"").unwrap(),
            Handle::new("mm", 2).unwrap()
        );
    }
}
