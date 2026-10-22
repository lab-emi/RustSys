// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Numbers and names as the command line shows them (PROTO-REQ-0100).

/// Units with fixed decimals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Unit {
    Ns,
    Area,
    Seconds,
    Percent,
    Count,
    /// A value in library time units.
    LibTime,
    Plain,
}

impl Unit {
    pub fn decimals(self) -> usize {
        match self {
            Unit::Ns | Unit::Area | Unit::LibTime => 4,
            Unit::Seconds => 3,
            Unit::Percent => 1,
            Unit::Count => 0,
            Unit::Plain => 4,
        }
    }

    /// Suffix after a value in text.
    pub fn suffix(self) -> &'static str {
        match self {
            Unit::Ns => " ns",
            Unit::Seconds => " s",
            Unit::Percent => " %",
            _ => "",
        }
    }
}

/// The text of a missing value.
pub const MISSING: &str = "—";

/// A number with the decimals of its unit: `-0.0` shows as zero, a true negative keeps its sign.
pub fn number(v: f64, unit: Unit) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    if unit == Unit::Count {
        return format!("{}", v.round() as i64);
    }
    let d = unit.decimals();
    let s = format!("{v:.d$}");
    if v == 0.0 {
        // `-0.0` and `0.0`.
        return format!("{:.d$}", 0.0);
    }
    if v < 0.0 && !s.starts_with('-') {
        return format!("-{s}");
    }
    s
}

/// Shortest decimal that reads back to the same `f64` (exact mode).
pub fn exact(v: f64) -> String {
    format!("{v}")
}

/// A name from an input or report with control characters escaped (PROTO-REQ-0100).
pub fn name(s: &str) -> std::borrow::Cow<'_, str> {
    if s.chars().any(char::is_control) {
        std::borrow::Cow::Owned(
            s.chars()
                .map(|c| {
                    if c.is_control() {
                        format!("\\u{{{:x}}}", c as u32)
                    } else {
                        c.to_string()
                    }
                })
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Seconds as `m:ss.t`.
pub fn clock(seconds: f64) -> String {
    let t = (seconds * 10.0).floor().max(0.0) as u64;
    format!("{}:{:02}.{}", t / 600, (t / 10) % 60, t % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    // PROTO-REQ-0100
    #[test]
    fn numbers() {
        assert_eq!(number(-0.0, Unit::Ns), "0.0000");
        assert_eq!(number(-0.00001, Unit::Ns), "-0.0000");
        assert_eq!(number(1.23456, Unit::Ns), "1.2346");
        assert_eq!(number(3.0, Unit::Count), "3");
        assert_eq!(number(12.345, Unit::Seconds), "12.345");
        assert_eq!(exact(0.1), "0.1");
        assert_eq!(name("a\u{1b}[2Jb"), "a\\u{1b}[2Jb");
        assert_eq!(clock(125.47), "2:05.4");
    }
}
