// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Aggregated messages of the reader (LIBERTY-REQ-0052, LIBERTY-REQ-0053): one message per code
//! and construct path, with the number of occurrences and the first location in the text.

use std::collections::HashMap;

use rusteda_core::diag::{Diag, Severity};

/// The message codes of the reader (LIBERTY-REQ-0053).
pub(super) mod code {
    /// A unit attribute whose text is not a valid unit (error).
    pub(crate) const BAD_UNIT: &str = "L0001";
    /// A threshold percentage or slew derate attribute that is out of range or inverted (error).
    pub(crate) const BAD_THRESHOLD: &str = "L0002";
    /// A construct the reader knows but does not represent in the model.
    pub(crate) const UNSUPPORTED: &str = "L0100";
    /// A construct the reader does not know.
    pub(crate) const UNKNOWN: &str = "L0101";
    /// An attribute, group or name given more than once where one is expected.
    pub(crate) const DUPLICATE: &str = "L0102";
    /// A value outside the names of an enumerated attribute.
    pub(crate) const UNKNOWN_VALUE: &str = "L0110";
    /// A numeric value that is not a number.
    pub(crate) const NOT_A_NUMBER: &str = "L0111";
    /// A timing group without a related pin.
    pub(crate) const NO_RELATED_PIN: &str = "L0112";
    /// Cell problems (LIBERTY-REQ-0057): each one marks its cell unusable.
    pub(crate) const BAD_DIRECTION: &str = "L0200";
    pub(crate) const BAD_FUNCTION: &str = "L0201";
    pub(crate) const UNKNOWN_NAME: &str = "L0202";
    pub(crate) const BAD_SEQUENTIAL: &str = "L0203";
    pub(crate) const BAD_LIMIT: &str = "L0204";
    pub(crate) const NO_TEMPLATE: &str = "L0205";
    pub(crate) const TABLE_SIZE: &str = "L0206";
    pub(crate) const TABLE_INDEX: &str = "L0207";
    pub(crate) const BAD_BUS: &str = "L0208";
}

/// The help line of a code.
fn help(code: &str) -> Option<&'static str> {
    Some(match code {
        code::BAD_UNIT => "the default unit applies; values in this unit are probably misread",
        code::BAD_THRESHOLD => "the default threshold or derate applies",
        code::UNSUPPORTED => {
            "the construct is not represented in the library model and has no effect"
        }
        code::UNKNOWN => {
            "the construct is not a Liberty construct this reader knows; it has no effect"
        }
        code::DUPLICATE => "only one of the repeated statements takes effect",
        code::UNKNOWN_VALUE => "the value is kept as written but has no known meaning",
        code::NOT_A_NUMBER => "the attribute takes its default value",
        code::NO_RELATED_PIN => "the timing group produces no timing arc",
        c if c.starts_with("L02") => "cells with problems are marked unusable",
        _ => return None,
    })
}

/// One aggregated message.
#[derive(Debug)]
struct Entry {
    severity: Severity,
    code: &'static str,
    path: String,
    count: u64,
    line: u32,
    col: u32,
    /// The text of the first occurrence.
    text: String,
}

/// Collects the messages of one read.
#[derive(Debug)]
pub(super) struct Diags {
    file: String,
    entries: Vec<Entry>,
    /// Entry by code followed by construct path.
    index: HashMap<String, usize>,
    /// Reused key buffer.
    key: String,
    /// While set, reports are dropped (a construct read a second time).
    pub(super) muted: bool,
}

impl Diags {
    pub(super) fn new(file: &str) -> Diags {
        Diags {
            file: file.to_string(),
            entries: Vec::new(),
            index: HashMap::new(),
            key: String::new(),
            muted: false,
        }
    }

    /// Counts one occurrence of `code` at the construct `path`, located at `at` (line, column).
    /// `text` describes the occurrence; it is evaluated for the first occurrence in source order
    /// only.
    pub(super) fn report(
        &mut self,
        severity: Severity,
        code: &'static str,
        path: &str,
        at: (u32, u32),
        text: impl FnOnce() -> String,
    ) {
        if self.muted {
            return;
        }
        self.key.clear();
        self.key.push_str(code);
        self.key.push_str(path);
        if let Some(&i) = self.index.get(self.key.as_str()) {
            let e = &mut self.entries[i];
            e.count += 1;
            if at < (e.line, e.col) {
                (e.line, e.col) = at;
                e.text = text();
            }
            return;
        }
        self.index.insert(self.key.clone(), self.entries.len());
        self.entries.push(Entry {
            severity,
            code,
            path: path.to_string(),
            count: 1,
            line: at.0,
            col: at.1,
            text: text(),
        });
    }

    /// A warning: see [`Diags::report`].
    pub(super) fn warn(
        &mut self,
        code: &'static str,
        path: &str,
        at: (u32, u32),
        text: impl FnOnce() -> String,
    ) {
        self.report(Severity::Warning, code, path, at, text);
    }

    /// The messages in order of their first location, then code and construct path.
    pub(super) fn finish(mut self) -> Vec<Diag> {
        self.entries.sort_by(|a, b| {
            (a.line, a.col, a.code, &a.path).cmp(&(b.line, b.col, b.code, &b.path))
        });
        self.entries
            .into_iter()
            .map(|e| {
                let times = if e.count == 1 {
                    "occurrence"
                } else {
                    "occurrences"
                };
                let mut d = Diag::new(
                    e.severity,
                    e.code,
                    format!("{}: {} ({} {times})", e.path, e.text, e.count),
                )
                .at(self.file.clone(), e.line, e.col);
                if let Some(h) = help(e.code) {
                    d = d.with_help(h);
                }
                d
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // LIBERTY-REQ-0052: one message per code and path, counted, located at the first occurrence
    // in source order, in order of first location.
    #[test]
    fn aggregation() {
        let mut d = Diags::new("a.lib");
        d.warn(code::UNKNOWN, "library/cell/x", (9, 3), || {
            "unknown x".into()
        });
        d.warn(code::UNKNOWN, "library/cell/x", (4, 5), || {
            "unknown x at 4".into()
        });
        d.warn(code::UNKNOWN, "library/cell/x", (12, 1), || {
            "never built".into()
        });
        d.warn(code::UNSUPPORTED, "library/cell/x", (2, 1), || {
            "other code".into()
        });
        d.report(
            Severity::Error,
            code::BAD_UNIT,
            "library/time_unit",
            (3, 2),
            || "bad".into(),
        );
        d.muted = true;
        d.warn(code::UNKNOWN, "library/cell/x", (1, 1), || "muted".into());
        let out = d.finish();
        let lines: Vec<String> = out.iter().map(Diag::render_first_line).collect();
        assert_eq!(
            lines,
            [
                "warning[L0100]: a.lib:2:1: library/cell/x: other code (1 occurrence)",
                "error[L0001]: a.lib:3:2: library/time_unit: bad (1 occurrence)",
                "warning[L0101]: a.lib:4:5: library/cell/x: unknown x at 4 (3 occurrences)",
            ]
        );
        assert!(out.iter().all(|d| d.help.is_some()));
    }
}
