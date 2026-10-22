// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Names of netlist objects (CORE-REQ-0110 to 0113): the characters every name may use, the
//! segments and escapes of port, net and instance names, and the conversions between a name and
//! its raw text.
//!
//! Every name, of a module, cell, cell pin, port, net or instance, is non-empty and consists of
//! printable ASCII characters (33 to 126), so that it has an exact form in a netlist file: a
//! Verilog escaped identifier allows exactly these characters. Module, cell and pin names are
//! kept verbatim ([`check_verbatim`]). Port, net and instance names are hierarchical
//! ([`check_hierarchical`]): one or more segments joined by unescaped `/`; inside a segment `\`
//! escapes the next character, which must be `/` or `\`, and no segment is empty. A segment's
//! raw text, for example an escaped identifier without its backslash, becomes a segment by
//! [`escape_segment`], and [`join`] builds a hierarchical name from raw segments, as flattening
//! does with an instance path. [`segments`] and [`unescape`] give the raw text back.
//!
//! ```
//! use rusteda_core::names::{check_hierarchical, escape_segment, join, segments, unescape};
//!
//! assert_eq!(escape_segment("a/b"), r"a\/b");
//! let name = join(["u1", r"q\x/y"]);
//! assert_eq!(name, r"u1/q\\x\/y");
//! assert_eq!(segments(&name), ["u1", r"q\x/y"]);
//! assert_eq!(unescape(&name), r"u1/q\x/y");
//! assert!(check_hierarchical(&name).is_ok());
//! assert!(check_hierarchical("u1//q").is_err());
//! ```

use std::fmt;

/// Why a name breaks the rules of its kind; byte offsets count from the start of the name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NameError {
    /// The name is empty.
    Empty,
    /// A character outside printable ASCII (33 to 126).
    NotPrintable { at: usize, ch: char },
    /// A backslash that is not followed by `/` or `\` (hierarchical names only).
    BadEscape { at: usize },
    /// An unescaped `/` that begins or ends the name or follows another one, so that a segment
    /// is empty (hierarchical names only).
    EmptySegment { at: usize },
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::Empty => write!(f, "the name is empty"),
            NameError::NotPrintable { at, ch } => write!(
                f,
                "character U+{:04X} at byte {at} is not printable ASCII (33 to 126)",
                u32::from(*ch)
            ),
            NameError::BadEscape { at } => {
                write!(
                    f,
                    "the backslash at byte {at} is not followed by '/' or '\\'"
                )
            }
            NameError::EmptySegment { at } => {
                write!(f, "the '/' at byte {at} leaves an empty segment")
            }
        }
    }
}

impl std::error::Error for NameError {}

/// True for printable ASCII: the characters 33 (`!`) to 126 (`~`).
fn is_printable(c: u8) -> bool {
    (33..=126).contains(&c)
}

/// Checks the rules of every name (CORE-REQ-0110): it is non-empty and consists of printable
/// ASCII characters. These are all the rules of module, cell and pin names.
pub fn check_verbatim(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    match name.bytes().position(|c| !is_printable(c)) {
        None => Ok(()),
        // The bytes before `at` are ASCII, so a character starts at `at`.
        Some(at) => Err(NameError::NotPrintable {
            at,
            ch: name[at..]
                .chars()
                .next()
                .unwrap_or(char::REPLACEMENT_CHARACTER),
        }),
    }
}

/// Checks the rules of port, net and instance names (CORE-REQ-0110, 0111): those of
/// [`check_verbatim`], every backslash followed by `/` or `\`, and no empty segment.
pub fn check_hierarchical(name: &str) -> Result<(), NameError> {
    check_verbatim(name)?;
    let b = name.as_bytes();
    // Whether the current segment has no character yet.
    let mut empty = true;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                if !matches!(b.get(i + 1), Some(b'/' | b'\\')) {
                    return Err(NameError::BadEscape { at: i });
                }
                empty = false;
                i += 2;
            }
            b'/' if empty => return Err(NameError::EmptySegment { at: i }),
            b'/' => {
                empty = true;
                i += 1;
            }
            _ => {
                empty = false;
                i += 1;
            }
        }
    }
    if empty {
        // The name is not empty, so it ends with an unescaped '/'.
        return Err(NameError::EmptySegment { at: b.len() - 1 });
    }
    Ok(())
}

/// True for a simple identifier, `[A-Za-z_][A-Za-z0-9_$]*` (CORE-REQ-0113). This is the lexical
/// rule only: a format writer adds its own rules, for example the Verilog writer escapes keywords
/// too.
pub fn is_simple_identifier(s: &str) -> bool {
    let b = s.as_bytes();
    b.first()
        .is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_')
        && b.iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$')
}

/// The segment of a raw text (CORE-REQ-0112): `\` becomes `\\` and `/` becomes `\/`, so the
/// segment has no unescaped `/` and [`unescape`] gives the text back.
pub fn escape_segment(raw: &str) -> String {
    let mut s = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(c, '/' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

/// The hierarchical name of raw segments, for example the instance path that flattening
/// follows (CORE-REQ-0112): each segment escaped by [`escape_segment`], joined by `/`. The name
/// is valid when there is a segment and every one is non-empty printable ASCII; no segments give
/// the empty string, which is no valid name.
pub fn join<I>(raws: I) -> String
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut s = String::new();
    for (k, raw) in raws.into_iter().enumerate() {
        if k > 0 {
            s.push('/');
        }
        s.push_str(&escape_segment(raw.as_ref()));
    }
    s
}

/// Calls `f` with each character of the raw text of `name`, and with `None` at each unescaped
/// `/`: `\/` gives `/`, `\\` gives `\`, and a backslash before anything else (only in an invalid
/// name) is kept.
fn walk(name: &str, mut f: impl FnMut(Option<char>)) {
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next_if(|&e| matches!(e, '/' | '\\')) {
                Some(e) => f(Some(e)),
                None => f(Some('\\')),
            },
            '/' => f(None),
            c => f(Some(c)),
        }
    }
}

/// The raw text of a name (CORE-REQ-0112): `\/` becomes `/`, `\\` becomes `\`, and an unescaped
/// `/` stays `/`. So `unescape(&escape_segment(raw)) == raw` for every text, and a flat name's
/// raw text is its segment's text; the separators of a hierarchical name become slashes that
/// [`escape_segment`] would escape.
pub fn unescape(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    walk(name, |c| s.push(c.unwrap_or('/')));
    s
}

/// The raw texts of the segments of a name, in order (CORE-REQ-0112): split at each unescaped
/// `/`, each unescaped. `segments(&join(raws)) == raws` for every non-empty list of texts; the
/// segments of a valid name are non-empty.
pub fn segments(name: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    walk(name, |c| match c {
        Some(c) => out.last_mut().into_iter().for_each(|s| s.push(c)),
        None => out.push(String::new()),
    });
    out
}

/// `name` for messages: printable ASCII characters as they are and every other character as
/// `\u{...}`, so that a rejected name shows on one line.
pub fn for_display(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    for c in name.chars() {
        if u8::try_from(c).is_ok_and(is_printable) {
            s.push(c);
        } else {
            s.extend(c.escape_unicode());
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64*.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            (x.wrapping_mul(0x2545_F491_4F6C_DD1D) % n as u64) as usize
        }

        /// A text of up to `max` characters, mostly printable ASCII with many slashes and
        /// backslashes; with `wild`, also blanks, control characters, DEL and non-ASCII ones.
        fn text(&mut self, max: usize, wild: bool) -> String {
            const ODD: [char; 8] = [' ', '\t', '\n', '\0', '\u{7f}', 'é', '\u{a0}', '中'];
            (0..self.below(max + 1))
                .map(|_| match self.below(10) {
                    0 | 1 => '/',
                    2 | 3 => '\\',
                    4 if wild => ODD[self.below(ODD.len())],
                    _ => char::from(33 + self.below(94) as u8),
                })
                .collect()
        }
    }

    /// The rules of hierarchical names, stated as a scan over the characters: printable
    /// ASCII, each backslash escaping '/' or '\', and segments that are not empty.
    fn valid(name: &str) -> bool {
        if name.is_empty() || !name.bytes().all(|c| (33..=126).contains(&c)) {
            return false;
        }
        let mut lengths = vec![0usize];
        let mut chars = name.chars();
        while let Some(c) = chars.next() {
            if c == '/' {
                lengths.push(0);
                continue;
            }
            if c == '\\' && !matches!(chars.next(), Some('/' | '\\')) {
                return false;
            }
            if let Some(n) = lengths.last_mut() {
                *n += 1;
            }
        }
        lengths.iter().all(|&n| n > 0)
    }

    // CORE-REQ-0110, 0111: the rules, with the offset and reason of each problem.
    #[test]
    fn rules_and_reasons() {
        for name in [
            "a", "u1/n2", r"a\/b", r"a\\b", r"\/", r"\\", r"a\//b", "!~", "x[3]/y.z",
        ] {
            assert_eq!(check_hierarchical(name), Ok(()), "{name}");
            assert_eq!(check_verbatim(name), Ok(()), "{name}");
        }
        let cases = [
            ("", NameError::Empty),
            ("a b", NameError::NotPrintable { at: 1, ch: ' ' }),
            (
                "ab\u{7f}",
                NameError::NotPrintable {
                    at: 2,
                    ch: '\u{7f}',
                },
            ),
            ("xé", NameError::NotPrintable { at: 1, ch: 'é' }),
            ("\tx", NameError::NotPrintable { at: 0, ch: '\t' }),
            (r"a\b", NameError::BadEscape { at: 1 }),
            (r"a\", NameError::BadEscape { at: 1 }),
            (r"a\\\", NameError::BadEscape { at: 3 }),
            ("/a", NameError::EmptySegment { at: 0 }),
            ("a/", NameError::EmptySegment { at: 1 }),
            ("a//b", NameError::EmptySegment { at: 2 }),
            ("/", NameError::EmptySegment { at: 0 }),
            (r"a\\/", NameError::EmptySegment { at: 3 }),
        ];
        for (name, want) in &cases {
            assert_eq!(check_hierarchical(name).as_ref(), Err(want), "{name:?}");
        }
        // Verbatim names may hold any printable ASCII character, slashes and backslashes too.
        for name in ["a//b", "/", r"a\b", r"\"] {
            assert_eq!(check_verbatim(name), Ok(()), "{name}");
        }
        assert_eq!(check_verbatim(""), Err(NameError::Empty));
        let messages = [
            NameError::Empty,
            NameError::NotPrintable { at: 2, ch: 'é' },
            NameError::BadEscape { at: 1 },
            NameError::EmptySegment { at: 0 },
        ]
        .map(|e| e.to_string());
        assert_eq!(
            messages,
            [
                "the name is empty",
                "character U+00E9 at byte 2 is not printable ASCII (33 to 126)",
                "the backslash at byte 1 is not followed by '/' or '\\'",
                "the '/' at byte 0 leaves an empty segment",
            ]
        );
        assert_eq!(for_display("a b\u{e9}\\/c"), r"a\u{20}b\u{e9}\/c");
    }

    // CORE-REQ-0113: the lexical rule of simple identifiers; keywords are the writers' concern.
    #[test]
    fn simple_identifiers() {
        for s in ["a", "_", "a$1", "A_b9", "module"] {
            assert!(is_simple_identifier(s), "{s}");
        }
        for s in ["", "1a", "$a", "a.b", "a/b", r"a\b", "a b", "é"] {
            assert!(!is_simple_identifier(s), "{s}");
        }
    }

    // CORE-REQ-0112: the conversions on fixed examples.
    #[test]
    fn conversions() {
        assert_eq!(escape_segment(r"a/b\c"), r"a\/b\\c");
        assert_eq!(escape_segment("plain"), "plain");
        assert_eq!(unescape(r"u1/a\/b\\c"), r"u1/a/b\c");
        assert_eq!(segments(r"u1/a\/b\\c"), ["u1", r"a/b\c"]);
        assert_eq!(join(["u1", "a/b"]), r"u1/a\/b");
        assert_eq!(join(Vec::<String>::new()), "");
        assert_eq!(segments(""), [""]);
        // Invalid names still convert: a stray backslash is kept, empty segments appear.
        assert_eq!(unescape(r"a\b\"), r"a\b\");
        assert_eq!(segments("/a//"), ["", "a", "", ""]);
    }

    // CORE-REQ-0110 to 0112: random raw texts escape, join and convert back exactly, and the
    // check accepts a joined name exactly when every segment is non-empty printable ASCII.
    #[test]
    fn random_texts_round_trip() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..5_000 {
            let wild = rng.below(4) == 0;
            let raw = rng.text(12, wild);
            let seg = escape_segment(&raw);
            assert_eq!(unescape(&seg), raw);
            assert_eq!(segments(&seg), std::slice::from_ref(&raw));
            assert_eq!(
                check_hierarchical(&seg).is_ok(),
                check_verbatim(&raw).is_ok()
            );
            let raws: Vec<String> = (0..1 + rng.below(4)).map(|_| rng.text(5, wild)).collect();
            let name = join(&raws);
            assert_eq!(segments(&name), raws);
            assert_eq!(unescape(&name), raws.join("/"));
            let ok = raws.iter().all(|r| check_verbatim(r).is_ok());
            assert_eq!(check_hierarchical(&name).is_ok(), ok, "{name:?}");
        }
    }

    // CORE-REQ-0110, 0111: random texts, most of them invalid, are accepted exactly when the
    // rules hold, and the segments of an accepted name escape and join back to it.
    #[test]
    fn random_texts_are_checked() {
        let mut rng = Rng(0x5EED);
        let mut accepted = 0;
        for _ in 0..20_000 {
            let wild = rng.below(3) == 0;
            let name = rng.text(8, wild);
            let r = check_hierarchical(&name);
            assert_eq!(r.is_ok(), valid(&name), "{name:?}: {r:?}");
            match r {
                Ok(()) => {
                    accepted += 1;
                    assert_eq!(join(segments(&name)), name);
                }
                Err(NameError::NotPrintable { at, .. } | NameError::BadEscape { at }) => {
                    assert!(at < name.len());
                }
                Err(NameError::EmptySegment { at }) => assert_eq!(name.as_bytes()[at], b'/'),
                Err(e) => assert_eq!(e, NameError::Empty),
            }
        }
        assert!(accepted > 1_000 && accepted < 15_000, "{accepted} accepted");
    }
}
