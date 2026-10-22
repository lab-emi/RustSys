// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Separate development driver for optional observation exports, allowing
//! existing checker binaries to stay frozen during a verification campaign.

#[path = "check_config.rs"]
mod shared;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    shared::main()
}
