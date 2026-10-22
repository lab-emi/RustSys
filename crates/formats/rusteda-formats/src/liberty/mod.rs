// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Liberty cell library reader (`docs/specs/liberty/`): builds the library model of
//! `rusteda_core::liberty`, which this module re-exports, and reports every construct the model
//! does not represent (`read_file_diags`, `read_text_diags`).

mod build;
mod bus;
mod classify;
mod diags;
mod func;
pub mod syntax;
mod tables;
mod units;
mod wire_load;

pub use build::{read_file, read_file_diags, read_text, read_text_diags};
pub use func::{FuncError, parse_function};
pub use rusteda_core::liberty::*;

/// Errors of the Liberty reader.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{file}:{line}:{col}: {msg}")]
    Syntax {
        file: String,
        line: u32,
        col: u32,
        msg: String,
    },
    #[error("{file}: {msg}")]
    Model { file: String, msg: String },
    #[error("{path}: {msg}")]
    Io { path: String, msg: String },
}
