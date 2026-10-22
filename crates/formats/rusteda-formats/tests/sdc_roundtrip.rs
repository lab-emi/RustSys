// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Integration tests for canonical SDC writing, `CONSTR v1` binary section round trips and
//! corruption rejection, and metamorphic equivalences (SDC-REQ-0001, SDC-REQ-0003,
//! SDC-REQ-0016, SDC-REQ-0040 to 0053).

use std::path::PathBuf;

use rusteda_core::diag::Severity;
use rusteda_formats::sdc::{self, Env, Limits, Sdc, Source, section_id};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("verification/fixtures/synthetic/sdc")
}

// SDC-REQ-0003, SDC-REQ-0040 to 0050: every synthetic fixture round-trips through both
// `sdc::write` -> `sdc::read_str` and `Sdc::to_sections` -> `Sdc::from_sections`.
#[test]
fn fixtures_roundtrip_canonical_text_and_constr_v1_sections() {
    let dir = fixture_dir();
    for name in [
        "basic.sdc",
        "tcl_subset.sdc",
        "exceptions.sdc",
        "drc_and_synth.sdc",
        "sourced_clocks.sdc",
    ] {
        let path = dir.join(name);
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let sources = [Source::new(path.to_string_lossy().into_owned(), raw)];
        let (sdc1, diags1) = sdc::read(&sources, &Env::default(), &Limits::default());
        assert!(
            diags1.iter().all(|d| d.severity != Severity::Error),
            "{name}: unexpected errors {diags1:?}"
        );

        // Canonical writer round-trip and idempotence (SDC-REQ-0003)
        let text1 = sdc::write(&sdc1);
        let (sdc2, diags2) = sdc::read_str(&text1);
        assert!(
            diags2.iter().all(|d| d.severity != Severity::Error),
            "{name}: canonical re-read errors {diags2:?}\ntext:\n{text1}"
        );
        assert_eq!(sdc1, sdc2, "{name}: canonical AST mismatch");
        let text2 = sdc::write(&sdc2);
        assert_eq!(text1, text2, "{name}: canonical text not idempotent");

        // Binary CONSTR v1 round-trip and bit-exact equality (SDC-REQ-0040 to 0050)
        let sections = sdc1.to_sections();
        assert_eq!(sections.len(), 13);
        let sdc_bin = Sdc::from_sections(&sections)
            .unwrap_or_else(|e| panic!("{name}: from_sections failed: {e}"));
        assert_eq!(sdc1, sdc_bin, "{name}: binary AST mismatch");
        assert!(
            sdc1.exact_eq(&sdc_bin),
            "{name}: binary round-trip not bit-exact"
        );
    }
}

// SDC-REQ-0049, SDC-REQ-0050: `Sdc::from_sections` rejects every corrupted section cleanly with
// `ConstrError` and never panics.
#[test]
fn constr_v1_rejects_corrupted_sections() {
    let raw = std::fs::read_to_string(fixture_dir().join("basic.sdc")).expect("read basic.sdc");
    let (sdc, _) = sdc::read_str(&raw);
    let base = sdc.to_sections();

    // Missing section
    assert!(Sdc::from_sections(&base[..12]).is_err());

    // Duplicate section
    let mut dup = base.clone();
    dup.push(base[0].clone());
    assert!(Sdc::from_sections(&dup).is_err());

    // Truncated HEADER payload
    let mut bad_hdr = base.clone();
    let hdr = bad_hdr
        .iter_mut()
        .find(|s| s.id == section_id::HEADER)
        .unwrap();
    hdr.bytes.truncate(10);
    assert!(Sdc::from_sections(&bad_hdr).is_err());

    // Invalid UTF-8 in STR_BYTES
    let mut bad_str = base.clone();
    let str_sec = bad_str
        .iter_mut()
        .find(|s| s.id == section_id::STR_BYTES)
        .unwrap();
    str_sec.bytes.push(0xFF);
    str_sec.count = str_sec.bytes.len() as u64;
    assert!(Sdc::from_sections(&bad_str).is_err());

    // Out-of-bounds string index in SOURCES
    let mut bad_src = base.clone();
    let src_sec = bad_src
        .iter_mut()
        .find(|s| s.id == section_id::SOURCES)
        .unwrap();
    src_sec.bytes[0..4].copy_from_slice(&999_999u32.to_le_bytes());
    assert!(Sdc::from_sections(&bad_src).is_err());

    // Invalid opcode in CMDS
    let mut bad_cmd = base;
    let cmd_sec = bad_cmd
        .iter_mut()
        .find(|s| s.id == section_id::CMDS)
        .unwrap();
    cmd_sec.bytes[0..2].copy_from_slice(&0xFFFEu16.to_le_bytes());
    assert!(Sdc::from_sections(&bad_cmd).is_err());
}

// SDC-REQ-0003, SDC-REQ-0016, SDC-REQ-0028: metamorphic properties:
// (a) `set_units -time ps` with values x1000 yields identical canonical `Cmd` values to `set_units -time ns` with values x1;
// (b) permuting option order on SDC commands produces an identical `Sdc`;
// (c) `[list a b]` vs `{a b}` produces an identical `Sdc`;
// (d) nested `-from [list [get_ports a] [get_ports b]]` in `-from` preserves `Obj::Union`.
#[test]
fn metamorphic_unit_scaling_option_order_list_quoting_and_nested_from() {
    // (a) Unit scaling equivalence: ps * 1000 == ns * 1
    let sdc_ns = sdc::read_str(
        "set_units -time ns -capacitance pF\n\
         create_clock -name clk -period 2.5 -waveform {0.0 1.25} [get_ports clk]\n\
         set_input_delay -clock [get_clocks clk] -max 0.5 [get_ports din]\n\
         set_load -pin_load 0.02 [get_ports dout]\n",
    )
    .0;
    let sdc_ps = sdc::read_str(
        "set_units -time ps -capacitance fF\n\
         create_clock -name clk -period 2500.0 -waveform {0.0 1250.0} [get_ports clk]\n\
         set_input_delay -clock [get_clocks clk] -max 500.0 [get_ports din]\n\
         set_load -pin_load 20.0 [get_ports dout]\n",
    )
    .0;
    assert_eq!(sdc_ns.cmds[1..], sdc_ps.cmds[1..]);

    // (b) Option permutation equivalence
    let perm_a = sdc::read_str(
        "create_clock -name clk -period 4.0 -waveform {0.0 2.0} -add [get_ports clk]\n\
         set_input_delay -clock [get_clocks clk] -max -rise -add_delay 0.6 [get_ports din]\n",
    )
    .0;
    let perm_b = sdc::read_str(
        "create_clock [get_ports clk] -add -waveform {0.0 2.0} -period 4.0 -name clk\n\
         set_input_delay 0.6 [get_ports din] -add_delay -rise -max -clock [get_clocks clk]\n",
    )
    .0;
    assert_eq!(perm_a, perm_b);
    assert_eq!(sdc::write(&perm_a), sdc::write(&perm_b));

    // (c) `[list a b]` vs `{a b}` equivalence
    let list_form =
        sdc::read_str("create_clock -name clk -period 2.0 [get_ports [list clk_a clk_b]]\n").0;
    let brace_form =
        sdc::read_str("create_clock -name clk -period 2.0 [get_ports {clk_a clk_b}]\n").0;
    assert_eq!(list_form, brace_form);

    // (d) Nested `-from [list [get_ports a] [get_pins u0/q]]` preserved across text and binary round-trips
    let nested = sdc::read_str(
        "set_false_path -from [list [get_ports a] [get_pins u0/q]] -to [list [get_ports b] [get_pins u1/d]]\n",
    )
    .0;
    let nested_text = sdc::write(&nested);
    let (nested_reread, diags) = sdc::read_str(&nested_text);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(nested, nested_reread);
    let nested_bin = Sdc::from_sections(&nested.to_sections()).expect("from_sections");
    assert_eq!(nested, nested_bin);
}
