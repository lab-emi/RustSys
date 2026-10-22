// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The units of a library (LIBERTY-REQ-0017): each one expressed in a fixed unit of its
//! quantity and marked as given by the library or defaulted.

/// One unit of a library: the size of one library unit of a quantity.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Unit {
    /// One library unit expressed in the fixed unit of the quantity (see [`Units`]).
    pub scale: f64,
    /// True when the library states the unit and the statement is valid; false when the default
    /// applies, either because the attribute is absent or because its text is not a valid unit.
    pub given: bool,
    /// The text of the unit attribute as written, when the library has one; the two values of
    /// `capacitive_load_unit` are joined by `,`.
    pub text: Option<String>,
}

impl Unit {
    /// The defaulted unit with the given scale.
    pub fn defaulted(scale: f64) -> Unit {
        Unit {
            scale,
            given: false,
            text: None,
        }
    }

    /// A unit the library states with `text`.
    pub fn given(scale: f64, text: impl Into<String>) -> Unit {
        Unit {
            scale,
            given: true,
            text: Some(text.into()),
        }
    }
}

impl Default for Unit {
    /// One fixed unit, defaulted.
    fn default() -> Unit {
        Unit::defaulted(1.0)
    }
}

/// The units of a library. Each [`Unit::scale`] is expressed in a fixed unit per quantity:
/// time in ns (`time_unit`), capacitance in pF (`capacitive_load_unit`), resistance in kOhm
/// (`pulling_resistance_unit`), voltage in V (`voltage_unit`), current in mA (`current_unit`) and
/// leakage power in nW (`leakage_power_unit`). An absent unit is defaulted to one fixed unit.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Units {
    pub time: Unit,
    pub capacitance: Unit,
    pub resistance: Unit,
    pub voltage: Unit,
    pub current: Unit,
    pub leakage_power: Unit,
}

#[cfg(test)]
mod tests {
    use super::*;

    // LIBERTY-REQ-0017: absent units default to one fixed unit and are marked defaulted.
    #[test]
    fn defaults_are_marked() {
        let u = Units::default();
        for unit in [
            &u.time,
            &u.capacitance,
            &u.resistance,
            &u.voltage,
            &u.current,
            &u.leakage_power,
        ] {
            assert_eq!(unit.scale, 1.0);
            assert!(!unit.given);
            assert_eq!(unit.text, None);
        }
        let ps = Unit::given(1e-3, "1ps");
        assert!(ps.given);
        assert_eq!(ps.text.as_deref(), Some("1ps"));
    }
}
