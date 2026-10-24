// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Readers and writers of the RustEDA tools: SystemVerilog ([`verilog`], `docs/specs/verilog/`),
//! Liberty ([`liberty`], `docs/specs/liberty/`), SDC ([`sdc`], `docs/specs/sdc/`), detailed RC
//! SPEF ([`spef`], `docs/specs/rusteda-cli/spef.md`), BLIF ([`blif`], `docs/specs/blif/`) and
//! AIGER ([`aiger`], `docs/specs/aiger/`).

pub mod aiger;
pub mod blif;
pub mod liberty;
pub mod sdc;
pub mod spef;
pub mod verilog;
