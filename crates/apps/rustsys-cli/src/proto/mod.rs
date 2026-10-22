// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The job protocol of the `rustsys` command (`docs/specs/proto/`): typed jobs, canonical
//! commands, execution with events, run records, report loading, text results and tables.

pub mod argv;
pub mod codes;
pub mod exec;
pub mod fields;
pub mod filter;
pub mod format;
pub mod job;
pub mod load;
pub mod message;
pub mod options;
pub mod order;
pub mod progress;
pub mod record;
mod runners;
pub mod table;
pub mod text;
pub mod write;

pub use rusteda_core::job as jobs;
pub use rusteda_core::job::{CancelToken, Ctx, Diag, Event, Observer, Severity};
/// The engine view model (PROTO-REQ-0002).
pub use rustsys::view as synth;

/// Program version for reports and run records.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How a run ended (PROTO-REQ-0031).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExitClass {
    Ok,
    Rejected,
    Internal,
    /// A negative verdict: `not-equivalent`.
    Negative,
    /// An inconclusive verdict: `undecided`, a budget ran out before the check decided.
    Undecided,
    Cancelled,
}

impl ExitClass {
    /// Process exit status (CLI-REQ-0009).
    pub fn status(self) -> u8 {
        match self {
            ExitClass::Ok => 0,
            ExitClass::Rejected => 1,
            ExitClass::Internal => 2,
            ExitClass::Negative => 3,
            ExitClass::Undecided => 5,
            ExitClass::Cancelled => 130,
        }
    }

    /// Whether the run finished with a result whose report and text go to the caller: `Ok`,
    /// `Negative` and `Undecided` (PROTO-REQ-0031).
    pub fn has_result(self) -> bool {
        matches!(
            self,
            ExitClass::Ok | ExitClass::Negative | ExitClass::Undecided
        )
    }
}

/// The products a job runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Product {
    Synth,
    CheckSynth,
}

impl Product {
    pub const ALL: [Product; 2] = [Product::Synth, Product::CheckSynth];

    /// The name in run ids and run references.
    pub fn name(self) -> &'static str {
        match self {
            Product::Synth => "synth",
            Product::CheckSynth => "check-synth",
        }
    }

    /// The subcommand words of the `rustsys` command.
    pub fn command(self) -> &'static str {
        match self {
            Product::Synth => "synth",
            Product::CheckSynth => "check synth",
        }
    }

    /// The code letter of the command's messages (CLI-REQ-0004).
    pub fn code_letter(self) -> char {
        match self {
            Product::Synth => 'S',
            Product::CheckSynth => 'C',
        }
    }

    pub fn parse(s: &str) -> Option<Product> {
        Product::ALL.into_iter().find(|p| p.name() == s)
    }
}

#[cfg(test)]
mod tests {
    mod exec;
    mod messages;
    mod reset;

    use super::ExitClass;
    use super::record::RunStatus;

    // CLI-REQ-0009, PROTO-REQ-0031, PROTO-REQ-0052: one exit status and run status per class;
    // runs that end with a verdict, `undecided` included, keep their result.
    #[test]
    fn exit_statuses() {
        let all = [
            ExitClass::Ok,
            ExitClass::Rejected,
            ExitClass::Internal,
            ExitClass::Negative,
            ExitClass::Undecided,
            ExitClass::Cancelled,
        ];
        let status: Vec<u8> = all.iter().map(|c| c.status()).collect();
        assert_eq!(status, [0, 1, 2, 3, 5, 130]);
        let kept: Vec<bool> = all.iter().map(|c| c.has_result()).collect();
        assert_eq!(kept, [true, false, false, true, true, false]);
        let names: Vec<serde_json::Value> = all
            .iter()
            .map(|c| serde_json::to_value(RunStatus::from(*c)).unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "ok",
                "rejected",
                "internal",
                "negative",
                "undecided",
                "cancelled"
            ]
        );
    }
}
