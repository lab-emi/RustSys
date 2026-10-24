// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Tests for the `LIBERTY` (`.rlib`) codec and cache loader (`WS-REQ-0110` to `WS-REQ-0117`).

mod common;

use std::collections::BTreeMap;
use std::fs;

use rusteda_core::liberty::{
    AntennaDiodeType, Cell, Direction, Func, InternalPower, LevelShifterType, Library,
    OperatingConditions, PgPin, PgType, Pin, SeqKind, Sequential, Statetable, SwitchCellType,
    Table, Template, TimingArc, TreeType, Unit, Units, WireLoad, WireLoadAreaRule, WireLoadMode,
    WireLoadSelection, WireLoadTable,
};
use rusteda_workspace::rlib::{self, FORMAT_VERSION, KIND, SEC_F64, SEC_MODEL, SEC_STR, VERSIONS};
use rusteda_workspace::{ErrorCode, Hash64, Rbin, RbinWriter, Rev, Workspace};

use common::{inv, temp_dir};

fn sample_table(template: &str, v1: Option<&str>, v2: Option<&str>, v3: Option<&str>) -> Table {
    Table {
        template: template.to_string(),
        variable_1: v1.map(str::to_string),
        variable_2: v2.map(str::to_string),
        variable_3: v3.map(str::to_string),
        index_1: vec![0.01, 0.1, 0.5],
        index_2: vec![0.002, 0.02],
        index_3: if v3.is_some() { vec![1.0, 2.0] } else { vec![] },
        values: if v3.is_some() {
            (0..12).map(|i| f64::from(i) * 0.125).collect()
        } else {
            vec![0.05, 0.08, 0.12, 0.18, 0.25, 0.42]
        },
    }
}

fn rich_library() -> Library {
    let mut units = Units::default();
    units.time = Unit::given(1e-3, "1ps");
    units.capacitance = Unit::given(1e-3, "1,ff");
    units.resistance = Unit::defaulted(1.0);
    units.voltage = Unit::given(1.0, "1V");
    units.current = Unit::given(1.0, "1mA");
    units.leakage_power = Unit::given(1e-3, "1pW");

    let mut templates = BTreeMap::new();
    templates.insert(
        "delay_2d".to_string(),
        Template {
            variable_1: Some("input_net_transition".to_string()),
            variable_2: Some("total_output_net_capacitance".to_string()),
            index_1: vec![0.01, 0.1, 0.5],
            index_2: vec![0.002, 0.02],
            variable_3: None,
            index_3: Vec::new(),
        },
    );
    templates.insert(
        "delay_3d".to_string(),
        Template {
            variable_1: Some("input_net_transition".to_string()),
            variable_2: Some("total_output_net_capacitance".to_string()),
            index_1: vec![0.01, 0.1, 0.5],
            index_2: vec![0.002, 0.02],
            variable_3: Some("related_out_total_output_net_capacitance".to_string()),
            index_3: vec![1.0, 2.0],
        },
    );

    let func_expr = Func::Xor(
        Box::new(Func::And(
            Box::new(Func::Var("A".to_string())),
            Box::new(Func::Not(Box::new(Func::Var("B".to_string())))),
        )),
        Box::new(Func::Or(
            Box::new(Func::Const(false)),
            Box::new(Func::Const(true)),
        )),
    );

    let pin_a = Pin {
        name: "A".to_string(),
        direction: Direction::Input,
        function_text: None,
        function: None,
        three_state: None,
        three_state_function: None,
        capacitance: 0.0042,
        capacitance_given: true,
        rise_capacitance: Some(0.0040),
        fall_capacitance: Some(0.0044),
        rise_capacitance_range: Some((0.0038, 0.0043)),
        fall_capacitance_range: Some((0.0041, 0.0047)),
        min_capacitance: None,
        max_capacitance: None,
        max_transition: Some(0.75),
        max_fanout: None,
        fanout_load: Some(1.25),
        // Pin pulse attributes, carried since format version 4 (WS-REQ-0117).
        min_pulse_width_high: Some(0.125),
        min_pulse_width_low: Some(0.15),
        min_period: Some(0.5),
        is_clock: true,
        arcs: Vec::new(),
        internal_power: Vec::new(),
        isolation_cell_enable_pin: true,
        isolation_cell_data_pin: false,
        level_shifter_enable_pin: false,
        level_shifter_data_pin: true,
        always_on: true,
        related_power_pin: Some("VDD".to_string()),
        related_ground_pin: Some("VSS".to_string()),
        power_down_function: Some("!VDD + VSS".to_string()),
        state_function_text: None,
        state_function: None,
        internal_node: None,
        clock_gate_clock_pin: true,
        clock_gate_enable_pin: true,
        clock_gate_out_pin: false,
        clock_gate_test_pin: true,
        clock_gate_obs_pin: false,
        line: 25,
    };

    let pin_y = Pin {
        name: "Y".to_string(),
        direction: Direction::Output,
        function_text: Some("(A & !B) ^ 1".to_string()),
        function: Some(func_expr.clone()),
        three_state: Some("!EN".to_string()),
        three_state_function: Some(Func::Not(Box::new(Func::Var("EN".to_string())))),
        capacitance: 0.0,
        capacitance_given: false,
        rise_capacitance: None,
        fall_capacitance: None,
        rise_capacitance_range: None,
        fall_capacitance_range: None,
        min_capacitance: Some(0.001),
        max_capacitance: Some(0.25),
        max_transition: Some(0.5),
        max_fanout: Some(16.0),
        fanout_load: None,
        min_pulse_width_high: None,
        min_pulse_width_low: None,
        min_period: None,
        is_clock: false,
        arcs: vec![TimingArc {
            related_pin: "A".to_string(),
            timing_sense: Some("positive_unate".to_string()),
            timing_type: "combinational".to_string(),
            cell_rise: Some(sample_table(
                "delay_2d",
                Some("input_net_transition"),
                Some("total_output_net_capacitance"),
                None,
            )),
            cell_fall: Some(sample_table(
                "delay_3d",
                Some("input_net_transition"),
                Some("total_output_net_capacitance"),
                Some("related_out_total_output_net_capacitance"),
            )),
            rise_transition: Some(sample_table("delay_2d", None, None, None)),
            fall_transition: Some(sample_table("delay_2d", None, None, None)),
            rise_constraint: Some(sample_table("delay_2d", None, None, None)),
            fall_constraint: None,
            when_text: Some("A & !B".to_string()),
            when: Some(Func::And(
                Box::new(Func::Var("A".to_string())),
                Box::new(Func::Not(Box::new(Func::Var("B".to_string())))),
            )),
            sdf_cond: Some("A == 1'b1 && B == 1'b0".to_string()),
            line: 42,
        }],
        internal_power: vec![InternalPower {
            related_pin: Some("A".to_string()),
            rise_power: Some(sample_table("delay_2d", None, None, None)),
            fall_power: None,
        }],
        isolation_cell_enable_pin: false,
        isolation_cell_data_pin: true,
        level_shifter_enable_pin: true,
        level_shifter_data_pin: false,
        always_on: false,
        related_power_pin: Some("VDD".to_string()),
        related_ground_pin: Some("VSS".to_string()),
        power_down_function: None,
        state_function_text: Some("(A * M0)".to_string()),
        state_function: Some(Func::And(
            Box::new(Func::Var("A".to_string())),
            Box::new(Func::Var("M0".to_string())),
        )),
        internal_node: None,
        clock_gate_clock_pin: false,
        clock_gate_enable_pin: false,
        clock_gate_out_pin: true,
        clock_gate_test_pin: false,
        clock_gate_obs_pin: true,
        line: 40,
    };

    let pin_io = Pin {
        name: "IO".to_string(),
        direction: Direction::Inout,
        ..pin_a.clone()
    };
    let pin_int = Pin {
        name: "INT".to_string(),
        direction: Direction::Internal,
        internal_node: Some("M0".to_string()),
        ..pin_a.clone()
    };

    let mut pg_vdd = PgPin::new("VDD");
    pg_vdd.pg_type = Some(PgType::PrimaryPower);
    pg_vdd.voltage_name = Some("VDD_CORE".to_string());

    let mut pg_vss = PgPin::new("VSS");
    pg_vss.pg_type = Some(PgType::PrimaryGround);
    pg_vss.voltage_name = Some("VSS_CORE".to_string());

    let mut pg_custom = PgPin::new("VBIAS");
    pg_custom.pg_type = Some(PgType::Other("custom_bias".to_string()));
    pg_custom.voltage_name = None;

    let seq_ff = Sequential {
        kind: SeqKind::FlipFlop,
        state: "IQ".to_string(),
        state_n: "IQN".to_string(),
        data_text: "D".to_string(),
        data: Func::Var("D".to_string()),
        clock_text: "CLK".to_string(),
        clock: Func::Var("CLK".to_string()),
        clocked_on_also: Some("CLK2".to_string()),
        clear_text: Some("!RST_N".to_string()),
        clear: Some(Func::Not(Box::new(Func::Var("RST_N".to_string())))),
        preset_text: Some("!SET_N".to_string()),
        preset: Some(Func::Not(Box::new(Func::Var("SET_N".to_string())))),
        clear_preset_var1: Some("L".to_string()),
        clear_preset_var2: Some("H".to_string()),
    };

    let seq_latch = Sequential {
        kind: SeqKind::Latch,
        clocked_on_also: None,
        clear_text: None,
        clear: None,
        preset_text: None,
        preset: None,
        clear_preset_var1: None,
        clear_preset_var2: None,
        ..seq_ff.clone()
    };

    let statetable = Statetable {
        input_nodes: vec!["A".to_string(), "EN".to_string()],
        internal_nodes: vec!["M0".to_string()],
        table: "L L : - : L, L H : - : H, H - : - : N".to_string(),
        rows: vec![
            "L L : - : L".to_string(),
            "L H : - : H".to_string(),
            "H - : - : N".to_string(),
        ],
    };

    let cell1 = Cell {
        name: "RICH_CELL".to_string(),
        area: 12.5,
        dont_use: true,
        pins: vec![pin_a, pin_y, pin_io, pin_int],
        pg_pins: vec!["VDD".to_string(), "VSS".to_string(), "VBIAS".to_string()],
        sequentials: vec![seq_ff, seq_latch],
        has_statetable: true,
        has_test_cell: true,
        is_clock_gating: true,
        has_bus: true,
        leakage_power: Some(0.03125),
        problems: vec!["synthetic warning".to_string()],
        pg_pin_groups: vec![pg_vdd, pg_vss, pg_custom],
        is_isolation_cell: true,
        is_level_shifter: true,
        level_shifter_type: Some(LevelShifterType::LowToHigh),
        always_on: true,
        cell_footprint: Some("rich_fp".to_string()),
        dont_touch: true,
        is_macro_cell: true,
        pad_cell: true,
        switch_cell_type: Some(SwitchCellType::FineGrain),
        retention_cell: Some("ret_style_1".to_string()),
        antenna_diode_type: Some(AntennaDiodeType::PowerAndGround),
        clock_gating_integrated_cell: Some("latch_posedge".to_string()),
        statetable: Some(statetable),
        line: 10,
    };

    let cell2 = Cell {
        name: "OTHER_ENUMS_CELL".to_string(),
        level_shifter_type: Some(LevelShifterType::Other("custom_ls".to_string())),
        switch_cell_type: Some(SwitchCellType::Other("custom_sw".to_string())),
        antenna_diode_type: Some(AntennaDiodeType::Other("custom_ad".to_string())),
        clock_gating_integrated_cell: None,
        statetable: None,
        line: 100,
        ..cell1.clone()
    };

    let mut wire_loads = BTreeMap::new();
    wire_loads.insert(
        "Small".to_string(),
        WireLoad {
            name: "Small".to_string(),
            resistance: 0.0745,
            capacitance: 1.42e-5,
            area: 0.1,
            slope: 8.3631,
            fanout_length: vec![(1, 23.2746), (2, 32.1136), (6, 84.2649)],
        },
    );

    let mut wire_load_tables = BTreeMap::new();
    wire_load_tables.insert(
        "wlt_1".to_string(),
        WireLoadTable {
            name: "wlt_1".to_string(),
            fanout_length: vec![(1, 10.0), (2, 22.0)],
            fanout_capacitance: vec![(1, 0.01), (2, 0.025)],
            fanout_resistance: vec![(1, 0.05), (2, 0.11)],
            fanout_area: vec![(1, 1.0), (2, 2.0)],
        },
    );

    let mut wire_load_selections = BTreeMap::new();
    wire_load_selections.insert(
        "sel_area".to_string(),
        WireLoadSelection {
            name: "sel_area".to_string(),
            rules: vec![
                WireLoadAreaRule {
                    min_area: 0.0,
                    max_area: 500.0,
                    wire_load: "Small".to_string(),
                },
                WireLoadAreaRule {
                    min_area: 500.0,
                    max_area: 2000.0,
                    wire_load: "Medium".to_string(),
                },
            ],
        },
    );

    let mut operating_conditions = BTreeMap::new();
    operating_conditions.insert(
        "tt_025C_1v80".to_string(),
        OperatingConditions {
            name: "tt_025C_1v80".to_string(),
            process: Some(1.0),
            temperature: Some(25.0),
            voltage: Some(1.8),
            tree_type: Some(TreeType::BalancedTree),
        },
    );
    operating_conditions.insert(
        "custom_oc".to_string(),
        OperatingConditions {
            name: "custom_oc".to_string(),
            process: None,
            temperature: Some(-40.0),
            voltage: None,
            tree_type: Some(TreeType::Other("custom_tree".to_string())),
        },
    );

    Library {
        name: "rich_lib".to_string(),
        time_unit_ns: 1e-3,
        cap_unit_pf: 1e-3,
        default_max_capacitance: Some(0.5),
        default_max_transition: Some(1.2),
        default_max_fanout: Some(32.0),
        default_fanout_load: 1.0,
        templates,
        cells: vec![cell1, cell2],
        units,
        default_input_pin_cap: Some(0.002),
        default_output_pin_cap: Some(0.001),
        default_inout_pin_cap: Some(0.003),
        input_threshold_pct_rise: Some(50.0),
        input_threshold_pct_fall: Some(50.0),
        output_threshold_pct_rise: Some(50.0),
        output_threshold_pct_fall: Some(50.0),
        slew_lower_threshold_pct_rise: Some(20.0),
        slew_upper_threshold_pct_rise: Some(80.0),
        slew_lower_threshold_pct_fall: Some(30.0),
        slew_upper_threshold_pct_fall: Some(70.0),
        slew_derate_from_library: Some(0.5),
        wire_loads,
        wire_load_tables,
        wire_load_selections,
        default_wire_load: Some("Small".to_string()),
        default_wire_load_mode: Some(WireLoadMode::Top),
        default_wire_load_selection: Some("sel_area".to_string()),
        default_wire_load_area: Some(0.1),
        default_wire_load_capacitance: Some(1.42e-5),
        default_wire_load_resistance: Some(0.0745),
        operating_conditions,
        default_operating_conditions: Some("tt_025C_1v80".to_string()),
        nom_process: Some(1.0),
        nom_temperature: Some(25.0),
        nom_voltage: Some(1.8),
    }
}

/// WS-REQ-0110, WS-REQ-0111, WS-REQ-0112, WS-REQ-0113: Container layout, upstream header,
/// exact model round-trip (`decode(encode(lib)) == lib`), and byte-identical re-encode.
#[test]
fn round_trip_and_reproducible_bytes() {
    let lib = rich_library();
    let src_hash = Hash64::of(b"library (rich_lib) {}");
    let bytes1 = rlib::encode_with_source(&lib, src_hash).unwrap();

    // Check Rbin header and sections.
    let rbin = Rbin::from_bytes(bytes1.clone(), KIND, VERSIONS).unwrap();
    rbin.verify_hash().unwrap();
    assert_eq!(rbin.kind(), KIND);
    assert_eq!(rbin.format_version(), FORMAT_VERSION);
    assert_eq!(rbin.upstream_hash("lib"), Some(src_hash));
    assert!(rbin.section::<u8>(SEC_STR).is_ok());
    assert!(rbin.section::<u8>(SEC_MODEL).is_ok());
    assert!(rbin.section::<u64>(SEC_F64).is_ok());

    let decoded = rlib::decode(&bytes1).unwrap();
    assert_eq!(decoded, lib);

    // Re-encoding the decoded library produces byte-for-byte identical output.
    let bytes2 = rlib::encode_with_source(&decoded, src_hash).unwrap();
    assert_eq!(bytes1, bytes2);
}

/// WS-REQ-0114: Truncated, corrupted, and fuzzed `.rlib` inputs fail with `CORRUPT_TABLE`
/// (or `SCHEMA_TOO_NEW` for a newer `format_version`) and never panic.
#[test]
fn corrupted_and_truncated_inputs_return_corrupt_table_without_panic() {
    let lib = rich_library();
    let valid = rlib::encode(&lib).unwrap();

    // 1. Every truncation prefix fails cleanly with CORRUPT_TABLE.
    for len in (0..valid.len()).step_by(7) {
        let err = rlib::decode(&valid[..len]).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable, "len={len}");
    }

    // 2. Single-byte bit flips across the file fail with CORRUPT_TABLE (or decode if padding).
    for idx in (0..valid.len()).step_by(13) {
        let mut mutated = valid.clone();
        mutated[idx] ^= 0x5A;
        if let Err(err) = rlib::decode(&mutated) {
            assert!(
                matches!(
                    err.code(),
                    ErrorCode::CorruptTable | ErrorCode::SchemaTooNew
                ),
                "unexpected error code {:?} at byte {idx}",
                err.code()
            );
        }
    }

    // 3. Invalid UTF-8 in SEC_STR -> CORRUPT_TABLE.
    {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION).unwrap();
        w.section_bytes(SEC_STR, 1, vec![0xFF, 0xFE]).unwrap();
        w.section_bytes(SEC_MODEL, 1, vec![0; 64]).unwrap();
        w.section(SEC_F64, &[0u64; 0]).unwrap();
        let err = rlib::decode(&w.to_bytes()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
    }

    // 4. Trailing bytes in SEC_MODEL -> CORRUPT_TABLE.
    {
        let rbin = Rbin::from_bytes(valid.clone(), KIND, VERSIONS).unwrap();
        let str_b: &[u8] = rbin.section(SEC_STR).unwrap();
        let mut model_b: Vec<u8> = rbin.section::<u8>(SEC_MODEL).unwrap().to_vec();
        let f64_b: &[u64] = rbin.section(SEC_F64).unwrap();
        model_b.push(0);

        let mut w = RbinWriter::new(KIND, FORMAT_VERSION).unwrap();
        w.section_bytes(SEC_STR, 1, str_b.to_vec()).unwrap();
        w.section_bytes(SEC_MODEL, 1, model_b).unwrap();
        w.section(SEC_F64, f64_b).unwrap();
        let err = rlib::decode(&w.to_bytes()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
    }

    // 5. Huge count in SEC_MODEL does not OOM and returns CORRUPT_TABLE.
    {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION).unwrap();
        w.section_bytes(SEC_STR, 1, b"x".to_vec()).unwrap();
        // name=(0,1), followed by u32::MAX everywhere
        let mut model = vec![0, 0, 0, 0, 1, 0, 0, 0];
        model.extend_from_slice(&[0xFF; 128]);
        w.section_bytes(SEC_MODEL, 1, model).unwrap();
        w.section(SEC_F64, &[0u64; 0]).unwrap();
        let err = rlib::decode(&w.to_bytes()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CorruptTable);
    }

    // 6. Newer format_version -> SCHEMA_TOO_NEW.
    {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION + 1).unwrap();
        w.section_bytes(SEC_STR, 1, Vec::new()).unwrap();
        w.section_bytes(SEC_MODEL, 1, Vec::new()).unwrap();
        w.section(SEC_F64, &[0u64; 0]).unwrap();
        let err = rlib::decode(&w.to_bytes()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::SchemaTooNew);
    }
}

/// WS-REQ-0115, WS-REQ-0116: `rlib::put`, `rlib::libraries_of_corner`, `rlib::load`, and
/// `rlib::load_or_recompile` (transparently recompiling an older `format_version` and rejecting
/// a newer `format_version` with `SCHEMA_TOO_NEW`).
#[test]
fn cache_put_load_corners_and_transparent_recompile() {
    let dir = temp_dir("rlib-cache");
    let ws = Workspace::init(&dir, &inv("rusteda init"))
        .unwrap()
        .workspace;
    let base = ws.resolve_base("rev0").unwrap();
    let mut txn = ws.begin(base, &inv("rusteda import")).unwrap();

    let lib_tt = rich_library();
    let lib_tt_bytes = b"library (rich_lib) { /* tt */ }";
    let cached_tt = rlib::put(&ws, &lib_tt, lib_tt_bytes).unwrap();
    assert!(cached_tt.compiled_now);
    txn.add_library(cached_tt.entry("tt", "rich_tt.lib"))
        .unwrap();

    let mut lib_ff = rich_library();
    lib_ff.name = "rich_lib_ff".to_string();
    let lib_ff_bytes = b"library (rich_lib_ff) { /* ff */ }";
    let cached_ff = rlib::put(&ws, &lib_ff, lib_ff_bytes).unwrap();
    txn.add_library(cached_ff.entry("ff", "rich_ff.lib"))
        .unwrap();

    let committed = txn.commit(BTreeMap::new()).unwrap();
    assert_eq!(committed.rev, Rev(1));

    let rev1 = ws.revision(Rev(1)).unwrap();
    let tt_entries = rlib::libraries_of_corner(&rev1, "tt");
    assert_eq!(tt_entries.len(), 1);
    assert_eq!(tt_entries[0].name, "rich_lib");

    let ff_entries = rlib::libraries_of_corner(&rev1, "ff");
    assert_eq!(ff_entries.len(), 1);
    assert_eq!(ff_entries[0].name, "rich_lib_ff");

    // Load via mmap + decode.
    let loaded_tt = rlib::load(&ws, &rev1, "rich_lib", "tt").unwrap();
    assert_eq!(loaded_tt, lib_tt);

    // No recompile needed when current.
    let (loaded2, recompiled) = rlib::load_or_recompile(
        &ws,
        &rev1,
        "rich_lib",
        "tt",
        |_| -> Result<Library, String> {
            panic!("compiler must not run when .rlib is current");
        },
    )
    .unwrap();
    assert!(!recompiled);
    assert_eq!(loaded2, lib_tt);

    // Replace cached .rlib with an older format_version (0) -> load_or_recompile transparently
    // recompiles from cached .lib and reports `recompiled = true`.
    let mut old_w = RbinWriter::new(KIND, 0).unwrap();
    old_w.section_bytes(SEC_STR, 1, Vec::new()).unwrap();
    fs::write(&cached_tt.rlib_path, old_w.to_bytes()).unwrap();

    let (loaded3, recompiled) = rlib::load_or_recompile(
        &ws,
        &rev1,
        "rich_lib",
        "tt",
        |raw| -> Result<Library, String> {
            assert_eq!(raw, lib_tt_bytes);
            Ok(lib_tt.clone())
        },
    )
    .unwrap();
    assert!(recompiled);
    assert_eq!(loaded3, lib_tt);

    // Replace cached .rlib with a newer format_version (FORMAT_VERSION + 1) -> SCHEMA_TOO_NEW.
    let mut new_w = RbinWriter::new(KIND, FORMAT_VERSION + 1).unwrap();
    new_w.section_bytes(SEC_STR, 1, Vec::new()).unwrap();
    fs::write(&cached_tt.rlib_path, new_w.to_bytes()).unwrap();

    let err = rlib::load_or_recompile(
        &ws,
        &rev1,
        "rich_lib",
        "tt",
        |_| -> Result<Library, String> { Ok(lib_tt.clone()) },
    )
    .unwrap_err();
    match err {
        rusteda_workspace::PutError::Workspace(e) => {
            assert_eq!(e.code(), ErrorCode::SchemaTooNew);
        }
        other => panic!("expected Workspace(SchemaTooNew), got {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

/// WS-REQ-0117: format version 4 carries the pin pulse attributes (`min_pulse_width_high`,
/// `min_pulse_width_low`, `min_period`) through encode and decode, and a cached version 3 `.rlib`,
/// which cannot hold them, is compiled again from its `.lib` instead of being read without them.
#[test]
fn version_4_carries_pin_pulse_attributes_and_recompiles_version_3() {
    assert_eq!(FORMAT_VERSION, 4);
    assert_eq!(VERSIONS, 4..=4);
    let lib = rich_library();
    let decoded = rlib::decode(&rlib::encode(&lib).unwrap()).unwrap();
    let pin = |l: &Library| l.cells[0].pins[0].clone();
    assert_eq!(
        (
            pin(&decoded).min_pulse_width_high,
            pin(&decoded).min_pulse_width_low,
            pin(&decoded).min_period
        ),
        (Some(0.125), Some(0.15), Some(0.5))
    );

    let dir = temp_dir("rlib-v3");
    let ws = Workspace::init(&dir, &inv("rusteda init"))
        .unwrap()
        .workspace;
    let base = ws.resolve_base("rev0").unwrap();
    let mut txn = ws.begin(base, &inv("rusteda import")).unwrap();
    let lib_bytes = b"library (rich_lib) { /* v3 */ }";
    let cached = rlib::put(&ws, &lib, lib_bytes).unwrap();
    txn.add_library(cached.entry("tt", "rich.lib")).unwrap();
    txn.commit(BTreeMap::new()).unwrap();
    let rev1 = ws.revision(Rev(1)).unwrap();
    let mut v3 = RbinWriter::new(KIND, 3).unwrap();
    v3.section_bytes(SEC_STR, 1, Vec::new()).unwrap();
    fs::write(&cached.rlib_path, v3.to_bytes()).unwrap();
    let (loaded, recompiled) = rlib::load_or_recompile(
        &ws,
        &rev1,
        "rich_lib",
        "tt",
        |raw| -> Result<Library, String> {
            assert_eq!(raw, lib_bytes);
            Ok(lib.clone())
        },
    )
    .unwrap();
    assert!(recompiled, "a version 3 .rlib is compiled again");
    assert_eq!(pin(&loaded).min_period, Some(0.5));
    let _ = fs::remove_dir_all(&dir);
}
