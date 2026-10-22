// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The core of the RustEDA tools (`docs/specs/core/`): coded messages ([`diag`]), the context of
//! engine runs ([`job`]), the Liberty library model ([`liberty`]), per-database symbol interning
//! ([`symbol`]), generational arenas with typed ids ([`arena`]), netlist object names ([`names`])
//! and the gate-level netlist with its dense, frozen view ([`netlist`]).

pub mod arena;
pub mod diag;
pub mod job;
pub mod liberty;
pub mod names;
pub mod netlist;
pub mod symbol;
