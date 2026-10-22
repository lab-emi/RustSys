// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Verdicts and exit statuses of `rustsys check synth` (CHECK-REQ-0001, CHECK-REQ-0002,
//! CHECK-REQ-0023, CHECK-REQ-0081, CLI-REQ-0009): 0 for `equivalent` and
//! `initial-state-refinement`, 3 for `not-equivalent`, also when a budget ran out besides, 5 for
//! `undecided` and 1 for a netlist that the checker rejects, with the code of the construct.

use std::path::{Path, PathBuf};
use std::process::Output;

use rustsys_fixtures::liberty::{Variant, library};

fn dir(tag: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("check-status-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("rich.lib"), library(Variant::Rich)).unwrap();
    std::fs::write(d.join("full.lib"), library(Variant::Full)).unwrap();
    d
}

fn run(dir: &Path, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_rustsys"))
        .args(args)
        .current_dir(dir)
        .env_remove("RUSTSYS_RUNS_ROOT")
        .env_remove("NO_COLOR")
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn json(p: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// Each register is 1 only after the one before it was, under a condition that random simulation
/// never meets.
const CHAIN: &str = "module m(input logic clk, rn, input logic [19:0] a, output logic y);
  logic k1, k2, k3, k4, k5, k6;
  wire rare = &a;
  always_ff @(posedge clk or negedge rn)
    if (!rn) begin
      k1 <= 0; k2 <= 0; k3 <= 0; k4 <= 0; k5 <= 0; k6 <= 0;
    end else begin
      k1 <= rare; k2 <= k1 & rare; k3 <= k2 & rare;
      k4 <= k3 & rare; k5 <= k4 & rare; k6 <= k5 & rare;
    end
  assign y = k6;
endmodule
";

/// A netlist without the registers, its output tied to 0.
const TIED: &str = "module m(clk, rn, a, y);
  input clk; input rn; input [19:0] a; output y;
  wt_tielo_x1 t0 (.L(y));
endmodule
";

/// The tied netlist with an output that the RTL does not have.
const EXTRA_PORT: &str = "module m(clk, rn, a, y, z);
  input clk; input rn; input [19:0] a; output y; output z;
  wt_tielo_x1 t0 (.L(y));
  wt_tielo_x1 t1 (.L(z));
endmodule
";

// CHECK-REQ-0002, CLI-REQ-0009: a synthesized netlist is equivalent (0); the tied netlist differs
// at the output (3); with the reset pulse, discovery proposes the registers as constants and
// stops at its repetition limit with relations still rejected, so the check is undecided (5):
// the verdict on standard output, the limit as `warning[C0005]` with its help line, the run
// recorded as `undecided` and the report with format version 2.
#[test]
fn verdicts_have_their_exit_statuses() {
    let d = dir("verdicts");
    std::fs::write(d.join("m.sv"), CHAIN).unwrap();
    std::fs::write(d.join("tied.v"), TIED).unwrap();
    let o = run(&d, &["synth", "--lib", "rich.lib", "--out", "m.v", "m.sv"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let check = |netlist: &str, extra: &[&str]| {
        let mut a = vec!["check", "synth", "--lib", "rich.lib", "--netlist", netlist];
        a.extend_from_slice(extra);
        a.push("m.sv");
        run(&d, &a)
    };
    let o = check("m.v", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stdout).starts_with("verdict: equivalent ("));
    let o = check("tied.v", &[]);
    assert_eq!(o.status.code(), Some(3), "{}", text(&o.stderr));
    assert_eq!(
        text(&o.stdout),
        "mismatch[output]: y\nverdict: not-equivalent (outputs 1, state pairs 0, unmatched RTL state bits 6)\n"
    );
    let o = check("tied.v", &["--after-reset", "rn=0", "--run-dir", "run"]);
    assert_eq!(o.status.code(), Some(5), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert_eq!(
        out,
        "verdict: undecided (outputs 0, state pairs 0, unmatched RTL state bits 0)\n"
    );
    let err = text(&o.stderr);
    assert!(
        err.contains(
            "warning[C0005]: state correspondence discovery reached its limit of 4 repetitions"
        ) && err.contains("  help: inconclusive, not rejected")
            && err.contains("the verdict is undecided (exit status 5)")
            && !err.contains("error["),
        "{err}"
    );
    let rec = json(&d.join("run/run.json"));
    assert_eq!(rec["status"], "undecided");
    assert_eq!(rec["exit_status"], 5);
    let rep = json(&d.join("run/check.json"));
    assert_eq!(rep["format_version"], 2);
    assert_eq!(rep["verdict"], "undecided");
    assert_eq!(rep["undecided"].as_array().map(Vec::len), Some(1));
    // The report's default sections print what the command printed (CLI-REQ-0101).
    let o = run(&d, &["show", "run"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), out);
    let o = check("tied.v", &["--after-reset", "rn=0", "--print", "json"]);
    assert_eq!(o.status.code(), Some(5));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["verdict"],
        "undecided"
    );
    // A port difference decides although discovery stops at its limit (3), and the limit is
    // still reported.
    std::fs::write(d.join("extra.v"), EXTRA_PORT).unwrap();
    let o = check(
        "extra.v",
        &["--after-reset", "rn=0", "--report", "extra.json"],
    );
    assert_eq!(o.status.code(), Some(3), "{}", text(&o.stderr));
    assert_eq!(
        text(&o.stdout),
        "mismatch[ports]: RTL [] netlist [(\"z\", false, None)]\nverdict: not-equivalent (outputs 0, state pairs 0, unmatched RTL state bits 0)\n"
    );
    assert!(
        text(&o.stderr)
            .contains("warning[C0005]: state correspondence discovery reached its limit"),
        "{}",
        text(&o.stderr)
    );
    let rep = json(&d.join("extra.json"));
    assert_eq!(rep["verdict"], "not-equivalent");
    assert_eq!(rep["undecided"].as_array().map(Vec::len), Some(1));
    assert_eq!(rep["internal_error"], false);
}

/// An unreset register whose proven initial value synthesis chooses.
const UNRESET: &str = "module m(input clk, rn, en, a, output logic q, t, y);
  always_ff @(posedge clk) if (en) q <= t;
  always_ff @(posedge clk or negedge rn) if (!rn) t <= 0; else t <= q;
  always_ff @(posedge clk) y <= a ^ q;
endmodule
";

// CHECK-REQ-0050, CHECK-REQ-0081, CLI-REQ-0009: the initial-state refinement is spelled
// `initial-state-refinement` in the text and the JSON report, with exit status 0.
#[test]
fn initial_state_refinement_spelling() {
    let d = dir("refinement");
    std::fs::write(d.join("m.sv"), UNRESET).unwrap();
    let mode = ["--after-reset", "rn=0", "--uninitialized-dont-care"];
    let mut a = vec!["synth", "--lib", "full.lib", "--out", "m.v"];
    a.extend_from_slice(&mode);
    a.push("m.sv");
    let o = run(&d, &a);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let mut a = vec!["check", "synth", "--lib", "full.lib", "--netlist", "m.v"];
    a.extend_from_slice(&mode);
    a.extend_from_slice(&["--report", "check.json", "m.sv"]);
    let o = run(&d, &a);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(
        text(&o.stdout).starts_with("verdict: initial-state-refinement ("),
        "{}",
        text(&o.stdout)
    );
    assert_eq!(
        json(&d.join("check.json"))["verdict"],
        "initial-state-refinement"
    );
}

/// Cells whose behavior lies in Liberty attributes that the library model does not keep.
const PROBE_LIB: &str = r#"library (probe) {
  cell (od_buf) {
    area : 1;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "A"; driver_type : open_drain; }
  }
  cell (xf_buf) {
    area : 1;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "A"; x_function : "!A"; }
  }
}
"#;

// CHECK-REQ-0023, CLI-REQ-0004: a netlist with a construct outside the checker's model is
// rejected (1) with the construct's code, its location and the code's help line, and nothing on
// standard output; the command reads the Liberty attributes that the library model does not keep
// from the library text.
#[test]
fn unmodeled_constructs_are_rejected_with_their_codes() {
    let d = dir("unmodeled");
    std::fs::write(
        d.join("m.sv"),
        "module m(input a, output y);\n  assign y = a;\nendmodule\n",
    )
    .unwrap();
    std::fs::write(d.join("probe.lib"), PROBE_LIB).unwrap();
    for (net, lib, code, help) in [
        (
            "module m(a, y);\n  input a; output y;\n  od_buf u1 (.A(a), .Y(y));\nendmodule\n",
            "probe.lib",
            "error[C0101]: m.v:3:10: instance u1 of cell od_buf: pin Y has driver_type",
            "  help: the checker does not model high impedance or drive strengths",
        ),
        (
            "module m(a, y);\n  input a; output y;\n  xf_buf u1 (.A(a), .Y(y));\nendmodule\n",
            "probe.lib",
            "error[C0104]: m.v:3:10: instance u1 of cell xf_buf: pin Y has x_function",
            "  help: the checker models cells described by function",
        ),
        (
            "module m(a, y);\n  input a; output y;\n  wtf_ebufn_1 u1 (.A(a), .TE_B(1'b1), .Z(y));\nendmodule\n",
            "full.lib",
            "error[C0101]: m.v:3:",
            "  help: the checker does not model high impedance",
        ),
        (
            "module m(a, y);\n  input a; output y;\n  assign y = 1'bz;\nendmodule\n",
            "rich.lib",
            "error[C0103]: m.v:3:",
            "  help: write constants as 1'b0 or 1'b1",
        ),
        (
            "module m(a, y);\n  input a; output y;\n  assign (highz1, strong0) y = a;\nendmodule\n",
            "rich.lib",
            "error[C0101]: m.v:3:",
            "  help: the checker does not model high impedance",
        ),
        (
            "module m(a, y);\n  input a; output y;\n  supply1 s;\n  wt_inv_x1 u1 (.A(a), .Y(s));\n  assign y = a;\nendmodule\n",
            "rich.lib",
            "error[C0105]: m.v:4:",
            "  help: the checker models nets with one driver",
        ),
    ] {
        std::fs::write(d.join("m.v"), net).unwrap();
        let o = run(
            &d,
            &["check", "synth", "--lib", lib, "--netlist", "m.v", "m.sv"],
        );
        assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
        assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
        let err = text(&o.stderr);
        assert!(err.contains(code) && err.contains(help), "{err}");
    }
}

/// Distributivity of 8-bit products: the netlist is synthesized from `a * c + b * c` and checked
/// against `(a + b) * c`, which share too few internal points for sweeping at its default budget.
const DIST_NETLIST_RTL: &str = "module m(input logic [7:0] a, b, c, output logic [16:0] y);
  assign y = a * c + b * c;
endmodule
";

const DIST: &str = "module m(input logic [7:0] a, b, c, output logic [16:0] y);
  assign y = (a + b) * c;
endmodule
";

/// Whether `y` depends on its own value is a proof that the checker's RTL model needs.
const OWN: &str = "module m(input logic [5:0] a, b, output logic y);
  always_comb if (a * b != b * a) y = y; else y = 1'b0;
endmodule
";

// CHECK-REQ-0002, CLI-REQ-0009, PROTO-REQ-0020: `--budget` and `--model-budget` set the conflict
// budgets; one that runs out leaves the check undecided (5) with a `warning[C0005]` that names
// it, and the canonical command spells both out. An erroneous netlist is still rejected (1).
#[test]
fn budgets_are_options_and_undecided_is_status_5() {
    let d = dir("budgets");
    std::fs::write(d.join("s.sv"), DIST_NETLIST_RTL).unwrap();
    std::fs::write(d.join("dist.sv"), DIST).unwrap();
    let o = run(
        &d,
        &["synth", "--lib", "rich.lib", "--out", "dist.v", "s.sv"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let check = |rtl: &str, net: &str, extra: &[&str]| {
        let mut a = vec!["check", "synth", "--lib", "rich.lib", "--netlist", net];
        a.extend_from_slice(extra);
        a.push(rtl);
        run(&d, &a)
    };
    let o = check("dist.sv", "dist.v", &["--budget", "0", "--run-dir", "run"]);
    assert_eq!(o.status.code(), Some(5), "{}", text(&o.stderr));
    assert!(text(&o.stdout).starts_with("verdict: undecided ("));
    let err = text(&o.stderr);
    assert!(
        err.contains("warning[C0005]: output y[16]: the conflict budget of 0 per proof ran out")
            && err.contains("--budget and --model-budget raise the conflict budgets")
            && !err.contains("error["),
        "{err}"
    );
    let rec = json(&d.join("run/run.json"));
    assert_eq!(rec["exit_status"], 5);
    assert!(
        rec["command"]
            .as_str()
            .is_some_and(|c| c.contains(" --budget 0 --model-budget 200000 ")),
        "{rec}"
    );
    std::fs::write(d.join("own.sv"), OWN).unwrap();
    let own = |body: &str| {
        format!(
            "module m(a, b, y);\n  input [5:0] a; input [5:0] b; output y;\n  {body}\nendmodule\n"
        )
    };
    std::fs::write(d.join("own.v"), own("wt_tielo_x1 t0 (.L(y));")).unwrap();
    let o = check("own.sv", "own.v", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let o = check("own.sv", "own.v", &["--model-budget", "0"]);
    assert_eq!(o.status.code(), Some(5), "{}", text(&o.stderr));
    assert!(
        text(&o.stderr).contains("y depends on its own value")
            && text(&o.stderr).contains("conflict budget of 0 per proof"),
        "{}",
        text(&o.stderr)
    );
    std::fs::write(d.join("bad.v"), own("wt_nosuch t0 (.L(y));")).unwrap();
    let o = check("own.sv", "bad.v", &["--model-budget", "0"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
    assert!(
        text(&o.stderr).contains("error[C0001]: bad.v:3:3: unknown cell wt_nosuch")
            && !text(&o.stderr).contains("C0005"),
        "{}",
        text(&o.stderr)
    );
}
