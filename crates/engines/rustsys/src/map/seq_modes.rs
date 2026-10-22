// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded Boolean recognition of enabled and two-data flip-flop selectors.
//! At most four function variables, six role permutations, eight polarity
//! choices and eight assignments per choice. No cell-name conventions.

use rusteda_formats::liberty::{SeqKind, Sequential};

use super::library::PolPin;

#[derive(Debug, Clone)]
pub(super) struct DataMode {
    pub data: String,
    pub inverted: bool,
    pub enable: Option<PolPin>,
    /// An external inactive-arm input. It must be wired to the same stored
    /// state, with this polarity compensated; it is never left unconstrained.
    pub hold: Option<PolPin>,
}

pub(super) fn infer(s: &Sequential, inputs: &[String]) -> Result<Vec<DataMode>, String> {
    let unsupported = || {
        "next_state/data_in is not a supported data/enable/hold selector (SYNTH-REQ-0523)"
            .to_owned()
    };
    if s.kind != SeqKind::FlipFlop {
        return Err(unsupported());
    }
    let vars = s.data.vars();
    if vars.len() > 4 {
        return Err(unsupported());
    }
    let is_state = |v: &str| v == s.state || (!s.state_n.is_empty() && v == s.state_n);
    let has_state = vars.iter().any(|v| is_state(v));
    let external: Vec<_> = vars.iter().filter(|v| !is_state(v)).collect();
    if external.iter().any(|v| !inputs.contains(v)) {
        return Err("next_state refers to an unknown input or state name".into());
    }
    let scan = external.len() == 3 && !has_state;
    if !(scan || external.len() == 2 && has_state) {
        return Err(unsupported());
    }
    let names: Vec<_> = vars.iter().map(String::as_str).collect();
    let table = s.data.truth_table(&names).ok_or_else(unsupported)?;
    let mut modes = Vec::new();
    for (di, data) in external.iter().enumerate() {
        for (ei, enable) in external.iter().enumerate() {
            if di == ei {
                continue;
            }
            let hold = scan.then(|| {
                external
                    .iter()
                    .enumerate()
                    .find(|(i, _)| *i != di && *i != ei)
                    .unwrap()
                    .1
            });
            for flags in 0..if scan { 8 } else { 4 } {
                let inverted = flags & 1 != 0;
                let low = flags & 2 != 0;
                let hold_low = flags & 4 != 0;
                let matches = (0u32..8).all(|assignment| {
                    let d = assignment & 1 != 0;
                    let e = assignment & 2 != 0;
                    let h = assignment & 4 != 0;
                    let mut index = 0usize;
                    for (i, name) in vars.iter().enumerate() {
                        let value = if name == *data {
                            d
                        } else if name == *enable {
                            e
                        } else if hold.is_some_and(|pin| name == *pin) || *name == s.state {
                            h
                        } else if *name == s.state_n {
                            !h
                        } else {
                            return false;
                        };
                        index |= usize::from(value) << i;
                    }
                    let expected = if e ^ low { d ^ inverted } else { h ^ hold_low };
                    (table >> index) & 1 == u64::from(expected)
                });
                if matches {
                    modes.push(DataMode {
                        data: (*data).clone(),
                        inverted,
                        enable: Some(PolPin {
                            pin: (*enable).clone(),
                            active_low: low,
                        }),
                        hold: hold.map(|pin| PolPin {
                            pin: (*pin).clone(),
                            active_low: hold_low,
                        }),
                    });
                    if modes.len() > 8 {
                        return Err("sequential mode budget reached".into());
                    }
                }
            }
        }
    }
    if modes.is_empty() {
        Err(unsupported())
    } else {
        Ok(modes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusteda_formats::liberty::parse_function;

    fn sequential(data: &str) -> Sequential {
        Sequential {
            kind: SeqKind::FlipFlop,
            state: "S".into(),
            state_n: "SN".into(),
            data_text: data.into(),
            data: parse_function(data).unwrap(),
            clock_text: "CK".into(),
            clock: parse_function("CK").unwrap(),
            clocked_on_also: None,
            clear_text: None,
            clear: None,
            preset_text: None,
            preset: None,
            clear_preset_var1: None,
            clear_preset_var2: None,
        }
    }

    // SYNTH-REQ-0523: canonical state/complement and inactive-arm semantics.
    #[test]
    fn recognizes_enable_polarity_and_rejects_toggle_hold() {
        let pins = vec!["D".into(), "E".into(), "CK".into()];
        let modes = infer(&sequential("(E&D)|(!E&S)"), &pins).unwrap();
        assert!(modes.iter().any(|m| {
            m.data == "D"
                && !m.inverted
                && m.hold.is_none()
                && m.enable
                    .as_ref()
                    .is_some_and(|e| e.pin == "E" && !e.active_low)
        }));
        let modes = infer(&sequential("(!E&!D)|(E&!SN)"), &pins).unwrap();
        assert!(
            modes
                .iter()
                .any(|m| m.inverted && m.enable.as_ref().is_some_and(|e| e.active_low))
        );
        assert!(infer(&sequential("(E&D)|(!E&!S)"), &pins).is_err());
    }

    // SYNTH-REQ-0523: either data arm can be bound to stored-state feedback.
    #[test]
    fn recognizes_two_data_roles_without_pin_name_rules() {
        let pins = vec!["A".into(), "B".into(), "SEL".into(), "CK".into()];
        let modes = infer(&sequential("(!SEL&A)|(SEL&B)"), &pins).unwrap();
        assert_eq!(modes.len(), 2);
        assert!(modes.iter().any(|m| m.data == "A"
            && m.hold.as_ref().is_some_and(|p| p.pin == "B")
            && m.enable.as_ref().is_some_and(|p| p.active_low)));
        assert!(modes.iter().any(|m| m.data == "B"
            && m.hold.as_ref().is_some_and(|p| p.pin == "A")
            && m.enable.as_ref().is_some_and(|p| !p.active_low)));
    }
}
