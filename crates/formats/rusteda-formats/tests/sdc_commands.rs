// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Integration tests for the declarative SDC command schema, object queries, DRC/synthesis
//! diagnostics, and Phase 2b diagnostics (SDC-REQ-0005, SDC-REQ-0006, SDC-REQ-0016 to 0039).

use std::path::PathBuf;

use rusteda_core::diag::Severity;
use rusteda_formats::sdc::{
    self, AnalysisType, CaseValue, ClockGroupsKind, CmdKind, Filter, FilterAtom, FilterOp, Obj,
    ObjQuery, SetupHoldScope, StartEndScope, WireLoadMode,
};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("verification/fixtures/synthetic/sdc")
}

// SDC-REQ-0016 to 0037: load synthetic fixtures `basic.sdc` and `exceptions.sdc` covering all 26
// Phase 2a timing and environment commands.
#[test]
fn all_phase_2a_commands_parse_and_normalize_units() {
    let dir = fixture_dir();
    let basic_text = std::fs::read_to_string(dir.join("basic.sdc")).expect("read basic.sdc");
    let (basic, diags_b) = sdc::read_str(&basic_text);
    assert!(diags_b.is_empty(), "{diags_b:?}");
    assert_eq!(basic.cmds.len(), 19);

    assert!(matches!(
        &basic.cmds[1].kind,
        CmdKind::CurrentDesign { name } if name == "top_core"
    ));
    assert!(matches!(
        &basic.cmds[2].kind,
        CmdKind::SetOperatingConditions {
            analysis_type: Some(AnalysisType::OnChipVariation),
            min: Some(min),
            max: Some(max),
            ..
        } if min == "fast_lib" && max == "slow_lib"
    ));
    assert!(matches!(
        &basic.cmds[3].kind,
        CmdKind::SetWireLoadModel { name, min: true, .. } if name == "wl_small"
    ));
    assert!(matches!(
        &basic.cmds[4].kind,
        CmdKind::SetWireLoadMode {
            mode: WireLoadMode::Enclosed
        }
    ));
    assert!(matches!(
        &basic.cmds[5].kind,
        CmdKind::SetWireLoadSelectionGroup { group_name, max: true, .. } if group_name == "area_table"
    ));
    assert!(matches!(
        &basic.cmds[6].kind,
        CmdKind::CreateClock { name: Some(n), period, waveform: Some([r, f]), .. }
            if n == "sys_clk" && (*period - 2.5).abs() < 1e-12 && r.abs() < 1e-12 && (*f - 1.25).abs() < 1e-12
    ));

    let exc_text =
        std::fs::read_to_string(dir.join("exceptions.sdc")).expect("read exceptions.sdc");
    let (exc, diags_e) = sdc::read_str(&exc_text);
    assert!(diags_e.is_empty(), "{diags_e:?}");
    assert_eq!(exc.cmds.len(), 16);

    assert!(matches!(
        &exc.cmds[3].kind,
        CmdKind::SetClockGroups {
            kind: ClockGroupsKind::Asynchronous,
            name: Some(n),
            groups,
            ..
        } if n == "async_ab" && groups.len() == 2
    ));
    assert!(matches!(
        &exc.cmds[4].kind,
        CmdKind::SetFalsePath {
            setup_hold: SetupHoldScope::Setup,
            ..
        }
    ));
    assert!(matches!(
        &exc.cmds[5].kind,
        CmdKind::SetFalsePath {
            setup_hold: SetupHoldScope::Hold,
            ..
        }
    ));
    assert!(matches!(
        &exc.cmds[6].kind,
        CmdKind::SetMulticyclePath {
            multiplier: 2,
            setup_hold: SetupHoldScope::Setup,
            start_end: StartEndScope::End,
            ..
        }
    ));
    assert!(matches!(
        &exc.cmds[8].kind,
        CmdKind::SetMaxDelay { delay, .. } if (*delay - 1.5).abs() < 1e-12
    ));
    assert!(matches!(
        &exc.cmds[9].kind,
        CmdKind::SetMinDelay { delay, .. } if (*delay - 0.2).abs() < 1e-12
    ));
    assert!(matches!(
        &exc.cmds[10].kind,
        CmdKind::SetCaseAnalysis {
            value: CaseValue::Zero,
            ..
        }
    ));
    assert!(matches!(&exc.cmds[11].kind, CmdKind::SetLogicZero { .. }));
    assert!(matches!(&exc.cmds[12].kind, CmdKind::SetLogicOne { .. }));
    assert!(matches!(
        &exc.cmds[13].kind,
        CmdKind::SetDisableTiming { from: Some(f), to: Some(t), .. } if f == "A" && t == "Z"
    ));
    assert!(matches!(
        &exc.cmds[14].kind,
        CmdKind::SetMaxTimeBorrow { value, .. } if (*value - 0.4).abs() < 1e-12
    ));
    assert!(matches!(
        &exc.cmds[15].kind,
        CmdKind::GroupPath { name: Some(n), .. } if n == "reg2reg"
    ));
}

// SDC-REQ-0005, SDC-REQ-0006: object queries (`get_ports`, `get_pins`, `get_cells`, `get_nets`,
// `get_clocks`, `get_lib_cells`, `get_lib_pins`, `all_inputs`, `all_outputs`, `all_clocks`,
// `all_registers`, `current_design`) and `-filter` predicate parsing.
#[test]
fn object_queries_and_filter_expressions() {
    let script = r#"
create_clock -name clk -period 2.0 [get_ports -regexp -nocase {^clk_.*}]
set_false_path -from [get_pins -hierarchical -of_objects [get_cells u_core] -filter "direction == in && is_hierarchical != true && full_name =~ *din*"] \
               -to [all_registers -clock [get_clocks clk] -edge_triggered -output_pins]
set_dont_use [get_lib_cells slow_lib/BUF*]
set_disable_timing [get_lib_pins slow_lib/BUF_X1/A]
set_dont_touch [get_nets -of_objects [get_pins u_core/z]]
set_max_delay 1.0 -from [all_inputs -no_clocks] -to [all_outputs]
set_max_fanout 8.0 [all_clocks]
"#;
    let (sdc, diags) = sdc::read_str(script);
    assert!(
        diags.iter().all(|d| d.severity != Severity::Error),
        "{diags:?}"
    );
    assert_eq!(sdc.cmds.len(), 7);

    if let CmdKind::SetFalsePath {
        from: Some(from_ep),
        ..
    } = &sdc.cmds[1].kind
    {
        let expected_filter = Filter::And(
            Box::new(Filter::And(
                Box::new(Filter::Atom(FilterAtom {
                    attr: "direction".to_string(),
                    op: FilterOp::Eq,
                    value: "in".to_string(),
                })),
                Box::new(Filter::Atom(FilterAtom {
                    attr: "is_hierarchical".to_string(),
                    op: FilterOp::Ne,
                    value: "true".to_string(),
                })),
            )),
            Box::new(Filter::Atom(FilterAtom {
                attr: "full_name".to_string(),
                op: FilterOp::Match,
                value: "*din*".to_string(),
            })),
        );
        assert_eq!(
            from_ep.obj,
            Obj::Pins(ObjQuery {
                patterns: Vec::new(),
                hierarchical: true,
                regexp: false,
                nocase: false,
                of_objects: Some(Box::new(Obj::cells(["u_core"]))),
                filter: Some(expected_filter),
                quiet: false,
            })
        );
    } else {
        panic!("expected SetFalsePath, got {:?}", sdc.cmds[1]);
    }
}

// SDC-REQ-0038, SDC-REQ-0039: DRC commands emit `D0021` (`Severity::Warning`) and synthesis
// commands emit `D0020` (`Severity::Note`) while recording typed `Cmd` nodes.
#[test]
fn drc_and_synthesis_commands_emit_d0021_and_d0020() {
    let dir = fixture_dir();
    let text =
        std::fs::read_to_string(dir.join("drc_and_synth.sdc")).expect("read drc_and_synth.sdc");
    let (sdc, diags) = sdc::read_str(&text);
    assert_eq!(sdc.cmds.len(), 16);
    assert!(
        diags.iter().all(|d| d.severity != Severity::Error),
        "{diags:?}"
    );
    let d0021_count = diags
        .iter()
        .filter(|d| d.code == "D0021" && d.severity == Severity::Warning)
        .count();
    let d0020_count = diags
        .iter()
        .filter(|d| d.code == "D0020" && d.severity == Severity::Note)
        .count();
    assert_eq!(d0021_count, 6, "{diags:?}");
    assert_eq!(d0020_count, 7, "{diags:?}");
}

// SDC-REQ-0002, SDC-REQ-0016, SDC-REQ-0018, SDC-REQ-0019: schema error codes `D0003` (unknown
// option), `D0004` (arity / duplicate / mutually exclusive), `D0005` (type mismatch), `D0010`
// (unknown or Phase 2b command), `D0011` (Phase 2b option `-through`, `-reference_pin`), and
// `D0030` (invalid unit).
#[test]
fn schema_and_phase_2b_diagnostics() {
    // D0003: unknown option
    let (_, d0003) = sdc::read_str("create_clock -bogus_flag 1.0 [get_ports clk]\n");
    assert!(d0003.iter().any(|d| d.code == "D0003"), "{d0003:?}");

    // D0004: missing required -period or mutually exclusive options
    let (_, d0004_req) = sdc::read_str("create_clock -name clk [get_ports clk]\n");
    assert!(d0004_req.iter().any(|d| d.code == "D0004"), "{d0004_req:?}");

    let (_, d0004_excl) = sdc::read_str(
        "set_clock_groups -asynchronous -physically_exclusive -group [get_clocks a]\n",
    );
    assert!(
        d0004_excl.iter().any(|d| d.code == "D0004"),
        "{d0004_excl:?}"
    );

    // D0005: non-numeric period
    let (_, d0005) = sdc::read_str("create_clock -name clk -period not_a_float [get_ports clk]\n");
    assert!(d0005.iter().any(|d| d.code == "D0005"), "{d0005:?}");

    // D0030: invalid unit scale on set_units
    let (_, d0030) = sdc::read_str("set_units -time fortnights\n");
    assert!(d0030.iter().any(|d| d.code == "D0030"), "{d0030:?}");

    // D0010: Phase 2b commands and unknown commands
    for cmd_line in [
        "create_generated_clock -name gclk -source [get_ports clk] -divide_by 2 [get_pins reg/Q]",
        "set_clock_sense -stop_propagation [get_pins mux/Z]",
        "set_timing_derate -early 0.95",
        "set_propagated_clock [all_clocks]",
        "unset_propagated_clock [all_clocks]",
        "set_sense -type clock -stop_propagation [get_pins mux/Z]",
        "set_data_check -from [get_pins a] -to [get_pins b] -setup 0.1",
        "set_ideal_network [get_ports rst_n]",
        "totally_unknown_command -foo bar",
    ] {
        let (_, diags) = sdc::read_str(cmd_line);
        assert!(
            diags
                .iter()
                .any(|d| d.code == "D0010" && d.severity == Severity::Error),
            "expected D0010 for `{cmd_line}`, got {diags:?}"
        );
    }

    // D0011: Phase 2b options (`-through`, `-rise_through`, `-fall_through`, `-reference_pin`)
    for opt_line in [
        "set_false_path -through [get_pins u1/Z]",
        "set_multicycle_path 2 -rise_through [get_pins u1/Z]",
        "set_max_delay 1.0 -fall_through [get_pins u1/Z]",
        "set_input_delay 0.5 -reference_pin [get_pins clk_buf/Z] [get_ports din]",
        // SDC-REQ-0023: edge flags of the single-clock uncertainty are refused, not dropped.
        "set_clock_uncertainty -setup -rise 0.3 [get_clocks clk]",
        "set_clock_uncertainty -fall 0.3 [get_clocks clk]",
    ] {
        let (_, diags) = sdc::read_str(opt_line);
        assert!(
            diags
                .iter()
                .any(|d| d.code == "D0011" && d.severity == Severity::Error),
            "expected D0011 for `{opt_line}`, got {diags:?}"
        );
    }
}
