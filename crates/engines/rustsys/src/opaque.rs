// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Validate preserved macro interfaces against the supplied Liberty libraries.

use crate::{SynthError, diagnostics::fail, elab::Design};
use rusteda_core::job::{Ctx, JobError};
use rusteda_formats::liberty::Library;

pub(crate) fn validate_interfaces(
    ctx: Ctx<'_>,
    design: &mut Design,
    libs: &[Library],
) -> Result<(), JobError<SynthError>> {
    for instance in &mut design.opaque_instances {
        let cell = libs
            .iter()
            .flat_map(|l| &l.cells)
            .find(|c| c.name == instance.cell)
            .ok_or_else(|| {
                fail(
                    ctx,
                    vec![format!(
                        "error[S0134]: no Liberty interface for opaque cell {}",
                        instance.cell
                    )],
                    false,
                )
            })?;
        let pins: rustc_hash::FxHashMap<_, _> = cell
            .pins
            .iter()
            .map(|pin| (pin.name.as_str(), pin))
            .collect();
        for other in libs
            .iter()
            .flat_map(|l| &l.cells)
            .filter(|c| c.name == instance.cell)
        {
            let same_pins = other.pins.len() == pins.len()
                && other.pins.iter().all(|pin| {
                    pins.get(pin.name.as_str())
                        .is_some_and(|p| p.direction == pin.direction)
                });
            if !other.is_macro_cell
                || !other.problems.is_empty()
                || !same_pins
                || other.area != cell.area
            {
                return Err(fail(
                    ctx,
                    vec![format!(
                        "error[S0134]: conflicting opaque macro interfaces for {} in supplied libraries",
                        cell.name
                    )],
                    false,
                ));
            }
        }
        if !cell.problems.is_empty() {
            return Err(fail(
                ctx,
                vec![format!(
                    "error[S0134]: invalid Liberty interface for {}: {:?}",
                    cell.name, cell.problems
                )],
                false,
            ));
        }
        let mut count = 0usize;
        for opaque in &instance.ports {
            let port = design
                .ports
                .iter()
                .find(|p| p.name == opaque.cut)
                .expect("opaque cut");
            for bit in 0..port.width {
                let name = opaque.range.map_or_else(
                    || opaque.name.clone(),
                    |range| {
                        let index = if range.left >= range.right {
                            range.right + i64::from(bit)
                        } else {
                            range.right - i64::from(bit)
                        };
                        format!("{}[{index}]", opaque.name)
                    },
                );
                let pin = pins.get(name.as_str()).ok_or_else(|| {
                    fail(
                        ctx,
                        vec![format!(
                            "error[S0134]: {}.{} absent from Liberty",
                            instance.cell, name
                        )],
                        false,
                    )
                })?;
                let direction = if port.input {
                    rusteda_formats::liberty::Direction::Output
                } else {
                    rusteda_formats::liberty::Direction::Input
                };
                if pin.direction != direction {
                    return Err(fail(
                        ctx,
                        vec![format!(
                            "error[S0134]: direction mismatch for {}.{}",
                            instance.cell, name
                        )],
                        false,
                    ));
                }
                count += 1;
            }
        }
        if count != cell.pins.len() || !cell.area.is_finite() || cell.area <= 0.0 {
            return Err(fail(
                ctx,
                vec![format!(
                    "error[S0134]: incomplete interface or missing area for {}",
                    instance.cell
                )],
                false,
            ));
        }
        instance.area = cell.area;
    }
    Ok(())
}
