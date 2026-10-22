// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use crate::SynthError;
use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::verilog::{Diagnostic, SourceMap};

pub(crate) fn render(sm: &SourceMap, d: &[Diagnostic]) -> Vec<String> {
    d.iter().map(|x| x.render(sm)).collect()
}

/// Streams a message text: one message per line (CLI-REQ-0004); a line that is no message
/// continues the message before it, with its severity and code.
pub(crate) fn stream(ctx: Ctx<'_>, text: &str) {
    let mut lead: Option<(rusteda_core::job::Severity, String)> = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let d = match rusteda_core::job::Diag::parse(line) {
            Some(d) => {
                lead = Some((d.severity, d.code.clone()));
                d
            }
            None => match &lead {
                Some((sev, code)) => rusteda_core::job::Diag::new(*sev, code.clone(), line.trim()),
                None => rusteda_core::job::Diag::note("S0999", line.trim()),
            },
        };
        ctx.diag(|| d);
    }
}

/// A failure whose messages were streamed.
pub(crate) fn fail(ctx: Ctx<'_>, messages: Vec<String>, internal: bool) -> JobError<SynthError> {
    for m in &messages {
        stream(ctx, m);
    }
    JobError::Failed(SynthError { messages, internal })
}
