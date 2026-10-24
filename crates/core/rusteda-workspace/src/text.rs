// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Text-mode output and POSIX shell quoting (WS-REQ-0095, WS-REQ-0096).
//!
//! In text mode results go to stdout: a mutation starts with `rev<N>  <command>  (base rev<M>)`
//! and ends with `next: <command>` lines. Diagnostics go to stderr as
//! `<severity>[<code>]: <location>: <text>`, followed by `  help: <hint>` when there is one. The
//! error of a failed command is one of them: an error about the workspace prints with its `W`
//! code, `error[W0010]: revision conflict: ...`, and a usage error, a bug or a cancellation with
//! the application's code, `error[A0003]: ...`, so every line on stderr reads as a diagnostic
//! with a code of one letter and four digits; JSON keeps the envelope code (WS-REQ-0097).

use std::borrow::Cow;

use crate::envelope::{DiagCode, Diagnostic};
use crate::error::Error;
use crate::rev::Rev;

/// The first line of a mutation's text output: `rev<N>  <command>  (base rev<M>)`.
pub fn mutation_header(new_rev: Rev, command: &str, base: Rev) -> String {
    format!("{new_rev}  {command}  (base {base})")
}

/// A follow-up line: `next: <command>`.
pub fn next_line(command: &str) -> String {
    format!("next: {command}")
}

/// A diagnostic for stderr: `<severity>[<code>]: <file>:<line>:<col>: <message>`, the
/// location omitted when there is none, and a second line `  help: <help>` when there is help.
pub fn render_diagnostic(d: &Diagnostic) -> String {
    let mut text = match &d.location {
        Some(l) => format!(
            "{}[{}]: {}:{}:{}: {}",
            d.severity, d.code, l.file, l.line, l.col, d.message
        ),
        None => format!("{}[{}]: {}", d.severity, d.code, d.message),
    };
    if let Some(help) = &d.help {
        text.push_str("\n  help: ");
        text.push_str(help);
    }
    text
}

/// An error for stderr: its diagnostic ([`Error::to_diagnostic`]), `error[W<nnnn>]: <message>`
/// for an error about the workspace, `error[<app_code>]: <message>` for `USAGE`, `INTERNAL`,
/// `CANCELLED` and `BACKEND_UNAVAILABLE`, and an optional `  help:` line (WS-REQ-0097).
pub fn render_workspace_error(error: &Error, app_code: DiagCode) -> String {
    render_diagnostic(&error.to_diagnostic(app_code))
}

/// Quotes a word for POSIX shells (and zsh): words of `[A-Za-z0-9_@%+=:,./-]` stay as they are
/// unless they start with `=` (zsh expands `=cmd`); anything else, and the empty word, is put in
/// single quotes, with `'` written as `'\''` (WS-REQ-0096).
pub fn shell_quote(word: &str) -> Cow<'_, str> {
    let plain = !word.is_empty()
        && !word.starts_with('=')
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b));
    if plain {
        return Cow::Borrowed(word);
    }
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('\'');
    for c in word.chars() {
        if c == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    Cow::Owned(quoted)
}

/// Joins words into one POSIX command line, quoting each with [`shell_quote`].
pub fn shell_join<I, S>(words: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut line = String::new();
    for word in words {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&shell_quote(word.as_ref()));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Severity;

    // WS-REQ-0096
    #[test]
    fn quoting() {
        let cases = [
            ("rev3", "rev3"),
            ("--format=json", "--format=json"),
            ("./alu_ws/a.v", "./alu_ws/a.v"),
            ("a,b:c@d%e+f", "a,b:c@d%e+f"),
            ("", "''"),
            ("=cmd", "'=cmd'"),
            ("a b", "'a b'"),
            ("it's", "'it'\\''s'"),
            ("$HOME", "'$HOME'"),
            ("u_alu/reg[31]/CLK", "'u_alu/reg[31]/CLK'"),
            ("*", "'*'"),
            ("~/x", "'~/x'"),
            ("#p1", "'#p1'"),
            ("a;b", "'a;b'"),
            ("wns_ns >= 0.0", "'wns_ns >= 0.0'"),
            ("é", "'é'"),
            ("!x", "'!x'"),
            ("{a,b}", "'{a,b}'"),
        ];
        for (word, quoted) in cases {
            assert_eq!(shell_quote(word), quoted, "{word}");
        }
        assert_eq!(
            shell_join(["rustsys", "opt", "--stop-when", "wns_ns >= 0.0", "-D", "=x"]),
            "rustsys opt --stop-when 'wns_ns >= 0.0' -D '=x'"
        );
        assert_eq!(shell_join(Vec::<String>::new()), "");
    }

    // WS-REQ-0095
    #[test]
    fn text_lines() {
        assert_eq!(
            mutation_header(Rev(4), "ruststa run", Rev(3)),
            "rev4  ruststa run  (base rev3)"
        );
        assert_eq!(
            next_line("ruststa report -w ws --rev rev4"),
            "next: ruststa report -w ws --rev rev4"
        );
        let d = Diagnostic::new(
            Severity::Warning,
            DiagCode::of("L0003"),
            "pin Z: attribute ignored",
        )
        .at("lib/a.lib", 12, 5)
        .with_help("see the Liberty notes");
        assert_eq!(
            render_diagnostic(&d),
            "warning[L0003]: lib/a.lib:12:5: pin Z: attribute ignored\n  help: see the Liberty notes"
        );
        let d = Diagnostic::new(Severity::Error, DiagCode::of("W0006"), "no table");
        assert_eq!(render_diagnostic(&d), "error[W0006]: no table");
        let e = Error::RevisionConflict {
            base: Rev(3),
            head: Rev(4),
        };
        assert_eq!(
            render_workspace_error(&e, APP),
            "error[W0010]: revision conflict: --base-rev is rev3 but HEAD is rev4\n  \
             help: HEAD moved; review rev4 and rerun with --base-rev rev4"
        );
        // Review CR2-8: a usage error is the application's: it prints with the A code.
        assert_eq!(
            render_workspace_error(&Error::usage("bad"), APP),
            "error[A0003]: bad"
        );
    }

    /// The application code of the tests: the timing tool's code of an invalid command line.
    const APP: DiagCode = DiagCode::of("A0003");

    /// The first line of a diagnostic as the shared diagnostic reader takes it (JOB-REQ-0007):
    /// severity, `[`, a code of one uppercase letter and four digits, `]: `, then the text.
    fn reads_as_a_diagnostic(line: &str) -> bool {
        let Some((severity, rest)) = line.split_once('[') else {
            return false;
        };
        let Some((code, text)) = rest.split_once("]: ") else {
            return false;
        };
        ["note", "warning", "error"].contains(&severity)
            && code.len() == 5
            && code.as_bytes()[0].is_ascii_uppercase()
            && code[1..].bytes().all(|b| b.is_ascii_digit())
            && !text.is_empty()
    }

    // WS-REQ-0097 (review CW-8): the text-mode line of every error reads as a diagnostic.
    #[test]
    fn error_lines_are_diagnostic_lines() {
        let errors = [
            Error::usage("unknown flag --x"),
            Error::input_rejected("a.v:3:1: x/z literal"),
            Error::RevisionConflict {
                base: Rev(3),
                head: Rev(4),
            },
            Error::UnknownRevision {
                spec: "rev9".into(),
                reason: "no such revision".into(),
            },
            Error::corrupt("ws/manifest.json", crate::Corruption::BadMagic),
            Error::internal("bug"),
            Error::Cancelled,
        ];
        for e in errors {
            let text = render_workspace_error(&e, APP);
            let first = text.lines().next().unwrap();
            assert!(reads_as_a_diagnostic(first), "{first}");
            // Review CR2-8: W for the workspace's errors, the application's code otherwise.
            let code = e.diag_code().unwrap_or(APP);
            assert_eq!(code.letter() == 'A', e.diag_code().is_none(), "{first}");
            assert!(first.starts_with(&format!("error[{code}]: ")), "{first}");
            for line in text.lines().skip(1) {
                assert!(line.starts_with("  help: "), "{line}");
            }
        }
        let d = Diagnostic::new(
            Severity::Warning,
            crate::codes::HISTORY_NOT_WRITTEN,
            "rev1 is committed",
        );
        assert!(reads_as_a_diagnostic(&render_diagnostic(&d)));
    }
}
