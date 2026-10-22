// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Logical lines of BLIF text (BLIF-REQ-0010): comments, continuation lines and words.

/// A word with its line and column (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Word<'t> {
    pub text: &'t str,
    pub line: u32,
    pub col: u32,
}

/// The logical lines of a text, one at a time.
pub(super) struct Lines<'t> {
    rest: &'t str,
    line: u32,
}

impl<'t> Lines<'t> {
    pub fn new(text: &'t str) -> Lines<'t> {
        Lines {
            rest: text,
            line: 0,
        }
    }

    /// The next physical line without its line feed, or `None` at the end of the text.
    fn physical(&mut self) -> Option<&'t str> {
        if self.rest.is_empty() {
            return None;
        }
        self.line += 1;
        let (l, rest) = match self.rest.find('\n') {
            Some(i) => (&self.rest[..i], &self.rest[i + 1..]),
            None => (self.rest, ""),
        };
        self.rest = rest;
        Some(l)
    }

    /// The next non-empty logical line, as its words.
    pub fn next_line(&mut self) -> Option<Vec<Word<'t>>> {
        let mut words = Vec::new();
        loop {
            let Some(raw) = self.physical() else {
                return (!words.is_empty()).then_some(words);
            };
            let text = match raw.find('#') {
                Some(i) => &raw[..i],
                None => raw,
            };
            let trimmed = text.trim_end_matches(is_blank);
            let (body, more) = match trimmed.strip_suffix('\\') {
                Some(b) => (b, true),
                None => (trimmed, false),
            };
            split(body, self.line, &mut words);
            if !more && !words.is_empty() {
                return Some(words);
            }
        }
    }
}

/// Blanks: space, tab and carriage return (BLIF-REQ-0010).
pub(super) fn is_blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r')
}

/// Appends the words of `text`, located on `line`; columns count characters from 1.
fn split<'t>(text: &'t str, line: u32, out: &mut Vec<Word<'t>>) {
    let mut start: Option<(usize, u32)> = None;
    let mut col = 0u32;
    for (i, c) in text.char_indices() {
        col = col.saturating_add(1);
        if is_blank(c) {
            if let Some((s, sc)) = start.take() {
                out.push(Word {
                    text: &text[s..i],
                    line,
                    col: sc,
                });
            }
        } else if start.is_none() {
            start = Some((i, col));
        }
    }
    if let Some((s, sc)) = start {
        out.push(Word {
            text: &text[s..],
            line,
            col: sc,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> Vec<Vec<(String, u32, u32)>> {
        let mut l = Lines::new(text);
        let mut out = Vec::new();
        while let Some(ws) = l.next_line() {
            out.push(
                ws.iter()
                    .map(|w| (w.text.to_string(), w.line, w.col))
                    .collect(),
            );
        }
        out
    }

    // BLIF-REQ-0010: comments, continuation lines, blanks and empty lines.
    #[test]
    fn logical_lines() {
        let t = ".model m # comment\r\n\n.inputs a \\\n  b\\\n c # x \\\n.end";
        let l = all(t);
        assert_eq!(l.len(), 3, "{l:?}");
        assert_eq!(l[0], [(".model".into(), 1, 1), ("m".into(), 1, 8)]);
        assert_eq!(
            l[1],
            [
                (".inputs".into(), 3, 1),
                ("a".into(), 3, 9),
                ("b".into(), 4, 3),
                ("c".into(), 5, 2)
            ]
        );
        assert_eq!(l[2], [(".end".into(), 6, 1)]);
        assert!(all("# only\n\n   \\\n").is_empty());
    }
}
