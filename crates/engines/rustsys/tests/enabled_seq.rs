// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Real-PDK enabled/hold sequential mapping controls.
//! Two shared immutable libraries via OnceLock (~200MiB conservative), <1000
//! nodes/fixture, at most two test workers: expected<1GiB, run under a4GiB guard.
//! These tests export evidence; external LEC of every resulting netlist is
//! mandatory before claiming validation. Missing PDKs are errors, never passes.

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusteda_formats::liberty::Library;
use rustsys::{Goal, Sources, SynthOptions, synthesize};

const SOURCE: &str = r#"
module enabled(input clk,en,rst_n,input [7:0] d,
               output reg [7:0] q,qr,output reg qfall,qlow);
  always @(posedge clk) if(en) q<=d;
  always @(posedge clk or negedge rst_n) if(!rst_n) qr<=0; else if(en) qr<=d;
  always @(negedge clk) if(en) qfall<=d[0];
  always @(posedge clk) if(!en) qlow<=d[1];
endmodule
"#;

fn paths() -> [PathBuf; 2] {
    let root = std::env::var_os("RUSTSYS_PDK_ROOT")
        .map(PathBuf::from)
        .expect("UNVERIFIED: RUSTSYS_PDK_ROOT is required");
    [
        root.join("sky130_fd_sc_hd__tt_025C_1v80.lib"),
        root.join("gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib"),
    ]
}

fn libraries() -> &'static [Library; 2] {
    static LIBS: OnceLock<[Library; 2]> = OnceLock::new();
    LIBS.get_or_init(|| {
        paths().map(|p| {
            let text = std::fs::read_to_string(&p).expect("UNVERIFIED: pinned PDK missing");
            assert!(text.len() < 64 * 1024 * 1024, "PDK source byte guard");
            rusteda_formats::liberty::read_text(&text, &p.to_string_lossy()).unwrap()
        })
    })
}

fn export(index: usize, label: &str) {
    let lib = &libraries()[index];
    let design = common::design(SOURCE);
    let eligible = rustsys::map::library::CellLib::build(std::slice::from_ref(lib), &[]).unwrap();
    assert!(
        eligible.seq.iter().any(|c| c.enable.is_some()),
        "PDK must exercise an actual supported enable mode"
    );
    if index == 1 {
        assert!(
            eligible.seq.iter().any(|c| c.hold.is_some()),
            "GF180 must exercise external hold feedback"
        );
    }
    let selected = rustsys::map::select_seq(&design, &eligible).unwrap();
    assert!(
        selected.iter().filter(|c| c.enable.is_some()).count() >= 8,
        "at least the enabled data bank must use the new mode"
    );
    assert!(
        rustsys::map::select_seq_with_limits(
            &design,
            &eligible,
            rustsys::map::SeqLimits {
                max_states: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
    let sources = Sources::Texts(vec![("enabled.v".into(), SOURCE.into())]);
    let options = SynthOptions {
        goal: Goal::Area,
        ..Default::default()
    };
    let synth = |threads| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| synthesize(&sources, std::slice::from_ref(lib), &options).unwrap())
    };
    let serial = synth(1);
    let parallel = synth(16);
    assert_eq!(serial.text, parallel.text);
    assert_eq!(serial.report.area, parallel.report.area);
    assert_eq!(parallel.report.state_bits_retained, 18);
    let root = std::env::var_os("RUSTSYS_SEQ_FIXTURES_OUT")
        .expect("UNVERIFIED: evidence output directory required");
    let out = Path::new(&root).join(label);
    std::fs::create_dir_all(&out).unwrap();
    for name in ["rtl.v", "netlist.v", "report.json"] {
        assert!(!out.join(name).exists(), "do not overwrite prior evidence");
    }
    std::fs::write(out.join("rtl.v"), SOURCE).unwrap();
    std::fs::write(out.join("netlist.v"), &parallel.text).unwrap();
    std::fs::write(
        out.join("report.json"),
        serde_json::to_vec_pretty(&parallel.report).unwrap(),
    )
    .unwrap();
    std::fs::write(out.join("metadata.json"),serde_json::to_vec_pretty(&serde_json::json!({
        "library":paths()[index],"top":"enabled","threads":[1,16],"netlist_identical":true,
        "native_selected_enable_states":selected.iter().filter(|c|c.enable.is_some()).count(),
        "native_selected_external_hold_states":selected.iter().filter(|c|c.hold.is_some()).count(),
        "LEC":"not_run","initial_state":"arbitrary; no reset assumption"
    })).unwrap()).unwrap();
}

// SYNTH-REQ-0520, SYNTH-REQ-0523, SYNTH-REQ-0601: real-library dual-PDK coverage.
#[test]
#[ignore = "requires pinned PDKs and a fresh evidence directory; external LEC follows"]
fn sky130_enabled_state_controls() {
    export(0, "sky130hd_tt");
}

// SYNTH-REQ-0520, SYNTH-REQ-0523, SYNTH-REQ-0601: real scan-cell feedback binding.
#[test]
#[ignore = "requires pinned PDKs and a fresh evidence directory; external LEC follows"]
fn gf180_enabled_state_controls() {
    export(1, "gf180mcu7t_tt");
}
