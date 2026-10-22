// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The netlists of the corpus designs pass through the core netlist model unchanged
//! (VERILOG-REQ-0300, VERILOG-REQ-0301): the writer model converts to a `GateNetlist`, through
//! its frozen tables and back to the same text, and reading the text gives the same tables.
//! Connections keep their order, so this holds whatever order a library lists its pins in.

use rusteda_core::netlist::PinDir;
use rusteda_formats::liberty::{Direction, Library};
use rusteda_formats::verilog::netlist::{from_gate_netlist, read_structural, to_gate_netlist};
use rusteda_formats::verilog::write::write_netlist;
use rustsys::{Goal, Sources, SynthOptions, synthesize};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;

/// Pins of a library cell in library order, or in reverse order.
fn cell_pins(lib: &Library, reverse: bool) -> impl Fn(&str) -> Option<Vec<(String, PinDir)>> + '_ {
    move |name| {
        let cell = lib.cells.iter().find(|c| c.name == name)?;
        let dir = |d| match d {
            Direction::Input => PinDir::Input,
            Direction::Output => PinDir::Output,
            Direction::Inout => PinDir::Inout,
            Direction::Internal => PinDir::Other,
        };
        let mut pins: Vec<(String, PinDir)> = cell
            .pins
            .iter()
            .map(|p| (p.name.clone(), dir(p.direction)))
            .collect();
        if reverse {
            pins.reverse();
        }
        Some(pins)
    }
}

/// Synthesizes the corpus designs except `skip` with one library and goal of the corpus suite,
/// optionally with load-aware buffering and sizing, and checks both round trips; the
/// conversions see the library's pins in reverse order with `reverse_pins`.
fn run(
    variant: Variant,
    goal: Goal,
    electrical: Option<rustsys::electrical::Options>,
    skip: &[&str],
    reverse_pins: bool,
) {
    let lib = rusteda_formats::liberty::read_text(&library(variant), variant.name()).unwrap();
    let pins = cell_pins(&lib, reverse_pins);
    let mut failures = Vec::new();
    let mut checked = 0;
    for d in CORPUS.iter().filter(|d| !skip.contains(&d.name)) {
        let files = d
            .files
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        let opts = SynthOptions {
            top: Some(d.top.to_string()),
            goal,
            electrical: electrical.clone(),
            ..SynthOptions::default()
        };
        let out = match synthesize(&Sources::Texts(files), std::slice::from_ref(&lib), &opts) {
            Ok(o) => o,
            // A library without latches cannot implement the latch design.
            Err(_) if variant == Variant::Minimal && d.name == "latches" => continue,
            Err(e) => panic!("{} synthesis failed: {}", d.name, e.messages.join("\n")),
        };
        let tag = format!("{} [{variant:?} {goal:?}]", d.name);
        assert_eq!(write_netlist(&out.netlist), out.text, "{tag}");
        let frozen = match to_gate_netlist(&out.netlist, &pins) {
            Ok(g) => g.freeze(),
            Err(e) => {
                failures.push(format!("{tag}: conversion failed: {e}"));
                continue;
            }
        };
        let back = frozen
            .thaw()
            .map_err(|e| e.to_string())
            .and_then(|g| from_gate_netlist(&g).map_err(|e| e.to_string()));
        match back {
            Ok(m) if write_netlist(&m) == out.text => {}
            Ok(m) => failures.push(format!(
                "{tag}: the text changed:\n{}",
                first_difference(&out.text, &write_netlist(&m))
            )),
            Err(e) => failures.push(format!("{tag}: conversion back failed: {e}")),
        }
        match read_structural(&out.text, &format!("{}_mapped.v", d.name), &pins) {
            Ok(g) if g.freeze() == frozen => {}
            Ok(_) => failures.push(format!("{tag}: the read netlist differs")),
            Err(e) => failures.push(format!("{tag}: reading failed:\n{e}")),
        }
        checked += 1;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        checked + 1 + skip.len() >= CORPUS.len(),
        "{checked} designs checked"
    );
}

/// The first differing line of two texts, for failure messages.
fn first_difference(a: &str, b: &str) -> String {
    let (mut la, mut lb) = (a.lines(), b.lines());
    for k in 1.. {
        match (la.next(), lb.next()) {
            (None, None) => break,
            (x, y) if x != y => return format!("line {k}: {x:?} became {y:?}"),
            _ => {}
        }
    }
    String::new()
}

#[test]
fn corpus_rich_delay() {
    run(Variant::Rich, Goal::Delay, None, &[], false);
}

#[test]
fn corpus_rich_area() {
    run(Variant::Rich, Goal::Area, None, &[], false);
}

#[test]
fn corpus_reset_only() {
    run(Variant::ResetOnly, Goal::Delay, None, &[], false);
}

#[test]
fn corpus_minimal() {
    run(Variant::Minimal, Goal::Area, None, &[], false);
}

#[test]
fn corpus_full() {
    run(Variant::Full, Goal::Delay, None, &[], false);
}

// Netlists rewritten by buffering and sizing (new buffer cells and nets, other drive
// strengths) round-trip too. The processor design is left out: its buffering alone takes
// longer than all other runs together, and the runs above cover its netlist.
#[test]
fn corpus_full_electrical() {
    let opts = rustsys::electrical::Options {
        period_ns: 1.0,
        buffer_fanout: 4,
        ..Default::default()
    };
    run(Variant::Full, Goal::Delay, Some(opts), &["rv_core"], false);
}

// The engine writes connections in its own order, which the synthetic libraries' pin order
// happens to match; with the pins listed in reverse (outputs first) the round trips still hold.
#[test]
fn corpus_rich_reversed_pins() {
    run(Variant::Rich, Goal::Delay, None, &[], true);
}
