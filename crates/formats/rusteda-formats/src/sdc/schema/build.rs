// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Typed [`CmdKind`] builder from validated [`ParsedArgs`] (`SDC-REQ-0016` to `SDC-REQ-0039`).

use super::super::Units;
use super::super::cmd::{
    AnalysisType, CaseValue, ClockGroupsKind, CmdKind, EarlyLateScope, WireLoadMode,
};
use super::parse::{
    ActiveUnits, CAP_UNITS, CURR_UNITS, POWER_UNITS, ParsedArgs, RES_UNITS, TIME_UNITS, VOLT_UNITS,
    parse_f64_val, parse_obj_val, parse_u32_val, parse_unit_spec,
};

pub(super) fn build_cmd_kind(
    name: &str,
    p: &ParsedArgs,
    active: &mut ActiveUnits,
    sdc_units: &mut Units,
    current_design: &mut Option<String>,
) -> Result<CmdKind, (&'static str, String)> {
    match name {
        "set_units" => {
            if p.opts.is_empty() {
                return Err((
                    "D0004",
                    "'set_units' requires at least one unit option".to_string(),
                ));
            }
            let time = if let Some(raw) = p.get_str("-time") {
                let sc = parse_unit_spec(&raw, TIME_UNITS).ok_or_else(|| {
                    (
                        "D0030",
                        format!("invalid '-time' unit '{raw}' in 'set_units'"),
                    )
                })?;
                active.time = sc;
                sdc_units.time_to_ns = Some(1.0);
                Some("ns".to_string())
            } else {
                None
            };
            let capacitance = if let Some(raw) = p.get_str("-capacitance") {
                let sc = parse_unit_spec(&raw, CAP_UNITS).ok_or_else(|| {
                    (
                        "D0030",
                        format!("invalid '-capacitance' unit '{raw}' in 'set_units'"),
                    )
                })?;
                active.cap = sc;
                sdc_units.cap_to_pf = Some(1.0);
                Some("pF".to_string())
            } else {
                None
            };
            let resistance = if let Some(raw) = p.get_str("-resistance") {
                let sc = parse_unit_spec(&raw, RES_UNITS).ok_or_else(|| {
                    (
                        "D0030",
                        format!("invalid '-resistance' unit '{raw}' in 'set_units'"),
                    )
                })?;
                active.res = sc;
                sdc_units.res_to_kohm = Some(1.0);
                Some("kOhm".to_string())
            } else {
                None
            };
            let voltage = if let Some(raw) = p.get_str("-voltage") {
                let sc = parse_unit_spec(&raw, VOLT_UNITS).ok_or_else(|| {
                    (
                        "D0030",
                        format!("invalid '-voltage' unit '{raw}' in 'set_units'"),
                    )
                })?;
                active.voltage = sc;
                sdc_units.voltage_to_v = Some(1.0);
                Some("V".to_string())
            } else {
                None
            };
            let current = if let Some(raw) = p.get_str("-current") {
                let sc = parse_unit_spec(&raw, CURR_UNITS).ok_or_else(|| {
                    (
                        "D0030",
                        format!("invalid '-current' unit '{raw}' in 'set_units'"),
                    )
                })?;
                active.current = sc;
                sdc_units.current_to_ma = Some(1.0);
                Some("mA".to_string())
            } else {
                None
            };
            let power = if let Some(raw) = p.get_str("-power") {
                let sc = parse_unit_spec(&raw, POWER_UNITS).ok_or_else(|| {
                    (
                        "D0030",
                        format!("invalid '-power' unit '{raw}' in 'set_units'"),
                    )
                })?;
                active.power = sc;
                sdc_units.power_to_nw = Some(1.0);
                Some("nW".to_string())
            } else {
                None
            };
            Ok(CmdKind::SetUnits {
                time,
                capacitance,
                resistance,
                voltage,
                current,
                power,
            })
        }
        "current_design" => {
            let design = p.pos[0].as_scalar_str().ok_or_else(|| {
                (
                    "D0005",
                    "'current_design' expects a design name string".to_string(),
                )
            })?;
            *current_design = Some(design.clone());
            Ok(CmdKind::CurrentDesign { name: design })
        }
        "set_operating_conditions" => {
            let condition = if let Some(pos0) = p.pos.first() {
                Some(pos0.as_scalar_str().ok_or_else(|| {
                    (
                        "D0005",
                        "'set_operating_conditions' expects a condition name string".to_string(),
                    )
                })?)
            } else {
                None
            };
            let analysis_type = match p.get_str("-analysis_type") {
                Some(s) => Some(AnalysisType::parse(&s).ok_or_else(|| {
                    (
                        "D0005",
                        format!("invalid '-analysis_type' '{s}' in 'set_operating_conditions'"),
                    )
                })?),
                None => None,
            };
            let min = p.get_str("-min");
            let max = p.get_str("-max");
            if condition.is_none() && min.is_none() && max.is_none() && analysis_type.is_none() {
                return Err((
                    "D0004",
                    "'set_operating_conditions' requires a condition or '-min'/'-max'/'-analysis_type'".to_string(),
                ));
            }
            Ok(CmdKind::SetOperatingConditions {
                condition,
                min,
                max,
                library: p.get_str("-library"),
                min_library: p.get_str("-min_library"),
                max_library: p.get_str("-max_library"),
                analysis_type,
                objects: p.get_obj("-object_list"),
            })
        }
        "set_wire_load_model" => {
            let wlm_name = p.get_str("-name").ok_or_else(|| {
                (
                    "D0004",
                    "'set_wire_load_model' requires '-name <model_name>'".to_string(),
                )
            })?;
            let objects = match p.pos.first() {
                Some(v) => Some(parse_obj_val(v, name, "object_list")?),
                None => None,
            };
            Ok(CmdKind::SetWireLoadModel {
                name: wlm_name,
                library: p.get_str("-library"),
                min: p.has_flag("-min"),
                max: p.has_flag("-max"),
                objects,
            })
        }
        "set_wire_load_mode" => {
            let mode_str = p.pos[0].as_scalar_str().ok_or_else(|| {
                (
                    "D0005",
                    "'set_wire_load_mode' expects 'top', 'enclosed', or 'segmented'".to_string(),
                )
            })?;
            let mode = WireLoadMode::parse(&mode_str).ok_or_else(|| {
                (
                    "D0005",
                    format!(
                        "invalid wire-load mode '{mode_str}'; expected 'top', 'enclosed', or 'segmented'"
                    ),
                )
            })?;
            Ok(CmdKind::SetWireLoadMode { mode })
        }
        "set_wire_load_selection_group" => {
            let group_name = p.pos[0].as_scalar_str().ok_or_else(|| {
                (
                    "D0005",
                    "'set_wire_load_selection_group' expects a group name string".to_string(),
                )
            })?;
            let objects = match p.pos.get(1) {
                Some(v) => Some(parse_obj_val(v, name, "object_list")?),
                None => None,
            };
            Ok(CmdKind::SetWireLoadSelectionGroup {
                group_name,
                library: p.get_str("-library"),
                min: p.has_flag("-min"),
                max: p.has_flag("-max"),
                objects,
            })
        }
        "create_clock" => {
            let raw_period = p.get_float("-period").ok_or_else(|| {
                (
                    "D0004",
                    "'create_clock' requires '-period <value>'".to_string(),
                )
            })?;
            if raw_period <= 0.0 {
                return Err((
                    "D0005",
                    format!("'create_clock' requires a positive '-period', got {raw_period}"),
                ));
            }
            let period = active.time.apply(raw_period);
            let clk_name = p.get_str("-name");
            let sources = match p.pos.first() {
                Some(v) => Some(parse_obj_val(v, name, "source_objects")?),
                None => None,
            };
            if clk_name.is_none() && sources.is_none() {
                return Err((
                    "D0004",
                    "'create_clock' requires '-name' or source objects".to_string(),
                ));
            }
            let waveform = match p.get_float_pair("-waveform") {
                Some([r, f]) => {
                    let r_ns = active.time.apply(r);
                    let f_ns = active.time.apply(f);
                    if r_ns < 0.0 || f_ns <= r_ns || f_ns > period {
                        return Err((
                            "D0005",
                            format!(
                                "'create_clock' '-waveform {{{r} {f}}}' must satisfy 0 <= rise < fall <= period"
                            ),
                        ));
                    }
                    Some([r_ns, f_ns])
                }
                None => None,
            };
            Ok(CmdKind::CreateClock {
                name: clk_name,
                period,
                waveform,
                add: p.has_flag("-add"),
                comment: p.get_str("-comment"),
                sources,
            })
        }
        "set_clock_groups" => {
            let kind = if p.has_flag("-asynchronous") {
                ClockGroupsKind::Asynchronous
            } else if p.has_flag("-logically_exclusive") {
                ClockGroupsKind::LogicallyExclusive
            } else if p.has_flag("-physically_exclusive") {
                ClockGroupsKind::PhysicallyExclusive
            } else {
                return Err((
                    "D0004",
                    "'set_clock_groups' requires '-asynchronous', '-logically_exclusive', or '-physically_exclusive'".to_string(),
                ));
            };
            let groups = p.get_objs("-group");
            if groups.is_empty() {
                return Err((
                    "D0004",
                    "'set_clock_groups' requires at least one '-group'".to_string(),
                ));
            }
            Ok(CmdKind::SetClockGroups {
                kind,
                name: p.get_str("-name"),
                allow_paths: p.has_flag("-allow_paths"),
                comment: p.get_str("-comment"),
                groups,
            })
        }
        "set_clock_uncertainty" => {
            // The model keeps no edge for the single-clock form, so `-rise` and `-fall` are refused
            // instead of being dropped (SDC-REQ-0023).
            for flag in ["-rise", "-fall"] {
                if p.has_flag(flag) {
                    return Err((
                        "D0011",
                        format!("option '{flag}' of 'set_clock_uncertainty' is not supported yet"),
                    ));
                }
            }
            let from = p.end_from();
            let to = p.to_end();
            if from.is_some() != to.is_some() {
                return Err((
                    "D0004",
                    "'set_clock_uncertainty' inter-clock mode requires both '-from' and '-to'"
                        .to_string(),
                ));
            }
            let (raw_unc, objects) = if from.is_some() {
                if p.pos.len() != 1 {
                    return Err((
                        "D0004",
                        "'set_clock_uncertainty' with '-from'/'-to' takes 1 positional argument (uncertainty)"
                            .to_string(),
                    ));
                }
                (parse_f64_val(&p.pos[0], name, "uncertainty")?, None)
            } else {
                if p.pos.len() != 2 {
                    return Err((
                        "D0004",
                        "'set_clock_uncertainty' without '-from'/'-to' requires uncertainty and object_list"
                            .to_string(),
                    ));
                }
                if let Ok(u) = parse_f64_val(&p.pos[0], name, "uncertainty") {
                    (u, Some(parse_obj_val(&p.pos[1], name, "object_list")?))
                } else {
                    let obj = parse_obj_val(&p.pos[0], name, "object_list")?;
                    let u = parse_f64_val(&p.pos[1], name, "uncertainty")?;
                    (u, Some(obj))
                }
            };
            Ok(CmdKind::SetClockUncertainty {
                uncertainty: active.time.apply(raw_unc),
                setup_hold: p.setup_hold_scope(),
                from,
                to,
                objects,
            })
        }
        "set_clock_latency" => {
            let raw_delay = parse_f64_val(&p.pos[0], name, "delay")?;
            let objects = parse_obj_val(&p.pos[1], name, "object_list")?;
            let source = p.has_flag("-source");
            let early_late = p.early_late_scope();
            if early_late != EarlyLateScope::Both && !source {
                return Err((
                    "D0004",
                    "'set_clock_latency' options '-early' and '-late' require '-source'"
                        .to_string(),
                ));
            }
            Ok(CmdKind::SetClockLatency {
                delay: active.time.apply(raw_delay),
                source,
                edge: p.edge_scope(),
                min_max: p.min_max_scope(),
                early_late,
                clock: p.get_obj("-clock"),
                objects,
            })
        }
        "set_clock_transition" => {
            let raw_tr = parse_f64_val(&p.pos[0], name, "transition")?;
            let clocks = parse_obj_val(&p.pos[1], name, "clock_list")?;
            Ok(CmdKind::SetClockTransition {
                transition: active.time.apply(raw_tr),
                edge: p.edge_scope(),
                min_max: p.min_max_scope(),
                clocks,
            })
        }
        "set_input_delay" | "set_output_delay" => {
            let raw_delay = parse_f64_val(&p.pos[0], name, "delay_value")?;
            let ports_or_pins = parse_obj_val(&p.pos[1], name, "port_pin_list")?;
            let clock = p.get_obj("-clock");
            let clock_fall = p.has_flag("-clock_fall");
            if clock_fall && clock.is_none() {
                return Err((
                    "D0004",
                    format!("'{name}' option '-clock_fall' requires '-clock'"),
                ));
            }
            let delay = active.time.apply(raw_delay);
            let edge = p.edge_scope();
            let min_max = p.min_max_scope();
            let level_sensitive = p.has_flag("-level_sensitive");
            let add_delay = p.has_flag("-add_delay");
            let network_latency_included = p.has_flag("-network_latency_included");
            let source_latency_included = p.has_flag("-source_latency_included");
            let reference_pin = p.get_obj("-reference_pin");
            if name == "set_input_delay" {
                Ok(CmdKind::SetInputDelay {
                    delay,
                    clock,
                    clock_fall,
                    level_sensitive,
                    edge,
                    min_max,
                    add_delay,
                    network_latency_included,
                    source_latency_included,
                    reference_pin,
                    ports_or_pins,
                })
            } else {
                Ok(CmdKind::SetOutputDelay {
                    delay,
                    clock,
                    clock_fall,
                    level_sensitive,
                    edge,
                    min_max,
                    add_delay,
                    network_latency_included,
                    source_latency_included,
                    reference_pin,
                    ports_or_pins,
                })
            }
        }
        "set_input_transition" => {
            let raw_tr = parse_f64_val(&p.pos[0], name, "transition")?;
            let ports = parse_obj_val(&p.pos[1], name, "port_list")?;
            let clock = p.get_obj("-clock");
            let clock_fall = p.has_flag("-clock_fall");
            if clock_fall && clock.is_none() {
                return Err((
                    "D0004",
                    "'set_input_transition' option '-clock_fall' requires '-clock'".to_string(),
                ));
            }
            Ok(CmdKind::SetInputTransition {
                transition: active.time.apply(raw_tr),
                edge: p.edge_scope(),
                min_max: p.min_max_scope(),
                clock,
                clock_fall,
                ports,
            })
        }
        "set_driving_cell" => {
            let lib_cell = p.get_str("-lib_cell").ok_or_else(|| {
                (
                    "D0004",
                    "'set_driving_cell' requires '-lib_cell <cell_name>'".to_string(),
                )
            })?;
            let ports = parse_obj_val(&p.pos[0], name, "port_list")?;
            Ok(CmdKind::SetDrivingCell {
                lib_cell,
                library: p.get_str("-library"),
                pin: p.get_str("-pin"),
                from_pin: p.get_str("-from_pin"),
                edge: p.edge_scope(),
                min_max: p.min_max_scope(),
                input_transition_rise: p
                    .get_float("-input_transition_rise")
                    .map(|v| active.time.apply(v)),
                input_transition_fall: p
                    .get_float("-input_transition_fall")
                    .map(|v| active.time.apply(v)),
                multiply_by: p.get_float("-multiply_by"),
                dont_scale: p.has_flag("-dont_scale"),
                no_design_rule: p.has_flag("-no_design_rule"),
                clock: p.get_obj("-clock"),
                clock_fall: p.has_flag("-clock_fall"),
                ports,
            })
        }
        "set_load" => {
            let raw_val = parse_f64_val(&p.pos[0], name, "value")?;
            let objects = parse_obj_val(&p.pos[1], name, "objects")?;
            Ok(CmdKind::SetLoad {
                value: active.cap.apply(raw_val),
                pin_load: p.has_flag("-pin_load"),
                wire_load: p.has_flag("-wire_load"),
                subtract_pin_load: p.has_flag("-subtract_pin_load"),
                min_max: p.min_max_scope(),
                objects,
            })
        }
        "set_false_path" => {
            let from = p.end_from();
            let to = p.to_end();
            if from.is_none() && to.is_none() {
                return Err((
                    "D0004",
                    "'set_false_path' requires at least one of '-from' or '-to'".to_string(),
                ));
            }
            Ok(CmdKind::SetFalsePath {
                setup_hold: p.setup_hold_scope(),
                edge: p.edge_scope(),
                from,
                through: Vec::new(),
                to,
                comment: p.get_str("-comment"),
            })
        }
        "set_multicycle_path" => {
            let multiplier = parse_u32_val(&p.pos[0], name, "path_multiplier")?;
            let from = p.end_from();
            let to = p.to_end();
            Ok(CmdKind::SetMulticyclePath {
                multiplier,
                setup_hold: p.setup_hold_scope(),
                start_end: p.start_end_scope(),
                edge: p.edge_scope(),
                from,
                through: Vec::new(),
                to,
                comment: p.get_str("-comment"),
            })
        }
        "set_max_delay" | "set_min_delay" => {
            let raw_delay = parse_f64_val(&p.pos[0], name, "delay_value")?;
            let delay = active.time.apply(raw_delay);
            let edge = p.edge_scope();
            let from = p.end_from();
            let to = p.to_end();
            let ignore_clock_latency = p.has_flag("-ignore_clock_latency");
            let comment = p.get_str("-comment");
            if name == "set_max_delay" {
                Ok(CmdKind::SetMaxDelay {
                    delay,
                    edge,
                    from,
                    through: Vec::new(),
                    to,
                    ignore_clock_latency,
                    comment,
                })
            } else {
                Ok(CmdKind::SetMinDelay {
                    delay,
                    edge,
                    from,
                    through: Vec::new(),
                    to,
                    ignore_clock_latency,
                    comment,
                })
            }
        }
        "set_case_analysis" => {
            let val_str = p.pos[0].as_scalar_str().ok_or_else(|| {
                (
                    "D0005",
                    "'set_case_analysis' expects '0', '1', 'rise', or 'fall'".to_string(),
                )
            })?;
            let value = CaseValue::parse(&val_str).ok_or_else(|| {
                (
                    "D0005",
                    format!(
                        "invalid 'set_case_analysis' value '{val_str}'; expected 0, 1, rise, or fall"
                    ),
                )
            })?;
            let ports_or_pins = parse_obj_val(&p.pos[1], name, "port_or_pin_list")?;
            Ok(CmdKind::SetCaseAnalysis {
                value,
                ports_or_pins,
            })
        }
        "set_logic_zero" => {
            let ports = parse_obj_val(&p.pos[0], name, "port_list")?;
            Ok(CmdKind::SetLogicZero { ports })
        }
        "set_logic_one" => {
            let ports = parse_obj_val(&p.pos[0], name, "port_list")?;
            Ok(CmdKind::SetLogicOne { ports })
        }
        "set_disable_timing" => {
            let from = p.get_str("-from");
            let to = p.get_str("-to");
            if from.is_some() != to.is_some() {
                return Err((
                    "D0004",
                    "'set_disable_timing' requires both '-from' and '-to' when either is specified"
                        .to_string(),
                ));
            }
            let objects = parse_obj_val(&p.pos[0], name, "cell_or_pin_list")?;
            Ok(CmdKind::SetDisableTiming { from, to, objects })
        }
        "set_max_time_borrow" => {
            let raw_val = parse_f64_val(&p.pos[0], name, "delay_value")?;
            if raw_val < 0.0 {
                return Err((
                    "D0005",
                    format!("'set_max_time_borrow' expects a non-negative delay, got {raw_val}"),
                ));
            }
            let objects = parse_obj_val(&p.pos[1], name, "object_list")?;
            Ok(CmdKind::SetMaxTimeBorrow {
                value: active.time.apply(raw_val),
                objects,
            })
        }
        "group_path" => {
            let grp_name = p.get_str("-name");
            let default = p.has_flag("-default");
            if grp_name.is_none() && !default {
                return Err((
                    "D0004",
                    "'group_path' requires '-name <name>' or '-default'".to_string(),
                ));
            }
            let from = p.end_from();
            let to = p.to_end();
            if from.is_none() && to.is_none() {
                return Err((
                    "D0004",
                    "'group_path' requires at least one of '-from' or '-to'".to_string(),
                ));
            }
            Ok(CmdKind::GroupPath {
                name: grp_name,
                default,
                weight: p.get_float("-weight"),
                critical_range: p.get_float("-critical_range").map(|v| active.time.apply(v)),
                from,
                through: Vec::new(),
                to,
                comment: p.get_str("-comment"),
            })
        }
        "set_max_transition" => {
            let raw_val = parse_f64_val(&p.pos[0], name, "transition_value")?;
            let objects = parse_obj_val(&p.pos[1], name, "object_list")?;
            Ok(CmdKind::SetMaxTransition {
                value: active.time.apply(raw_val),
                clock_path: p.has_flag("-clock_path"),
                data_path: p.has_flag("-data_path"),
                edge: p.edge_scope(),
                objects,
            })
        }
        "set_max_capacitance" => {
            let raw_val = parse_f64_val(&p.pos[0], name, "capacitance_value")?;
            let objects = parse_obj_val(&p.pos[1], name, "object_list")?;
            Ok(CmdKind::SetMaxCapacitance {
                value: active.cap.apply(raw_val),
                clock_path: p.has_flag("-clock_path"),
                data_path: p.has_flag("-data_path"),
                edge: p.edge_scope(),
                objects,
            })
        }
        "set_min_capacitance" => {
            let raw_val = parse_f64_val(&p.pos[0], name, "capacitance_value")?;
            let objects = parse_obj_val(&p.pos[1], name, "object_list")?;
            Ok(CmdKind::SetMinCapacitance {
                value: active.cap.apply(raw_val),
                clock_path: p.has_flag("-clock_path"),
                data_path: p.has_flag("-data_path"),
                edge: p.edge_scope(),
                objects,
            })
        }
        "set_max_fanout" => {
            let value = parse_f64_val(&p.pos[0], name, "fanout_value")?;
            let objects = parse_obj_val(&p.pos[1], name, "object_list")?;
            Ok(CmdKind::SetMaxFanout { value, objects })
        }
        "set_fanout_load" => {
            let value = parse_f64_val(&p.pos[0], name, "fanout_load_value")?;
            let ports = parse_obj_val(&p.pos[1], name, "port_list")?;
            Ok(CmdKind::SetFanoutLoad { value, ports })
        }
        "set_port_fanout_number" => {
            let value = parse_u32_val(&p.pos[0], name, "fanout_number")?;
            let ports = parse_obj_val(&p.pos[1], name, "port_list")?;
            Ok(CmdKind::SetPortFanoutNumber { value, ports })
        }
        "set_dont_use" => {
            let lib_cells = parse_obj_val(&p.pos[0], name, "cell_list")?;
            Ok(CmdKind::SetDontUse { lib_cells })
        }
        "set_dont_touch" => {
            let objects = parse_obj_val(&p.pos[0], name, "object_list")?;
            Ok(CmdKind::SetDontTouch { objects })
        }
        "set_dont_touch_network" => {
            let objects = parse_obj_val(&p.pos[0], name, "object_list")?;
            Ok(CmdKind::SetDontTouchNetwork {
                no_propagate: p.has_flag("-no_propagate"),
                objects,
            })
        }
        "set_size_only" => {
            let instances = parse_obj_val(&p.pos[0], name, "instance_list")?;
            Ok(CmdKind::SetSizeOnly {
                instances,
                no_instances: p.has_flag("-no_instances"),
            })
        }
        "set_max_area" => {
            let value = parse_f64_val(&p.pos[0], name, "area_value")?;
            Ok(CmdKind::SetMaxArea {
                value,
                ignore_tns: p.has_flag("-ignore_tns"),
            })
        }
        "set_max_dynamic_power" | "set_max_leakage_power" => {
            let value = parse_f64_val(&p.pos[0], name, "power_value")?;
            let pos_unit = match p.pos.get(1) {
                Some(v) => Some(
                    v.as_scalar_str()
                        .ok_or_else(|| ("D0005", format!("'{name}' expects a unit string")))?,
                ),
                None => None,
            };
            let unit = p.get_str("-unit").or(pos_unit);
            if name == "set_max_dynamic_power" {
                Ok(CmdKind::SetMaxDynamicPower { value, unit })
            } else {
                Ok(CmdKind::SetMaxLeakagePower { value, unit })
            }
        }
        _ => unreachable!(),
    }
}
