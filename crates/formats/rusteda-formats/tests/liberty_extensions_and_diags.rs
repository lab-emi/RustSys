// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Tests of Liberty extensions RS-LIB.A, RS-LIB.B, RS-LIB.C and RS-LIB.D and the reader's
//! diagnostic channel (`docs/specs/core/README.md`, `docs/specs/liberty/liberty.md`).

use std::path::{Path, PathBuf};

use rusteda_core::diag::Severity;
use rusteda_formats::liberty::{
    AntennaDiodeType, Direction, LevelShifterType, PgType, SwitchCellType, TableVariable,
    TimingSense, TimingType, TreeType, WireLoadMode, read_file_diags, read_text, read_text_diags,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

// LIBERTY-REQ-0005, LIBERTY-REQ-0006, LIBERTY-REQ-0010 to LIBERTY-REQ-0019, LIBERTY-REQ-0023 to
// LIBERTY-REQ-0025, LIBERTY-REQ-0031 to LIBERTY-REQ-0034, LIBERTY-REQ-0040 to LIBERTY-REQ-0042,
// LIBERTY-REQ-0050, LIBERTY-REQ-0054, LIBERTY-REQ-0055: synthetic fixture exercising every
// RS-LIB.A and RS-LIB.D construct with zero warnings.
#[test]
fn synthetic_fixture_covers_a_and_d_with_zero_warnings() {
    let path = repo_root().join("verification/fixtures/synthetic/liberty/extensions_ad.lib");
    let (lib, diags) = read_file_diags(&path).expect("fixture reads");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:#?}");

    // Units (LIBERTY-REQ-0005, LIBERTY-REQ-0006, LIBERTY-REQ-0017).
    assert!(lib.units.time.given && (lib.units.time.scale - 0.1).abs() < 1e-12);
    assert!(lib.units.capacitance.given && (lib.units.capacitance.scale - 0.01).abs() < 1e-12);
    assert!(lib.units.resistance.given && (lib.units.resistance.scale - 0.1).abs() < 1e-12);
    assert!(lib.units.voltage.given && (lib.units.voltage.scale - 1.0).abs() < 1e-12);
    assert!(lib.units.current.given && (lib.units.current.scale - 1e-3).abs() < 1e-12);
    assert!(lib.units.leakage_power.given && (lib.units.leakage_power.scale - 1e-3).abs() < 1e-12);
    assert_eq!(lib.time_unit_ns, lib.units.time.scale);
    assert_eq!(lib.cap_unit_pf, lib.units.capacitance.scale);

    // Templates in deterministic order (LIBERTY-REQ-0031).
    let tpl_names: Vec<&str> = lib.templates.keys().map(String::as_str).collect();
    assert_eq!(tpl_names, ["p2", "t2", "t3"]);
    let t3 = &lib.templates["t3"];
    assert_eq!(t3.variable(1), Some(TableVariable::RelatedPinTransition));
    assert_eq!(
        t3.variable(2),
        Some(TableVariable::ConstrainedPinTransition)
    );
    assert_eq!(
        t3.variable(3),
        Some(TableVariable::TotalOutputNetCapacitance)
    );

    // Cells and pins in source order (LIBERTY-REQ-0019) and RS-LIB.D attributes
    // (LIBERTY-REQ-0040 to LIBERTY-REQ-0042).
    assert_eq!(lib.cells.len(), 2);
    let iso = &lib.cells[0];
    assert_eq!(iso.name, "ISO_BUF");
    assert!(iso.is_isolation_cell && iso.always_on && iso.dont_touch && !iso.dont_use);
    assert_eq!(iso.cell_footprint.as_deref(), Some("iso_buf"));
    assert_eq!(iso.pg_pin_groups.len(), 5);
    assert_eq!(iso.pg_pin_groups[0].pg_type, Some(PgType::PrimaryPower));
    assert_eq!(iso.pg_pin_groups[0].voltage_name.as_deref(), Some("VDD"));
    assert_eq!(iso.pg_pin_groups[1].pg_type, Some(PgType::BackupPower));
    assert_eq!(iso.pg_pin_groups[2].pg_type, Some(PgType::PrimaryGround));
    assert_eq!(iso.pg_pin_groups[3].pg_type, Some(PgType::Pwell));
    assert_eq!(iso.pg_pin_groups[4].pg_type, Some(PgType::Nwell));

    let a = iso.pin("A").unwrap();
    assert!(a.isolation_cell_data_pin && !a.isolation_cell_enable_pin);
    assert!(!a.capacitance_given);
    assert_eq!(lib.pin_capacitance(a), 1.5);
    assert_eq!(lib.pin_max_transition(a), Some(12.0));
    assert_eq!(lib.pin_fanout_load(a), 1.0);

    let en = iso.pin("EN").unwrap();
    assert!(en.isolation_cell_enable_pin && en.always_on);
    assert!(en.capacitance_given);
    assert_eq!(lib.pin_capacitance(en), 2.5);
    assert_eq!(en.min_capacitance, Some(0.5));
    assert_eq!(en.related_power_pin.as_deref(), Some("VDDB"));

    let y = iso.pin("Y").unwrap();
    assert_eq!(y.direction, Direction::Output);
    assert_eq!(y.capacitance, 0.75);
    assert_eq!(lib.pin_capacitance(y), 0.75);
    assert_eq!(y.min_capacitance, Some(1.0));
    assert_eq!(lib.pin_max_capacitance(y), Some(50.0));
    assert_eq!(lib.pin_max_fanout(y), Some(8.0));
    assert_eq!(y.power_down_function.as_deref(), Some("!VDD + VSS"));
    assert_eq!(
        y.three_state_function
            .as_ref()
            .and_then(|f| f.truth_table(&["EN"])),
        Some(0b01)
    );

    // Per-table index overrides and 3-axis lookup (LIBERTY-REQ-0032, LIBERTY-REQ-0033).
    assert_eq!(y.arcs.len(), 2);
    let arc0 = &y.arcs[0];
    assert_eq!(arc0.kind(), TimingType::Combinational);
    assert_eq!(arc0.sense(), Some(TimingSense::PositiveUnate));
    let cr = arc0.cell_rise.as_ref().unwrap();
    assert_eq!(cr.index_1, vec![2.0, 4.0]);
    assert_eq!(cr.index_2, vec![10.0, 20.0]);
    assert_eq!(cr.lookup(3.0, 15.0), cr.lookup3(3.0, 15.0, 0.0));

    let arc1 = &y.arcs[1];
    assert_eq!(arc1.kind(), TimingType::ThreeStateEnable);
    let cr3 = arc1.cell_rise.as_ref().unwrap();
    assert_eq!(cr3.shape3(), (2, 2, 2));
    assert!(cr3.is_consistent3() && !cr3.is_consistent());
    assert_eq!(cr3.index_1, vec![1.0, 3.0]);
    assert_eq!(cr3.index_2, vec![2.0, 4.0]);
    assert_eq!(cr3.index_3, vec![5.0, 15.0]);
    assert!((cr3.lookup3(2.0, 3.0, 10.0) - 4.5).abs() < 1e-12);

    // Second cell: level shifter, switch, retention, antenna diode, macro, pad.
    let ls = &lib.cells[1];
    assert!(ls.is_level_shifter && ls.is_macro_cell && ls.pad_cell);
    assert_eq!(ls.level_shifter_type, Some(LevelShifterType::LowToHigh));
    assert_eq!(ls.switch_cell_type, Some(SwitchCellType::FineGrain));
    assert_eq!(ls.retention_cell.as_deref(), Some("ret_style_1"));
    assert_eq!(
        ls.antenna_diode_type,
        Some(AntennaDiodeType::PowerAndGround)
    );
    assert_eq!(ls.pg_pin_groups[0].pg_type, Some(PgType::InternalPower));
    assert_eq!(ls.pg_pin_groups[2].pg_type, Some(PgType::InternalGround));
    assert_eq!(ls.pg_pin_groups[3].pg_type, Some(PgType::DeepNwell));
    assert_eq!(ls.pg_pin_groups[4].pg_type, Some(PgType::DeepPwell));
    assert!(ls.pin("D").unwrap().level_shifter_data_pin);
    assert!(ls.pin("LSE").unwrap().level_shifter_enable_pin);
    assert_eq!(lib.pin_capacitance(ls.pin("Z").unwrap()), 2.0);
    assert_eq!(lib.pin_capacitance(ls.pin("INT").unwrap()), 0.0);
    assert_eq!(lib.pin_max_transition(ls.pin("INT").unwrap()), None);
}

// LIBERTY-REQ-0005, LIBERTY-REQ-0006, LIBERTY-REQ-0017, LIBERTY-REQ-0050 to LIBERTY-REQ-0058:
// defaulted vs given units, unparseable unit -> L0001 error, and one aggregated warning per
// (code, construct_path) with count and first source location.
#[test]
fn units_and_aggregated_diagnostics() {
    let text = r#"library (diag_test) {
  time_unit : "bad_time";
  capacitive_load_unit (1, pf);
  unknown_lib_attr : 1;
  unknown_lib_attr : 2;
  operating_conditions (typ) {}
  lu_table_template (tpl) {
    variable_1 : future_axis_var;
    index_1 ("1.0, 2.0");
  }
  cell (C1) {
    area : "not_a_num";
    area : 2.0;
    dont_use : "maybe";
    level_shifter_type : custom_ls;
    mystery_group (m1) {}
    mystery_group (m2) {}
    pin (A) {
      direction : input;
      when : "A";
    }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        timing_type : combinational;
      }
      timing () {
        related_pin : "A";
        timing_type : vendor_special_arc;
        when : "A";
        mode : "m1";
        cell_rise (tpl) { values ("1.0, 2.0"); }
      }
      timing () {
        related_pin : "A";
        when : "!A";
        mode : "m2";
        cell_rise (tpl) { values ("1.0, 2.0"); }
      }
    }
  }
  cell (C_BAD) {
    pin (P) {
      direction : sideways;
      function : "UNKNOWN_PIN";
      max_capacitance : -5.0;
      timing () {
        related_pin : "P";
        cell_rise (no_such_tpl) {
          index_1 ("2.0, 1.0");
          values ("1.0, 2.0, 3.0");
        }
      }
    }
  }
}
"#;
    let (lib, diags) = read_text_diags(text, "diag.lib").expect("parses");
    let same = read_text(text, "diag.lib").expect("parses without diags");
    assert_eq!(lib.name, same.name);
    assert_eq!(lib.cells.len(), same.cells.len());

    // time_unit had invalid text -> defaulted (scale 1.0, given = false) + L0001 error;
    // capacitive_load_unit was valid -> given = true; voltage_unit was omitted -> defaulted.
    assert!(!lib.units.time.given && lib.units.time.scale == 1.0);
    assert_eq!(lib.units.time.text.as_deref(), Some("bad_time"));
    assert!(lib.units.capacitance.given && lib.units.capacitance.scale == 1.0);
    assert!(!lib.units.voltage.given && lib.units.voltage.text.is_none());

    // L0001 error at line 2.
    let l0001 = diags.iter().find(|d| d.code == "L0001").expect("L0001");
    assert_eq!(l0001.severity, Severity::Error);
    assert_eq!(l0001.location.as_ref().map(|l| l.line), Some(2));
    assert!(l0001.message.contains("library/time_unit"));
    assert!(l0001.message.contains("(1 occurrence)"));

    // Duplicate library/unknown_lib_attr is aggregated into 1 warning with 2 occurrences at line 4.
    let unk_attr = diags
        .iter()
        .find(|d| d.code == "L0101" && d.message.starts_with("library/unknown_lib_attr:"))
        .expect("unknown_lib_attr");
    assert_eq!(unk_attr.location.as_ref().map(|l| l.line), Some(4));
    assert!(
        unk_attr.message.contains("(2 occurrences)"),
        "{}",
        unk_attr.message
    );

    // Mystery group in cell aggregated with 2 occurrences at line 16.
    let unk_grp = diags
        .iter()
        .find(|d| d.code == "L0101" && d.message.starts_with("library/cell/mystery_group:"))
        .expect("mystery_group");
    assert_eq!(unk_grp.location.as_ref().map(|l| l.line), Some(16));
    assert!(unk_grp.message.contains("(2 occurrences)"));

    // Unsupported timing/mode aggregated across two timing groups (lines 32 and 38).
    let mode_diag = diags
        .iter()
        .find(|d| d.code == "L0100" && d.message.starts_with("library/cell/pin/timing/mode:"))
        .expect("timing/mode");
    assert_eq!(mode_diag.location.as_ref().map(|l| l.line), Some(32));
    assert!(mode_diag.message.contains("(2 occurrences)"));

    // Duplicate area (L0102), not-a-number area (L0111), unknown values (L0110), missing related_pin (L0112).
    assert!(
        diags
            .iter()
            .any(|d| d.code == "L0102" && d.message.starts_with("library/cell/area:"))
    );
    assert!(
        diags
            .iter()
            .any(|d| d.code == "L0111" && d.message.starts_with("library/cell/area:"))
    );
    assert!(diags.iter().any(|d| {
        d.code == "L0110"
            && d.message
                .starts_with("library/lu_table_template/variable_1:")
    }));
    assert!(diags.iter().any(|d| {
        d.code == "L0110"
            && d.message
                .starts_with("library/cell/pin/timing/timing_type:")
    }));
    assert!(
        diags
            .iter()
            .any(|d| d.code == "L0112" && d.message.starts_with("library/cell/pin/timing:"))
    );

    // Cell problems in C_BAD (L0200, L0202, L0204, L0205, L0206, L0207).
    assert!(!lib.cells[1].problems.is_empty());
    for code in ["L0200", "L0202", "L0204", "L0205", "L0206", "L0207"] {
        assert!(
            diags.iter().any(|d| d.code == code),
            "missing {code} in {diags:#?}"
        );
    }
}

// LIBERTY-REQ-0007, LIBERTY-REQ-0026, LIBERTY-REQ-0043 to LIBERTY-REQ-0046, LIBERTY-REQ-0059:
// synthetic fixture exercising every RS-LIB.C construct with zero warnings, plus validation of
// invalid thresholds/derate (L0002), bad statetable (L0203), bad when/state_function (L0201,
// L0202), and bad/unsupported bus/bundle (L0208).
#[test]
fn synthetic_fixture_covers_c_with_zero_warnings() {
    let path = repo_root().join("verification/fixtures/synthetic/liberty/extensions_c.lib");
    let (lib, diags) = read_file_diags(&path).expect("fixture reads");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:#?}");

    // Slew thresholds and derate (LIBERTY-REQ-0007, LIBERTY-REQ-0045).
    assert_eq!(lib.input_threshold_pct_rise, Some(50.0));
    assert_eq!(lib.input_threshold_pct_fall, Some(50.0));
    assert_eq!(lib.output_threshold_pct_rise, Some(50.0));
    assert_eq!(lib.output_threshold_pct_fall, Some(50.0));
    assert_eq!(lib.slew_lower_threshold_pct_rise, Some(20.0));
    assert_eq!(lib.slew_upper_threshold_pct_rise, Some(80.0));
    assert_eq!(lib.slew_lower_threshold_pct_fall, Some(30.0));
    assert_eq!(lib.slew_upper_threshold_pct_fall, Some(70.0));
    assert_eq!(lib.slew_derate_from_library, Some(0.5));

    // Clock-gating cell, statetable, internal_node, state_function, when, sdf_cond, and line numbers
    // (LIBERTY-REQ-0026, LIBERTY-REQ-0043, LIBERTY-REQ-0044, LIBERTY-REQ-0046).
    let icg = lib.cell("ICG_POS").expect("ICG_POS");
    assert!(icg.problems.is_empty(), "{:?}", icg.problems);
    assert!(icg.is_clock_gating && icg.has_statetable);
    assert_eq!(
        icg.clock_gating_integrated_cell.as_deref(),
        Some("latch_posedge")
    );
    assert!(icg.line > 0);
    let st = icg.statetable.as_ref().expect("statetable");
    assert_eq!(st.input_nodes, ["CLK", "EN", "TE"]);
    assert_eq!(st.internal_nodes, ["M0"]);
    assert_eq!(
        st.rows,
        [
            "L L L : - : L",
            "L H - : - : H",
            "L - H : - : H",
            "H - - : - : N",
        ]
    );
    assert!(icg.pin("CLK").unwrap().clock_gate_clock_pin);
    assert!(icg.pin("EN").unwrap().clock_gate_enable_pin);
    assert!(icg.pin("TE").unwrap().clock_gate_test_pin);
    assert!(icg.pin("OBS").unwrap().clock_gate_obs_pin);
    assert_eq!(icg.pin("M0").unwrap().internal_node.as_deref(), Some("M0"));
    let gclk = icg.pin("GCLK").unwrap();
    assert!(gclk.clock_gate_out_pin);
    assert!(gclk.line > icg.line);
    assert_eq!(gclk.state_function_text.as_deref(), Some("(CLK * M0)"));
    assert_eq!(
        gclk.state_function
            .as_ref()
            .and_then(|f| f.truth_table(&["CLK", "M0"])),
        Some(0b1000)
    );
    assert_eq!(gclk.arcs.len(), 2);
    assert_eq!(gclk.arcs[0].kind(), TimingType::CombinationalRise);
    assert_eq!(gclk.arcs[0].when_text.as_deref(), Some("EN + TE"));
    assert_eq!(
        gclk.arcs[0]
            .when
            .as_ref()
            .and_then(|f| f.truth_table(&["EN", "TE"])),
        Some(0b1110)
    );
    assert_eq!(
        gclk.arcs[0].sdf_cond.as_deref(),
        Some("EN == 1'b1 || TE == 1'b1")
    );
    assert!(gclk.arcs[0].line > gclk.line);
    assert_eq!(gclk.arcs[1].kind(), TimingType::CombinationalFall);
    assert_eq!(gclk.arcs[1].when_text.as_deref(), Some("!EN & !TE"));
    assert_eq!(
        gclk.arcs[1]
            .when
            .as_ref()
            .and_then(|f| f.truth_table(&["EN", "TE"])),
        Some(0b0001)
    );

    // Bus and bundle expansion (LIBERTY-REQ-0059).
    let bus_cell = lib.cell("BUS_BUF").expect("BUS_BUF");
    assert!(bus_cell.problems.is_empty(), "{:?}", bus_cell.problems);
    assert!(bus_cell.has_bus);
    let pin_names: Vec<&str> = bus_cell.pins.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(pin_names, ["A[1]", "A[0]", "Y0", "Y1"]);
    assert_eq!(bus_cell.pin("A[1]").unwrap().capacitance, 1.0);
    assert_eq!(bus_cell.pin("A[0]").unwrap().capacitance, 1.5);
    assert_eq!(bus_cell.pin("Y0").unwrap().max_capacitance, Some(25.0));
    assert_eq!(bus_cell.pin("Y1").unwrap().max_capacitance, Some(30.0));
    assert_eq!(bus_cell.pin("Y0").unwrap().arcs[0].related_pin, "A[0]");
    assert_eq!(bus_cell.pin("Y1").unwrap().arcs[0].related_pin, "A[1]");

    // Validation of bad thresholds/derate (L0002), bad statetable (L0203), bad when/state_function
    // (L0201, L0202), and bad bus/bundle (L0208).
    let bad_c = r#"library (bad_c) {
  input_threshold_pct_rise : 0.0;
  slew_lower_threshold_pct_fall : 80.0;
  slew_upper_threshold_pct_fall : 20.0;
  slew_derate_from_library : 1.5;
  cell (BAD_ST) {
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      state_function : "NO_SUCH_NODE";
      timing () {
        related_pin : "A";
        when : "A &";
      }
    }
    statetable ("A", "M0") {
      table : "L : -";
    }
  }
  cell (BAD_BUS_CELL) {
    bus (B) {
      bus_type : missing_type;
      direction : input;
    }
  }
}
"#;
    let (bad_lib, bad_diags) = read_text_diags(bad_c, "bad_c.lib").expect("parses");
    assert_eq!(bad_lib.input_threshold_pct_rise, None);
    assert_eq!(bad_lib.slew_lower_threshold_pct_fall, None);
    assert_eq!(bad_lib.slew_upper_threshold_pct_fall, None);
    assert_eq!(bad_lib.slew_derate_from_library, None);
    assert_eq!(
        bad_diags.iter().filter(|d| d.code == "L0002").count(),
        3,
        "{bad_diags:#?}"
    );
    assert!(!bad_lib.cell("BAD_ST").unwrap().problems.is_empty());
    assert!(!bad_lib.cell("BAD_BUS_CELL").unwrap().problems.is_empty());
    for code in ["L0201", "L0202", "L0203", "L0208"] {
        assert!(
            bad_diags.iter().any(|d| d.code == code),
            "missing {code} in {bad_diags:#?}"
        );
    }
}

// LIBERTY-REQ-0047, LIBERTY-REQ-0048: synthetic fixture exercising every RS-LIB.B construct
// (wire_load, wire_load_table, wire_load_selection, default_wire_load*, operating_conditions,
// default_operating_conditions, nom_*) with zero warnings, plus diagnostics on malformed entries.
#[test]
fn synthetic_fixture_covers_b_with_zero_warnings() {
    let path = repo_root().join("verification/fixtures/synthetic/liberty/extensions_b.lib");
    let (lib, diags) = read_file_diags(&path).expect("fixture reads");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:#?}");

    // Nominal PVT and operating_conditions (LIBERTY-REQ-0048).
    assert_eq!(lib.nom_process, Some(1.0));
    assert_eq!(lib.nom_temperature, Some(25.0));
    assert_eq!(lib.nom_voltage, Some(1.8));
    assert_eq!(
        lib.default_operating_conditions.as_deref(),
        Some("tt_025C_1v80")
    );
    assert_eq!(lib.operating_conditions.len(), 3);
    let tt = lib.operating_condition("tt_025C_1v80").expect("tt");
    assert_eq!(tt.process, Some(1.0));
    assert_eq!(tt.temperature, Some(25.0));
    assert_eq!(tt.voltage, Some(1.8));
    assert_eq!(tt.tree_type, Some(TreeType::BalancedTree));
    let ff = lib.operating_condition("ff_n40C_1v95").expect("ff");
    assert_eq!(ff.tree_type, Some(TreeType::BestCaseTree));
    let ss = lib.operating_condition("ss_100C_1v60").expect("ss");
    assert_eq!(ss.tree_type, Some(TreeType::WorstCaseTree));

    // Wire loads, tables, selections and defaults (LIBERTY-REQ-0047).
    assert_eq!(lib.default_wire_load.as_deref(), Some("Small"));
    assert_eq!(lib.default_wire_load_mode, Some(WireLoadMode::Top));
    assert_eq!(
        lib.default_wire_load_selection.as_deref(),
        Some("sel_by_area")
    );
    assert_eq!(lib.default_wire_load_area, Some(0.05));
    assert_eq!(lib.default_wire_load_capacitance, Some(1.42e-05));
    assert_eq!(lib.default_wire_load_resistance, Some(0.0745));

    assert_eq!(lib.wire_loads.len(), 2);
    let small = lib.wire_load("Small").expect("Small");
    assert_eq!(small.capacitance, 1.42e-05);
    assert_eq!(small.resistance, 0.0745);
    assert_eq!(small.area, 0.05);
    assert_eq!(small.slope, 8.3631);
    assert_eq!(
        small.fanout_length,
        vec![(1, 23.2746), (2, 32.1136), (4, 64.0974)]
    );
    assert!((small.length_for_fanout(3) - 48.1055).abs() < 1e-9);
    assert!((small.length_for_fanout(5) - (64.0974 + 8.3631)).abs() < 1e-9);

    let wlt = &lib.wire_load_tables["wlt_std"];
    assert_eq!(wlt.fanout_length, vec![(1, 10.0), (2, 22.0)]);
    assert_eq!(wlt.fanout_capacitance, vec![(1, 0.001), (2, 0.0025)]);
    assert_eq!(wlt.fanout_resistance, vec![(1, 0.05), (2, 0.12)]);
    assert_eq!(wlt.fanout_area, vec![(1, 0.5), (2, 1.1)]);

    let sel = lib.wire_load_selection("sel_by_area").expect("sel_by_area");
    assert_eq!(sel.rules.len(), 2);
    assert_eq!(sel.select(250.0), Some("Small"));
    assert_eq!(sel.select(1500.0), Some("Medium"));

    // Diagnostics for unknown WireLoadMode/TreeType (L0110) and invalid numeric entries (L0111).
    let bad_b = r#"library (bad_b) {
  default_wire_load_mode : custom_mode;
  operating_conditions (oc1) {
    tree_type : custom_tree;
    voltage : "bad_v";
  }
  wire_load (wl1) {
    fanout_length (1, "bad_len");
  }
  wire_load_selection (sel1) {
    wire_load_from_area (0.0, "bad_max", "wl1");
  }
}
"#;
    let (bad_lib, bad_diags) = read_text_diags(bad_b, "bad_b.lib").expect("parses");
    assert_eq!(
        bad_lib.default_wire_load_mode,
        Some(WireLoadMode::Other("custom_mode".into()))
    );
    assert_eq!(
        bad_lib.operating_condition("oc1").unwrap().tree_type,
        Some(TreeType::Other("custom_tree".into()))
    );
    assert_eq!(
        bad_diags.iter().filter(|d| d.code == "L0110").count(),
        2,
        "{bad_diags:#?}"
    );
    assert_eq!(
        bad_diags.iter().filter(|d| d.code == "L0111").count(),
        3,
        "{bad_diags:#?}"
    );
}

fn pdk_dir() -> PathBuf {
    if let Ok(d) = std::env::var("RUSTEDA_PDK_DIR") {
        let p = PathBuf::from(d);
        if p.is_dir() {
            return p;
        }
    }
    repo_root().join("target/pdk")
}

// LIBERTY-REQ-0026, LIBERTY-REQ-0040 to LIBERTY-REQ-0048, LIBERTY-REQ-0050: measured counts on
// sky130_fd_sc_hd and gf180mcu 7t/9t PDK libraries, and ZERO warnings and ZERO errors across all
// 9 local PDK files.
#[test]
#[ignore = "needs local PDK libraries (RUSTEDA_PDK_DIR)"]
fn pdk_sky130_and_gf180_counts() {
    let dir = pdk_dir();
    let sky_tt = dir.join("sky130_fd_sc_hd__tt_025C_1v80.lib");
    assert!(
        sky_tt.is_file(),
        "PDK file not found at {} (set RUSTEDA_PDK_DIR)",
        sky_tt.display()
    );
    let (sky, sky_diags) = read_file_diags(&sky_tt).expect("sky130 tt reads");
    assert!(sky_diags.is_empty(), "sky130 tt diags: {sky_diags:#?}");
    assert_eq!(sky.input_threshold_pct_rise, Some(50.0));
    assert_eq!(sky.slew_lower_threshold_pct_rise, Some(20.0));
    assert_eq!(sky.slew_upper_threshold_pct_rise, Some(80.0));
    assert_eq!(sky.slew_derate_from_library, Some(1.0));

    // RS-LIB.B on sky130_fd_sc_hd__tt_025C_1v80: 4 wire_load groups (Small, Medium, Large, Huge),
    // default "Small", mode top, operating_conditions "tt_025C_1v80" with balanced_tree.
    assert_eq!(sky.default_wire_load.as_deref(), Some("Small"));
    assert_eq!(sky.default_wire_load_mode, Some(WireLoadMode::Top));
    let wl_names: Vec<&str> = sky.wire_loads.keys().map(String::as_str).collect();
    assert_eq!(wl_names, ["Huge", "Large", "Medium", "Small"]);
    for name in ["Small", "Medium", "Large", "Huge"] {
        let wl = sky.wire_load(name).expect(name);
        assert_eq!(wl.capacitance, 1.42e-05);
        assert_eq!(wl.resistance, 0.0745);
        assert_eq!(wl.area, 0.0);
        assert_eq!(wl.slope, 8.3631);
        assert_eq!(
            wl.fanout_length,
            vec![
                (1, 23.2746),
                (2, 32.1136),
                (3, 48.4862),
                (4, 64.0974),
                (5, 86.2649),
                (6, 84.2649),
            ]
        );
    }
    assert_eq!(
        sky.default_operating_conditions.as_deref(),
        Some("tt_025C_1v80")
    );
    let sky_oc = sky
        .operating_condition("tt_025C_1v80")
        .expect("tt_025C_1v80");
    assert_eq!(sky_oc.process, Some(1.0));
    assert_eq!(sky_oc.temperature, Some(25.0));
    assert_eq!(sky_oc.voltage, Some(1.8));
    assert_eq!(sky_oc.tree_type, Some(TreeType::BalancedTree));
    assert_eq!(sky.nom_process, Some(1.0));
    assert_eq!(sky.nom_temperature, Some(25.0));
    assert_eq!(sky.nom_voltage, Some(1.8));

    let iso_cells = sky.cells.iter().filter(|c| c.is_isolation_cell).count();
    let iso_en_pins = sky
        .cells
        .iter()
        .flat_map(|c| &c.pins)
        .filter(|p| p.isolation_cell_enable_pin)
        .count();
    let aon_cells = sky.cells.iter().filter(|c| c.always_on).count();
    let aon_pins = sky
        .cells
        .iter()
        .flat_map(|c| &c.pins)
        .filter(|p| p.always_on)
        .count();
    let ls_cells = sky.cells.iter().filter(|c| c.is_level_shifter).count();
    let ls_data_pins = sky
        .cells
        .iter()
        .flat_map(|c| &c.pins)
        .filter(|p| p.level_shifter_data_pin)
        .count();
    let backup_power = sky
        .cells
        .iter()
        .flat_map(|c| &c.pg_pin_groups)
        .filter(|g| g.pg_type == Some(PgType::BackupPower))
        .count();
    let rel_power = sky
        .cells
        .iter()
        .flat_map(|c| &c.pins)
        .filter(|p| p.related_power_pin.is_some())
        .count();
    let pwr_down = sky
        .cells
        .iter()
        .flat_map(|c| &c.pins)
        .filter(|p| p.power_down_function.is_some())
        .count();
    let dont_use = sky.cells.iter().filter(|c| c.dont_use).count();
    let sky_st = sky.cells.iter().filter(|c| c.statetable.is_some()).count();
    let sky_sf = sky
        .cells
        .iter()
        .flat_map(|c| &c.pins)
        .filter(|p| p.state_function.is_some())
        .count();
    let sky_in = sky
        .cells
        .iter()
        .flat_map(|c| &c.pins)
        .filter(|p| p.internal_node.is_some())
        .count();
    assert_eq!(iso_cells, 11);
    assert_eq!(iso_en_pins, 11);
    assert_eq!(aon_cells, 10);
    assert_eq!(aon_pins, 3);
    assert_eq!(aon_cells + aon_pins, 13);
    assert_eq!(ls_cells, 7);
    assert_eq!(ls_data_pins, 7);
    assert_eq!(backup_power, 16);
    assert_eq!(rel_power, 1771);
    assert_eq!(pwr_down, 454);
    assert_eq!(dont_use, 0);
    assert_eq!(sky_st, 6);
    assert_eq!(sky_sf, 6);
    assert_eq!(sky_in, 6);

    for gf_name in [
        "gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib",
        "gf180mcu_fd_sc_mcu9t5v0__tt_025C_3v30.lib",
    ] {
        let (gf, gf_diags) = read_file_diags(&dir.join(gf_name)).expect("gf180 reads");
        assert!(gf_diags.is_empty(), "{gf_name} diags: {gf_diags:#?}");
        assert_eq!(gf.input_threshold_pct_rise, Some(50.0));
        assert_eq!(gf.slew_lower_threshold_pct_rise, Some(30.0));
        assert_eq!(gf.slew_upper_threshold_pct_rise, Some(70.0));
        assert_eq!(gf.slew_derate_from_library, Some(0.5));
        assert!(gf.wire_loads.is_empty());
        assert_eq!(gf.default_wire_load, None);
        assert_eq!(gf.operating_conditions.len(), 1);
        assert_eq!(gf.nom_process, Some(1.0));
        assert_eq!(gf.nom_temperature, Some(25.0));
        assert_eq!(gf.nom_voltage, Some(3.3));
        for g in gf.cells.iter().flat_map(|c| &c.pg_pin_groups) {
            assert!(
                matches!(
                    g.pg_type,
                    Some(
                        PgType::PrimaryPower
                            | PgType::PrimaryGround
                            | PgType::Nwell
                            | PgType::Pwell
                    )
                ),
                "{gf_name}: unexpected pg_type {:?}",
                g.pg_type
            );
        }
        let rp = gf
            .cells
            .iter()
            .flat_map(|c| &c.pins)
            .filter(|p| p.related_power_pin.is_some())
            .count();
        let rg = gf
            .cells
            .iter()
            .flat_map(|c| &c.pins)
            .filter(|p| p.related_ground_pin.is_some())
            .count();
        let icg = gf.cells.iter().filter(|c| c.is_clock_gating).count();
        let gf_st = gf.cells.iter().filter(|c| c.statetable.is_some()).count();
        let gf_sf = gf
            .cells
            .iter()
            .flat_map(|c| &c.pins)
            .filter(|p| p.state_function.is_some())
            .count();
        let gf_in = gf
            .cells
            .iter()
            .flat_map(|c| &c.pins)
            .filter(|p| p.internal_node.is_some())
            .count();
        let when_arcs = gf
            .cells
            .iter()
            .flat_map(|c| &c.pins)
            .flat_map(|p| &p.arcs)
            .filter(|a| a.when_text.is_some())
            .count();
        let when_parsed = gf
            .cells
            .iter()
            .flat_map(|c| &c.pins)
            .flat_map(|p| &p.arcs)
            .filter(|a| a.when.is_some())
            .count();
        let sdf_cond_arcs = gf
            .cells
            .iter()
            .flat_map(|c| &c.pins)
            .flat_map(|p| &p.arcs)
            .filter(|a| a.sdf_cond.is_some())
            .count();
        assert_eq!(rp, 835, "{gf_name} related_power_pin");
        assert_eq!(rg, 835, "{gf_name} related_ground_pin");
        assert_eq!(icg, 6, "{gf_name} clock_gating_integrated_cell");
        assert_eq!(gf_st, 6, "{gf_name} statetable");
        assert_eq!(gf_sf, 6, "{gf_name} state_function");
        assert_eq!(gf_in, 12, "{gf_name} internal_node");
        assert_eq!(when_arcs, 2892, "{gf_name} when_text");
        assert_eq!(when_parsed, 2892, "{gf_name} when parsed");
        assert_eq!(sdf_cond_arcs, 2892, "{gf_name} sdf_cond");
    }

    for f in [
        "sky130_fd_sc_hd__tt_025C_1v80.lib",
        "sky130_fd_sc_hd__ff_n40C_1v95.lib",
        "sky130_fd_sc_hd__ss_100C_1v60.lib",
        "gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib",
        "gf180mcu_fd_sc_mcu7t5v0__ff_n40C_3v60.lib",
        "gf180mcu_fd_sc_mcu7t5v0__ss_125C_3v00.lib",
        "gf180mcu_fd_sc_mcu9t5v0__tt_025C_3v30.lib",
        "gf180mcu_fd_sc_mcu9t5v0__ff_n40C_3v60.lib",
        "gf180mcu_fd_sc_mcu9t5v0__ss_125C_3v00.lib",
    ] {
        let p = dir.join(f);
        if p.is_file() {
            let (lib, diags) = read_file_diags(&p).unwrap_or_else(|e| panic!("{f}: {e}"));
            assert!(diags.is_empty(), "{f} diags: {diags:#?}");
            assert!(
                lib.cells.iter().all(|c| c.problems.is_empty()),
                "{f} cell problems"
            );
        }
    }
}
