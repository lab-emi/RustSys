// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Decoder for the `LIBERTY` v3 (`.rlib`) binary cache (`WS-REQ-0111` to `WS-REQ-0114`).

use std::collections::BTreeMap;
use std::path::Path;

use rusteda_core::liberty::{
    AntennaDiodeType, Cell, Direction, Func, InternalPower, LevelShifterType, Library,
    OperatingConditions, PgPin, PgType, Pin, SeqKind, Sequential, Statetable, SwitchCellType,
    Table, Template, TimingArc, TreeType, Unit, Units, WireLoad, WireLoadAreaRule, WireLoadMode,
    WireLoadSelection, WireLoadTable,
};

use super::{MAX_FUNC_DEPTH, corrupt_rlib};
use crate::error::Result;

pub(super) fn decode_model(
    path: &Path,
    str_pool: &str,
    model: &[u8],
    f64_bits: &[u64],
) -> Result<Library> {
    let mut dec = Decoder {
        path,
        str_pool,
        model,
        pos: 0,
        f64_bits,
    };
    let lib = dec.read_library()?;
    if dec.pos != dec.model.len() {
        return Err(corrupt_rlib(
            path,
            format!(
                "MODEL section has {} trailing byte(s)",
                dec.model.len() - dec.pos
            ),
        ));
    }
    Ok(lib)
}

struct Decoder<'a> {
    path: &'a Path,
    str_pool: &'a str,
    model: &'a [u8],
    pos: usize,
    f64_bits: &'a [u64],
}

impl<'a> Decoder<'a> {
    fn remaining(&self) -> usize {
        self.model.len().saturating_sub(self.pos)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| corrupt_rlib(self.path, "MODEL byte offset overflow"))?;
        let slice = self.model.get(self.pos..end).ok_or_else(|| {
            corrupt_rlib(
                self.path,
                format!("truncated MODEL stream at byte {}", self.pos),
            )
        })?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_bool(&mut self) -> Result<bool> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(corrupt_rlib(
                self.path,
                format!("invalid bool tag {other} in MODEL stream"),
            )),
        }
    }

    fn read_u32(&mut self) -> Result<u32> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_count(&mut self, min_elem_bytes: usize) -> Result<usize> {
        let n = self.read_u32()? as usize;
        let required = n
            .checked_mul(min_elem_bytes)
            .ok_or_else(|| corrupt_rlib(self.path, "MODEL element count overflow"))?;
        if required > self.remaining() {
            return Err(corrupt_rlib(
                self.path,
                format!(
                    "MODEL count {n} requires at least {required} bytes, only {} remaining",
                    self.remaining()
                ),
            ));
        }
        Ok(n)
    }

    fn read_f64(&mut self) -> Result<f64> {
        let b = self.read_bytes(8)?;
        Ok(f64::from_bits(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ])))
    }

    fn read_opt_f64(&mut self) -> Result<Option<f64>> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_f64()?)),
            other => Err(corrupt_rlib(
                self.path,
                format!("invalid opt<f64> tag {other}"),
            )),
        }
    }

    fn read_opt_f64_pair(&mut self) -> Result<Option<(f64, f64)>> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some((self.read_f64()?, self.read_f64()?))),
            other => Err(corrupt_rlib(
                self.path,
                format!("invalid opt<(f64, f64)> tag {other}"),
            )),
        }
    }

    fn read_str(&mut self) -> Result<String> {
        let off = self.read_u32()? as usize;
        let len = self.read_u32()? as usize;
        let end = off
            .checked_add(len)
            .ok_or_else(|| corrupt_rlib(self.path, "STR slice overflow"))?;
        let s = self.str_pool.get(off..end).ok_or_else(|| {
            corrupt_rlib(
                self.path,
                format!("STR slice ({off}, {len}) out of bounds or not on UTF-8 boundary"),
            )
        })?;
        Ok(s.to_string())
    }

    fn read_opt_str(&mut self) -> Result<Option<String>> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_str()?)),
            other => Err(corrupt_rlib(
                self.path,
                format!("invalid opt<str> tag {other}"),
            )),
        }
    }

    fn read_f64_slice(&mut self) -> Result<Vec<f64>> {
        let start = self.read_u32()? as usize;
        let len = self.read_u32()? as usize;
        let end = start
            .checked_add(len)
            .ok_or_else(|| corrupt_rlib(self.path, "F64 slice overflow"))?;
        let slice = self.f64_bits.get(start..end).ok_or_else(|| {
            corrupt_rlib(
                self.path,
                format!(
                    "F64 slice ({start}, {len}) exceeds pool length {}",
                    self.f64_bits.len()
                ),
            )
        })?;
        Ok(slice
            .iter()
            .map(|&bits| f64::from_bits(u64::from_le(bits)))
            .collect())
    }

    fn read_unit(&mut self) -> Result<Unit> {
        let scale = self.read_f64()?;
        let given = self.read_bool()?;
        let text = self.read_opt_str()?;
        let mut u = Unit::default();
        u.scale = scale;
        u.given = given;
        u.text = text;
        Ok(u)
    }

    fn read_units(&mut self) -> Result<Units> {
        let mut u = Units::default();
        u.time = self.read_unit()?;
        u.capacitance = self.read_unit()?;
        u.resistance = self.read_unit()?;
        u.voltage = self.read_unit()?;
        u.current = self.read_unit()?;
        u.leakage_power = self.read_unit()?;
        Ok(u)
    }

    fn read_template(&mut self) -> Result<Template> {
        let variable_1 = self.read_opt_str()?;
        let variable_2 = self.read_opt_str()?;
        let variable_3 = self.read_opt_str()?;
        let index_1 = self.read_f64_slice()?;
        let index_2 = self.read_f64_slice()?;
        let index_3 = self.read_f64_slice()?;
        Ok(Template {
            variable_1,
            variable_2,
            index_1,
            index_2,
            variable_3,
            index_3,
        })
    }

    fn read_table(&mut self) -> Result<Table> {
        let template = self.read_str()?;
        let variable_1 = self.read_opt_str()?;
        let variable_2 = self.read_opt_str()?;
        let variable_3 = self.read_opt_str()?;
        let index_1 = self.read_f64_slice()?;
        let index_2 = self.read_f64_slice()?;
        let index_3 = self.read_f64_slice()?;
        let values = self.read_f64_slice()?;
        Ok(Table {
            template,
            variable_1,
            variable_2,
            index_1,
            index_2,
            values,
            variable_3,
            index_3,
        })
    }

    fn read_opt_table(&mut self) -> Result<Option<Table>> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_table()?)),
            other => Err(corrupt_rlib(
                self.path,
                format!("invalid opt<Table> tag {other}"),
            )),
        }
    }

    fn read_func(&mut self, depth: usize) -> Result<Func> {
        if depth > MAX_FUNC_DEPTH {
            return Err(corrupt_rlib(
                self.path,
                "Func recursion depth limit exceeded",
            ));
        }
        match self.read_u8()? {
            0 => Ok(Func::Const(false)),
            1 => Ok(Func::Const(true)),
            2 => Ok(Func::Var(self.read_str()?)),
            3 => Ok(Func::Not(Box::new(self.read_func(depth + 1)?))),
            4 => {
                let a = self.read_func(depth + 1)?;
                let b = self.read_func(depth + 1)?;
                Ok(Func::And(Box::new(a), Box::new(b)))
            }
            5 => {
                let a = self.read_func(depth + 1)?;
                let b = self.read_func(depth + 1)?;
                Ok(Func::Or(Box::new(a), Box::new(b)))
            }
            6 => {
                let a = self.read_func(depth + 1)?;
                let b = self.read_func(depth + 1)?;
                Ok(Func::Xor(Box::new(a), Box::new(b)))
            }
            other => Err(corrupt_rlib(self.path, format!("invalid Func tag {other}"))),
        }
    }

    fn read_opt_func(&mut self) -> Result<Option<Func>> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_func(0)?)),
            other => Err(corrupt_rlib(
                self.path,
                format!("invalid opt<Func> tag {other}"),
            )),
        }
    }

    fn read_arc(&mut self) -> Result<TimingArc> {
        let related_pin = self.read_str()?;
        let timing_sense = self.read_opt_str()?;
        let timing_type = self.read_str()?;
        let cell_rise = self.read_opt_table()?;
        let cell_fall = self.read_opt_table()?;
        let rise_transition = self.read_opt_table()?;
        let fall_transition = self.read_opt_table()?;
        let rise_constraint = self.read_opt_table()?;
        let fall_constraint = self.read_opt_table()?;
        let when_text = self.read_opt_str()?;
        let when = self.read_opt_func()?;
        let sdf_cond = self.read_opt_str()?;
        let line = self.read_u32()?;
        Ok(TimingArc {
            related_pin,
            timing_sense,
            timing_type,
            cell_rise,
            cell_fall,
            rise_transition,
            fall_transition,
            rise_constraint,
            fall_constraint,
            when_text,
            when,
            sdf_cond,
            line,
        })
    }

    fn read_internal_power(&mut self) -> Result<InternalPower> {
        let related_pin = self.read_opt_str()?;
        let rise_power = self.read_opt_table()?;
        let fall_power = self.read_opt_table()?;
        Ok(InternalPower {
            related_pin,
            rise_power,
            fall_power,
        })
    }

    fn read_pin(&mut self) -> Result<Pin> {
        let name = self.read_str()?;
        let direction = match self.read_u8()? {
            0 => Direction::Input,
            1 => Direction::Output,
            2 => Direction::Inout,
            3 => Direction::Internal,
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid Direction tag {other}"),
                ));
            }
        };
        let function_text = self.read_opt_str()?;
        let function = self.read_opt_func()?;
        let three_state = self.read_opt_str()?;
        let three_state_function = self.read_opt_func()?;
        let capacitance = self.read_f64()?;
        let capacitance_given = self.read_bool()?;
        let rise_capacitance = self.read_opt_f64()?;
        let fall_capacitance = self.read_opt_f64()?;
        let rise_capacitance_range = self.read_opt_f64_pair()?;
        let fall_capacitance_range = self.read_opt_f64_pair()?;
        let min_capacitance = self.read_opt_f64()?;
        let max_capacitance = self.read_opt_f64()?;
        let max_transition = self.read_opt_f64()?;
        let max_fanout = self.read_opt_f64()?;
        let fanout_load = self.read_opt_f64()?;
        let is_clock = self.read_bool()?;
        let arc_count = self.read_count(30)?;
        let mut arcs = Vec::with_capacity(arc_count);
        for _ in 0..arc_count {
            arcs.push(self.read_arc()?);
        }
        let ip_count = self.read_count(3)?;
        let mut internal_power = Vec::with_capacity(ip_count);
        for _ in 0..ip_count {
            internal_power.push(self.read_internal_power()?);
        }
        let isolation_cell_enable_pin = self.read_bool()?;
        let isolation_cell_data_pin = self.read_bool()?;
        let level_shifter_enable_pin = self.read_bool()?;
        let level_shifter_data_pin = self.read_bool()?;
        let always_on = self.read_bool()?;
        let related_power_pin = self.read_opt_str()?;
        let related_ground_pin = self.read_opt_str()?;
        let power_down_function = self.read_opt_str()?;
        let state_function_text = self.read_opt_str()?;
        let state_function = self.read_opt_func()?;
        let internal_node = self.read_opt_str()?;
        let clock_gate_clock_pin = self.read_bool()?;
        let clock_gate_enable_pin = self.read_bool()?;
        let clock_gate_out_pin = self.read_bool()?;
        let clock_gate_test_pin = self.read_bool()?;
        let clock_gate_obs_pin = self.read_bool()?;
        let line = self.read_u32()?;
        Ok(Pin {
            name,
            direction,
            function_text,
            function,
            three_state,
            three_state_function,
            capacitance,
            capacitance_given,
            rise_capacitance,
            fall_capacitance,
            rise_capacitance_range,
            fall_capacitance_range,
            min_capacitance,
            max_capacitance,
            max_transition,
            max_fanout,
            fanout_load,
            is_clock,
            arcs,
            internal_power,
            isolation_cell_enable_pin,
            isolation_cell_data_pin,
            level_shifter_enable_pin,
            level_shifter_data_pin,
            always_on,
            related_power_pin,
            related_ground_pin,
            power_down_function,
            state_function_text,
            state_function,
            internal_node,
            clock_gate_clock_pin,
            clock_gate_enable_pin,
            clock_gate_out_pin,
            clock_gate_test_pin,
            clock_gate_obs_pin,
            line,
        })
    }

    fn read_sequential(&mut self) -> Result<Sequential> {
        let kind = match self.read_u8()? {
            0 => SeqKind::FlipFlop,
            1 => SeqKind::Latch,
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid SeqKind tag {other}"),
                ));
            }
        };
        let state = self.read_str()?;
        let state_n = self.read_str()?;
        let data_text = self.read_str()?;
        let data = self.read_func(0)?;
        let clock_text = self.read_str()?;
        let clock = self.read_func(0)?;
        let clocked_on_also = self.read_opt_str()?;
        let clear_text = self.read_opt_str()?;
        let clear = self.read_opt_func()?;
        let preset_text = self.read_opt_str()?;
        let preset = self.read_opt_func()?;
        let clear_preset_var1 = self.read_opt_str()?;
        let clear_preset_var2 = self.read_opt_str()?;
        Ok(Sequential {
            kind,
            state,
            state_n,
            data_text,
            data,
            clock_text,
            clock,
            clocked_on_also,
            clear_text,
            clear,
            preset_text,
            preset,
            clear_preset_var1,
            clear_preset_var2,
        })
    }

    fn read_statetable(&mut self) -> Result<Statetable> {
        let in_count = self.read_count(8)?;
        let mut input_nodes = Vec::with_capacity(in_count);
        for _ in 0..in_count {
            input_nodes.push(self.read_str()?);
        }
        let int_count = self.read_count(8)?;
        let mut internal_nodes = Vec::with_capacity(int_count);
        for _ in 0..int_count {
            internal_nodes.push(self.read_str()?);
        }
        let table = self.read_str()?;
        let row_count = self.read_count(8)?;
        let mut rows = Vec::with_capacity(row_count);
        for _ in 0..row_count {
            rows.push(self.read_str()?);
        }
        Ok(Statetable {
            input_nodes,
            internal_nodes,
            table,
            rows,
        })
    }

    fn read_pg_pin(&mut self) -> Result<PgPin> {
        let name = self.read_str()?;
        let pg_type = match self.read_u8()? {
            0 => None,
            1 => {
                let kind_tag = self.read_u8()?;
                let s = self.read_str()?;
                Some(match kind_tag {
                    0 => PgType::from_name(&s),
                    1 => PgType::Other(s),
                    other => {
                        return Err(corrupt_rlib(
                            self.path,
                            format!("invalid PgType variant tag {other}"),
                        ));
                    }
                })
            }
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid opt<PgType> tag {other}"),
                ));
            }
        };
        let voltage_name = self.read_opt_str()?;
        let mut pg = PgPin::new(name);
        pg.pg_type = pg_type;
        pg.voltage_name = voltage_name;
        Ok(pg)
    }

    fn read_cell(&mut self) -> Result<Cell> {
        let name = self.read_str()?;
        let area = self.read_f64()?;
        let dont_use = self.read_bool()?;
        let pin_count = self.read_count(40)?;
        let mut pins = Vec::with_capacity(pin_count);
        for _ in 0..pin_count {
            pins.push(self.read_pin()?);
        }
        let pg_pin_count = self.read_count(8)?;
        let mut pg_pins = Vec::with_capacity(pg_pin_count);
        for _ in 0..pg_pin_count {
            pg_pins.push(self.read_str()?);
        }
        let seq_count = self.read_count(33)?;
        let mut sequentials = Vec::with_capacity(seq_count);
        for _ in 0..seq_count {
            sequentials.push(self.read_sequential()?);
        }
        let has_statetable = self.read_bool()?;
        let has_test_cell = self.read_bool()?;
        let is_clock_gating = self.read_bool()?;
        let has_bus = self.read_bool()?;
        let leakage_power = self.read_opt_f64()?;
        let prob_count = self.read_count(8)?;
        let mut problems = Vec::with_capacity(prob_count);
        for _ in 0..prob_count {
            problems.push(self.read_str()?);
        }
        let pgg_count = self.read_count(10)?;
        let mut pg_pin_groups = Vec::with_capacity(pgg_count);
        for _ in 0..pgg_count {
            pg_pin_groups.push(self.read_pg_pin()?);
        }
        let is_isolation_cell = self.read_bool()?;
        let is_level_shifter = self.read_bool()?;
        let level_shifter_type = match self.read_u8()? {
            0 => None,
            1 => {
                let kind_tag = self.read_u8()?;
                let s = self.read_str()?;
                Some(match kind_tag {
                    0 => LevelShifterType::from_name(&s),
                    1 => LevelShifterType::Other(s),
                    other => {
                        return Err(corrupt_rlib(
                            self.path,
                            format!("invalid LevelShifterType variant tag {other}"),
                        ));
                    }
                })
            }
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid opt<LevelShifterType> tag {other}"),
                ));
            }
        };
        let always_on = self.read_bool()?;
        let cell_footprint = self.read_opt_str()?;
        let dont_touch = self.read_bool()?;
        let is_macro_cell = self.read_bool()?;
        let pad_cell = self.read_bool()?;
        let switch_cell_type = match self.read_u8()? {
            0 => None,
            1 => {
                let kind_tag = self.read_u8()?;
                let s = self.read_str()?;
                Some(match kind_tag {
                    0 => SwitchCellType::from_name(&s),
                    1 => SwitchCellType::Other(s),
                    other => {
                        return Err(corrupt_rlib(
                            self.path,
                            format!("invalid SwitchCellType variant tag {other}"),
                        ));
                    }
                })
            }
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid opt<SwitchCellType> tag {other}"),
                ));
            }
        };
        let retention_cell = self.read_opt_str()?;
        let antenna_diode_type = match self.read_u8()? {
            0 => None,
            1 => {
                let kind_tag = self.read_u8()?;
                let s = self.read_str()?;
                Some(match kind_tag {
                    0 => AntennaDiodeType::from_name(&s),
                    1 => AntennaDiodeType::Other(s),
                    other => {
                        return Err(corrupt_rlib(
                            self.path,
                            format!("invalid AntennaDiodeType variant tag {other}"),
                        ));
                    }
                })
            }
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid opt<AntennaDiodeType> tag {other}"),
                ));
            }
        };
        let clock_gating_integrated_cell = self.read_opt_str()?;
        let statetable = match self.read_u8()? {
            0 => None,
            1 => Some(self.read_statetable()?),
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid opt<Statetable> tag {other}"),
                ));
            }
        };
        let line = self.read_u32()?;
        Ok(Cell {
            name,
            area,
            dont_use,
            pins,
            pg_pins,
            sequentials,
            has_statetable,
            has_test_cell,
            is_clock_gating,
            has_bus,
            leakage_power,
            problems,
            pg_pin_groups,
            is_isolation_cell,
            is_level_shifter,
            level_shifter_type,
            always_on,
            cell_footprint,
            dont_touch,
            is_macro_cell,
            pad_cell,
            switch_cell_type,
            retention_cell,
            antenna_diode_type,
            clock_gating_integrated_cell,
            statetable,
            line,
        })
    }

    fn read_fanout_pairs(&mut self) -> Result<Vec<(u32, f64)>> {
        let count = self.read_count(12)?;
        let mut pairs = Vec::with_capacity(count);
        for _ in 0..count {
            pairs.push((self.read_u32()?, self.read_f64()?));
        }
        Ok(pairs)
    }

    fn read_wire_load(&mut self) -> Result<WireLoad> {
        let name = self.read_str()?;
        let resistance = self.read_f64()?;
        let capacitance = self.read_f64()?;
        let area = self.read_f64()?;
        let slope = self.read_f64()?;
        let fanout_length = self.read_fanout_pairs()?;
        Ok(WireLoad {
            name,
            resistance,
            capacitance,
            area,
            slope,
            fanout_length,
        })
    }

    fn read_wire_load_table(&mut self) -> Result<WireLoadTable> {
        let name = self.read_str()?;
        let fanout_length = self.read_fanout_pairs()?;
        let fanout_capacitance = self.read_fanout_pairs()?;
        let fanout_resistance = self.read_fanout_pairs()?;
        let fanout_area = self.read_fanout_pairs()?;
        Ok(WireLoadTable {
            name,
            fanout_length,
            fanout_capacitance,
            fanout_resistance,
            fanout_area,
        })
    }

    fn read_wire_load_selection(&mut self) -> Result<WireLoadSelection> {
        let name = self.read_str()?;
        let count = self.read_count(24)?;
        let mut rules = Vec::with_capacity(count);
        for _ in 0..count {
            let min_area = self.read_f64()?;
            let max_area = self.read_f64()?;
            let wire_load = self.read_str()?;
            rules.push(WireLoadAreaRule {
                min_area,
                max_area,
                wire_load,
            });
        }
        Ok(WireLoadSelection { name, rules })
    }

    fn read_operating_conditions(&mut self) -> Result<OperatingConditions> {
        let name = self.read_str()?;
        let process = self.read_opt_f64()?;
        let temperature = self.read_opt_f64()?;
        let voltage = self.read_opt_f64()?;
        let tree_type = match self.read_u8()? {
            0 => None,
            1 => {
                let kind_tag = self.read_u8()?;
                let s = self.read_str()?;
                Some(match kind_tag {
                    0 => TreeType::from_name(&s),
                    1 => TreeType::Other(s),
                    other => {
                        return Err(corrupt_rlib(
                            self.path,
                            format!("invalid TreeType variant tag {other}"),
                        ));
                    }
                })
            }
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid opt<TreeType> tag {other}"),
                ));
            }
        };
        Ok(OperatingConditions {
            name,
            process,
            temperature,
            voltage,
            tree_type,
        })
    }

    fn read_library(&mut self) -> Result<Library> {
        let name = self.read_str()?;
        let time_unit_ns = self.read_f64()?;
        let cap_unit_pf = self.read_f64()?;
        let default_max_capacitance = self.read_opt_f64()?;
        let default_max_transition = self.read_opt_f64()?;
        let default_max_fanout = self.read_opt_f64()?;
        let default_fanout_load = self.read_f64()?;
        let default_input_pin_cap = self.read_opt_f64()?;
        let default_output_pin_cap = self.read_opt_f64()?;
        let default_inout_pin_cap = self.read_opt_f64()?;
        let units = self.read_units()?;
        let tmpl_count = self.read_count(35)?;
        let mut templates = BTreeMap::new();
        let mut prev_key: Option<String> = None;
        for _ in 0..tmpl_count {
            let k = self.read_str()?;
            if let Some(prev) = &prev_key
                && k <= *prev
            {
                return Err(corrupt_rlib(
                    self.path,
                    format!("template keys not in strictly ascending order: {prev:?} >= {k:?}"),
                ));
            }
            let v = self.read_template()?;
            prev_key = Some(k.clone());
            templates.insert(k, v);
        }
        let cell_count = self.read_count(53)?;
        let mut cells = Vec::with_capacity(cell_count);
        for _ in 0..cell_count {
            cells.push(self.read_cell()?);
        }
        let input_threshold_pct_rise = self.read_opt_f64()?;
        let input_threshold_pct_fall = self.read_opt_f64()?;
        let output_threshold_pct_rise = self.read_opt_f64()?;
        let output_threshold_pct_fall = self.read_opt_f64()?;
        let slew_lower_threshold_pct_rise = self.read_opt_f64()?;
        let slew_upper_threshold_pct_rise = self.read_opt_f64()?;
        let slew_lower_threshold_pct_fall = self.read_opt_f64()?;
        let slew_upper_threshold_pct_fall = self.read_opt_f64()?;
        let slew_derate_from_library = self.read_opt_f64()?;

        let wl_count = self.read_count(52)?;
        let mut wire_loads = BTreeMap::new();
        let mut prev_wl: Option<String> = None;
        for _ in 0..wl_count {
            let k = self.read_str()?;
            if let Some(prev) = &prev_wl
                && k <= *prev
            {
                return Err(corrupt_rlib(
                    self.path,
                    format!("wire_load keys not in strictly ascending order: {prev:?} >= {k:?}"),
                ));
            }
            let v = self.read_wire_load()?;
            prev_wl = Some(k.clone());
            wire_loads.insert(k, v);
        }

        let wlt_count = self.read_count(32)?;
        let mut wire_load_tables = BTreeMap::new();
        let mut prev_wlt: Option<String> = None;
        for _ in 0..wlt_count {
            let k = self.read_str()?;
            if let Some(prev) = &prev_wlt
                && k <= *prev
            {
                return Err(corrupt_rlib(
                    self.path,
                    format!(
                        "wire_load_table keys not in strictly ascending order: {prev:?} >= {k:?}"
                    ),
                ));
            }
            let v = self.read_wire_load_table()?;
            prev_wlt = Some(k.clone());
            wire_load_tables.insert(k, v);
        }

        let wls_count = self.read_count(20)?;
        let mut wire_load_selections = BTreeMap::new();
        let mut prev_wls: Option<String> = None;
        for _ in 0..wls_count {
            let k = self.read_str()?;
            if let Some(prev) = &prev_wls
                && k <= *prev
            {
                return Err(corrupt_rlib(
                    self.path,
                    format!(
                        "wire_load_selection keys not in strictly ascending order: {prev:?} >= {k:?}"
                    ),
                ));
            }
            let v = self.read_wire_load_selection()?;
            prev_wls = Some(k.clone());
            wire_load_selections.insert(k, v);
        }

        let default_wire_load = self.read_opt_str()?;
        let default_wire_load_mode = match self.read_u8()? {
            0 => None,
            1 => {
                let kind_tag = self.read_u8()?;
                let s = self.read_str()?;
                Some(match kind_tag {
                    0 => WireLoadMode::from_name(&s),
                    1 => WireLoadMode::Other(s),
                    other => {
                        return Err(corrupt_rlib(
                            self.path,
                            format!("invalid WireLoadMode variant tag {other}"),
                        ));
                    }
                })
            }
            other => {
                return Err(corrupt_rlib(
                    self.path,
                    format!("invalid opt<WireLoadMode> tag {other}"),
                ));
            }
        };
        let default_wire_load_selection = self.read_opt_str()?;
        let default_wire_load_area = self.read_opt_f64()?;
        let default_wire_load_capacitance = self.read_opt_f64()?;
        let default_wire_load_resistance = self.read_opt_f64()?;

        let oc_count = self.read_count(20)?;
        let mut operating_conditions = BTreeMap::new();
        let mut prev_oc: Option<String> = None;
        for _ in 0..oc_count {
            let k = self.read_str()?;
            if let Some(prev) = &prev_oc
                && k <= *prev
            {
                return Err(corrupt_rlib(
                    self.path,
                    format!(
                        "operating_conditions keys not in strictly ascending order: {prev:?} >= {k:?}"
                    ),
                ));
            }
            let v = self.read_operating_conditions()?;
            prev_oc = Some(k.clone());
            operating_conditions.insert(k, v);
        }
        let default_operating_conditions = self.read_opt_str()?;
        let nom_process = self.read_opt_f64()?;
        let nom_temperature = self.read_opt_f64()?;
        let nom_voltage = self.read_opt_f64()?;

        Ok(Library {
            name,
            time_unit_ns,
            cap_unit_pf,
            default_max_capacitance,
            default_max_transition,
            default_max_fanout,
            default_fanout_load,
            templates,
            cells,
            units,
            default_input_pin_cap,
            default_output_pin_cap,
            default_inout_pin_cap,
            input_threshold_pct_rise,
            input_threshold_pct_fall,
            output_threshold_pct_rise,
            output_threshold_pct_fall,
            slew_lower_threshold_pct_rise,
            slew_upper_threshold_pct_rise,
            slew_lower_threshold_pct_fall,
            slew_upper_threshold_pct_fall,
            slew_derate_from_library,
            wire_loads,
            wire_load_tables,
            wire_load_selections,
            default_wire_load,
            default_wire_load_mode,
            default_wire_load_selection,
            default_wire_load_area,
            default_wire_load_capacitance,
            default_wire_load_resistance,
            operating_conditions,
            default_operating_conditions,
            nom_process,
            nom_temperature,
            nom_voltage,
        })
    }
}
