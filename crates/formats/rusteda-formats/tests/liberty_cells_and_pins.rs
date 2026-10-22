// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Cell and pin attributes of the library model (LIBERTY-REQ-0011, LIBERTY-REQ-0012).

use rusteda_formats::liberty::{Direction, Library, read_text};

const LIB: &str = r#"
library (attrs) {
  time_unit : "1ns";
  capacitive_load_unit (1, pf);
  cell (PLAIN) {
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "!A"; }
  }
  cell (RICH) {
    area : 2.5;
    dont_use : true;
    pg_pin (VDD) { pg_type : primary_power; }
    pg_pin (VSS) { pg_type : primary_ground; }
    pin (A, B) { direction : input; capacitance : 0.002; }
    pin (C) {
      direction : input;
      capacitance : 0.003;
      rise_capacitance : 0.0031;
      fall_capacitance : 0.0029;
      rise_capacitance_range (0.0030, 0.0032);
      fall_capacitance_range (0.0028, 0.0030);
    }
    pin (CK) { direction : input; clock : true; capacitance : 0.001; }
    pin (N) { direction : internal; }
    pin (Z) { direction : inout; function : "A&B"; three_state : "!C"; }
    pin (Y) { direction : output; function : "A|B"; }
  }
  cell (GATE) {
    clock_gating_integrated_cell : "latch_posedge";
    statetable ("CK E", "IQ") { table : "L L : - : L"; }
    test_cell () { pin (E) { direction : input; } }
    bus (D) { bus_type : b2; }
    pin (CK) { direction : input; clock : true; }
    pin (E) { direction : input; }
    pin (GCK) { direction : output; }
  }
}
"#;

fn lib() -> Library {
    read_text(LIB, "attrs.lib").expect("library parses")
}

// LIBERTY-REQ-0015: explicit pin limits remain distinct from inherited library defaults.
#[test]
fn electrical_limits_and_fanout_weights() {
    let text = r#"library(l) {
        time_unit : "1ps"; capacitive_load_unit(1,ff);
        default_max_capacitance:40; default_max_transition:25;
        default_max_fanout:6; default_fanout_load:1.5;
        cell(c) {area:1;
          pin(A) {direction:input; fanout_load:2; max_transition:15;}
          pin(Y) {direction:output; function:"!A"; max_capacitance:80; max_fanout:12;}
        }
    }"#;
    let l = read_text(text, "limits.lib").unwrap();
    assert_eq!(l.time_unit_ns, 0.001);
    assert_eq!(l.cap_unit_pf, 0.001);
    assert_eq!(l.default_max_capacitance, Some(40.0));
    assert_eq!(l.default_max_transition, Some(25.0));
    assert_eq!(l.default_max_fanout, Some(6.0));
    assert_eq!(l.default_fanout_load, 1.5);
    let a = l.cells[0].pin("A").unwrap();
    let y = l.cells[0].pin("Y").unwrap();
    assert_eq!(a.fanout_load, Some(2.0));
    assert_eq!(a.max_transition, Some(15.0));
    assert_eq!(y.max_capacitance, Some(80.0));
    assert_eq!(y.max_transition, None);
    assert_eq!(y.max_fanout, Some(12.0));
}

// LIBERTY-REQ-0011: area and dont_use defaults.
#[test]
fn cell_defaults() {
    let l = lib();
    let c = l.cells.iter().find(|c| c.name == "PLAIN").unwrap();
    assert_eq!(c.area, 0.0);
    assert!(!c.dont_use);
    assert!(!c.has_statetable && !c.has_test_cell && !c.is_clock_gating && !c.has_bus);
    assert!(c.pg_pins.is_empty());
}

// LIBERTY-REQ-0011: explicit area, dont_use, pg_pin names, multi-name pin groups.
#[test]
fn cell_attributes() {
    let l = lib();
    let c = l.cells.iter().find(|c| c.name == "RICH").unwrap();
    assert_eq!(c.area, 2.5);
    assert!(c.dont_use);
    assert_eq!(c.pg_pins, vec!["VDD".to_string(), "VSS".to_string()]);
    let a = c.pin("A").unwrap();
    let b = c.pin("B").unwrap();
    assert_eq!((a.direction, a.capacitance), (Direction::Input, 0.002));
    assert_eq!((b.direction, b.capacitance), (Direction::Input, 0.002));
}

// LIBERTY-REQ-0011: the flags of special cells.
#[test]
fn cell_flags() {
    let l = lib();
    let c = l.cells.iter().find(|c| c.name == "GATE").unwrap();
    assert!(c.is_clock_gating);
    assert!(c.has_statetable);
    assert!(c.has_test_cell);
    assert!(c.has_bus);
}

// LIBERTY-REQ-0012: directions, function, three_state, capacitances with rise, fall and ranges,
// the clock flag, and the capacitance default.
#[test]
fn pin_attributes() {
    let l = lib();
    let c = l.cells.iter().find(|c| c.name == "RICH").unwrap();
    let pc = c.pin("C").unwrap();
    assert_eq!(pc.capacitance, 0.003);
    assert_eq!(pc.rise_capacitance, Some(0.0031));
    assert_eq!(pc.fall_capacitance, Some(0.0029));
    assert_eq!(pc.rise_capacitance_range, Some((0.0030, 0.0032)));
    assert_eq!(pc.fall_capacitance_range, Some((0.0028, 0.0030)));
    assert!(!pc.is_clock);
    assert!(c.pin("CK").unwrap().is_clock);
    assert_eq!(c.pin("N").unwrap().direction, Direction::Internal);
    let z = c.pin("Z").unwrap();
    assert_eq!(z.direction, Direction::Inout);
    assert_eq!(z.three_state.as_deref(), Some("!C"));
    assert_eq!(z.function_text.as_deref(), Some("A&B"));
    let y = c.pin("Y").unwrap();
    assert_eq!(y.direction, Direction::Output);
    assert!(y.function.is_some());
    assert_eq!(y.capacitance, 0.0);
    assert_eq!(y.rise_capacitance, None);
    assert_eq!(y.rise_capacitance_range, None);
}

// LIBERTY-REQ-0016: internal power entries per related pin, `power` as both directions, and the
// cell leakage power.
#[test]
fn internal_power_entries_are_kept() {
    let text = r#"
library (pw) {
  time_unit : "1ns";
  capacitive_load_unit (1, pf);
  lu_table_template (e2) { variable_1 : input_transition_time; variable_2 : total_output_net_capacitance;
    index_1 ("0.1, 0.5"); index_2 ("0.01, 0.1"); }
  cell (FF) {
    cell_leakage_power : 12.5;
    pin (CK) { direction : input; clock : true;
      internal_power () { power (scalar) { values ("0.03"); } } }
    pin (D) { direction : input; }
    pin (Q) { direction : output; function : "IQ";
      internal_power () { related_pin : "CK RN";
        rise_power (e2) { values ("1, 2", "3, 4"); }
        fall_power (e2) { values ("5, 6", "7, 8"); } } }
    pin (RN) { direction : input; }
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; clear : "!RN"; }
  }
}
"#;
    let lib = read_text(text, "pw.lib").expect("library parses");
    let ff = &lib.cells[0];
    assert_eq!(ff.leakage_power, Some(12.5));
    let ck = ff.pin("CK").unwrap();
    assert_eq!(ck.internal_power.len(), 1);
    assert_eq!(ck.internal_power[0].related_pin, None);
    assert_eq!(
        ck.internal_power[0].rise_power,
        ck.internal_power[0].fall_power
    );
    let q = ff.pin("Q").unwrap();
    let related: Vec<_> = q
        .internal_power
        .iter()
        .map(|p| p.related_pin.clone().unwrap())
        .collect();
    assert_eq!(related, ["CK", "RN"]);
    let rise = q.internal_power[0].rise_power.as_ref().unwrap();
    assert_eq!(rise.lookup(0.5, 0.1), 4.0);
    let fall = q.internal_power[1].fall_power.as_ref().unwrap();
    assert_eq!(fall.lookup(0.1, 0.01), 5.0);
    assert!(ff.pin("D").unwrap().internal_power.is_empty());
}
