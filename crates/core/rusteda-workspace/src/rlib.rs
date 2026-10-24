// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The `LIBERTY` v4 (`.rlib`) compiled library codec and workspace cache loader
//! (contract §5, `WS-REQ-0110` to `WS-REQ-0116`, `WS-REQ-0117`).
//!
//! ## Container layout (`WS-REQ-0110`)
//!
//! A compiled Liberty library (`cache/<name>-<h16>.rlib`) is an [`Rbin`] container of kind
//! [`KIND`] (`"LIBERTY"`) and format version [`FORMAT_VERSION`] (`4`, range [`VERSIONS`]; an
//! older `.rlib` is compiled again from its cached `.lib`, `WS-REQ-0084`).
//! When compiled from a source `.lib` file ([`encode_with_source`], [`put`]), the container
//! header records upstream `"lib"` mapped to the `xxh3-64` hash of the `.lib` bytes.
//!
//! Three sections are written in ascending section ID order:
//! - [`SEC_STR`] (`1`, `elem_size = 1`): concatenated UTF-8 bytes of all strings in the library,
//!   deduplicated in first-appearance order.
//! - [`SEC_MODEL`] (`2`, `elem_size = 1`): deterministic little-endian byte stream of the
//!   [`Library`] model.
//! - [`SEC_F64`] (`3`, `elem_size = 8`): pool of little-endian IEEE-754 `f64` bit patterns
//!   (`u64` in little-endian byte order) referenced by `(start, len)` slices from [`Template`]
//!   and [`Table`] records in `MODEL`.
//!
//! ## Normative `MODEL` byte-stream grammar (`WS-REQ-0111`, `WS-REQ-0112`)
//!
//! Primitives in `MODEL`:
//! - `u8`: 1 byte.
//! - `bool`: 1 byte (`0` = `false`, `1` = `true`).
//! - `u32`: 4 bytes, little-endian.
//! - `f64`: 8 bytes, little-endian IEEE-754 bits (`f64::to_bits().to_le_bytes()`).
//! - `str_ref`: `(offset: u32, len: u32)` indexing `&STR[offset..offset + len]`.
//! - `opt<T>`: `u8` tag (`0` = `None`, `1` = `Some(T)`).
//! - `f64_slice`: `(start: u32, len: u32)` indexing `len` elements starting at `start` in `F64`.
//! - `vec<T>`: `count: u32` followed by `count` encoded `T` records.
//!
//! Records in `MODEL`:
//! - `Library`: `name: str_ref`, `time_unit_ns: f64`, `cap_unit_pf: f64`,
//!   `default_max_capacitance: opt<f64>`, `default_max_transition: opt<f64>`,
//!   `default_max_fanout: opt<f64>`, `default_fanout_load: f64`,
//!   `default_input_pin_cap: opt<f64>`, `default_output_pin_cap: opt<f64>`,
//!   `default_inout_pin_cap: opt<f64>`, `units: Units`,
//!   `templates: vec<(str_ref, Template)>` (strictly ascending key order),
//!   `cells: vec<Cell>` (source order), `input_threshold_pct_rise: opt<f64>`,
//!   `input_threshold_pct_fall: opt<f64>`, `output_threshold_pct_rise: opt<f64>`,
//!   `output_threshold_pct_fall: opt<f64>`, `slew_lower_threshold_pct_rise: opt<f64>`,
//!   `slew_upper_threshold_pct_rise: opt<f64>`, `slew_lower_threshold_pct_fall: opt<f64>`,
//!   `slew_upper_threshold_pct_fall: opt<f64>`, `slew_derate_from_library: opt<f64>`,
//!   `wire_loads: vec<(str_ref, WireLoad)>` (strictly ascending key order),
//!   `wire_load_tables: vec<(str_ref, WireLoadTable)>` (strictly ascending key order),
//!   `wire_load_selections: vec<(str_ref, WireLoadSelection)>` (strictly ascending key order),
//!   `default_wire_load: opt<str_ref>`, `default_wire_load_mode: opt<WireLoadMode>`,
//!   `default_wire_load_selection: opt<str_ref>`, `default_wire_load_area: opt<f64>`,
//!   `default_wire_load_capacitance: opt<f64>`, `default_wire_load_resistance: opt<f64>`,
//!   `operating_conditions: vec<(str_ref, OperatingConditions)>` (strictly ascending key order),
//!   `default_operating_conditions: opt<str_ref>`, `nom_process: opt<f64>`,
//!   `nom_temperature: opt<f64>`, `nom_voltage: opt<f64>`.
//! - `Units`: six `Unit` records: `time`, `capacitance`, `resistance`, `voltage`, `current`,
//!   `leakage_power`.
//! - `Unit`: `scale: f64`, `given: bool`, `text: opt<str_ref>`.
//! - `Template`: `variable_1: opt<str_ref>`, `variable_2: opt<str_ref>`,
//!   `variable_3: opt<str_ref>`, `index_1: f64_slice`, `index_2: f64_slice`,
//!   `index_3: f64_slice`.
//! - `Table`: `template: str_ref`, `variable_1: opt<str_ref>`, `variable_2: opt<str_ref>`,
//!   `variable_3: opt<str_ref>`, `index_1: f64_slice`, `index_2: f64_slice`,
//!   `index_3: f64_slice`, `values: f64_slice`.
//! - `Func`: `u8` discriminant (`0` = `Const(false)`, `1` = `Const(true)`,
//!   `2` = `Var(str_ref)`, `3` = `Not(Func)`, `4` = `And(Func, Func)`, `5` = `Or(Func, Func)`,
//!   `6` = `Xor(Func, Func)`).
//! - `Direction`: `u8` (`0` = `Input`, `1` = `Output`, `2` = `Inout`, `3` = `Internal`).
//! - `SeqKind`: `u8` (`0` = `FlipFlop`, `1` = `Latch`).
//! - `PgType`, `LevelShifterType`, `SwitchCellType`, `AntennaDiodeType`, `WireLoadMode`,
//!   `TreeType`: `u8` tag (`0` = `from_name(str_ref)`, `1` = explicit `Other(str_ref)`).
//! - `WireLoad`: `name: str_ref`, `resistance: f64`, `capacitance: f64`, `area: f64`,
//!   `slope: f64`, `fanout_length: vec<(u32, f64)>`.
//! - `WireLoadTable`: `name: str_ref`, `fanout_length: vec<(u32, f64)>`,
//!   `fanout_capacitance: vec<(u32, f64)>`, `fanout_resistance: vec<(u32, f64)>`,
//!   `fanout_area: vec<(u32, f64)>`.
//! - `WireLoadAreaRule`: `min_area: f64`, `max_area: f64`, `wire_load: str_ref`.
//! - `WireLoadSelection`: `name: str_ref`, `rules: vec<WireLoadAreaRule>`.
//! - `OperatingConditions`: `name: str_ref`, `process: opt<f64>`, `temperature: opt<f64>`,
//!   `voltage: opt<f64>`, `tree_type: opt<TreeType>`.
//! - `TimingArc`: `related_pin: str_ref`, `timing_sense: opt<str_ref>`, `timing_type: str_ref`,
//!   `cell_rise: opt<Table>`, `cell_fall: opt<Table>`, `rise_transition: opt<Table>`,
//!   `fall_transition: opt<Table>`, `rise_constraint: opt<Table>`, `fall_constraint: opt<Table>`,
//!   `when_text: opt<str_ref>`, `when: opt<Func>`, `sdf_cond: opt<str_ref>`, `line: u32`.
//! - `InternalPower`: `related_pin: opt<str_ref>`, `rise_power: opt<Table>`,
//!   `fall_power: opt<Table>`.
//! - `Pin`: `name: str_ref`, `direction: Direction`, `function_text: opt<str_ref>`,
//!   `function: opt<Func>`, `three_state: opt<str_ref>`, `three_state_function: opt<Func>`,
//!   `capacitance: f64`, `capacitance_given: bool`, `rise_capacitance: opt<f64>`,
//!   `fall_capacitance: opt<f64>`, `rise_capacitance_range: opt<(f64, f64)>`,
//!   `fall_capacitance_range: opt<(f64, f64)>`, `min_capacitance: opt<f64>`,
//!   `max_capacitance: opt<f64>`, `max_transition: opt<f64>`, `max_fanout: opt<f64>`,
//!   `fanout_load: opt<f64>`, `is_clock: bool`, `arcs: vec<TimingArc>`,
//!   `internal_power: vec<InternalPower>`, `isolation_cell_enable_pin: bool`,
//!   `isolation_cell_data_pin: bool`, `level_shifter_enable_pin: bool`,
//!   `level_shifter_data_pin: bool`, `always_on: bool`, `related_power_pin: opt<str_ref>`,
//!   `related_ground_pin: opt<str_ref>`, `power_down_function: opt<str_ref>`,
//!   `state_function_text: opt<str_ref>`, `state_function: opt<Func>`,
//!   `internal_node: opt<str_ref>`, `clock_gate_clock_pin: bool`,
//!   `clock_gate_enable_pin: bool`, `clock_gate_out_pin: bool`, `clock_gate_test_pin: bool`,
//!   `clock_gate_obs_pin: bool`, `line: u32`, `min_pulse_width_high: opt<f64>`,
//!   `min_pulse_width_low: opt<f64>`, `min_period: opt<f64>` (the last three since version 4,
//!   `WS-REQ-0117`).
//! - `Sequential`: `kind: SeqKind`, `state: str_ref`, `state_n: str_ref`,
//!   `data_text: str_ref`, `data: Func`, `clock_text: str_ref`, `clock: Func`,
//!   `clocked_on_also: opt<str_ref>`, `clear_text: opt<str_ref>`, `clear: opt<Func>`,
//!   `preset_text: opt<str_ref>`, `preset: opt<Func>`, `clear_preset_var1: opt<str_ref>`,
//!   `clear_preset_var2: opt<str_ref>`.
//! - `Statetable`: `input_nodes: vec<str_ref>`, `internal_nodes: vec<str_ref>`,
//!   `table: str_ref`, `rows: vec<str_ref>`.
//! - `PgPin`: `name: str_ref`, `pg_type: opt<PgType>`, `voltage_name: opt<str_ref>`.
//! - `Cell`: `name: str_ref`, `area: f64`, `dont_use: bool`, `pins: vec<Pin>`,
//!   `pg_pins: vec<str_ref>`, `sequentials: vec<Sequential>`, `has_statetable: bool`,
//!   `has_test_cell: bool`, `is_clock_gating: bool`, `has_bus: bool`,
//!   `leakage_power: opt<f64>`, `problems: vec<str_ref>`, `pg_pin_groups: vec<PgPin>`,
//!   `is_isolation_cell: bool`, `is_level_shifter: bool`,
//!   `level_shifter_type: opt<LevelShifterType>`, `always_on: bool`,
//!   `cell_footprint: opt<str_ref>`, `dont_touch: bool`, `is_macro_cell: bool`,
//!   `pad_cell: bool`, `switch_cell_type: opt<SwitchCellType>`, `retention_cell: opt<str_ref>`,
//!   `antenna_diode_type: opt<AntennaDiodeType>`,
//!   `clock_gating_integrated_cell: opt<str_ref>`, `statetable: opt<Statetable>`, `line: u32`.

mod decode;

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::Path;

use rusteda_core::liberty::{
    Cell, Direction, Func, InternalPower, Library, OperatingConditions, PgPin, Pin, SeqKind,
    Sequential, Statetable, Table, Template, TimingArc, Unit, Units, WireLoad, WireLoadAreaRule,
    WireLoadSelection, WireLoadTable,
};

use crate::cache::{CachedLibrary, PutError};
use crate::error::{Corruption, Error, Result};
use crate::hash::Hash64;
use crate::rbin::{Rbin, RbinWriter};
use crate::schema::LibraryEntry;
use crate::workspace::{Revision, Workspace};

/// The `rbin` container kind for compiled Liberty libraries (`WS-REQ-0110`).
pub const KIND: &str = "LIBERTY";

/// The current `LIBERTY` format version (`WS-REQ-0110`, `WS-REQ-0117`).
pub const FORMAT_VERSION: u32 = 4;

/// The range of `LIBERTY` format versions supported by this build (`WS-REQ-0110`).
pub const VERSIONS: RangeInclusive<u32> = FORMAT_VERSION..=FORMAT_VERSION;

/// Section ID of the UTF-8 string pool (`elem_size = 1`, `WS-REQ-0110`).
pub const SEC_STR: u32 = 1;

/// Section ID of the model byte stream (`elem_size = 1`, `WS-REQ-0110`).
pub const SEC_MODEL: u32 = 2;

/// Section ID of the `f64` table value pool (`elem_size = 8`, `WS-REQ-0110`).
pub const SEC_F64: u32 = 3;

/// Maximum recursion depth for encoded [`Func`] expressions (`WS-REQ-0114`).
pub(super) const MAX_FUNC_DEPTH: usize = 256;

/// Encodes `lib` into a deterministic `LIBERTY` `.rlib` byte vector without an upstream
/// header entry (`WS-REQ-0110`, `WS-REQ-0112`, `WS-REQ-0113`).
pub fn encode(lib: &Library) -> Result<Vec<u8>> {
    encode_inner(lib, None)
}

/// Encodes `lib` into a deterministic `LIBERTY` `.rlib` byte vector recording `source_hash`
/// under upstream `"lib"` in the `rbin` header (`WS-REQ-0110`, `WS-REQ-0112`, `WS-REQ-0113`).
pub fn encode_with_source(lib: &Library, source_hash: Hash64) -> Result<Vec<u8>> {
    encode_inner(lib, Some(source_hash))
}

fn encode_inner(lib: &Library, source_hash: Option<Hash64>) -> Result<Vec<u8>> {
    let mut enc = Encoder::default();
    enc.write_library(lib)?;

    let mut writer = RbinWriter::new(KIND, FORMAT_VERSION)?;
    if let Some(hash) = source_hash {
        writer.upstream("lib", hash)?;
    }
    writer.section_bytes(SEC_STR, 1, enc.str_bytes)?;
    writer.section_bytes(SEC_MODEL, 1, enc.model)?;
    writer.section(SEC_F64, &enc.f64_bits)?;
    Ok(writer.to_bytes())
}

/// Validates and decodes a `LIBERTY` `.rlib` byte slice into a [`Library`] (`WS-REQ-0113`,
/// `WS-REQ-0114`).
pub fn decode(bytes: &[u8]) -> Result<Library> {
    let rbin = Rbin::from_bytes(bytes.to_vec(), KIND, VERSIONS)?;
    rbin.verify_hash()?;
    decode_rbin(&rbin)
}

/// Decodes a [`Library`] from an opened [`Rbin`] container (`WS-REQ-0113`, `WS-REQ-0114`).
pub fn decode_rbin(rbin: &Rbin) -> Result<Library> {
    let path = rbin.path();
    if rbin.kind() != KIND {
        return Err(Error::corrupt(
            path,
            Corruption::WrongKind {
                expected: KIND.to_string(),
                found: rbin.kind().to_string(),
            },
        ));
    }
    let fv = rbin.format_version();
    if fv > *VERSIONS.end() {
        return Err(Error::SchemaTooNew {
            path: path.to_path_buf(),
            found: format!("{KIND} v{fv}"),
            supported: format!("{KIND} v{}", VERSIONS.end()),
        });
    }
    if !VERSIONS.contains(&fv) {
        return Err(Error::corrupt(
            path,
            Corruption::UnsupportedVersion {
                found: fv,
                min: *VERSIONS.start(),
                max: *VERSIONS.end(),
            },
        ));
    }

    let str_bytes: &[u8] = rbin.section(SEC_STR)?;
    let model_bytes: &[u8] = rbin.section(SEC_MODEL)?;
    let f64_bits: &[u64] = rbin.section(SEC_F64)?;

    let str_pool = std::str::from_utf8(str_bytes)
        .map_err(|e| corrupt_rlib(path, format!("STR section is not valid UTF-8: {e}")))?;

    decode::decode_model(path, str_pool, model_bytes, f64_bits)
}

/// Compiles and stores `lib` and its verbatim `lib_bytes` in `ws`'s library cache
/// (`cache/<name>-<h16>.lib` and `cache/<name>-<h16>.rlib`, `WS-REQ-0115`).
pub fn put(ws: &Workspace, lib: &Library, lib_bytes: &[u8]) -> Result<CachedLibrary> {
    ws.cache()
        .put_library(&lib.name, lib_bytes, KIND, VERSIONS, |raw| {
            encode_with_source(lib, Hash64::of(raw))
        })
        .map_err(|e| match e {
            PutError::Compile(err) | PutError::Workspace(err) => err,
        })
}

/// Returns the [`LibraryEntry`] items of `rev` belonging to `corner`, in revision order
/// (`WS-REQ-0116`).
pub fn libraries_of_corner<'a>(rev: &'a Revision, corner: &str) -> Vec<&'a LibraryEntry> {
    rev.meta()
        .libraries
        .iter()
        .filter(|l| l.corner == corner)
        .collect()
}

/// Finds the [`LibraryEntry`] of `rev` with the given `name` and `corner` (`WS-REQ-0116`).
pub fn find_library<'a>(rev: &'a Revision, name: &str, corner: &str) -> Result<&'a LibraryEntry> {
    rev.meta()
        .libraries
        .iter()
        .find(|l| l.name == name && l.corner == corner)
        .ok_or_else(|| Error::MissingTable {
            rev: rev.rev(),
            table: format!("library {name} ({corner})"),
        })
}

/// Maps and decodes `entry` from the workspace cache of `rev` (`WS-REQ-0116`).
pub fn load_entry(rev: &Revision, entry: &LibraryEntry) -> Result<Library> {
    let rbin = rev.open_library(entry, KIND, VERSIONS)?;
    decode_rbin(&rbin)
}

/// Maps and decodes the library `(name, corner)` of `rev` (`WS-REQ-0116`).
pub fn load(_ws: &Workspace, rev: &Revision, name: &str, corner: &str) -> Result<Library> {
    let entry = find_library(rev, name, corner)?;
    load_entry(rev, entry)
}

/// Maps and decodes the library `(name, corner)` of `rev`, transparently recompiling its
/// `.rlib` from the cached `.lib` via `parse` when the `.rlib` is missing, stale, or has an
/// older `format_version` (`WS-REQ-0084`, `WS-REQ-0116`). Returns `(library, recompiled)`.
pub fn load_or_recompile<E>(
    ws: &Workspace,
    rev: &Revision,
    name: &str,
    corner: &str,
    parse: impl FnOnce(&[u8]) -> std::result::Result<Library, E>,
) -> std::result::Result<(Library, bool), PutError<E>> {
    let entry = find_library(rev, name, corner).map_err(PutError::Workspace)?;
    let cache = ws.cache();
    let mut recompiled = false;
    let rbin = cache.open_rlib_or_recompile(&entry.name, entry.hash, KIND, VERSIONS, |raw| {
        recompiled = true;
        let parsed = parse(raw).map_err(PutError::Compile)?;
        encode_with_source(&parsed, entry.hash).map_err(PutError::Workspace)
    });
    let rbin = match rbin {
        Ok(r) => r,
        Err(PutError::Compile(inner)) => return Err(inner),
        Err(PutError::Workspace(err)) => return Err(PutError::Workspace(err)),
    };
    let lib = decode_rbin(&rbin).map_err(PutError::Workspace)?;
    Ok((lib, recompiled))
}

pub(super) fn corrupt_rlib(path: &Path, reason: impl Into<String>) -> Error {
    Error::corrupt(
        path,
        Corruption::Json {
            reason: reason.into(),
        },
    )
}

#[derive(Default)]
struct Encoder {
    str_bytes: Vec<u8>,
    str_index: BTreeMap<String, (u32, u32)>,
    model: Vec<u8>,
    f64_bits: Vec<u64>,
}

impl Encoder {
    fn intern_str(&mut self, s: &str) -> Result<(u32, u32)> {
        if let Some(&span) = self.str_index.get(s) {
            return Ok(span);
        }
        let offset = u32::try_from(self.str_bytes.len())
            .map_err(|_| Error::internal("rlib STR section exceeds u32::MAX bytes"))?;
        let len = u32::try_from(s.len())
            .map_err(|_| Error::internal("rlib string exceeds u32::MAX bytes"))?;
        self.str_bytes.extend_from_slice(s.as_bytes());
        let span = (offset, len);
        self.str_index.insert(s.to_string(), span);
        Ok(span)
    }

    fn write_u8(&mut self, v: u8) {
        self.model.push(v);
    }

    fn write_bool(&mut self, v: bool) {
        self.write_u8(u8::from(v));
    }

    fn write_u32(&mut self, v: u32) {
        self.model.extend_from_slice(&v.to_le_bytes());
    }

    fn write_usize_u32(&mut self, v: usize) -> Result<()> {
        let n = u32::try_from(v).map_err(|_| Error::internal("rlib count exceeds u32::MAX"))?;
        self.write_u32(n);
        Ok(())
    }

    fn write_f64(&mut self, v: f64) {
        self.model.extend_from_slice(&v.to_bits().to_le_bytes());
    }

    fn write_opt_f64(&mut self, v: Option<f64>) {
        match v {
            None => self.write_u8(0),
            Some(x) => {
                self.write_u8(1);
                self.write_f64(x);
            }
        }
    }

    fn write_opt_f64_pair(&mut self, v: Option<(f64, f64)>) {
        match v {
            None => self.write_u8(0),
            Some((a, b)) => {
                self.write_u8(1);
                self.write_f64(a);
                self.write_f64(b);
            }
        }
    }

    fn write_str(&mut self, s: &str) -> Result<()> {
        let (off, len) = self.intern_str(s)?;
        self.write_u32(off);
        self.write_u32(len);
        Ok(())
    }

    fn write_opt_str(&mut self, s: Option<&str>) -> Result<()> {
        match s {
            None => self.write_u8(0),
            Some(text) => {
                self.write_u8(1);
                self.write_str(text)?;
            }
        }
        Ok(())
    }

    fn write_f64_slice(&mut self, slice: &[f64]) -> Result<()> {
        let start = u32::try_from(self.f64_bits.len())
            .map_err(|_| Error::internal("rlib F64 section exceeds u32::MAX elements"))?;
        let len = u32::try_from(slice.len())
            .map_err(|_| Error::internal("rlib f64 slice exceeds u32::MAX elements"))?;
        self.f64_bits
            .extend(slice.iter().map(|v| v.to_bits().to_le()));
        self.write_u32(start);
        self.write_u32(len);
        Ok(())
    }

    fn write_unit(&mut self, u: &Unit) -> Result<()> {
        self.write_f64(u.scale);
        self.write_bool(u.given);
        self.write_opt_str(u.text.as_deref())
    }

    fn write_units(&mut self, u: &Units) -> Result<()> {
        self.write_unit(&u.time)?;
        self.write_unit(&u.capacitance)?;
        self.write_unit(&u.resistance)?;
        self.write_unit(&u.voltage)?;
        self.write_unit(&u.current)?;
        self.write_unit(&u.leakage_power)
    }

    fn write_template(&mut self, t: &Template) -> Result<()> {
        self.write_opt_str(t.variable_1.as_deref())?;
        self.write_opt_str(t.variable_2.as_deref())?;
        self.write_opt_str(t.variable_3.as_deref())?;
        self.write_f64_slice(&t.index_1)?;
        self.write_f64_slice(&t.index_2)?;
        self.write_f64_slice(&t.index_3)
    }

    fn write_table(&mut self, t: &Table) -> Result<()> {
        self.write_str(&t.template)?;
        self.write_opt_str(t.variable_1.as_deref())?;
        self.write_opt_str(t.variable_2.as_deref())?;
        self.write_opt_str(t.variable_3.as_deref())?;
        self.write_f64_slice(&t.index_1)?;
        self.write_f64_slice(&t.index_2)?;
        self.write_f64_slice(&t.index_3)?;
        self.write_f64_slice(&t.values)
    }

    fn write_opt_table(&mut self, t: Option<&Table>) -> Result<()> {
        match t {
            None => self.write_u8(0),
            Some(tbl) => {
                self.write_u8(1);
                self.write_table(tbl)?;
            }
        }
        Ok(())
    }

    fn write_func(&mut self, f: &Func, depth: usize) -> Result<()> {
        if depth > MAX_FUNC_DEPTH {
            return Err(Error::internal("rlib Func expression exceeds max depth"));
        }
        match f {
            Func::Const(false) => self.write_u8(0),
            Func::Const(true) => self.write_u8(1),
            Func::Var(v) => {
                self.write_u8(2);
                self.write_str(v)?;
            }
            Func::Not(a) => {
                self.write_u8(3);
                self.write_func(a, depth + 1)?;
            }
            Func::And(a, b) => {
                self.write_u8(4);
                self.write_func(a, depth + 1)?;
                self.write_func(b, depth + 1)?;
            }
            Func::Or(a, b) => {
                self.write_u8(5);
                self.write_func(a, depth + 1)?;
                self.write_func(b, depth + 1)?;
            }
            Func::Xor(a, b) => {
                self.write_u8(6);
                self.write_func(a, depth + 1)?;
                self.write_func(b, depth + 1)?;
            }
        }
        Ok(())
    }

    fn write_opt_func(&mut self, f: Option<&Func>) -> Result<()> {
        match f {
            None => self.write_u8(0),
            Some(func) => {
                self.write_u8(1);
                self.write_func(func, 0)?;
            }
        }
        Ok(())
    }

    fn write_named_enum(&mut self, is_known: bool, name: &str) -> Result<()> {
        self.write_u8(if is_known { 0 } else { 1 });
        self.write_str(name)
    }

    fn write_arc(&mut self, a: &TimingArc) -> Result<()> {
        self.write_str(&a.related_pin)?;
        self.write_opt_str(a.timing_sense.as_deref())?;
        self.write_str(&a.timing_type)?;
        self.write_opt_table(a.cell_rise.as_ref())?;
        self.write_opt_table(a.cell_fall.as_ref())?;
        self.write_opt_table(a.rise_transition.as_ref())?;
        self.write_opt_table(a.fall_transition.as_ref())?;
        self.write_opt_table(a.rise_constraint.as_ref())?;
        self.write_opt_table(a.fall_constraint.as_ref())?;
        self.write_opt_str(a.when_text.as_deref())?;
        self.write_opt_func(a.when.as_ref())?;
        self.write_opt_str(a.sdf_cond.as_deref())?;
        self.write_u32(a.line);
        Ok(())
    }

    fn write_internal_power(&mut self, ip: &InternalPower) -> Result<()> {
        self.write_opt_str(ip.related_pin.as_deref())?;
        self.write_opt_table(ip.rise_power.as_ref())?;
        self.write_opt_table(ip.fall_power.as_ref())
    }

    fn write_pin(&mut self, p: &Pin) -> Result<()> {
        self.write_str(&p.name)?;
        let dir_tag = match p.direction {
            Direction::Input => 0,
            Direction::Output => 1,
            Direction::Inout => 2,
            Direction::Internal => 3,
        };
        self.write_u8(dir_tag);
        self.write_opt_str(p.function_text.as_deref())?;
        self.write_opt_func(p.function.as_ref())?;
        self.write_opt_str(p.three_state.as_deref())?;
        self.write_opt_func(p.three_state_function.as_ref())?;
        self.write_f64(p.capacitance);
        self.write_bool(p.capacitance_given);
        self.write_opt_f64(p.rise_capacitance);
        self.write_opt_f64(p.fall_capacitance);
        self.write_opt_f64_pair(p.rise_capacitance_range);
        self.write_opt_f64_pair(p.fall_capacitance_range);
        self.write_opt_f64(p.min_capacitance);
        self.write_opt_f64(p.max_capacitance);
        self.write_opt_f64(p.max_transition);
        self.write_opt_f64(p.max_fanout);
        self.write_opt_f64(p.fanout_load);
        self.write_bool(p.is_clock);
        self.write_usize_u32(p.arcs.len())?;
        for arc in &p.arcs {
            self.write_arc(arc)?;
        }
        self.write_usize_u32(p.internal_power.len())?;
        for ip in &p.internal_power {
            self.write_internal_power(ip)?;
        }
        self.write_bool(p.isolation_cell_enable_pin);
        self.write_bool(p.isolation_cell_data_pin);
        self.write_bool(p.level_shifter_enable_pin);
        self.write_bool(p.level_shifter_data_pin);
        self.write_bool(p.always_on);
        self.write_opt_str(p.related_power_pin.as_deref())?;
        self.write_opt_str(p.related_ground_pin.as_deref())?;
        self.write_opt_str(p.power_down_function.as_deref())?;
        self.write_opt_str(p.state_function_text.as_deref())?;
        self.write_opt_func(p.state_function.as_ref())?;
        self.write_opt_str(p.internal_node.as_deref())?;
        self.write_bool(p.clock_gate_clock_pin);
        self.write_bool(p.clock_gate_enable_pin);
        self.write_bool(p.clock_gate_out_pin);
        self.write_bool(p.clock_gate_test_pin);
        self.write_bool(p.clock_gate_obs_pin);
        self.write_u32(p.line);
        self.write_opt_f64(p.min_pulse_width_high);
        self.write_opt_f64(p.min_pulse_width_low);
        self.write_opt_f64(p.min_period);
        Ok(())
    }

    fn write_sequential(&mut self, s: &Sequential) -> Result<()> {
        let kind_tag = match s.kind {
            SeqKind::FlipFlop => 0,
            SeqKind::Latch => 1,
        };
        self.write_u8(kind_tag);
        self.write_str(&s.state)?;
        self.write_str(&s.state_n)?;
        self.write_str(&s.data_text)?;
        self.write_func(&s.data, 0)?;
        self.write_str(&s.clock_text)?;
        self.write_func(&s.clock, 0)?;
        self.write_opt_str(s.clocked_on_also.as_deref())?;
        self.write_opt_str(s.clear_text.as_deref())?;
        self.write_opt_func(s.clear.as_ref())?;
        self.write_opt_str(s.preset_text.as_deref())?;
        self.write_opt_func(s.preset.as_ref())?;
        self.write_opt_str(s.clear_preset_var1.as_deref())?;
        self.write_opt_str(s.clear_preset_var2.as_deref())
    }

    fn write_statetable(&mut self, st: &Statetable) -> Result<()> {
        self.write_usize_u32(st.input_nodes.len())?;
        for n in &st.input_nodes {
            self.write_str(n)?;
        }
        self.write_usize_u32(st.internal_nodes.len())?;
        for n in &st.internal_nodes {
            self.write_str(n)?;
        }
        self.write_str(&st.table)?;
        self.write_usize_u32(st.rows.len())?;
        for r in &st.rows {
            self.write_str(r)?;
        }
        Ok(())
    }

    fn write_pg_pin(&mut self, pg: &PgPin) -> Result<()> {
        self.write_str(&pg.name)?;
        match &pg.pg_type {
            None => self.write_u8(0),
            Some(t) => {
                self.write_u8(1);
                self.write_named_enum(t.is_known(), t.name())?;
            }
        }
        self.write_opt_str(pg.voltage_name.as_deref())
    }

    fn write_cell(&mut self, c: &Cell) -> Result<()> {
        self.write_str(&c.name)?;
        self.write_f64(c.area);
        self.write_bool(c.dont_use);
        self.write_usize_u32(c.pins.len())?;
        for pin in &c.pins {
            self.write_pin(pin)?;
        }
        self.write_usize_u32(c.pg_pins.len())?;
        for pg in &c.pg_pins {
            self.write_str(pg)?;
        }
        self.write_usize_u32(c.sequentials.len())?;
        for seq in &c.sequentials {
            self.write_sequential(seq)?;
        }
        self.write_bool(c.has_statetable);
        self.write_bool(c.has_test_cell);
        self.write_bool(c.is_clock_gating);
        self.write_bool(c.has_bus);
        self.write_opt_f64(c.leakage_power);
        self.write_usize_u32(c.problems.len())?;
        for prob in &c.problems {
            self.write_str(prob)?;
        }
        self.write_usize_u32(c.pg_pin_groups.len())?;
        for pg in &c.pg_pin_groups {
            self.write_pg_pin(pg)?;
        }
        self.write_bool(c.is_isolation_cell);
        self.write_bool(c.is_level_shifter);
        match &c.level_shifter_type {
            None => self.write_u8(0),
            Some(t) => {
                self.write_u8(1);
                self.write_named_enum(t.is_known(), t.name())?;
            }
        }
        self.write_bool(c.always_on);
        self.write_opt_str(c.cell_footprint.as_deref())?;
        self.write_bool(c.dont_touch);
        self.write_bool(c.is_macro_cell);
        self.write_bool(c.pad_cell);
        match &c.switch_cell_type {
            None => self.write_u8(0),
            Some(t) => {
                self.write_u8(1);
                self.write_named_enum(t.is_known(), t.name())?;
            }
        }
        self.write_opt_str(c.retention_cell.as_deref())?;
        match &c.antenna_diode_type {
            None => self.write_u8(0),
            Some(t) => {
                self.write_u8(1);
                self.write_named_enum(t.is_known(), t.name())?;
            }
        }
        self.write_opt_str(c.clock_gating_integrated_cell.as_deref())?;
        match &c.statetable {
            None => self.write_u8(0),
            Some(st) => {
                self.write_u8(1);
                self.write_statetable(st)?;
            }
        }
        self.write_u32(c.line);
        Ok(())
    }

    fn write_fanout_pairs(&mut self, pairs: &[(u32, f64)]) -> Result<()> {
        self.write_usize_u32(pairs.len())?;
        for &(fanout, val) in pairs {
            self.write_u32(fanout);
            self.write_f64(val);
        }
        Ok(())
    }

    fn write_wire_load(&mut self, wl: &WireLoad) -> Result<()> {
        self.write_str(&wl.name)?;
        self.write_f64(wl.resistance);
        self.write_f64(wl.capacitance);
        self.write_f64(wl.area);
        self.write_f64(wl.slope);
        self.write_fanout_pairs(&wl.fanout_length)
    }

    fn write_wire_load_table(&mut self, wlt: &WireLoadTable) -> Result<()> {
        self.write_str(&wlt.name)?;
        self.write_fanout_pairs(&wlt.fanout_length)?;
        self.write_fanout_pairs(&wlt.fanout_capacitance)?;
        self.write_fanout_pairs(&wlt.fanout_resistance)?;
        self.write_fanout_pairs(&wlt.fanout_area)
    }

    fn write_wire_load_rule(&mut self, r: &WireLoadAreaRule) -> Result<()> {
        self.write_f64(r.min_area);
        self.write_f64(r.max_area);
        self.write_str(&r.wire_load)
    }

    fn write_wire_load_selection(&mut self, sel: &WireLoadSelection) -> Result<()> {
        self.write_str(&sel.name)?;
        self.write_usize_u32(sel.rules.len())?;
        for rule in &sel.rules {
            self.write_wire_load_rule(rule)?;
        }
        Ok(())
    }

    fn write_operating_conditions(&mut self, oc: &OperatingConditions) -> Result<()> {
        self.write_str(&oc.name)?;
        self.write_opt_f64(oc.process);
        self.write_opt_f64(oc.temperature);
        self.write_opt_f64(oc.voltage);
        match &oc.tree_type {
            None => self.write_u8(0),
            Some(t) => {
                self.write_u8(1);
                self.write_named_enum(t.is_known(), t.name())?;
            }
        }
        Ok(())
    }

    fn write_library(&mut self, lib: &Library) -> Result<()> {
        self.write_str(&lib.name)?;
        self.write_f64(lib.time_unit_ns);
        self.write_f64(lib.cap_unit_pf);
        self.write_opt_f64(lib.default_max_capacitance);
        self.write_opt_f64(lib.default_max_transition);
        self.write_opt_f64(lib.default_max_fanout);
        self.write_f64(lib.default_fanout_load);
        self.write_opt_f64(lib.default_input_pin_cap);
        self.write_opt_f64(lib.default_output_pin_cap);
        self.write_opt_f64(lib.default_inout_pin_cap);
        self.write_units(&lib.units)?;
        self.write_usize_u32(lib.templates.len())?;
        for (name, tmpl) in &lib.templates {
            self.write_str(name)?;
            self.write_template(tmpl)?;
        }
        self.write_usize_u32(lib.cells.len())?;
        for cell in &lib.cells {
            self.write_cell(cell)?;
        }
        self.write_opt_f64(lib.input_threshold_pct_rise);
        self.write_opt_f64(lib.input_threshold_pct_fall);
        self.write_opt_f64(lib.output_threshold_pct_rise);
        self.write_opt_f64(lib.output_threshold_pct_fall);
        self.write_opt_f64(lib.slew_lower_threshold_pct_rise);
        self.write_opt_f64(lib.slew_upper_threshold_pct_rise);
        self.write_opt_f64(lib.slew_lower_threshold_pct_fall);
        self.write_opt_f64(lib.slew_upper_threshold_pct_fall);
        self.write_opt_f64(lib.slew_derate_from_library);
        self.write_usize_u32(lib.wire_loads.len())?;
        for (name, wl) in &lib.wire_loads {
            self.write_str(name)?;
            self.write_wire_load(wl)?;
        }
        self.write_usize_u32(lib.wire_load_tables.len())?;
        for (name, wlt) in &lib.wire_load_tables {
            self.write_str(name)?;
            self.write_wire_load_table(wlt)?;
        }
        self.write_usize_u32(lib.wire_load_selections.len())?;
        for (name, sel) in &lib.wire_load_selections {
            self.write_str(name)?;
            self.write_wire_load_selection(sel)?;
        }
        self.write_opt_str(lib.default_wire_load.as_deref())?;
        match &lib.default_wire_load_mode {
            None => self.write_u8(0),
            Some(m) => {
                self.write_u8(1);
                self.write_named_enum(m.is_known(), m.name())?;
            }
        }
        self.write_opt_str(lib.default_wire_load_selection.as_deref())?;
        self.write_opt_f64(lib.default_wire_load_area);
        self.write_opt_f64(lib.default_wire_load_capacitance);
        self.write_opt_f64(lib.default_wire_load_resistance);
        self.write_usize_u32(lib.operating_conditions.len())?;
        for (name, oc) in &lib.operating_conditions {
            self.write_str(name)?;
            self.write_operating_conditions(oc)?;
        }
        self.write_opt_str(lib.default_operating_conditions.as_deref())?;
        self.write_opt_f64(lib.nom_process);
        self.write_opt_f64(lib.nom_temperature);
        self.write_opt_f64(lib.nom_voltage);
        Ok(())
    }
}
