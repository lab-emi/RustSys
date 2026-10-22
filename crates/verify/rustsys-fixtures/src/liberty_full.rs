// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! A synthetic library written in the full style of production Liberty files: operating
//! conditions, voltage maps, power and ground pins, leakage and internal power, two-dimensional
//! tables with inline indices, timing checks, several drive strengths, alternative function
//! syntaxes, and cells that synthesis must leave alone (scan, clock gating through a state
//! table, three-state, multi-bit, bus holder, antenna, fill and decoupling cells). Names and
//! numbers are invented; they model no real process.

use std::fmt::Write as _;

struct Comb {
    base: &'static str,
    inputs: &'static [&'static str],
    out: &'static str,
    func: &'static str,
    sense: &'static str,
    area: f64,
}

const fn g(
    base: &'static str,
    inputs: &'static [&'static str],
    out: &'static str,
    func: &'static str,
    sense: &'static str,
    area: f64,
) -> Comb {
    Comb {
        base,
        inputs,
        out,
        func,
        sense,
        area,
    }
}

/// Combinational cells; every function is written in a different Liberty syntax where possible.
const COMB: &[Comb] = &[
    g("inv", &["A"], "Y", "!A", "negative_unate", 3.6),
    g("buf", &["A"], "X", "A", "positive_unate", 5.4),
    g("dly", &["A"], "X", "(A)", "positive_unate", 9.1),
    g("nand2", &["A", "B"], "Y", "!(A&B)", "negative_unate", 5.4),
    g(
        "nand3",
        &["A", "B", "C"],
        "Y",
        "(A*B*C)'",
        "negative_unate",
        7.3,
    ),
    g(
        "nand4",
        &["A", "B", "C", "D"],
        "Y",
        "!(A B C D)",
        "negative_unate",
        9.1,
    ),
    g("nor2", &["A", "B"], "Y", "!(A|B)", "negative_unate", 5.4),
    g(
        "nor3",
        &["A", "B", "C"],
        "Y",
        "(A+B+C)'",
        "negative_unate",
        7.3,
    ),
    g(
        "nor4",
        &["A", "B", "C", "D"],
        "Y",
        "!(A+B+C+D)",
        "negative_unate",
        9.1,
    ),
    g("and2", &["A", "B"], "X", "A&B", "positive_unate", 7.3),
    g(
        "and3",
        &["A", "B", "C"],
        "X",
        "A*B*C",
        "positive_unate",
        9.1,
    ),
    g(
        "and4",
        &["A", "B", "C", "D"],
        "X",
        "A B C D",
        "positive_unate",
        10.9,
    ),
    g("or2", &["A", "B"], "X", "A|B", "positive_unate", 7.3),
    g("or3", &["A", "B", "C"], "X", "A+B+C", "positive_unate", 9.1),
    g(
        "or4",
        &["A", "B", "C", "D"],
        "X",
        "(A|B)|(C|D)",
        "positive_unate",
        10.9,
    ),
    g("xor2", &["A", "B"], "X", "(A^B)", "non_unate", 12.7),
    g("xnor2", &["A", "B"], "Y", "!(A^B)", "non_unate", 12.7),
    g("nand2b", &["A_N", "B"], "Y", "!(!A_N&B)", "non_unate", 7.3),
    g("nor2b", &["A", "B_N"], "Y", "!(A|!B_N)", "non_unate", 7.3),
    g(
        "a21oi",
        &["A1", "A2", "B1"],
        "Y",
        "!((A1&A2)|B1)",
        "negative_unate",
        7.3,
    ),
    g(
        "o21ai",
        &["A1", "A2", "B1"],
        "Y",
        "((A1+A2)*B1)'",
        "negative_unate",
        7.3,
    ),
    g(
        "a22oi",
        &["A1", "A2", "B1", "B2"],
        "Y",
        "!((A1&A2)|(B1&B2))",
        "negative_unate",
        9.1,
    ),
    g(
        "o22ai",
        &["A1", "A2", "B1", "B2"],
        "Y",
        "!((A1|A2)&(B1|B2))",
        "negative_unate",
        9.1,
    ),
    g(
        "a21o",
        &["A1", "A2", "B1"],
        "X",
        "(A1&A2)|B1",
        "positive_unate",
        9.1,
    ),
    g(
        "o21a",
        &["A1", "A2", "B1"],
        "X",
        "(A1|A2)&B1",
        "positive_unate",
        9.1,
    ),
    g(
        "a221oi",
        &["A1", "A2", "B1", "B2", "C1"],
        "Y",
        "!((A1&A2)|(B1&B2)|C1)",
        "negative_unate",
        10.9,
    ),
    g(
        "mux2",
        &["A0", "A1", "S"],
        "X",
        "(A0&!S)|(A1&S)",
        "non_unate",
        12.7,
    ),
    g(
        "mux2i",
        &["A0", "A1", "S"],
        "Y",
        "!((A0 S')+(A1 S))",
        "non_unate",
        10.9,
    ),
    g(
        "mux4",
        &["A0", "A1", "A2", "A3", "S0", "S1"],
        "X",
        "(A0&!S0&!S1)|(A1&S0&!S1)|(A2&!S0&S1)|(A3&S0&S1)",
        "non_unate",
        23.6,
    ),
    g(
        "maj3",
        &["A", "B", "C"],
        "X",
        "(A&B)|(A&C)|(B&C)",
        "positive_unate",
        10.9,
    ),
];

/// Drive strengths: suffix, area factor, delay slope factor.
const DRIVES: &[(&str, f64, f64)] = &[("_1", 1.0, 1.0), ("_2", 1.35, 0.55), ("_4", 2.1, 0.3)];

fn header(out: &mut String) {
    out.push_str(
        "/*\n * Synthetic library in production style. Values are invented.\n */\n\
library (wt_full) {\n\
  technology (cmos);\n\
  delay_model : table_lookup;\n\
  date : \"2026\";\n\
  revision : 1.0;\n\
  comment : \"generated\";\n\
  library_features (report_delay_calculation, report_power_calculation);\n\
  time_unit : \"1ns\";\n\
  voltage_unit : \"1V\";\n\
  current_unit : \"1mA\";\n\
  pulling_resistance_unit : \"1kohm\";\n\
  leakage_power_unit : \"1pW\";\n\
  capacitive_load_unit (1, pf);\n\
  nom_process : 1;\n\
  nom_voltage : 1.2;\n\
  nom_temperature : 25;\n\
  input_threshold_pct_rise : 50;\n\
  input_threshold_pct_fall : 50;\n\
  output_threshold_pct_rise : 50;\n\
  output_threshold_pct_fall : 50;\n\
  slew_lower_threshold_pct_rise : 20;\n\
  slew_upper_threshold_pct_rise : 80;\n\
  slew_lower_threshold_pct_fall : 20;\n\
  slew_upper_threshold_pct_fall : 80;\n\
  slew_derate_from_library : 1;\n\
  default_max_transition : 2.5;\n\
  default_fanout_load : 1;\n\
  default_inout_pin_cap : 0.005;\n\
  default_input_pin_cap : 0.005;\n\
  default_output_pin_cap : 0;\n\
  default_cell_leakage_power : 0;\n\
  default_leakage_power_density : 0;\n\
  k_process_cell_rise : 0;\n\
  k_temp_cell_rise : 0.0012;\n\
  in_place_swap_mode : match_footprint;\n\
  define (wt_family, cell, string);\n\
  define_cell_area (pad_area, pad_slots);\n\
  voltage_map (VDD, 1.2);\n\
  voltage_map (VSS, 0.0);\n\
  operating_conditions (typ_1p20v_25c) {\n\
    process : 1;\n\
    voltage : 1.2;\n\
    temperature : 25;\n\
    tree_type : balanced_tree;\n\
  }\n\
  default_operating_conditions : typ_1p20v_25c;\n\
  input_voltage (cmos_in) { vil : 0.0; vih : 1.2; vimin : 0.0; vimax : 1.2; }\n\
  output_voltage (cmos_out) { vol : 0.0; voh : 1.2; vomin : 0.0; vomax : 1.2; }\n\
  wire_load (\"wt_small\") {\n\
    resistance : 0.0001;\n\
    capacitance : 0.0002;\n\
    area : 0.1;\n\
    slope : 1.0;\n\
    fanout_length (1, 1.5);\n\
    fanout_length (2, 3.1);\n\
  }\n\
  default_wire_load : \"wt_small\";\n\
  lu_table_template (del_3x3) {\n\
    variable_1 : input_net_transition;\n\
    variable_2 : total_output_net_capacitance;\n\
    index_1 (\"0.01, 0.1, 1\");\n\
    index_2 (\"0.001, 0.01, 0.1\");\n\
  }\n\
  lu_table_template (chk_2x2) {\n\
    variable_1 : related_pin_transition;\n\
    variable_2 : constrained_pin_transition;\n\
    index_1 (\"0.01, 1\");\n\
    index_2 (\"0.01, 1\");\n\
  }\n\
  power_lut_template (pwr_3x3) {\n\
    variable_1 : input_transition_time;\n\
    variable_2 : total_output_net_capacitance;\n\
    index_1 (\"1, 2, 3\");\n\
    index_2 (\"1, 2, 3\");\n\
  }\n\
  power_lut_template (pwr_1d) {\n\
    variable_1 : input_transition_time;\n\
    index_1 (\"1, 2\");\n\
  }\n\
  type (bus4) {\n\
    base_type : array;\n\
    data_type : bit;\n\
    bit_width : 4;\n\
    bit_from : 3;\n\
    bit_to : 0;\n\
    downto : true;\n\
  }\n",
    );
}

fn pg_pins(out: &mut String) {
    out.push_str(
        "    pg_pin (VDD) { voltage_name : VDD; pg_type : primary_power; }\n\
    pg_pin (VSS) { voltage_name : VSS; pg_type : primary_ground; }\n",
    );
}

/// Delay table with inline indices and scientific notation.
fn delay(out: &mut String, kind: &str, d0: f64, k: f64) {
    let _ = writeln!(out, "        {kind} (del_3x3) {{");
    let _ = writeln!(out, "          index_1 (\"0.01, 0.1, 1\");");
    let _ = writeln!(out, "          index_2 (\"0.001, 0.01, 0.1\");");
    let mut rows = Vec::new();
    for s in [0.01, 0.1, 1.0] {
        let row: Vec<String> = [0.001, 0.01, 0.1]
            .iter()
            .map(|l| format!("{:.4e}", d0 + k * l + 0.08 * s))
            .collect();
        rows.push(format!("\"{}\"", row.join(", ")));
    }
    let _ = writeln!(
        out,
        "          values ( {} );",
        rows.join(", \\\n            ")
    );
    let _ = writeln!(out, "        }}");
}

fn power(out: &mut String, related: &str) {
    let _ = writeln!(out, "      internal_power () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        related_pg_pin : VDD;");
    for kind in ["rise_power", "fall_power"] {
        let _ = writeln!(out, "        {kind} (pwr_3x3) {{");
        let _ = writeln!(out, "          index_1 (\"0.01, 0.1, 1\");");
        let _ = writeln!(out, "          index_2 (\"0.001, 0.01, 0.1\");");
        let _ = writeln!(
            out,
            "          values (\"1.1e-03, 1.2e-03, 1.4e-03\", \"1.2e-03, 1.3e-03, 1.5e-03\", \\\n                  \"1.5e-03, 1.6e-03, 1.9e-03\");"
        );
        let _ = writeln!(out, "        }}");
    }
    let _ = writeln!(out, "      }}");
}

fn comb_cell(out: &mut String, c: &Comb, drive: (&str, f64, f64)) {
    let name = format!("wtf_{}{}", c.base, drive.0);
    let _ = writeln!(out, "  cell ({name}) {{");
    let _ = writeln!(out, "    area : {:.4};", c.area * drive.1);
    let _ = writeln!(out, "    cell_footprint : \"{}\";", c.base);
    let _ = writeln!(out, "    wt_family : \"{}\";", c.base);
    let _ = writeln!(out, "    cell_leakage_power : 12.5;");
    let _ = writeln!(
        out,
        "    leakage_power () {{ when : \"!{}\"; value : 10.1; related_pg_pin : VDD; }}",
        c.inputs[0]
    );
    let _ = writeln!(
        out,
        "    leakage_power () {{ when : \"{}\"; value : 14.9; related_pg_pin : VDD; }}",
        c.inputs[0]
    );
    pg_pins(out);
    for i in c.inputs {
        let _ = writeln!(out, "    pin ({i}) {{");
        let _ = writeln!(out, "      direction : input;");
        let _ = writeln!(out, "      related_power_pin : VDD;");
        let _ = writeln!(out, "      related_ground_pin : VSS;");
        let _ = writeln!(out, "      input_voltage : cmos_in;");
        let _ = writeln!(out, "      capacitance : {:.5};", 0.0021 * drive.1);
        let _ = writeln!(out, "      rise_capacitance : {:.5};", 0.0022 * drive.1);
        let _ = writeln!(out, "      fall_capacitance : {:.5};", 0.0020 * drive.1);
        let _ = writeln!(out, "      max_transition : 2.5;");
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "    pin ({}) {{", c.out);
    let _ = writeln!(out, "      direction : output;");
    let _ = writeln!(out, "      related_power_pin : VDD;");
    let _ = writeln!(out, "      related_ground_pin : VSS;");
    let _ = writeln!(out, "      output_voltage : cmos_out;");
    let _ = writeln!(out, "      max_capacitance : {:.3};", 0.15 / drive.2);
    let _ = writeln!(out, "      function : \"{}\";", c.func);
    for (j, i) in c.inputs.iter().enumerate() {
        power(out, i);
        let _ = writeln!(out, "      timing () {{");
        let _ = writeln!(out, "        related_pin : \"{i}\";");
        let _ = writeln!(out, "        timing_sense : {};", c.sense);
        let _ = writeln!(out, "        timing_type : combinational;");
        let d0 = 0.02 + 0.006 * c.inputs.len() as f64 + 0.003 * j as f64;
        let k = 4.5 * drive.2;
        delay(out, "cell_rise", d0, k);
        delay(out, "rise_transition", d0 * 0.8, k * 1.6);
        delay(out, "cell_fall", d0 * 0.9, k * 0.8);
        delay(out, "fall_transition", d0 * 0.7, k * 1.3);
        let _ = writeln!(out, "      }}");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "  }}");
}

/// Sequential cells: (name, area, state group, inputs with clock flag, outputs with functions).
type SeqDef = (
    &'static str,
    f64,
    &'static str,
    &'static [(&'static str, bool)],
    &'static [(&'static str, &'static str)],
);

const SEQ: &[SeqDef] = &[
    (
        "wtf_dff_1",
        23.6,
        "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CLK\"; }",
        &[("D", false), ("CLK", true)],
        &[("Q", "IQ"), ("Q_N", "IQN")],
    ),
    (
        "wtf_dffrq_1",
        27.2,
        "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CLK\"; clear : \"RESET_B'\"; }",
        &[("D", false), ("CLK", true), ("RESET_B", false)],
        &[("Q", "IQ")],
    ),
    (
        "wtf_dffr_1",
        29.0,
        "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CLK\"; clear : \"!RESET_B\"; }",
        &[("D", false), ("CLK", true), ("RESET_B", false)],
        &[("Q", "IQ"), ("Q_N", "IQN")],
    ),
    (
        "wtf_dffs_1",
        29.0,
        "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CLK\"; preset : \"!SET_B\"; }",
        &[("D", false), ("CLK", true), ("SET_B", false)],
        &[("Q", "IQ"), ("Q_N", "IQN")],
    ),
    (
        "wtf_dffrs_1",
        32.7,
        "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CLK\"; clear : \"!RESET_B\"; preset : \"!SET_B\"; clear_preset_var1 : L; clear_preset_var2 : H; }",
        &[
            ("D", false),
            ("CLK", true),
            ("RESET_B", false),
            ("SET_B", false),
        ],
        &[("Q", "IQ"), ("Q_N", "IQN")],
    ),
    (
        "wtf_dffn_1",
        25.4,
        "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"!CLK_N\"; }",
        &[("D", false), ("CLK_N", true)],
        &[("Q", "IQ"), ("Q_N", "IQN")],
    ),
    (
        "wtf_dlh_1",
        18.1,
        "latch (IQ, IQN) { data_in : \"D\"; enable : \"GATE\"; }",
        &[("D", false), ("GATE", true)],
        &[("Q", "IQ")],
    ),
    (
        "wtf_dll_1",
        18.1,
        "latch (IQ, IQN) { data_in : \"D\"; enable : \"!GATE_N\"; }",
        &[("D", false), ("GATE_N", true)],
        &[("Q", "IQ"), ("Q_N", "IQN")],
    ),
    (
        "wtf_dlhr_1",
        21.8,
        "latch (IQ, IQN) { data_in : \"D\"; enable : \"GATE\"; clear : \"!RESET_B\"; }",
        &[("D", false), ("GATE", true), ("RESET_B", false)],
        &[("Q", "IQ"), ("Q_N", "IQN")],
    ),
];

fn check(out: &mut String, related: &str, kind: &str) {
    let _ = writeln!(out, "      timing () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        timing_type : {kind};");
    for t in ["rise_constraint", "fall_constraint"] {
        let _ = writeln!(
            out,
            "        {t} (chk_2x2) {{ values (\"0.05, 0.07\", \"0.09, 0.11\"); }}"
        );
    }
    let _ = writeln!(out, "      }}");
}

fn seq_cell(out: &mut String, d: &SeqDef) {
    let (name, area, group, inputs, outputs) = *d;
    let latch = group.starts_with("latch");
    let clk = inputs.iter().find(|x| x.1).map_or("CLK", |x| x.0);
    let neg = clk.ends_with("_N");
    let edge = match (latch, neg) {
        (false, false) => "rising_edge",
        (false, true) => "falling_edge",
        (true, false) => "rising_edge",
        (true, true) => "falling_edge",
    };
    let _ = writeln!(out, "  cell ({name}) {{");
    let _ = writeln!(out, "    area : {area};");
    let _ = writeln!(out, "    cell_leakage_power : 80.2;");
    pg_pins(out);
    let _ = writeln!(out, "    {group}");
    for (p, is_clk) in inputs {
        let _ = writeln!(out, "    pin ({p}) {{");
        let _ = writeln!(out, "      direction : input;");
        let _ = writeln!(out, "      related_power_pin : VDD;");
        let _ = writeln!(out, "      related_ground_pin : VSS;");
        let _ = writeln!(out, "      capacitance : 0.0024;");
        if *is_clk {
            let _ = writeln!(out, "      clock : true;");
            let _ = writeln!(out, "      min_pulse_width_high : 0.12;");
            let _ = writeln!(out, "      min_pulse_width_low : 0.12;");
            power(out, p);
        } else if *p == "D" {
            let (s, h) = if neg {
                ("setup_falling", "hold_falling")
            } else {
                ("setup_rising", "hold_rising")
            };
            check(out, clk, s);
            check(out, clk, h);
        } else {
            let (r, m) = if neg {
                ("recovery_falling", "removal_falling")
            } else {
                ("recovery_rising", "removal_rising")
            };
            check(out, clk, r);
            check(out, clk, m);
        }
        let _ = writeln!(out, "    }}");
    }
    for (p, f) in outputs {
        let _ = writeln!(out, "    pin ({p}) {{");
        let _ = writeln!(out, "      direction : output;");
        let _ = writeln!(out, "      related_power_pin : VDD;");
        let _ = writeln!(out, "      related_ground_pin : VSS;");
        let _ = writeln!(out, "      max_capacitance : 0.2;");
        let _ = writeln!(out, "      function : \"{f}\";");
        let _ = writeln!(out, "      timing () {{");
        let _ = writeln!(out, "        related_pin : \"{clk}\";");
        let _ = writeln!(out, "        timing_type : {edge};");
        delay(out, "cell_rise", 0.21, 3.8);
        delay(out, "rise_transition", 0.05, 7.0);
        delay(out, "cell_fall", 0.19, 3.3);
        delay(out, "fall_transition", 0.04, 6.1);
        let _ = writeln!(out, "      }}");
        for (ip, _) in inputs
            .iter()
            .filter(|x| x.0.contains("SET") || x.0.contains("RESET"))
        {
            let _ = writeln!(out, "      timing () {{");
            let _ = writeln!(out, "        related_pin : \"{ip}\";");
            let _ = writeln!(out, "        timing_sense : non_unate;");
            let _ = writeln!(out, "        timing_type : clear;");
            delay(out, "cell_fall", 0.18, 3.3);
            delay(out, "fall_transition", 0.04, 6.1);
            let _ = writeln!(out, "      }}");
        }
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "  }}");
}

/// Cells that synthesis must never use.
const SPECIAL: &str = r#"  cell (wtf_sdff_1) {
    area : 30.8;
    pg_pin (VDD) { voltage_name : VDD; pg_type : primary_power; }
    pg_pin (VSS) { voltage_name : VSS; pg_type : primary_ground; }
    ff (IQ, IQN) { next_state : "(D&!SCE)|(SCD&SCE)"; clocked_on : "CLK"; }
    pin (D) { direction : input; capacitance : 0.002; nextstate_type : data; }
    pin (SCD) { direction : input; capacitance : 0.002; nextstate_type : scan_in; }
    pin (SCE) { direction : input; capacitance : 0.003; nextstate_type : scan_enable; }
    pin (CLK) { direction : input; capacitance : 0.002; clock : true; }
    pin (Q) { direction : output; function : "IQ"; }
    test_cell () {
      ff (IQ, IQN) { next_state : "D"; clocked_on : "CLK"; }
      pin (D) { direction : input; }
      pin (SCD) { direction : input; signal_type : test_scan_in; }
      pin (SCE) { direction : input; signal_type : test_scan_enable; }
      pin (CLK) { direction : input; }
      pin (Q) { direction : output; function : "IQ"; signal_type : test_scan_out; }
    }
  }
  cell (wtf_dffe_1) {
    area : 30.8;
    ff (IQ, IQN) { next_state : "(D&DE)|(IQ&!DE)"; clocked_on : "CLK"; }
    pin (D) { direction : input; capacitance : 0.002; }
    pin (DE) { direction : input; capacitance : 0.002; }
    pin (CLK) { direction : input; capacitance : 0.002; clock : true; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (wtf_icg_1) {
    area : 25.4;
    clock_gating_integrated_cell : "latch_posedge";
    dont_touch : true;
    pg_pin (VDD) { voltage_name : VDD; pg_type : primary_power; }
    pg_pin (VSS) { voltage_name : VSS; pg_type : primary_ground; }
    statetable ("CLK GATE", "IQ") {
      table : "L L : - : L, \
               L H : - : H, \
               H - : - : N";
    }
    pin (IQ) { direction : internal; internal_node : "IQ"; }
    pin (GATE) { direction : input; capacitance : 0.002; clock_gate_enable_pin : true; }
    pin (CLK) { direction : input; capacitance : 0.002; clock : true; clock_gate_clock_pin : true; }
    pin (GCLK) {
      direction : output;
      function : "CLK&IQ";
      clock_gate_out_pin : true;
      state_function : "CLK&IQ";
    }
  }
  cell (wtf_ebufn_1) {
    area : 14.5;
    pin (A) { direction : input; capacitance : 0.002; }
    pin (TE_B) { direction : input; capacitance : 0.002; }
    pin (Z) { direction : output; function : "A"; three_state : "TE_B"; }
  }
  cell (wtf_dff4_1) {
    area : 90.7;
    bus (D) { bus_type : bus4; direction : input; capacitance : 0.002; }
    pin (CLK) { direction : input; capacitance : 0.004; clock : true; }
    bus (Q) {
      bus_type : bus4;
      direction : output;
      pin (Q[0]) { function : "IQ0"; }
    }
  }
  cell (wtf_hold_1) {
    area : 9.1;
    dont_touch : true;
    pin (Z) { direction : inout; driver_type : bus_hold; }
  }
  cell (wtf_antenna) {
    area : 3.6;
    dont_touch : true;
    pin (A) { direction : input; capacitance : 0.001; antenna_diode_type : power; }
  }
  cell (wtf_decap_4) {
    area : 7.3;
    is_decap_cell : true;
    dont_touch : true;
    pg_pin (VDD) { voltage_name : VDD; pg_type : primary_power; }
    pg_pin (VSS) { voltage_name : VSS; pg_type : primary_ground; }
  }
  cell (wtf_fill_1) {
    area : 1.8;
    is_filler_cell : true;
    dont_touch : true;
  }
  cell (wtf_tiehi) {
    area : 5.4;
    pg_pin (VDD) { voltage_name : VDD; pg_type : primary_power; }
    pg_pin (VSS) { voltage_name : VSS; pg_type : primary_ground; }
    pin (L_HI) { direction : output; function : "1"; related_power_pin : VDD; related_ground_pin : VSS; }
  }
  cell (wtf_tielo) {
    area : 5.4;
    pg_pin (VDD) { voltage_name : VDD; pg_type : primary_power; }
    pg_pin (VSS) { voltage_name : VSS; pg_type : primary_ground; }
    pin (L_LO) { direction : output; function : "0"; related_power_pin : VDD; related_ground_pin : VSS; }
  }
  cell (wtf_inv_16) {
    area : 30.0;
    dont_use : true;
    pin (A) { direction : input; capacitance : 0.03; }
    pin (Y) { direction : output; function : "!A"; }
  }
"#;

/// Liberty text of the production-style library.
pub fn library() -> String {
    let mut out = String::new();
    header(&mut out);
    for c in COMB {
        for d in DRIVES {
            comb_cell(&mut out, c, *d);
        }
    }
    for d in SEQ {
        seq_cell(&mut out, d);
    }
    out.push_str(SPECIAL);
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {

    #[test]
    fn reads_without_problems_in_usable_cells() {
        let lib =
            rusteda_formats::liberty::read_text(&super::library(), "wt_full.lib").expect("parses");
        assert_eq!(lib.name, "wt_full");
        let comb = super::COMB.len() * super::DRIVES.len();
        assert!(lib.cells.len() >= comb + super::SEQ.len() + 10);
        for c in &lib.cells {
            let special = ["sdff", "dffe", "icg", "ebufn", "dff4", "hold", "antenna"];
            if !special.iter().any(|s| c.name.contains(s)) {
                assert!(c.problems.is_empty(), "{}: {:?}", c.name, c.problems);
            }
        }
        let icg = lib.cells.iter().find(|c| c.name == "wtf_icg_1").unwrap();
        assert!(icg.is_clock_gating && icg.has_statetable);
        let sdff = lib.cells.iter().find(|c| c.name == "wtf_sdff_1").unwrap();
        assert!(sdff.has_test_cell);
        let bank = lib.cells.iter().find(|c| c.name == "wtf_dff4_1").unwrap();
        assert!(bank.has_bus);
        let inv = lib.cells.iter().find(|c| c.name == "wtf_inv_1").unwrap();
        assert_eq!(inv.pg_pins, vec!["VDD".to_string(), "VSS".to_string()]);
    }
}
