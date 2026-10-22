// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! `rusteda revert` implementation (RCLI-REQ-0018, WS-REQ-0068, WS-REQ-0069).

use rusteda_workspace::text::{mutation_header, next_line};
use rusteda_workspace::{Envelope, Error, Invocation, Workspace};

use crate::OutputOutcome;

/// Runs `rusteda revert -w <ws> --base-rev <base> --to <to>` (RCLI-REQ-0018).
pub(crate) fn run_revert(
    ws: &Workspace,
    ws_arg: &str,
    base_arg: &str,
    to_arg: &str,
    inv: &Invocation,
) -> Result<OutputOutcome, Error> {
    let base = match ws.resolve_base(base_arg) {
        Ok(b) => b,
        Err(e) => {
            let _ = ws.log_rejected(inv, None, &e);
            return Err(e);
        }
    };
    let target = match ws.resolve_query(to_arg) {
        Ok(r) => r,
        Err(e) => {
            let _ = ws.log_rejected(inv, Some(base.rev()), &e);
            return Err(e);
        }
    };
    let reverted = ws.revert(base, target, inv)?;
    let mut env = Envelope::new("rusteda revert");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(reverted.previous);
    env.rev = Some(reverted.head);
    env.set("head_after", &reverted.head)?;
    env.diagnostics = reverted.warnings;
    let to_s = reverted.head.to_string();
    env.push_next_action(&["rusteda", "show", "-w", ws_arg, "--rev", &to_s]);
    env.push_next_action(&["rusteda", "log", "-w", ws_arg]);

    let mut lines = Vec::new();
    lines.push(mutation_header(
        reverted.head,
        "rusteda revert",
        reverted.previous,
    ));
    lines.push(format!("  head: {}", reverted.head));
    for next in &env.next_actions {
        lines.push(next_line(next));
    }
    Ok(OutputOutcome {
        envelope: env,
        text_lines: lines,
    })
}
