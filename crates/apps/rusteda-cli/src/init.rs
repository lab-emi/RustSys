// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! `rusteda init` implementation (RCLI-REQ-0010, WS-REQ-0001, WS-REQ-0069).

use std::path::Path;

use rusteda_workspace::text::{mutation_header, next_line};
use rusteda_workspace::{Envelope, Error, Invocation, Rev, Workspace};

use crate::OutputOutcome;

/// Runs `rusteda init -w <dir>` (RCLI-REQ-0010).
pub(crate) fn run_init(dir: &Path, ws_arg: &str, inv: &Invocation) -> Result<OutputOutcome, Error> {
    let init = Workspace::init(dir, inv)?;
    let mut env = Envelope::new("rusteda init");
    env.workspace = Some(ws_arg.to_string());
    let rev = Rev(0);
    env.new_rev = Some(rev);
    env.diagnostics = init.warnings;
    let rev_s = rev.to_string();
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &rev_s]);
    env.push_next_action(&["rusteda", "log", "-w", ws_arg]);

    let mut lines = Vec::new();
    lines.push(mutation_header(rev, "rusteda init", rev));
    lines.push(format!("  workspace: {ws_arg}"));
    for next in &env.next_actions {
        lines.push(next_line(next));
    }
    Ok(OutputOutcome {
        envelope: env,
        text_lines: lines,
    })
}
