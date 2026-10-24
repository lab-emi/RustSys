// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The class of every construct of a library (LIBERTY-REQ-0051, LIBERTY-REQ-0054 to
//! LIBERTY-REQ-0056): consumed into the model, ignorable, unsupported or unknown. Unsupported and
//! unknown constructs, attributes in the wrong form and repeated singletons get one aggregated
//! warning each. The builder classifies each group it reads when it reads it, so that the text is
//! walked once.

use super::diags::{Diags, code};
use super::syntax::{AttrValue, Group};

/// The form a consumed attribute takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `name : value ;`
    Simple,
    /// `name ( values ) ;`
    Complex,
}

/// The class of an attribute in its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrClass {
    /// Read into the model; one statement expected.
    Consumed(Form),
    /// Read into the model; a list of complex statements.
    List,
    Ignorable,
    Unsupported,
    Unknown,
}

/// The groups the reader reads, each with its own classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ctx {
    Library,
    Template,
    Type,
    WireLoad,
    WireLoadTable,
    WireLoadSelection,
    OperatingConditions,
    Cell,
    PgPin,
    Pin,
    Bus,
    Bundle,
    Statetable,
    Timing,
    InternalPower,
    Table,
    Ff,
    Latch,
}

/// The class of a group in its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupClass {
    /// Read into the model; `true` when one group of the kind is expected.
    Consumed(bool),
    Ignorable,
    Unsupported,
    Unknown,
}

use AttrClass::{Consumed as C, Ignorable as I, List as L, Unsupported as U};
use Form::{Complex as X, Simple as S};

/// The class of attribute `name` in a group of context `ctx` (LIBERTY-REQ-0054 to
/// LIBERTY-REQ-0056).
pub(super) fn is_supported_in_bus_or_bundle(ctx: Ctx, g: &Group) -> Result<(), String> {
    for a in &g.attrs {
        match attr_class(ctx, &a.name) {
            C(_) | L | I => {}
            U => return Err(format!("unsupported attribute '{}' in {}", a.name, g.kind)),
            AttrClass::Unknown => {
                return Err(format!("unknown attribute '{}' in {}", a.name, g.kind));
            }
        }
    }
    for c in &g.groups {
        match group_class(ctx, &c.kind) {
            GroupClass::Consumed(_) | GroupClass::Ignorable => {}
            GroupClass::Unsupported => {
                return Err(format!("unsupported group '{}' in {}", c.kind, g.kind));
            }
            GroupClass::Unknown => {
                return Err(format!("unknown group '{}' in {}", c.kind, g.kind));
            }
        }
    }
    Ok(())
}

fn attr_class(ctx: Ctx, name: &str) -> AttrClass {
    match ctx {
        Ctx::Library => match name {
            "time_unit"
            | "pulling_resistance_unit"
            | "voltage_unit"
            | "current_unit"
            | "leakage_power_unit"
            | "default_max_capacitance"
            | "default_max_transition"
            | "default_max_fanout"
            | "default_fanout_load"
            | "default_input_pin_cap"
            | "default_output_pin_cap"
            | "default_inout_pin_cap"
            | "input_threshold_pct_rise"
            | "input_threshold_pct_fall"
            | "output_threshold_pct_rise"
            | "output_threshold_pct_fall"
            | "slew_lower_threshold_pct_rise"
            | "slew_lower_threshold_pct_fall"
            | "slew_upper_threshold_pct_rise"
            | "slew_upper_threshold_pct_fall"
            | "slew_derate_from_library"
            | "bus_naming_style"
            | "nom_process"
            | "nom_temperature"
            | "nom_voltage"
            | "default_operating_conditions"
            | "default_wire_load"
            | "default_wire_load_mode"
            | "default_wire_load_selection"
            | "default_wire_load_area"
            | "default_wire_load_capacitance"
            | "default_wire_load_resistance" => C(S),
            "capacitive_load_unit" => C(X),
            "define" | "define_group" | "define_cell_area" => L,
            "comment"
            | "date"
            | "revision"
            | "technology"
            | "library_features"
            | "simulation"
            | "in_place_swap_mode"
            | "default_cell_leakage_power"
            | "default_leakage_power_density"
            | "voltage_map"
            | "delay_model" => I,
            "default_max_utilization" | "nom_calc_mode" | "piece_type" => U,
            n if n.starts_with("k_process_")
                || n.starts_with("k_volt_")
                || n.starts_with("k_temp_") =>
            {
                U
            }
            _ => AttrClass::Unknown,
        },
        Ctx::Template => match name {
            "variable_1" | "variable_2" | "variable_3" => C(S),
            "index_1" | "index_2" | "index_3" => C(X),
            _ => AttrClass::Unknown,
        },
        Ctx::Type => match name {
            "base_type" | "data_type" | "bit_width" | "bit_from" | "bit_to" | "downto" => C(S),
            _ => AttrClass::Unknown,
        },
        Ctx::WireLoad => match name {
            "resistance" | "capacitance" | "area" | "slope" => C(S),
            "fanout_length" => L,
            _ => AttrClass::Unknown,
        },
        Ctx::WireLoadTable => match name {
            "fanout_length" | "fanout_capacitance" | "fanout_resistance" | "fanout_area" => L,
            _ => AttrClass::Unknown,
        },
        Ctx::WireLoadSelection => match name {
            "wire_load_from_area" => L,
            _ => AttrClass::Unknown,
        },
        Ctx::OperatingConditions => match name {
            "process" | "temperature" | "voltage" | "tree_type" => C(S),
            _ => AttrClass::Unknown,
        },
        Ctx::Cell => match name {
            "area"
            | "dont_use"
            | "cell_leakage_power"
            | "is_isolation_cell"
            | "is_level_shifter"
            | "level_shifter_type"
            | "always_on"
            | "cell_footprint"
            | "dont_touch"
            | "is_macro_cell"
            | "pad_cell"
            | "switch_cell_type"
            | "retention_cell"
            | "antenna_diode_type"
            | "clock_gating_integrated_cell" => C(S),
            "driver_waveform"
            | "driver_waveform_rise"
            | "driver_waveform_fall"
            | "input_voltage_range"
            | "output_voltage_range" => I,
            "interface_timing"
            | "timing_model_type"
            | "is_clock_gating_cell"
            | "is_clock_isolation_cell"
            | "auxiliary_pad_cell"
            | "pad_type"
            | "single_bit_degenerate"
            | "threshold_voltage_group"
            | "preferred" => U,
            _ => AttrClass::Unknown,
        },
        Ctx::PgPin => match name {
            "pg_type" | "voltage_name" => C(S),
            "physical_connection"
            | "related_bias_pin"
            | "direction"
            | "user_pg_type"
            | "std_cell_main_rail" => I,
            _ => AttrClass::Unknown,
        },
        Ctx::Pin => match name {
            "direction"
            | "function"
            | "three_state"
            | "capacitance"
            | "rise_capacitance"
            | "fall_capacitance"
            | "min_capacitance"
            | "max_capacitance"
            | "max_transition"
            | "max_fanout"
            | "fanout_load"
            | "clock"
            | "isolation_cell_enable_pin"
            | "isolation_cell_data_pin"
            | "level_shifter_enable_pin"
            | "level_shifter_data_pin"
            | "always_on"
            | "related_power_pin"
            | "related_ground_pin"
            | "power_down_function"
            | "clock_gate_clock_pin"
            | "clock_gate_enable_pin"
            | "clock_gate_test_pin"
            | "clock_gate_out_pin"
            | "clock_gate_obs_pin"
            | "internal_node"
            | "state_function"
            | "min_pulse_width_high"
            | "min_pulse_width_low"
            | "min_period" => C(S),
            "rise_capacitance_range" | "fall_capacitance_range" => C(X),
            "driver_waveform"
            | "driver_waveform_rise"
            | "driver_waveform_fall"
            | "input_voltage"
            | "output_voltage"
            | "nextstate_type"
            | "driver_type"
            | "inverted_output"
            | "input_signal_level"
            | "output_signal_level" => I,
            "signal_type" | "test_output_only" | "is_pad" | "pulling_resistance"
            | "pulling_current" | "x_function" | "bundle" | "members" => U,
            _ => AttrClass::Unknown,
        },
        Ctx::Bus => match name {
            "bus_type" => C(S),
            other => attr_class(Ctx::Pin, other),
        },
        Ctx::Bundle => match name {
            "members" => C(X),
            other => attr_class(Ctx::Pin, other),
        },
        Ctx::Statetable => match name {
            "table" => C(S),
            _ => AttrClass::Unknown,
        },
        Ctx::Timing => match name {
            "related_pin" | "timing_sense" | "timing_type" | "when" | "sdf_cond" => C(S),
            "related_output_pin" => I,
            "related_bus_pins"
            | "related_bus_equivalent"
            | "mode"
            | "sdf_edges"
            | "sdf_cond_start"
            | "sdf_cond_end"
            | "when_start"
            | "when_end"
            | "default_timing" => U,
            _ => AttrClass::Unknown,
        },
        Ctx::InternalPower => match name {
            "related_pin" => C(S),
            "when" | "related_pg_pin" => I,
            "related_output_pin"
            | "mode"
            | "equal_or_opposite_output"
            | "falling_together_group"
            | "rising_together_group"
            | "switching_together_group"
            | "switching_interval" => U,
            _ => AttrClass::Unknown,
        },
        Ctx::Table => match name {
            "index_1" | "index_2" | "index_3" | "values" => C(X),
            _ => AttrClass::Unknown,
        },
        Ctx::Ff => match name {
            "next_state" | "clocked_on" | "clocked_on_also" | "clear" | "preset"
            | "clear_preset_var1" | "clear_preset_var2" => C(S),
            "power_down_function" => U,
            _ => AttrClass::Unknown,
        },
        Ctx::Latch => match name {
            "data_in" | "enable" | "clear" | "preset" | "clear_preset_var1"
            | "clear_preset_var2" => C(S),
            "enable_also" | "power_down_function" => U,
            _ => AttrClass::Unknown,
        },
    }
}

/// The class of group `kind` in a group of context `ctx` (LIBERTY-REQ-0054 to
/// LIBERTY-REQ-0056).
fn group_class(ctx: Ctx, kind: &str) -> GroupClass {
    use GroupClass::{Consumed as G, Ignorable as GI, Unsupported as GU};
    match ctx {
        Ctx::Library => match kind {
            "cell"
            | "type"
            | "operating_conditions"
            | "wire_load"
            | "wire_load_table"
            | "wire_load_selection" => G(false),
            "lu_table_template" | "power_lut_template" | "output_current_template" => G(false),
            "normalized_driver_waveform" | "input_voltage" | "output_voltage" | "power_supply" => {
                GI
            }
            "scaling_factors"
            | "timing_range"
            | "ocv_table_template"
            | "noise_lut_template"
            | "propagation_lut_template"
            | "iv_lut_template"
            | "poly_template"
            | "power_poly_template"
            | "maxcap_lut_template"
            | "maxtrans_lut_template"
            | "compact_lut_template"
            | "base_curves"
            | "sensitization"
            | "fpga_isd"
            | "dc_current_template" => GU,
            _ => GroupClass::Unknown,
        },
        Ctx::Cell => match kind {
            "pin" | "pg_pin" | "ff" | "latch" | "type" | "bus" | "bundle" => G(false),
            "statetable" => G(true),
            "leakage_power"
            | "test_cell"
            | "dynamic_current"
            | "leakage_current"
            | "intrinsic_parasitic" => GI,
            "ff_bank"
            | "latch_bank"
            | "mode_definition"
            | "routing_track"
            | "generated_clock"
            | "functional_yield_metric"
            | "pin_group" => GU,
            _ => GroupClass::Unknown,
        },
        Ctx::Pin => match kind {
            "timing" => G(false),
            "internal_power" => G(false),
            "receiver_capacitance"
            | "ccsn_first_stage"
            | "ccsn_last_stage"
            | "input_ccb"
            | "output_ccb" => GI,
            "minimum_period"
            | "min_pulse_width"
            | "tlatch"
            | "electromigration"
            | "hyperbolic_noise_above_high"
            | "hyperbolic_noise_below_low"
            | "hyperbolic_noise_high"
            | "hyperbolic_noise_low"
            | "max_trans"
            | "max_cap" => GU,
            _ => GroupClass::Unknown,
        },
        Ctx::Bus | Ctx::Bundle => match kind {
            "pin" => G(false),
            other => group_class(Ctx::Pin, other),
        },
        Ctx::Timing => match kind {
            "cell_rise" | "cell_fall" | "rise_transition" | "fall_transition"
            | "rise_constraint" | "fall_constraint" => G(true),
            "output_current_rise"
            | "output_current_fall"
            | "receiver_capacitance1_rise"
            | "receiver_capacitance1_fall"
            | "receiver_capacitance2_rise"
            | "receiver_capacitance2_fall"
            | "ccsn_first_stage"
            | "ccsn_last_stage" => GI,
            "retaining_rise"
            | "retaining_fall"
            | "retain_rise_slew"
            | "retain_fall_slew"
            | "rise_propagation"
            | "fall_propagation"
            | "cell_degradation"
            | "noise_immunity_above_high"
            | "noise_immunity_below_low"
            | "noise_immunity_high"
            | "noise_immunity_low"
            | "propagated_noise_height_above_high"
            | "propagated_noise_height_below_low"
            | "propagated_noise_height_high"
            | "propagated_noise_height_low"
            | "steady_state_current_high"
            | "steady_state_current_low"
            | "steady_state_current_tristate" => GU,
            k if k.starts_with("ocv_") => GU,
            _ => GroupClass::Unknown,
        },
        Ctx::InternalPower => match kind {
            "power" | "rise_power" | "fall_power" => G(true),
            _ => GroupClass::Unknown,
        },
        Ctx::Template
        | Ctx::Type
        | Ctx::WireLoad
        | Ctx::WireLoadTable
        | Ctx::WireLoadSelection
        | Ctx::OperatingConditions
        | Ctx::PgPin
        | Ctx::Statetable
        | Ctx::Table
        | Ctx::Ff
        | Ctx::Latch => GroupClass::Unknown,
    }
}

/// User-defined attributes and groups, declared by the library's `define`, `define_group` and
/// `define_cell_area` statements (LIBERTY-REQ-0055).
#[derive(Debug, Default)]
struct Defined {
    /// (group kind, attribute name).
    attrs: Vec<(String, String)>,
    /// (parent group kind, group kind).
    groups: Vec<(String, String)>,
}

impl Defined {
    fn of(lib: &Group) -> Defined {
        let mut d = Defined::default();
        for a in &lib.attrs {
            let AttrValue::Complex(args) = &a.value else {
                continue;
            };
            let arg = |i: usize| args.get(i).map(|s| s.trim().to_string());
            match a.name.as_str() {
                "define" => {
                    if let (Some(name), Some(group)) = (arg(0), arg(1)) {
                        d.attrs.push((group, name));
                    }
                }
                "define_group" => {
                    if let (Some(name), Some(parent)) = (arg(0), arg(1)) {
                        d.groups.push((parent, name));
                    }
                }
                "define_cell_area" => {
                    if let Some(name) = arg(0) {
                        d.attrs.push(("cell".to_string(), name));
                    }
                }
                _ => {}
            }
        }
        d
    }

    fn attr(&self, group: &str, name: &str) -> bool {
        self.attrs.iter().any(|(g, n)| g == group && n == name)
    }

    fn group(&self, parent: &str, kind: &str) -> bool {
        self.groups.iter().any(|(p, k)| p == parent && k == kind)
    }
}

/// Classifies the constructs of the groups of one library.
#[derive(Debug)]
pub(super) struct Classifier {
    defined: Defined,
    /// Construct path buffer.
    path: String,
}

impl Classifier {
    /// The classifier of the library group `lib`, with its user-defined constructs.
    pub(super) fn new(lib: &Group) -> Classifier {
        Classifier {
            defined: Defined::of(lib),
            path: String::new(),
        }
    }

    /// Reports `code` for the construct `name` below the construct path `path` (empty at the
    /// top level of the file).
    fn warn(
        &mut self,
        diags: &mut Diags,
        code: &'static str,
        path: &str,
        name: &str,
        at: (u32, u32),
        text: impl FnOnce() -> String,
    ) {
        self.path.clear();
        if !path.is_empty() {
            self.path.push_str(path);
            self.path.push('/');
        }
        self.path.push_str(name);
        diags.warn(code, &self.path, at, text);
    }

    /// Classifies the attributes and child groups of `g`, a group of context `ctx` at the
    /// construct path `path`; consumed child groups are classified when the builder reads them.
    pub(super) fn group(&mut self, diags: &mut Diags, ctx: Ctx, g: &Group, path: &str) {
        let mut seen: Vec<&str> = Vec::new();
        for a in &g.attrs {
            let at = (a.line, a.col);
            match attr_class(ctx, &a.name) {
                C(form) => {
                    let wrong = match (&a.value, form) {
                        (AttrValue::Simple(_), X) => Some("simple"),
                        (AttrValue::Complex(_), S) => Some("complex"),
                        (AttrValue::Simple(_), S) | (AttrValue::Complex(_), X) => None,
                    };
                    if let Some(f) = wrong {
                        self.warn(diags, code::UNKNOWN, path, &a.name, at, || {
                            format!(
                                "'{}' in {f} form is not a construct of this reader; it is \
                                 ignored",
                                a.name
                            )
                        });
                    } else if seen.contains(&a.name.as_str()) {
                        self.warn(diags, code::DUPLICATE, path, &a.name, at, || {
                            format!(
                                "'{}' is given more than once; the first one is used",
                                a.name
                            )
                        });
                    } else {
                        seen.push(&a.name);
                    }
                }
                L => {
                    if matches!(&a.value, AttrValue::Simple(_)) {
                        self.warn(diags, code::UNKNOWN, path, &a.name, at, || {
                            format!(
                                "'{}' in simple form is not a construct of this reader; it is \
                                 ignored",
                                a.name
                            )
                        });
                    }
                }
                I => {}
                U => self.warn(diags, code::UNSUPPORTED, path, &a.name, at, || {
                    format!("'{}' is not supported; it is ignored", a.name)
                }),
                AttrClass::Unknown => {
                    if !self.defined.attr(&g.kind, &a.name) {
                        self.warn(diags, code::UNKNOWN, path, &a.name, at, || {
                            format!("unknown attribute '{}' is ignored", a.name)
                        });
                    }
                }
            }
        }
        seen.clear();
        for c in &g.groups {
            let at = (c.line, c.col);
            match group_class(ctx, &c.kind) {
                GroupClass::Consumed(single) => {
                    if single && seen.contains(&c.kind.as_str()) {
                        self.warn(diags, code::DUPLICATE, path, &c.kind, at, || {
                            format!(
                                "group '{}' is given more than once; the first one is used",
                                c.kind
                            )
                        });
                    } else {
                        seen.push(&c.kind);
                    }
                }
                GroupClass::Ignorable => {}
                GroupClass::Unsupported => {
                    self.warn(diags, code::UNSUPPORTED, path, &c.kind, at, || {
                        format!("group '{}' is not supported; it is ignored", c.kind)
                    })
                }
                GroupClass::Unknown => {
                    if !self.defined.group(&g.kind, &c.kind) {
                        self.warn(diags, code::UNKNOWN, path, &c.kind, at, || {
                            format!("unknown group '{}' is ignored", c.kind)
                        });
                    }
                }
            }
        }
    }

    /// Reports the statements of the file `root` other than its first library group `lib`.
    pub(super) fn outside(&mut self, diags: &mut Diags, root: &Group, lib: &Group) {
        for a in &root.attrs {
            self.warn(
                diags,
                code::UNSUPPORTED,
                "",
                &a.name,
                (a.line, a.col),
                || format!("'{}' outside the library group is ignored", a.name),
            );
        }
        for g in &root.groups {
            if !std::ptr::eq(g, lib) {
                self.warn(
                    diags,
                    code::UNSUPPORTED,
                    "",
                    &g.kind,
                    (g.line, g.col),
                    || {
                        format!(
                            "group '{}' outside the first library group is ignored",
                            g.kind
                        )
                    },
                );
            }
        }
    }
}
