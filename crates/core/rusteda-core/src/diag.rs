// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Coded messages (JOB-REQ-0007): `<severity>[<code>]: <file>:<line>:<column>: <text>`, the file
//! location optional.

/// Message severity, in increasing order of importance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Note,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }

    fn parse(s: &str) -> Option<Severity> {
        match s {
            "note" => Some(Severity::Note),
            "warning" => Some(Severity::Warning),
            "error" => Some(Severity::Error),
            _ => None,
        }
    }
}

/// Where a message points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

/// A coded message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub severity: Severity,
    /// One letter and four digits, for example `V0100`.
    pub code: String,
    pub location: Option<Location>,
    pub message: String,
    pub help: Option<String>,
}

impl Diag {
    pub fn new(severity: Severity, code: impl Into<String>, message: impl Into<String>) -> Diag {
        Diag {
            severity,
            code: code.into(),
            location: None,
            message: message.into(),
            help: None,
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Diag {
        Diag::new(Severity::Error, code, message)
    }

    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Diag {
        Diag::new(Severity::Warning, code, message)
    }

    pub fn note(code: impl Into<String>, message: impl Into<String>) -> Diag {
        Diag::new(Severity::Note, code, message)
    }

    pub fn at(mut self, file: impl Into<String>, line: u32, column: u32) -> Diag {
        self.location = Some(Location {
            file: file.into(),
            line,
            column,
        });
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Diag {
        self.help = Some(help.into());
        self
    }

    /// The message line, without the help line.
    pub fn render_first_line(&self) -> String {
        match &self.location {
            Some(l) => format!(
                "{}[{}]: {}:{}:{}: {}",
                self.severity.as_str(),
                self.code,
                l.file,
                l.line,
                l.column,
                self.message
            ),
            None => format!(
                "{}[{}]: {}",
                self.severity.as_str(),
                self.code,
                self.message
            ),
        }
    }

    /// The message line and, when there is help, a second line `  help: …`.
    pub fn render(&self) -> String {
        match &self.help {
            Some(h) => format!("{}\n  help: {h}", self.render_first_line()),
            None => self.render_first_line(),
        }
    }

    /// Reads one message line. The file location is the shortest prefix of the text that ends
    /// with `: ` and is `<file>:<line>:<column>` (JOB-REQ-0007).
    pub fn parse(text: &str) -> Option<Diag> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        if text.contains('\n') {
            return None;
        }
        let open = text.find('[')?;
        let severity = Severity::parse(&text[..open])?;
        let close = open + text[open..].find("]: ")?;
        let code = &text[open + 1..close];
        let valid = code.len() == 5
            && code.as_bytes()[0].is_ascii_uppercase()
            && code[1..].bytes().all(|b| b.is_ascii_digit());
        if !valid {
            return None;
        }
        let rest = &text[close + 3..];
        let mut d = Diag::new(severity, code, rest);
        let mut from = 0;
        while let Some(i) = rest[from..].find(": ") {
            let head = &rest[..from + i];
            from += i + 2;
            if let Some(l) = parse_location(head) {
                d.location = Some(l);
                d.message = rest[from..].to_string();
                break;
            }
        }
        Some(d)
    }
}

/// `<file>:<line>:<column>` with a non-empty file part.
fn parse_location(head: &str) -> Option<Location> {
    let mut parts = head.rsplitn(3, ':');
    let (Some(col), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let (Ok(line), Ok(column)) = (line.parse::<u32>(), col.parse::<u32>()) else {
        return None;
    };
    (!file.is_empty()).then(|| Location {
        file: file.to_string(),
        line,
        column,
    })
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // JOB-REQ-0007
    #[test]
    fn render_and_parse_round_trip() {
        let cases = [
            Diag::error("V0002", "unknown module 'x'").at("top.sv", 3, 7),
            Diag::warning("A0007", "--run-dir runs/u9 is not empty"),
            Diag::note("L0003", "cell LAT: pin Q: arc from pin 'G' ignored").at("a:b.sv", 1, 2),
            Diag::error("V0100", "expected ';': found 'x'").at("dir/f.sv", 10, 1),
        ];
        for d in cases {
            let line = d.render_first_line();
            assert_eq!(Diag::parse(&line), Some(d.clone()), "{line}");
        }
        let h = Diag::error("A0008", "no runs root").with_help("give --runs-root DIR");
        assert_eq!(
            h.render(),
            "error[A0008]: no runs root\n  help: give --runs-root DIR"
        );
        // A message stays small enough to be the error type of results.
        assert!(size_of::<Diag>() <= 120, "{}", size_of::<Diag>());
    }

    /// A deterministic generator (SplitMix64) for the property test below.
    struct Gen(u64);

    impl Gen {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
            xs[self.below(xs.len())]
        }
        fn name(&mut self) -> String {
            let n = 1 + self.below(8);
            (0..n)
                .map(|_| self.pick(&["a", "b", "z", "0", "9", "_", "-", ".", "µ", "Q"]))
                .collect()
        }
    }

    // JOB-REQ-0007 (property): `Diag::parse(&d.render_first_line()) == Some(d)` for every diag
    // without help, with or without file location. Message texts are words without `:` joined
    // by blanks or by `: `, so that a text never starts with a location of its own.
    #[test]
    fn render_parse_property() {
        let mut g = Gen(0x5EED);
        for _ in 0..20_000 {
            let sev = [Severity::Note, Severity::Warning, Severity::Error][g.below(3)];
            let letter = char::from(b'A' + g.below(26) as u8);
            let code = format!("{letter}{:04}", g.below(10_000));
            let words = 1 + g.below(6);
            let mut message = String::new();
            for i in 0..words {
                if i > 0 {
                    message.push_str(g.pick(&[" ", ": ", " (", ") "]));
                }
                message.push_str(g.pick(&[
                    "x", "no", "model", "'a'", "M1", "#7", "@x", "/p", "(", ")", "µ",
                ]));
            }
            let mut d = Diag::new(sev, code, message);
            if g.below(2) == 0 {
                let file = format!("{}{}", g.pick(&["", "dir/", "a:b/", "c:\\x\\"]), g.name());
                d = d.at(file + ".sv", g.next() as u32, g.next() as u32);
            }
            let line = d.render_first_line();
            assert_eq!(Diag::parse(&line), Some(d), "{line}");
        }
    }

    #[test]
    fn rejects_non_messages() {
        for t in [
            "",
            "hello",
            "error: plain",
            "error[S01]: short code",
            "fatal[S0100]: x",
            "error[S0100]: a\nb",
        ] {
            assert_eq!(Diag::parse(t), None, "{t}");
        }
        let d = Diag::parse("error[D0002]: SDC 3:5: bad").unwrap();
        assert_eq!(d.location, None);
        assert_eq!(d.message, "SDC 3:5: bad");
    }
}
