// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Enumerated attribute values of the library model: timing types and senses (LIBERTY-REQ-0023,
//! LIBERTY-REQ-0024), table variables (LIBERTY-REQ-0031), power and ground pin types
//! (LIBERTY-REQ-0042), the types of special cells (LIBERTY-REQ-0040), wire-load modes
//! (LIBERTY-REQ-0047) and interconnect tree types (LIBERTY-REQ-0048). Each enumeration maps the
//! Liberty names one to one and keeps any other name as `Other` with its text.

use std::fmt;

/// Defines an enumeration of Liberty names with an `Other` variant for unknown names, its
/// conversions from and to the Liberty name, and its `Display` (the Liberty name).
macro_rules! liberty_names {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$vmeta:meta])* $var:ident = $text:literal,)*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $($(#[$vmeta])* $var,)*
            /// A name that is not one of the above, as written in the library.
            Other(String),
        }

        impl $name {
            /// The value with the Liberty name `name`; any other name gives `Other(name)`.
            pub fn from_name(name: &str) -> $name {
                match name {
                    $($text => $name::$var,)*
                    other => $name::Other(other.to_string()),
                }
            }

            /// The Liberty name of the value.
            pub fn name(&self) -> &str {
                match self {
                    $($name::$var => $text,)*
                    $name::Other(text) => text,
                }
            }

            /// False for `Other`.
            pub fn is_known(&self) -> bool {
                !matches!(self, $name::Other(_))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.name())
            }
        }
    };
}

liberty_names! {
    /// The `timing_type` of a timing arc (LIBERTY-REQ-0023).
    pub enum TimingType {
        Combinational = "combinational",
        CombinationalRise = "combinational_rise",
        CombinationalFall = "combinational_fall",
        ThreeStateEnable = "three_state_enable",
        ThreeStateEnableRise = "three_state_enable_rise",
        ThreeStateEnableFall = "three_state_enable_fall",
        ThreeStateDisable = "three_state_disable",
        ThreeStateDisableRise = "three_state_disable_rise",
        ThreeStateDisableFall = "three_state_disable_fall",
        RisingEdge = "rising_edge",
        FallingEdge = "falling_edge",
        Preset = "preset",
        Clear = "clear",
        SetupRising = "setup_rising",
        SetupFalling = "setup_falling",
        HoldRising = "hold_rising",
        HoldFalling = "hold_falling",
        RecoveryRising = "recovery_rising",
        RecoveryFalling = "recovery_falling",
        RemovalRising = "removal_rising",
        RemovalFalling = "removal_falling",
        SkewRising = "skew_rising",
        SkewFalling = "skew_falling",
        NonSeqSetupRising = "non_seq_setup_rising",
        NonSeqSetupFalling = "non_seq_setup_falling",
        NonSeqHoldRising = "non_seq_hold_rising",
        NonSeqHoldFalling = "non_seq_hold_falling",
        NochangeHighHigh = "nochange_high_high",
        NochangeHighLow = "nochange_high_low",
        NochangeLowHigh = "nochange_low_high",
        NochangeLowLow = "nochange_low_low",
        MinPulseWidth = "min_pulse_width",
        MinimumPeriod = "minimum_period",
        MaxClockTreePath = "max_clock_tree_path",
        MinClockTreePath = "min_clock_tree_path",
    }
}

liberty_names! {
    /// The `timing_sense` of a timing arc (LIBERTY-REQ-0024).
    pub enum TimingSense {
        PositiveUnate = "positive_unate",
        NegativeUnate = "negative_unate",
        NonUnate = "non_unate",
    }
}

liberty_names! {
    /// A variable of a lookup-table template: what one axis of a table measures
    /// (LIBERTY-REQ-0031).
    pub enum TableVariable {
        InputNetTransition = "input_net_transition",
        TotalOutputNetCapacitance = "total_output_net_capacitance",
        RelatedPinTransition = "related_pin_transition",
        ConstrainedPinTransition = "constrained_pin_transition",
        OutputNetLength = "output_net_length",
        OutputNetWireCap = "output_net_wire_cap",
        OutputNetPinCap = "output_net_pin_cap",
        RelatedOutTotalOutputNetCapacitance = "related_out_total_output_net_capacitance",
        RelatedOutOutputNetLength = "related_out_output_net_length",
        RelatedOutOutputNetWireCap = "related_out_output_net_wire_cap",
        RelatedOutOutputNetPinCap = "related_out_output_net_pin_cap",
        EqualOrOppositeOutputNetCapacitance = "equal_or_opposite_output_net_capacitance",
        InputTransitionTime = "input_transition_time",
        NormalizedVoltage = "normalized_voltage",
        Time = "time",
        InputVoltage = "input_voltage",
        OutputVoltage = "output_voltage",
        InputNoiseHeight = "input_noise_height",
        InputNoiseWidth = "input_noise_width",
    }
}

liberty_names! {
    /// The `pg_type` of a power or ground pin (LIBERTY-REQ-0042).
    pub enum PgType {
        PrimaryPower = "primary_power",
        PrimaryGround = "primary_ground",
        BackupPower = "backup_power",
        BackupGround = "backup_ground",
        InternalPower = "internal_power",
        InternalGround = "internal_ground",
        Nwell = "nwell",
        Pwell = "pwell",
        DeepNwell = "deepnwell",
        DeepPwell = "deeppwell",
    }
}

liberty_names! {
    /// The `level_shifter_type` of a level-shifter cell (LIBERTY-REQ-0040): low to high, high
    /// to low, or both.
    pub enum LevelShifterType {
        LowToHigh = "LH",
        HighToLow = "HL",
        Both = "HL_LH",
    }
}

liberty_names! {
    /// The `switch_cell_type` of a power-switch cell (LIBERTY-REQ-0040).
    pub enum SwitchCellType {
        CoarseGrain = "coarse_grain",
        FineGrain = "fine_grain",
    }
}

liberty_names! {
    /// The `antenna_diode_type` of an antenna-diode cell (LIBERTY-REQ-0040).
    pub enum AntennaDiodeType {
        Power = "power",
        Ground = "ground",
        PowerAndGround = "power_and_ground",
    }
}

liberty_names! {
    /// The `default_wire_load_mode` of a library (LIBERTY-REQ-0047).
    pub enum WireLoadMode {
        Top = "top",
        Enclosed = "enclosed",
        Segmented = "segmented",
    }
}

liberty_names! {
    /// The `tree_type` of an `operating_conditions` group (LIBERTY-REQ-0048).
    pub enum TreeType {
        BestCaseTree = "best_case_tree",
        BalancedTree = "balanced_tree",
        WorstCaseTree = "worst_case_tree",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // LIBERTY-REQ-0023: every timing type round-trips through its Liberty name; any other name
    // is kept as `Other`.
    #[test]
    fn timing_type_names() {
        let names = [
            "combinational",
            "combinational_rise",
            "combinational_fall",
            "three_state_enable",
            "three_state_enable_rise",
            "three_state_enable_fall",
            "three_state_disable",
            "three_state_disable_rise",
            "three_state_disable_fall",
            "rising_edge",
            "falling_edge",
            "preset",
            "clear",
            "setup_rising",
            "setup_falling",
            "hold_rising",
            "hold_falling",
            "recovery_rising",
            "recovery_falling",
            "removal_rising",
            "removal_falling",
            "skew_rising",
            "skew_falling",
            "non_seq_setup_rising",
            "non_seq_setup_falling",
            "non_seq_hold_rising",
            "non_seq_hold_falling",
            "nochange_high_high",
            "nochange_high_low",
            "nochange_low_high",
            "nochange_low_low",
            "min_pulse_width",
            "minimum_period",
            "max_clock_tree_path",
            "min_clock_tree_path",
        ];
        for n in names {
            let t = TimingType::from_name(n);
            assert!(t.is_known(), "{n}");
            assert_eq!(t.name(), n);
            assert_eq!(t.to_string(), n);
        }
        assert_eq!(
            TimingType::from_name("setup_rising"),
            TimingType::SetupRising
        );
        let other = TimingType::from_name("retaining_rise_edge");
        assert_eq!(other, TimingType::Other("retaining_rise_edge".into()));
        assert!(!other.is_known());
        assert_eq!(other.name(), "retaining_rise_edge");
    }

    // LIBERTY-REQ-0024, LIBERTY-REQ-0031, LIBERTY-REQ-0042, LIBERTY-REQ-0040, LIBERTY-REQ-0047,
    // LIBERTY-REQ-0048
    #[test]
    fn other_enumerations() {
        assert_eq!(
            TimingSense::from_name("negative_unate"),
            TimingSense::NegativeUnate
        );
        assert!(!TimingSense::from_name("unate").is_known());
        assert_eq!(
            TableVariable::from_name("related_out_output_net_pin_cap"),
            TableVariable::RelatedOutOutputNetPinCap
        );
        assert_eq!(TableVariable::Time.name(), "time");
        assert_eq!(PgType::from_name("deepnwell"), PgType::DeepNwell);
        assert_eq!(PgType::from_name("backup_ground"), PgType::BackupGround);
        assert_eq!(LevelShifterType::from_name("HL_LH"), LevelShifterType::Both);
        assert_eq!(
            SwitchCellType::from_name("fine_grain"),
            SwitchCellType::FineGrain
        );
        assert_eq!(
            AntennaDiodeType::from_name("power_and_ground"),
            AntennaDiodeType::PowerAndGround
        );
        assert_eq!(
            AntennaDiodeType::from_name("diode"),
            AntennaDiodeType::Other("diode".into())
        );
        assert_eq!(WireLoadMode::from_name("top"), WireLoadMode::Top);
        assert_eq!(WireLoadMode::from_name("enclosed"), WireLoadMode::Enclosed);
        assert_eq!(
            WireLoadMode::from_name("segmented"),
            WireLoadMode::Segmented
        );
        assert!(!WireLoadMode::from_name("custom").is_known());
        assert_eq!(
            TreeType::from_name("best_case_tree"),
            TreeType::BestCaseTree
        );
        assert_eq!(TreeType::from_name("balanced_tree"), TreeType::BalancedTree);
        assert_eq!(
            TreeType::from_name("worst_case_tree"),
            TreeType::WorstCaseTree
        );
        assert!(!TreeType::from_name("binary_tree").is_known());
    }
}
