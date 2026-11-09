// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors
//! Tiny cut-interface cases: <100 nodes, no SAT enumeration; bounded by the
//! ordinary 2-worker / 2 GiB test guard. Real vendor-model and external CEC
//! controls accompany the Ibex integration harness.
use rusteda_formats::liberty::read_text;
use rustsys::{Sources, SynthOptions, synthesize};
use rustsys_fixtures::liberty::{Variant, library};

fn run(source: &str, ram: &str) -> Result<rustsys::SynthOutput, rustsys::SynthError> {
    run_with(source, ram, Default::default())
}
fn run_with(
    source: &str,
    ram: &str,
    opaque_limits: rustsys::elab::OpaqueLimits,
) -> Result<rustsys::SynthOutput, rustsys::SynthError> {
    let standard = read_text(&library(Variant::Full), "std.lib").unwrap();
    let memory = read_text(ram, "memory.lib").unwrap();
    synthesize(
        &Sources::Texts(vec![("top.v".into(), source.into())]),
        &[standard, memory],
        &SynthOptions {
            top: Some("top".into()),
            opaque_limits,
            ..Default::default()
        },
    )
}

#[test]
fn original_bus_ranges_preserve_pin_identity() {
    for (range, indices) in [("7:4", [4, 5, 6, 7]), ("0:3", [3, 2, 1, 0])] {
        let pins = indices
            .map(|i| format!("pin(D[{i}]){{direction:input;}}pin(Q[{i}]){{direction:output;}}"));
        let ram = format!(
            "library(M){{cell(RAM){{is_macro_cell:true;area:100;{}}}}}",
            pins.join("")
        );
        let rtl = format!(
            "module RAM(input [{range}] D,output [{range}] Q); endmodule module top(input [3:0] a,output [3:0] y); RAM ram(.D(a),.Q(y)); endmodule"
        );
        let out = run(&rtl, &ram).unwrap();
        assert_eq!(out.report.macro_cells, 1);
        assert_eq!(
            out.netlist
                .instances
                .iter()
                .filter(|i| i.cell == "RAM")
                .count(),
            1
        );
    }
}

#[test]
fn interface_only_module_without_macro_library_is_rejected() {
    let err = run(RTL, "library(M){}").unwrap_err();
    assert!(err.messages.join("\n").contains("S0134"));
    assert!(err.messages.join("\n").contains("only an interface"));
}

#[test]
fn implemented_child_is_not_classified_as_a_macro() {
    let rtl = "module driven(output y); wire q=1'b1; assign y=q; endmodule module top(output y); driven d(.y(y)); endmodule";
    assert_eq!(run(rtl, "library(M){}").unwrap().report.macro_cells, 0);
}

#[test]
fn macro_expansion_obeys_configured_instance_and_pin_budgets() {
    for limits in [
        rustsys::elab::OpaqueLimits {
            max_instances: 0,
            max_pin_bits: 2,
        },
        rustsys::elab::OpaqueLimits {
            max_instances: 1,
            max_pin_bits: 1,
        },
    ] {
        let err = run_with(RTL, RAM, limits).unwrap_err();
        assert!(
            err.messages
                .join("\n")
                .contains("opaque macro budget exceeded")
        );
    }
}

#[test]
fn parameterized_or_inout_macros_are_explicitly_rejected() {
    let parameterized = RTL.replace("module RAM(input", "module RAM #(parameter W=1)(input");
    assert!(
        run(&parameterized, RAM)
            .unwrap_err()
            .messages
            .join("\n")
            .contains("parameterized opaque")
    );
    let inout = RTL.replace("input D", "inout D");
    assert!(
        run(&inout, RAM)
            .unwrap_err()
            .messages
            .join("\n")
            .contains("S0134")
    );
}
const RAM: &str = "library(M){cell(RAM){is_macro_cell:true;area:100;pin(D){direction:input;}pin(Q){direction:output;}}}";
const RTL: &str = "module RAM(input D,output Q); endmodule module top(input a, output y);wire q; RAM ram(.D(a),.Q(q)); assign y=~q; endmodule";
#[test]
fn preserved_macro_drives_logic_without_becoming_zero() {
    let out = run(RTL, RAM).unwrap();
    assert_eq!(out.report.macro_cells, 1);
    assert_eq!(out.report.cells.get("RAM"), Some(&1));
    assert!(
        out.netlist
            .ports
            .iter()
            .all(|p| !p.name.starts_with("__rustsys_macro_cut_"))
    );
    assert!(
        out.netlist
            .instances
            .iter()
            .any(|i| i.name == "ram" && i.cell == "RAM")
    );
    assert!(out.report.area >= 100.0);
}
#[test]
fn interface_mismatch_is_an_error() {
    let wrong = RAM.replace("pin(Q){direction:output;}", "pin(Q){direction:input;}");
    assert!(run(RTL, &wrong).is_err());
}
#[test]
fn independent_macros_and_constant_pins_survive() {
    let rtl = "module RAM(input D,output Q); endmodule module top(output a,output b);RAM r0(.D(1'b0),.Q(a));RAM r1(.D(1'b1),.Q(b));endmodule";
    let out = run(rtl, RAM).unwrap();
    assert_eq!(out.report.macro_cells, 2);
    assert_eq!(out.report.cells.get("RAM"), Some(&2));
    assert!(
        out.netlist
            .ports
            .iter()
            .all(|p| !p.name.starts_with("__rustsys_macro_cut_"))
    );
}
