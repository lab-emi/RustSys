// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Diagnostics shared by the reader and its consumers.

use std::fmt;

use crate::verilog::span::{SourceMap, Span};

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Note,
    Warning,
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Note => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
        })
    }
}

/// A located message with a stable code.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    pub fn new(
        severity: Severity,
        code: &'static str,
        span: Span,
        message: impl Into<String>,
    ) -> Diagnostic {
        Diagnostic {
            severity,
            code,
            span,
            message: message.into(),
        }
    }

    pub fn error(code: &'static str, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Error, code, span, message)
    }

    pub fn warning(code: &'static str, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Warning, code, span, message)
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Renders as `<severity>[<code>]: <file>:<line>:<col>: <message>`.
    pub fn render(&self, sm: &SourceMap) -> String {
        format!(
            "{}[{}]: {}: {}",
            self.severity,
            self.code,
            sm.describe(self.span),
            self.message
        )
    }
}
