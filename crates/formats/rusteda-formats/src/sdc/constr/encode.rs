// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! `CONSTR v1` section encoder (`SDC-REQ-0040` to `SDC-REQ-0048`).

use rusteda_core::diag::Severity;

use super::super::Sdc;
use super::super::cmd::{
    AnalysisType, CaseValue, ClockGroupsKind, Cmd, CmdKind, EarlyLateScope, EdgeScope, MinMaxScope,
    SetupHoldScope, StartEndScope, WireLoadMode,
};
use super::super::obj::{Obj, ObjQuery};
use super::super::write::write_sdc;
use super::*;

impl Encoder {
    pub(super) fn encode_obj(&mut self, obj: &Obj) -> u32 {
        match obj {
            Obj::Ports(q) => self.encode_obj_query(OBJ_PORTS, q),
            Obj::Pins(q) => self.encode_obj_query(OBJ_PINS, q),
            Obj::Cells(q) => self.encode_obj_query(OBJ_CELLS, q),
            Obj::Nets(q) => self.encode_obj_query(OBJ_NETS, q),
            Obj::Clocks(q) => self.encode_obj_query(OBJ_CLOCKS, q),
            Obj::LibCells(q) => self.encode_obj_query(OBJ_LIB_CELLS, q),
            Obj::LibPins(q) => self.encode_obj_query(OBJ_LIB_PINS, q),
            Obj::AllInputs { no_clocks } => {
                let flags = if *no_clocks { OBJ_FLAG_NO_CLOCKS } else { 0 };
                self.write_obj_row(OBJ_ALL_INPUTS, flags, 0, 0, 0, NONE_U32)
            }
            Obj::AllOutputs => self.write_obj_row(OBJ_ALL_OUTPUTS, 0, 0, 0, 0, NONE_U32),
            Obj::AllClocks => self.write_obj_row(OBJ_ALL_CLOCKS, 0, 0, 0, 0, NONE_U32),
            Obj::AllRegisters => self.write_obj_row(OBJ_ALL_REGISTERS, 0, 0, 0, 0, NONE_U32),
            Obj::CurrentDesign => self.write_obj_row(OBJ_CURRENT_DESIGN, 0, 0, 0, 0, NONE_U32),
            Obj::Names(names) => {
                let start = self.pats_count as u32;
                for n in names {
                    self.push_pat(n);
                }
                self.write_obj_row(OBJ_NAMES, 0, 0, start, names.len() as u32, NONE_U32)
            }
            Obj::Union(items) => {
                let mut child_ids = Vec::with_capacity(items.len());
                for item in items {
                    child_ids.push(self.encode_obj(item));
                }
                let start = self.obj_child_count as u32;
                for cid in child_ids {
                    self.push_obj_child(cid);
                }
                self.write_obj_row(OBJ_UNION, 0, 0, start, items.len() as u32, NONE_U32)
            }
            Obj::Diff(lhs, rhs) => {
                let l_id = self.encode_obj(lhs);
                let r_id = self.encode_obj(rhs);
                self.write_obj_row(OBJ_DIFF, 0, 0, l_id, r_id, NONE_U32)
            }
        }
    }

    fn encode_obj_query(&mut self, kind: u8, q: &ObjQuery) -> u32 {
        let mut flags = 0u8;
        if q.hierarchical {
            flags |= OBJ_FLAG_HIER;
        }
        if q.regexp {
            flags |= OBJ_FLAG_REGEXP;
        }
        if q.nocase {
            flags |= OBJ_FLAG_NOCASE;
        }
        if q.quiet {
            flags |= OBJ_FLAG_QUIET;
        }
        let mut pad = 0u16;
        if let Some(of) = &q.of_objects {
            flags |= OBJ_FLAG_OF_OBJECTS;
            let child_id = self.encode_obj(of);
            let slot = self.obj_child_count as u16;
            self.push_obj_child(child_id);
            pad = slot;
        }
        let pat_start = self.pats_count as u32;
        for p in &q.patterns {
            self.push_pat(p);
        }
        let filter_id = match &q.filter {
            Some(f) => self.strs.intern(&f.to_canonical()),
            None => NONE_U32,
        };
        self.write_obj_row(
            kind,
            flags,
            pad,
            pat_start,
            q.patterns.len() as u32,
            filter_id,
        )
    }

    fn encode_cmd(&mut self, cmd: &Cmd) {
        let arg_start = self.args_count as u32;
        let mut flags = 0u32;
        let op = match &cmd.kind {
            CmdKind::SetUnits {
                time,
                capacitance,
                resistance,
                voltage,
                current,
                power,
            } => {
                self.push_arg_opt_str(KEY_UNIT_TIME, time);
                self.push_arg_opt_str(KEY_UNIT_CAP, capacitance);
                self.push_arg_opt_str(KEY_UNIT_RES, resistance);
                self.push_arg_opt_str(KEY_UNIT_VOLT, voltage);
                self.push_arg_opt_str(KEY_UNIT_CURR, current);
                self.push_arg_opt_str(KEY_UNIT_POWER, power);
                OP_SET_UNITS
            }
            CmdKind::CurrentDesign { name } => {
                self.push_arg_str(KEY_POS0, name);
                OP_CURRENT_DESIGN
            }
            CmdKind::SetOperatingConditions {
                condition,
                min,
                max,
                library,
                min_library,
                max_library,
                analysis_type,
                objects,
            } => {
                self.push_arg_opt_str(KEY_POS0, condition);
                self.push_arg_opt_str(KEY_MIN, min);
                self.push_arg_opt_str(KEY_MAX, max);
                self.push_arg_opt_str(KEY_LIBRARY, library);
                self.push_arg_opt_str(KEY_MIN_LIB, min_library);
                self.push_arg_opt_str(KEY_MAX_LIB, max_library);
                if let Some(at) = analysis_type {
                    let code = match at {
                        AnalysisType::Single => 0,
                        AnalysisType::BcWc => 1,
                        AnalysisType::OnChipVariation => 2,
                    };
                    self.push_arg_enum(KEY_ANALYSIS_TYPE, code);
                }
                self.push_arg_opt_obj(KEY_OBJECTS, objects);
                OP_SET_OP_COND
            }
            CmdKind::SetWireLoadModel {
                name,
                library,
                min,
                max,
                objects,
            } => {
                if *min {
                    flags |= 1 << 0;
                }
                if *max {
                    flags |= 1 << 1;
                }
                self.push_arg_str(KEY_NAME, name);
                self.push_arg_opt_str(KEY_LIBRARY, library);
                self.push_arg_opt_obj(KEY_OBJECTS, objects);
                OP_SET_WLM
            }
            CmdKind::SetWireLoadMode { mode } => {
                let code = match mode {
                    WireLoadMode::Top => 0,
                    WireLoadMode::Enclosed => 1,
                    WireLoadMode::Segmented => 2,
                };
                self.push_arg_enum(KEY_POS0, code);
                OP_SET_WL_MODE
            }
            CmdKind::SetWireLoadSelectionGroup {
                group_name,
                library,
                min,
                max,
                objects,
            } => {
                if *min {
                    flags |= 1 << 0;
                }
                if *max {
                    flags |= 1 << 1;
                }
                self.push_arg_str(KEY_POS0, group_name);
                self.push_arg_opt_str(KEY_LIBRARY, library);
                self.push_arg_opt_obj(KEY_OBJECTS, objects);
                OP_SET_WLSG
            }
            CmdKind::CreateClock {
                name,
                period,
                waveform,
                add,
                comment,
                sources,
            } => {
                if *add {
                    flags |= 1 << 0;
                }
                self.push_arg_opt_str(KEY_NAME, name);
                self.push_arg_num(KEY_PERIOD, *period);
                if let Some(wf) = waveform {
                    self.push_arg_f64_slice(KEY_WAVEFORM, wf);
                }
                self.push_arg_opt_str(KEY_COMMENT, comment);
                self.push_arg_opt_obj(KEY_OBJECTS, sources);
                OP_CREATE_CLOCK
            }
            CmdKind::CreateGeneratedClock {
                name,
                source,
                master_clock,
                divide_by,
                multiply_by,
                duty_cycle,
                edges,
                edge_shift,
                invert,
                combinational,
                add,
                comment,
                objects,
            } => {
                if *invert {
                    flags |= 1 << 0;
                }
                if *combinational {
                    flags |= 1 << 1;
                }
                if *add {
                    flags |= 1 << 2;
                }
                self.push_arg_opt_str(KEY_NAME, name);
                self.push_arg_obj(KEY_SOURCE, source);
                self.push_arg_opt_obj(KEY_MASTER_CLOCK, master_clock);
                if let Some(d) = divide_by {
                    self.push_arg_int(KEY_DIVIDE_BY, *d);
                }
                if let Some(m) = multiply_by {
                    self.push_arg_int(KEY_MULTIPLY_BY, *m);
                }
                if let Some(dc) = duty_cycle {
                    self.push_arg_num(KEY_DUTY_CYCLE, *dc);
                }
                if let Some([e0, e1, e2]) = edges {
                    let b = (*e1 as u64) | ((*e2 as u64) << 32);
                    self.push_arg(KEY_EDGES, ARG_INT_TRIPLE, *e0, b);
                }
                if let Some(es) = edge_shift {
                    self.push_arg_f64_slice(KEY_EDGE_SHIFT, es);
                }
                self.push_arg_opt_str(KEY_COMMENT, comment);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_CREATE_GEN_CLOCK
            }
            CmdKind::SetPropagatedClock { objects } => {
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_PROP_CLOCK
            }
            CmdKind::UnsetPropagatedClock { objects } => {
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_UNSET_PROP_CLOCK
            }
            CmdKind::SetClockGroups {
                kind,
                name,
                allow_paths,
                comment,
                groups,
            } => {
                let k_bits = match kind {
                    ClockGroupsKind::Asynchronous => 0,
                    ClockGroupsKind::LogicallyExclusive => 1,
                    ClockGroupsKind::PhysicallyExclusive => 2,
                };
                flags |= k_bits;
                if *allow_paths {
                    flags |= 1 << 2;
                }
                self.push_arg_opt_str(KEY_NAME, name);
                self.push_arg_opt_str(KEY_COMMENT, comment);
                for g in groups {
                    self.push_arg_obj(KEY_GROUP, g);
                }
                OP_SET_CLOCK_GROUPS
            }
            CmdKind::SetClockUncertainty {
                uncertainty,
                setup_hold,
                from,
                to,
                objects,
            } => {
                flags |= encode_setup_hold(*setup_hold);
                self.push_arg_num(KEY_POS0, *uncertainty);
                self.push_arg_end(KEY_FROM, from);
                self.push_arg_end(KEY_TO, to);
                self.push_arg_opt_obj(KEY_OBJECTS, objects);
                OP_SET_CLOCK_UNCERTAINTY
            }
            CmdKind::SetClockLatency {
                delay,
                source,
                edge,
                min_max,
                early_late,
                clock,
                objects,
            } => {
                if *source {
                    flags |= 1 << 0;
                }
                flags |= encode_edge(*edge) << 1;
                flags |= encode_min_max(*min_max) << 3;
                flags |= encode_early_late(*early_late) << 5;
                self.push_arg_num(KEY_POS0, *delay);
                self.push_arg_opt_obj(KEY_CLOCK, clock);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_CLOCK_LATENCY
            }
            CmdKind::SetClockTransition {
                transition,
                edge,
                min_max,
                clocks,
            } => {
                flags |= encode_edge(*edge);
                flags |= encode_min_max(*min_max) << 2;
                self.push_arg_num(KEY_POS0, *transition);
                self.push_arg_obj(KEY_OBJECTS, clocks);
                OP_SET_CLOCK_TRANSITION
            }
            CmdKind::SetInputDelay {
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
            } => {
                flags = pack_io_delay_flags(
                    *clock_fall,
                    *level_sensitive,
                    *edge,
                    *min_max,
                    *add_delay,
                    *network_latency_included,
                    *source_latency_included,
                );
                self.push_arg_num(KEY_POS0, *delay);
                self.push_arg_opt_obj(KEY_CLOCK, clock);
                self.push_arg_opt_obj(KEY_REFERENCE_PIN, reference_pin);
                self.push_arg_obj(KEY_OBJECTS, ports_or_pins);
                OP_SET_INPUT_DELAY
            }
            CmdKind::SetOutputDelay {
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
            } => {
                flags = pack_io_delay_flags(
                    *clock_fall,
                    *level_sensitive,
                    *edge,
                    *min_max,
                    *add_delay,
                    *network_latency_included,
                    *source_latency_included,
                );
                self.push_arg_num(KEY_POS0, *delay);
                self.push_arg_opt_obj(KEY_CLOCK, clock);
                self.push_arg_opt_obj(KEY_REFERENCE_PIN, reference_pin);
                self.push_arg_obj(KEY_OBJECTS, ports_or_pins);
                OP_SET_OUTPUT_DELAY
            }
            CmdKind::SetInputTransition {
                transition,
                edge,
                min_max,
                clock,
                clock_fall,
                ports,
            } => {
                flags |= encode_edge(*edge);
                flags |= encode_min_max(*min_max) << 2;
                if *clock_fall {
                    flags |= 1 << 4;
                }
                self.push_arg_num(KEY_POS0, *transition);
                self.push_arg_opt_obj(KEY_CLOCK, clock);
                self.push_arg_obj(KEY_OBJECTS, ports);
                OP_SET_INPUT_TRANSITION
            }
            CmdKind::SetDrivingCell {
                lib_cell,
                library,
                pin,
                from_pin,
                edge,
                min_max,
                input_transition_rise,
                input_transition_fall,
                multiply_by,
                dont_scale,
                no_design_rule,
                clock,
                clock_fall,
                ports,
            } => {
                flags |= encode_edge(*edge);
                flags |= encode_min_max(*min_max) << 2;
                if *dont_scale {
                    flags |= 1 << 4;
                }
                if *no_design_rule {
                    flags |= 1 << 5;
                }
                if *clock_fall {
                    flags |= 1 << 6;
                }
                self.push_arg_str(KEY_LIB_CELL, lib_cell);
                self.push_arg_opt_str(KEY_LIBRARY, library);
                self.push_arg_opt_str(KEY_PIN, pin);
                self.push_arg_opt_str(KEY_FROM_PIN, from_pin);
                if let Some(itr) = input_transition_rise {
                    self.push_arg_num(KEY_IN_TRANS_RISE, *itr);
                }
                if let Some(itf) = input_transition_fall {
                    self.push_arg_num(KEY_IN_TRANS_FALL, *itf);
                }
                if let Some(mb) = multiply_by {
                    self.push_arg_num(KEY_MULTIPLY_BY, *mb);
                }
                self.push_arg_opt_obj(KEY_CLOCK, clock);
                self.push_arg_obj(KEY_OBJECTS, ports);
                OP_SET_DRIVING_CELL
            }
            CmdKind::SetLoad {
                value,
                pin_load,
                wire_load,
                subtract_pin_load,
                min_max,
                objects,
            } => {
                if *pin_load {
                    flags |= 1 << 0;
                }
                if *wire_load {
                    flags |= 1 << 1;
                }
                if *subtract_pin_load {
                    flags |= 1 << 2;
                }
                flags |= encode_min_max(*min_max) << 3;
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_LOAD
            }
            CmdKind::SetFalsePath {
                setup_hold,
                edge,
                from,
                through,
                to,
                comment,
            } => {
                flags |= encode_setup_hold(*setup_hold);
                flags |= encode_edge(*edge) << 2;
                self.push_arg_end(KEY_FROM, from);
                for th in through {
                    self.push_arg_end(KEY_THROUGH, &Some(th.clone()));
                }
                self.push_arg_end(KEY_TO, to);
                self.push_arg_opt_str(KEY_COMMENT, comment);
                OP_SET_FALSE_PATH
            }
            CmdKind::SetMulticyclePath {
                multiplier,
                setup_hold,
                start_end,
                edge,
                from,
                through,
                to,
                comment,
            } => {
                flags |= encode_setup_hold(*setup_hold);
                flags |= encode_start_end(*start_end) << 2;
                flags |= encode_edge(*edge) << 4;
                self.push_arg_int(KEY_POS0, *multiplier);
                self.push_arg_end(KEY_FROM, from);
                for th in through {
                    self.push_arg_end(KEY_THROUGH, &Some(th.clone()));
                }
                self.push_arg_end(KEY_TO, to);
                self.push_arg_opt_str(KEY_COMMENT, comment);
                OP_SET_MULTICYCLE_PATH
            }
            CmdKind::SetMaxDelay {
                delay,
                edge,
                from,
                through,
                to,
                ignore_clock_latency,
                comment,
            } => {
                flags |= encode_edge(*edge);
                if *ignore_clock_latency {
                    flags |= 1 << 2;
                }
                self.push_arg_num(KEY_POS0, *delay);
                self.push_arg_end(KEY_FROM, from);
                for th in through {
                    self.push_arg_end(KEY_THROUGH, &Some(th.clone()));
                }
                self.push_arg_end(KEY_TO, to);
                self.push_arg_opt_str(KEY_COMMENT, comment);
                OP_SET_MAX_DELAY
            }
            CmdKind::SetMinDelay {
                delay,
                edge,
                from,
                through,
                to,
                ignore_clock_latency,
                comment,
            } => {
                flags |= encode_edge(*edge);
                if *ignore_clock_latency {
                    flags |= 1 << 2;
                }
                self.push_arg_num(KEY_POS0, *delay);
                self.push_arg_end(KEY_FROM, from);
                for th in through {
                    self.push_arg_end(KEY_THROUGH, &Some(th.clone()));
                }
                self.push_arg_end(KEY_TO, to);
                self.push_arg_opt_str(KEY_COMMENT, comment);
                OP_SET_MIN_DELAY
            }
            CmdKind::SetCaseAnalysis {
                value,
                ports_or_pins,
            } => {
                let code = match value {
                    CaseValue::Zero => 0,
                    CaseValue::One => 1,
                    CaseValue::Rise => 2,
                    CaseValue::Fall => 3,
                };
                self.push_arg_enum(KEY_POS0, code);
                self.push_arg_obj(KEY_OBJECTS, ports_or_pins);
                OP_SET_CASE_ANALYSIS
            }
            CmdKind::SetLogicZero { ports } => {
                self.push_arg_obj(KEY_OBJECTS, ports);
                OP_SET_LOGIC_ZERO
            }
            CmdKind::SetLogicOne { ports } => {
                self.push_arg_obj(KEY_OBJECTS, ports);
                OP_SET_LOGIC_ONE
            }
            CmdKind::SetDisableTiming { from, to, objects } => {
                self.push_arg_opt_str(KEY_FROM, from);
                self.push_arg_opt_str(KEY_TO, to);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_DISABLE_TIMING
            }
            CmdKind::SetMaxTimeBorrow { value, objects } => {
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_MAX_TIME_BORROW
            }
            CmdKind::SetTimingDerate {
                factor,
                early_late,
                edge,
                clock,
                data,
                net_delay,
                cell_delay,
                cell_check,
                static_derate,
                dynamic,
                increment,
                objects,
            } => {
                flags |= encode_early_late(*early_late);
                flags |= encode_edge(*edge) << 2;
                if *clock {
                    flags |= 1 << 4;
                }
                if *data {
                    flags |= 1 << 5;
                }
                if *net_delay {
                    flags |= 1 << 6;
                }
                if *cell_delay {
                    flags |= 1 << 7;
                }
                if *cell_check {
                    flags |= 1 << 8;
                }
                if *static_derate {
                    flags |= 1 << 9;
                }
                if *dynamic {
                    flags |= 1 << 10;
                }
                if *increment {
                    flags |= 1 << 11;
                }
                self.push_arg_num(KEY_POS0, *factor);
                self.push_arg_opt_obj(KEY_OBJECTS, objects);
                OP_SET_TIMING_DERATE
            }
            CmdKind::SetMinPulseWidth {
                value,
                high,
                low,
                objects,
            } => {
                if *high {
                    flags |= 1 << 0;
                }
                if *low {
                    flags |= 1 << 1;
                }
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_MIN_PULSE_WIDTH
            }
            CmdKind::SetIdealNetwork {
                no_propagate,
                objects,
            } => {
                if *no_propagate {
                    flags |= 1 << 0;
                }
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_IDEAL_NETWORK
            }
            CmdKind::GroupPath {
                name,
                default,
                weight,
                critical_range,
                from,
                through,
                to,
                comment,
            } => {
                if *default {
                    flags |= 1 << 0;
                }
                self.push_arg_opt_str(KEY_NAME, name);
                if let Some(w) = weight {
                    self.push_arg_num(KEY_WEIGHT, *w);
                }
                if let Some(cr) = critical_range {
                    self.push_arg_num(KEY_CRITICAL_RANGE, *cr);
                }
                self.push_arg_end(KEY_FROM, from);
                for th in through {
                    self.push_arg_end(KEY_THROUGH, &Some(th.clone()));
                }
                self.push_arg_end(KEY_TO, to);
                self.push_arg_opt_str(KEY_COMMENT, comment);
                OP_GROUP_PATH
            }
            CmdKind::SetMaxTransition {
                value,
                clock_path,
                data_path,
                edge,
                objects,
            } => {
                if *clock_path {
                    flags |= 1 << 0;
                }
                if *data_path {
                    flags |= 1 << 1;
                }
                flags |= encode_edge(*edge) << 2;
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_MAX_TRANSITION
            }
            CmdKind::SetMaxCapacitance {
                value,
                clock_path,
                data_path,
                edge,
                objects,
            } => {
                if *clock_path {
                    flags |= 1 << 0;
                }
                if *data_path {
                    flags |= 1 << 1;
                }
                flags |= encode_edge(*edge) << 2;
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_MAX_CAPACITANCE
            }
            CmdKind::SetMinCapacitance {
                value,
                clock_path,
                data_path,
                edge,
                objects,
            } => {
                if *clock_path {
                    flags |= 1 << 0;
                }
                if *data_path {
                    flags |= 1 << 1;
                }
                flags |= encode_edge(*edge) << 2;
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_MIN_CAPACITANCE
            }
            CmdKind::SetMaxFanout { value, objects } => {
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_MAX_FANOUT
            }
            CmdKind::SetFanoutLoad { value, ports } => {
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, ports);
                OP_SET_FANOUT_LOAD
            }
            CmdKind::SetPortFanoutNumber { value, ports } => {
                self.push_arg_int(KEY_POS0, *value);
                self.push_arg_obj(KEY_OBJECTS, ports);
                OP_SET_PORT_FANOUT_NUMBER
            }
            CmdKind::SetDontUse { lib_cells } => {
                self.push_arg_obj(KEY_OBJECTS, lib_cells);
                OP_SET_DONT_USE
            }
            CmdKind::SetDontTouch { objects } => {
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_DONT_TOUCH
            }
            CmdKind::SetDontTouchNetwork {
                no_propagate,
                objects,
            } => {
                if *no_propagate {
                    flags |= 1 << 0;
                }
                self.push_arg_obj(KEY_OBJECTS, objects);
                OP_SET_DONT_TOUCH_NETWORK
            }
            CmdKind::SetSizeOnly {
                instances,
                no_instances,
            } => {
                if *no_instances {
                    flags |= 1 << 0;
                }
                self.push_arg_obj(KEY_OBJECTS, instances);
                OP_SET_SIZE_ONLY
            }
            CmdKind::SetMaxArea { value, ignore_tns } => {
                if *ignore_tns {
                    flags |= 1 << 0;
                }
                self.push_arg_num(KEY_POS0, *value);
                OP_SET_MAX_AREA
            }
            CmdKind::SetMaxDynamicPower { value, unit } => {
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_opt_str(KEY_POS1, unit);
                OP_SET_MAX_DYNAMIC_POWER
            }
            CmdKind::SetMaxLeakagePower { value, unit } => {
                self.push_arg_num(KEY_POS0, *value);
                self.push_arg_opt_str(KEY_POS1, unit);
                OP_SET_MAX_LEAKAGE_POWER
            }
        };
        let arg_count = (self.args_count as u32) - arg_start;
        self.cmds_bytes.extend_from_slice(&op.to_le_bytes());
        self.cmds_bytes.extend_from_slice(&cmd.source.to_le_bytes());
        self.cmds_bytes.extend_from_slice(&flags.to_le_bytes());
        self.cmds_bytes.extend_from_slice(&cmd.line.to_le_bytes());
        self.cmds_bytes.extend_from_slice(&cmd.col.to_le_bytes());
        self.cmds_bytes.extend_from_slice(&arg_start.to_le_bytes());
        self.cmds_bytes.extend_from_slice(&arg_count.to_le_bytes());
        self.cmds_count += 1;
    }
}

#[allow(clippy::fn_params_excessive_bools)]
fn pack_io_delay_flags(
    clock_fall: bool,
    level_sensitive: bool,
    edge: EdgeScope,
    min_max: MinMaxScope,
    add_delay: bool,
    network_latency_included: bool,
    source_latency_included: bool,
) -> u32 {
    let mut f = 0u32;
    if clock_fall {
        f |= 1 << 0;
    }
    if level_sensitive {
        f |= 1 << 1;
    }
    f |= encode_edge(edge) << 2;
    f |= encode_min_max(min_max) << 4;
    if add_delay {
        f |= 1 << 6;
    }
    if network_latency_included {
        f |= 1 << 7;
    }
    if source_latency_included {
        f |= 1 << 8;
    }
    f
}

pub(super) fn encode_edge(e: EdgeScope) -> u32 {
    match e {
        EdgeScope::Both => 0,
        EdgeScope::Rise => 1,
        EdgeScope::Fall => 2,
    }
}

pub(super) fn decode_edge(v: u32) -> Result<EdgeScope, ConstrError> {
    match v & 0x3 {
        0 => Ok(EdgeScope::Both),
        1 => Ok(EdgeScope::Rise),
        2 => Ok(EdgeScope::Fall),
        other => Err(ConstrError(format!("invalid edge scope {other}"))),
    }
}

fn encode_min_max(m: MinMaxScope) -> u32 {
    match m {
        MinMaxScope::Both => 0,
        MinMaxScope::Min => 1,
        MinMaxScope::Max => 2,
    }
}

pub(super) fn decode_min_max(v: u32) -> Result<MinMaxScope, ConstrError> {
    match v & 0x3 {
        0 => Ok(MinMaxScope::Both),
        1 => Ok(MinMaxScope::Min),
        2 => Ok(MinMaxScope::Max),
        other => Err(ConstrError(format!("invalid min/max scope {other}"))),
    }
}

fn encode_setup_hold(sh: SetupHoldScope) -> u32 {
    match sh {
        SetupHoldScope::Both => 0,
        SetupHoldScope::Setup => 1,
        SetupHoldScope::Hold => 2,
    }
}

pub(super) fn decode_setup_hold(v: u32) -> Result<SetupHoldScope, ConstrError> {
    match v & 0x3 {
        0 => Ok(SetupHoldScope::Both),
        1 => Ok(SetupHoldScope::Setup),
        2 => Ok(SetupHoldScope::Hold),
        other => Err(ConstrError(format!("invalid setup/hold scope {other}"))),
    }
}

fn encode_early_late(el: EarlyLateScope) -> u32 {
    match el {
        EarlyLateScope::Both => 0,
        EarlyLateScope::Early => 1,
        EarlyLateScope::Late => 2,
    }
}

pub(super) fn decode_early_late(v: u32) -> Result<EarlyLateScope, ConstrError> {
    match v & 0x3 {
        0 => Ok(EarlyLateScope::Both),
        1 => Ok(EarlyLateScope::Early),
        2 => Ok(EarlyLateScope::Late),
        other => Err(ConstrError(format!("invalid early/late scope {other}"))),
    }
}

fn encode_start_end(se: StartEndScope) -> u32 {
    match se {
        StartEndScope::Default => 0,
        StartEndScope::Start => 1,
        StartEndScope::End => 2,
    }
}

pub(super) fn decode_start_end(v: u32) -> Result<StartEndScope, ConstrError> {
    match v & 0x3 {
        0 => Ok(StartEndScope::Default),
        1 => Ok(StartEndScope::Start),
        2 => Ok(StartEndScope::End),
        other => Err(ConstrError(format!("invalid start/end scope {other}"))),
    }
}

/// Encodes `sdc` into the 13 `CONSTR v1` sections (SDC-REQ-0040 to SDC-REQ-0048).
pub fn encode_sections(sdc: &Sdc) -> Vec<ConstrSection> {
    let mut enc = Encoder::new();

    // Header fields
    let cur_design_id = enc.strs.intern_opt(&sdc.current_design);
    let mut header_flags = 0u32;
    if sdc.units.voltage_to_v.is_some() {
        header_flags |= 1 << 0;
    }
    if sdc.units.current_to_ma.is_some() {
        header_flags |= 1 << 1;
    }
    if sdc.units.power_to_nw.is_some() {
        header_flags |= 1 << 2;
    }
    let time_f = sdc.units.time_to_ns.unwrap_or(f64::NAN);
    let cap_f = sdc.units.cap_to_pf.unwrap_or(f64::NAN);
    let res_f = sdc.units.res_to_kohm.unwrap_or(f64::NAN);
    let mut header_bytes = Vec::with_capacity(32);
    header_bytes.extend_from_slice(&time_f.to_bits().to_le_bytes());
    header_bytes.extend_from_slice(&cap_f.to_bits().to_le_bytes());
    header_bytes.extend_from_slice(&res_f.to_bits().to_le_bytes());
    header_bytes.extend_from_slice(&cur_design_id.to_le_bytes());
    header_bytes.extend_from_slice(&header_flags.to_le_bytes());

    // Sources (section 4, elem_size 16)
    for src in &sdc.sources {
        let p_id = enc.strs.intern(&src.path);
        enc.sources_bytes.extend_from_slice(&p_id.to_le_bytes());
        enc.sources_bytes
            .extend_from_slice(&src.flags.to_le_bytes());
        enc.sources_bytes.extend_from_slice(&src.xxh3.to_le_bytes());
        enc.sources_count += 1;
    }

    // Env (section 5, elem_size 8)
    for e in &sdc.env {
        let n_id = enc.strs.intern(&e.name);
        let v_id = enc.strs.intern(&e.value);
        let name_word = n_id | if e.written { 1u32 << 31 } else { 0 };
        enc.env_bytes.extend_from_slice(&name_word.to_le_bytes());
        enc.env_bytes.extend_from_slice(&v_id.to_le_bytes());
        enc.env_count += 1;
    }

    // Commands (section 6..11)
    for cmd in &sdc.cmds {
        enc.encode_cmd(cmd);
    }

    // Canonical SDC text (section 12)
    let canon_text = write_sdc(sdc);
    let canon_bytes = canon_text.into_bytes();

    // Diagnostics (section 13, elem_size 24)
    for d in &sdc.diags {
        let sev_u32 = match d.severity {
            Severity::Note => 0u32,
            Severity::Warning => 1u32,
            Severity::Error => 2u32,
        };
        let code_id = enc.strs.intern(&d.code);
        let word0 = (sev_u32 & 0x3) | (code_id << 2);
        let (file_id, line, col) = match &d.location {
            Some(loc) => (enc.strs.intern(&loc.file), loc.line, loc.column),
            None => (NONE_U32, 0, 0),
        };
        let msg_id = enc.strs.intern(&d.message);
        let help_id = enc.strs.intern_opt(&d.help);
        enc.diags_bytes.extend_from_slice(&word0.to_le_bytes());
        enc.diags_bytes.extend_from_slice(&file_id.to_le_bytes());
        enc.diags_bytes.extend_from_slice(&line.to_le_bytes());
        enc.diags_bytes.extend_from_slice(&col.to_le_bytes());
        enc.diags_bytes.extend_from_slice(&msg_id.to_le_bytes());
        enc.diags_bytes.extend_from_slice(&help_id.to_le_bytes());
        enc.diags_count += 1;
    }

    // String offsets (section 2)
    let mut str_offs_bytes = Vec::with_capacity(enc.strs.offs.len() * 4);
    for &off in &enc.strs.offs {
        str_offs_bytes.extend_from_slice(&off.to_le_bytes());
    }
    let str_offs_count = enc.strs.offs.len() as u64;
    let str_bytes_len = enc.strs.bytes.len() as u64;
    let canon_len = canon_bytes.len() as u64;

    vec![
        ConstrSection {
            id: section_id::STR_BYTES,
            elem_size: 1,
            count: str_bytes_len,
            bytes: enc.strs.bytes,
        },
        ConstrSection {
            id: section_id::STR_OFFS,
            elem_size: 4,
            count: str_offs_count,
            bytes: str_offs_bytes,
        },
        ConstrSection {
            id: section_id::HEADER,
            elem_size: 32,
            count: 1,
            bytes: header_bytes,
        },
        ConstrSection {
            id: section_id::SOURCES,
            elem_size: 16,
            count: enc.sources_count,
            bytes: enc.sources_bytes,
        },
        ConstrSection {
            id: section_id::ENV,
            elem_size: 8,
            count: enc.env_count,
            bytes: enc.env_bytes,
        },
        ConstrSection {
            id: section_id::CMDS,
            elem_size: 24,
            count: enc.cmds_count,
            bytes: enc.cmds_bytes,
        },
        ConstrSection {
            id: section_id::ARGS,
            elem_size: 16,
            count: enc.args_count,
            bytes: enc.args_bytes,
        },
        ConstrSection {
            id: section_id::F64,
            elem_size: 8,
            count: enc.f64_count,
            bytes: enc.f64_bytes,
        },
        ConstrSection {
            id: section_id::OBJS,
            elem_size: 16,
            count: enc.objs_count,
            bytes: enc.objs_bytes,
        },
        ConstrSection {
            id: section_id::OBJ_CHILD,
            elem_size: 4,
            count: enc.obj_child_count,
            bytes: enc.obj_child_bytes,
        },
        ConstrSection {
            id: section_id::PATS,
            elem_size: 4,
            count: enc.pats_count,
            bytes: enc.pats_bytes,
        },
        ConstrSection {
            id: section_id::CANON,
            elem_size: 1,
            count: canon_len,
            bytes: canon_bytes,
        },
        ConstrSection {
            id: section_id::DIAGS,
            elem_size: 24,
            count: enc.diags_count,
            bytes: enc.diags_bytes,
        },
    ]
}
