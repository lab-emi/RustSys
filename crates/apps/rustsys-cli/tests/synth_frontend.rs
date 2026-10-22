// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `synth-frontend`: preprocessing, top selection, parameter overrides, elaboration-time
//! tasks, diagnostics and supported constructs of RustSys. Functional results are proven with
//! the independent checker.

mod common;

use std::path::PathBuf;
use std::process::Command;

use rusteda_formats::liberty::Library;
use rusteda_formats::verilog::write::NlDir;
use rustsys::{Effort, Goal, Sources, SynthError, SynthOptions, SynthOutput, synthesize};
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_fixtures::liberty::Variant;

fn lib() -> Library {
    common::lib(Variant::Rich)
}

fn texts(files: &[(&str, &str)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect()
}

fn run(files: &[(&str, &str)], opts: SynthOptions) -> Result<SynthOutput, SynthError> {
    synthesize(&Sources::Texts(texts(files)), &[lib()], &opts)
}

fn width(o: &SynthOutput, name: &str) -> Option<Option<u32>> {
    o.netlist
        .ports
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.width)
}

/// Proves the netlist equivalent to the RTL texts with the checker.
fn equivalent(files: &[(&str, &str)], top: &str, o: &SynthOutput) {
    let rep = common::check_texts(&texts(files), top, &o.text, &lib());
    assert_eq!(
        rep.verdict,
        Verdict::Equivalent,
        "{top}: {:?} {:?}",
        rep.mismatches,
        rep.errors
    );
}

// SYNTH-REQ-0405, 0530 to 0535: the independently elaborated RTL agrees with the
// constrained netlist, including asynchronous state, carries and arithmetic selection.
#[test]
fn electrical_refinement_preserves_rtl_and_state_contract() {
    let src = r#"module arithmetic(input logic clk, rn, en,
        input logic [8:0] a, b, output logic [9:0] y, output logic [9:0] q);
        assign y = en ? ({1'b0,a} + {1'b0,b}) : ({1'b0,a} - {1'b0,b});
        always_ff @(posedge clk or negedge rn)
          if (!rn) q <= 10'b0; else if (en) q <= y;
        endmodule"#;
    for goal in [Goal::Area, Goal::Delay] {
        let out = run(
            &[("arithmetic.sv", src)],
            SynthOptions {
                goal,
                electrical: Some(rustsys::electrical::Options {
                    period_ns: 1.5,
                    buffer_fanout: 4,
                    ..Default::default()
                }),
                ..SynthOptions::default()
            },
        )
        .unwrap();
        equivalent(&[("arithmetic.sv", src)], "arithmetic", &out);
        let e = out.report.electrical.as_ref().unwrap();
        assert!((out.report.area - e.area_after).abs() < 1e-8);
        assert!(
            out.report
                .phase_s
                .iter()
                .any(|(phase, _)| phase == "electrical")
        );
    }
    let out = run(
        &[("arithmetic.sv", src)],
        SynthOptions {
            electrical: Some(rustsys::electrical::Options {
                period_ns: 0.001,
                area_limit: Some(1.0),
                ..Default::default()
            }),
            ..SynthOptions::default()
        },
    )
    .unwrap();
    let e = out.report.electrical.as_ref().unwrap();
    assert!(!e.area_limit_met && !e.estimated_timing_met);
    assert!(out.messages.iter().any(|m| m.contains("S0532")));
    assert!(out.messages.iter().any(|m| m.contains("S0533")));
}

/// A fresh directory under the OS temp dir.
fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustsys-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// SYNTH-REQ-0102: `SYNTHESIS` is predefined unless disabled; `-D` defines macros; includes are
// found next to the including file first, then in the `-I` directories in order.
#[test]
fn preprocessor() {
    let dir = temp_dir("pp");
    let (main, other) = (dir.join("main"), dir.join("other"));
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        main.join("top.sv"),
        "`include \"inc.svh\"\n`include \"only.svh\"\n\
         module pp (input logic [`W-1:0] a,\n\
         `ifdef SYNTHESIS output logic [`W-1:0] s `else output logic [`W-1:0] n `endif);\n\
         `ifdef SYNTHESIS assign s = `F(a); `else assign n = `F(a); `endif\nendmodule\n",
    )
    .unwrap();
    std::fs::write(main.join("inc.svh"), "`define F(x) `G(~(x))\n").unwrap();
    std::fs::write(other.join("inc.svh"), "`define F(x) (x)\n").unwrap();
    std::fs::write(other.join("only.svh"), "`define G(x) ((x) ^ 1)\n").unwrap();
    let opts = |predef: bool| SynthOptions {
        top: Some("pp".into()),
        defines: vec![("W".into(), "3".into())],
        include_dirs: vec![other.clone()],
        no_predefined_macros: !predef,
        ..SynthOptions::default()
    };
    let src = Sources::Files(vec![main.join("top.sv")]);
    let o = synthesize(&src, &[lib()], &opts(true)).unwrap();
    assert_eq!(width(&o, "s"), Some(Some(3)));
    assert_eq!(width(&o, "n"), None);
    // The function is ~a ^ 1 (the local include wins over the -I directory).
    let copts = CheckOptions {
        rtl: Input::Files(vec![main.join("top.sv")]),
        top: Some("pp".into()),
        defines: vec![("W".into(), "3".into())],
        include_dirs: vec![other.clone()],
        netlist: Input::Texts(vec![("n.v".into(), o.text.clone())]),
        ..CheckOptions::default()
    };
    assert_eq!(check(&copts, &[lib()]).verdict, Verdict::Equivalent);
    let wrong =
        "module pp (input logic [2:0] a, output logic [2:0] s);\n assign s = a;\nendmodule\n";
    let rep = common::check_texts(&texts(&[("w.sv", wrong)]), "pp", &o.text, &lib());
    assert_eq!(rep.verdict, Verdict::NotEquivalent);
    let o = synthesize(&src, &[lib()], &opts(false)).unwrap();
    assert_eq!(width(&o, "n"), Some(Some(3)));
    assert_eq!(width(&o, "s"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

// SYNTH-REQ-0104: without `--top`, the unique uninstantiated module is the top; otherwise the run
// fails and lists the candidates.
#[test]
fn top_selection() {
    let leaf = "module leaf (input logic a, output logic y); assign y = ~a; endmodule\n";
    let top = "module chip (input logic a, output logic y); leaf u (.a(a), .y(y)); endmodule\n";
    let o = run(&[("l.sv", leaf), ("t.sv", top)], SynthOptions::default()).unwrap();
    assert_eq!(o.netlist.name, "chip");
    let other = "module spare (input logic b, output logic z); assign z = b; endmodule\n";
    let Err(e) = run(
        &[("l.sv", leaf), ("t.sv", top), ("s.sv", other)],
        SynthOptions::default(),
    ) else {
        panic!("two candidates must fail");
    };
    let text = e.messages.join("\n");
    assert!(text.contains("chip") && text.contains("spare"), "{text}");
    let o = run(
        &[("l.sv", leaf), ("t.sv", top), ("s.sv", other)],
        SynthOptions {
            top: Some("spare".into()),
            ..SynthOptions::default()
        },
    )
    .unwrap();
    assert_eq!(o.netlist.name, "spare");
}

// SYNTH-REQ-0105: `-P` overrides top-level parameters with constant expressions; overriding a
// localparam or an unknown name is an error.
#[test]
fn parameter_overrides() {
    let m = "module pw #(parameter int W = 2, localparam int L = W + 1) \
             (input logic [W-1:0] a, output logic [L-1:0] y); assign y = a + 1'b1; endmodule\n";
    let with = |p: &[(&str, &str)]| {
        run(
            &[("p.sv", m)],
            SynthOptions {
                params: p
                    .iter()
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .collect(),
                ..SynthOptions::default()
            },
        )
    };
    let o = with(&[("W", "2 * 3")]).unwrap();
    assert_eq!(width(&o, "a"), Some(Some(6)));
    assert_eq!(width(&o, "y"), Some(Some(7)));
    assert!(
        with(&[("L", "4")]).is_err(),
        "localparam override must fail"
    );
    assert!(
        with(&[("NOPE", "4")]).is_err(),
        "unknown parameter must fail"
    );
}

// SYNTH-REQ-0124: `$error` and `$fatal` in generate constructs fail the run with their message;
// `$warning` and `$info` issue diagnostics.
#[test]
fn elaboration_tasks() {
    let m = |task: &str| {
        format!(
            "module et #(parameter int N = 1) (input logic a, output logic y);\n\
             if (N > 0) begin : g {task}(\"N is %0d\", N); end\n assign y = a;\nendmodule\n"
        )
    };
    for t in ["$error", "$fatal"] {
        let e = run(&[("e.sv", &m(t))], SynthOptions::default())
            .err()
            .unwrap_or_else(|| panic!("{t} must fail"));
        assert!(
            e.messages.join("\n").contains("N is"),
            "{t}: {:?}",
            e.messages
        );
    }
    for t in ["$warning", "$info"] {
        let o = run(&[("e.sv", &m(t))], SynthOptions::default()).unwrap();
        assert!(
            o.messages.iter().any(|x| x.contains("N is")),
            "{t}: {:?}",
            o.messages
        );
    }
}

// SYNTH-REQ-0141, SYNTH-REQ-0702: rejected constructs give `error[S<nnnn>]: file:line:column:`
// diagnostics on standard error and exit status 1; success exits with 0.
#[test]
fn diagnostics_and_exit_status() {
    let dir = temp_dir("diag");
    let bad = dir.join("bad.sv");
    std::fs::write(
        &bad,
        "module bad (input logic a, output logic y);\n  defparam u.X = 1;\n  assign y = a;\nendmodule\n",
    )
    .unwrap();
    let good = dir.join("good.sv");
    std::fs::write(
        &good,
        "module good (input logic a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();
    let lib_path = dir.join("rich.lib");
    std::fs::write(&lib_path, rustsys_fixtures::liberty::library(Variant::Rich)).unwrap();
    let exe = env!("CARGO_BIN_EXE_rustsys");
    let out = Command::new(exe)
        .args(["synth", "--lib"])
        .arg(&lib_path)
        .arg("--out")
        .arg(dir.join("bad_out.v"))
        .arg(&bad)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err
        .lines()
        .find(|l| l.starts_with("error["))
        .unwrap_or_else(|| panic!("no error line: {err}"));
    // error[Snnnn]: <file>:2:<col>: <message>
    let code = &line[6..11];
    assert!(
        code.starts_with('S') && code[1..].chars().all(|c| c.is_ascii_digit()),
        "{line}"
    );
    assert!(
        line[11..].starts_with("]: ") && line.contains("bad.sv:2:"),
        "{line}"
    );
    let out = Command::new(exe)
        .args(["synth", "--lib"])
        .arg(&lib_path)
        .arg("--out")
        .arg(dir.join("good_out.v"))
        .arg(&good)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A command-line error rejects the input as well.
    let out = Command::new(exe)
        .args(["synth", "--no-such-option"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

// SYNTH-REQ-0217: a `z` value is read per SYNTH-REQ-0201, with a warning; no tri-state cells.
#[test]
fn tristate_values_warn() {
    let m = "module tz (input logic en, input logic a, output logic y);\n\
             assign y = en ? a : 1'bz;\nendmodule\n";
    let o = run(&[("z.sv", m)], SynthOptions::default()).unwrap();
    assert!(
        o.messages.iter().any(|x| x.starts_with("warning[")),
        "{:?}",
        o.messages
    );
    equivalent(&[("z.sv", m)], "tz", &o);
}

// SYNTH-REQ-0306: declaration initializers of edge-driven variables are ignored with a warning.
#[test]
fn state_initializers_ignored() {
    let m = "module si (input logic clk, input logic [1:0] d, output logic [1:0] q);\n\
             logic [1:0] r = 2'b01;\n always_ff @(posedge clk) r <= d;\n assign q = r;\nendmodule\n";
    let o = run(&[("i.sv", m)], SynthOptions::default()).unwrap();
    assert!(
        o.messages.iter().any(|x| x.starts_with("warning[")),
        "{:?}",
        o.messages
    );
    assert_eq!(o.report.state_bits_retained, 2);
    equivalent(&[("i.sv", m)], "si", &o);
}

// SYNTH-REQ-0307: a `negedge` clock is a `posedge` clock of the complemented expression.
#[test]
fn negedge_is_posedge_of_complement() {
    let neg = "module ne (input logic clk, input logic d, output logic q);\n\
               always_ff @(negedge clk) q <= d;\nendmodule\n";
    let pos = "module ne (input logic clk, input logic d, output logic q);\n\
               logic nclk;\n assign nclk = ~clk;\n always_ff @(posedge nclk) q <= d;\nendmodule\n";
    let a = run(&[("n.sv", neg)], SynthOptions::default()).unwrap();
    let b = run(&[("p.sv", pos)], SynthOptions::default()).unwrap();
    // Each netlist is equivalent to both descriptions.
    for o in [&a, &b] {
        equivalent(&[("n.sv", neg)], "ne", o);
        equivalent(&[("p.sv", pos)], "ne", o);
    }
    let wrong = "module ne (input logic clk, input logic d, output logic q);\n\
                 always_ff @(posedge clk) q <= d;\nendmodule\n";
    let rep = common::check_texts(&texts(&[("w.sv", wrong)]), "ne", &a.text, &lib());
    assert_eq!(rep.verdict, Verdict::NotEquivalent);
}

// SYNTH-REQ-0605: generated net and instance names never collide with ports or state names, even
// when the design uses names in the style of generated ones.
#[test]
fn generated_names_do_not_collide() {
    let lib = lib();
    let base = "module gn (input logic clk, input logic [3:0] a, input logic [3:0] b, \
                output logic [3:0] y);\n logic [3:0] r;\n always_ff @(posedge clk) r <= a ^ b;\n\
                assign y = r + a;\nendmodule\n";
    let o = run(&[("g.sv", base)], SynthOptions::default()).unwrap();
    // Reuse generated names as port and state names.
    let mut taken: Vec<String> = o.netlist.wires.iter().map(|(n, _)| n.clone()).collect();
    taken.extend(o.netlist.instances.iter().map(|i| i.name.clone()));
    let simple: Vec<&String> = taken
        .iter()
        .filter(|n| rusteda_formats::verilog::write::is_simple_ident(n) && !n.starts_with('r'))
        .take(3)
        .collect();
    assert!(simple.len() >= 2, "{taken:?}");
    let m = format!(
        "module gn (input logic clk, input logic [3:0] {a}, input logic [3:0] b, \
         output logic [3:0] {y});\n logic [3:0] {r};\n always_ff @(posedge clk) {r} <= {a} ^ b;\n\
         assign {y} = {r} + {a};\nendmodule\n",
        a = simple[0],
        y = simple[1],
        r = simple.get(2).map_or("st", |s| s.as_str()),
    );
    let o = run(&[("g.sv", &m)], SynthOptions::default()).unwrap();
    let ports: Vec<&str> = o.netlist.ports.iter().map(|p| p.name.as_str()).collect();
    let mut seen = std::collections::HashSet::new();
    for (w, _) in &o.netlist.wires {
        assert!(
            !ports.contains(&w.as_str()),
            "wire {w} collides with a port"
        );
        assert!(seen.insert(w.clone()), "wire {w} declared twice");
    }
    let mut inst = std::collections::HashSet::new();
    for i in &o.netlist.instances {
        assert!(inst.insert(i.name.clone()), "instance {} twice", i.name);
    }
    assert!(o.netlist.ports.iter().any(|p| p.dir == NlDir::Output));
    let rep = common::check_texts(&texts(&[("g.sv", &m)]), "gn", &o.text, &lib);
    assert_eq!(rep.verdict, Verdict::Equivalent, "{:?}", rep.mismatches);
}

// SYNTH-REQ-0120, SYNTH-REQ-0131: module items and expressions beyond the corpus, proven
// equivalent by the checker.
#[test]
fn items_and_expressions() {
    let pkg = "package kp;\n typedef enum logic [1:0] {IDLE, RUN, STOP} st_e;\n\
               typedef struct packed { logic [3:0] hi; logic [3:0] lo; } pair_t;\n\
               typedef logic [3:0] n4_t;\n localparam int K = 3;\n function automatic logic [3:0] rot(logic [3:0] x);\n\
               return {x[2:0], x[3]};\n endfunction\nendpackage\n";
    let m = "module ks (clk, a, b, sel, y, z, w, v);\n\
             import kp::*;\n input logic clk;\n input logic [7:0] a, b;\n input logic [2:0] sel;\n\
             output logic [7:0] y;\n output logic [3:0] z;\n output logic [2:0] w;\n output logic v;\n\
             pair_t p;\n st_e s;\n logic [7:0] t;\n wire g1, g2;\n\
             and (g1, a[0], b[0]);\n xnor (g2, a[1], b[1], g1);\n\
             assign p = '{hi: a[7:4], lo: rot(a[3:0])};\n\
             assign t = {p.lo, p.hi} ^ {2{b[3:0]}};\n\
             assign y = sel inside {3'd1, [3'd4:3'd6]} ? t >>> sel : t[sel +: 4] * 3;\n\
             assign z = a[7:4] ==? 4'b1x0x ? 4'(signed'(b[3:0]) >>> 1) : {g1, g2, a[1:0]};\n\
             assign {w, v} = n4_t'{default: a[K]} ^ b[3:0];\n\
             always_ff @(posedge clk) s <= st_e'(sel[1:0]);\n\
             for (genvar i = 0; i < 2; i++) begin : gl\n if (i == 0) begin : g0\n\
             end else begin : g1b\n end\n end\nendmodule\n";
    let files = [("kp.sv", pkg), ("ks.sv", m)];
    let o = run(&files, SynthOptions::default()).unwrap();
    equivalent(&files, "ks", &o);
    for goal in [Goal::Area, Goal::Delay] {
        let o = common::synth(&texts(&files), "ks", &lib(), Effort::High, goal).unwrap();
        equivalent(&files, "ks", &o);
    }
}

// SYNTH-REQ-0413: identical inputs and options give a byte-identical netlist.
#[test]
fn deterministic_netlists() {
    let lib = lib();
    for d in rustsys_fixtures::rtl::CORPUS.iter().take(8) {
        let files: Vec<(String, String)> = d
            .files
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        for effort in [Effort::Low, Effort::Medium, Effort::High] {
            let a = common::synth(&files, d.top, &lib, effort, Goal::Delay).unwrap();
            let b = common::synth(&files, d.top, &lib, effort, Goal::Delay).unwrap();
            assert_eq!(a.text, b.text, "{} {effort:?}", d.name);
        }
    }
}

// SYNTH-REQ-0703: the report times every phase in execution order, within the total runtime.
#[test]
fn report_phase_times() {
    let d = rustsys_fixtures::rtl::CORPUS
        .iter()
        .find(|d| d.name == "fifo")
        .unwrap();
    let files: Vec<(String, String)> = d
        .files
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    let o = common::synth(&files, d.top, &lib(), Effort::Medium, Goal::Delay).unwrap();
    let names: Vec<&str> = o.report.phase_s.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["read", "elaborate", "optimize", "map", "emit"]);
    let sum: f64 = o.report.phase_s.iter().map(|(_, s)| *s).sum();
    assert!(o.report.phase_s.iter().all(|(_, s)| *s >= 0.0));
    assert!(
        sum <= o.report.runtime_s + 1e-9,
        "{sum} > {}",
        o.report.runtime_s
    );
}

// The explicit initialization contract survives stored-run import and reaches the checker.
#[test]
fn reset_contract_round_trips_through_from_run() {
    let dir = temp_dir("reset-contract");
    let src = dir.join("m.sv");
    let lib = dir.join("cells.lib");
    let run = dir.join("synth");
    let report = dir.join("check.json");
    std::fs::write(&src,"module m(input clk,rn,a,output logic q); always_ff @(posedge clk or negedge rn) if(!rn) q<=0; else q<=q&a; endmodule").unwrap();
    std::fs::write(&lib, rustsys_fixtures::liberty::library(Variant::Full)).unwrap();
    let exe = env!("CARGO_BIN_EXE_rustsys");
    let out = Command::new(exe)
        .args([
            "--threads",
            "1",
            "synth",
            "--after-reset",
            "rn=0",
            "--run-dir",
        ])
        .arg(&run)
        .arg("--lib")
        .arg(&lib)
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(exe)
        .args(["--threads", "1", "check", "synth", "--from-run"])
        .arg(&run)
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r: serde_json::Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(r["verdict"], "equivalent");
    assert_eq!(
        r["constant_states_checked"],
        serde_json::json!([["q", false]])
    );
    assert_eq!(r["after_reset"], serde_json::json!([["rn", false]]));
    let out = Command::new(exe)
        .args(["synth", "--after-reset", "rn=x", "--out"])
        .arg(dir.join("bad.v"))
        .arg("--lib")
        .arg(&lib)
        .arg(&src)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!dir.join("bad.v").exists());
}

// SYNTH-REQ-0224, SYNTH-REQ-0225, CHECK-REQ-0012, CHECK-REQ-0013: the case assumptions are
// applied identically by synthesis and by the independent checker. A decoder written as
// `case (1'b1)` over signal labels, with nonblocking state and nested statements in its items,
// is proved equivalent only when the checker is given the same assumptions.
#[test]
fn case_assumptions_hold_in_synthesis_and_checker_alike() {
    let src = r#"module dec(input logic clk, rn, a, b, c, input logic [3:0] x, y,
        output logic [3:0] q, output logic [1:0] k, output logic [1:0] m);
        always_ff @(posedge clk or negedge rn)
          if (!rn) begin q <= 4'd0; k <= 2'd0; end
          else begin
            (* parallel_case *) case (1'b1)
              a: begin q <= x; k <= 2'd1; end
              b: if (x[0]) q <= y; else k <= 2'd2;
              c: q <= x ^ y;
            endcase
          end
        always_comb begin
          m = 2'd3;
          unique case ({a, b})
            2'b01: m = 2'd1;
            2'b10: m = 2'd2;
            2'b11: m = c ? 2'd0 : 2'd1;
          endcase
        end
        endmodule"#;
    let files = [("dec.sv", src)];
    for (full, parallel) in [(false, true), (true, true), (true, false)] {
        let out = run(
            &files,
            SynthOptions {
                assume_full_case: full,
                assume_parallel_case: parallel,
                ..SynthOptions::default()
            },
        )
        .unwrap();
        assert_eq!(out.report.assume_parallel_case, parallel);
        let check_with = |assume_parallel_case: bool| {
            check(
                &CheckOptions {
                    rtl: Input::Texts(texts(&files)),
                    top: Some("dec".into()),
                    netlist: Input::Texts(vec![("dec.v".into(), out.text.clone())]),
                    assume_full_case: full,
                    assume_parallel_case,
                    ..CheckOptions::default()
                },
                &[lib()],
            )
        };
        let same = check_with(parallel);
        assert_eq!(
            same.verdict,
            Verdict::Equivalent,
            "{full} {parallel}: {same:?}"
        );
        if parallel {
            assert_eq!(check_with(false).verdict, Verdict::NotEquivalent, "{full}");
        }
    }
}

// SYNTH-REQ-0553: under the area goal, a missed period adds a delay-oriented alternative cover with
// the same boundary, so the kept result is at least as fast as the delay goal's cover when both
// meet the rules; with an ample period the area-oriented cover is kept, no larger than the delay
// goal's. Both are proved equivalent.
#[test]
fn a_missed_period_considers_a_delay_oriented_cover() {
    let src = r#"module dp(input logic clk, input logic [15:0] a, b, c, output logic [15:0] q);
        always_ff @(posedge clk) q <= (a + b) ^ (b + c) ^ {a[7:0] * c[7:0]};
        endmodule"#;
    let files = [("dp.sv", src)];
    let synth = |goal: Goal, period_ns: f64| {
        run(
            &files,
            SynthOptions {
                goal,
                electrical: Some(rustsys::electrical::Options {
                    period_ns,
                    ..Default::default()
                }),
                ..SynthOptions::default()
            },
        )
        .unwrap()
    };
    let slack = |o: &SynthOutput| o.report.electrical.as_ref().unwrap().estimated_slack_ns;
    let tight_area = synth(Goal::Area, 0.3);
    let tight_delay = synth(Goal::Delay, 0.3);
    assert!(
        !tight_delay
            .report
            .electrical
            .as_ref()
            .unwrap()
            .estimated_timing_met
    );
    assert!(
        slack(&tight_area) >= slack(&tight_delay) - 1e-9,
        "{} < {}",
        slack(&tight_area),
        slack(&tight_delay)
    );
    let easy_area = synth(Goal::Area, 100.0);
    let easy_delay = synth(Goal::Delay, 100.0);
    assert!(
        easy_area
            .report
            .electrical
            .as_ref()
            .unwrap()
            .estimated_timing_met
    );
    assert!(easy_area.report.area <= easy_delay.report.area + 1e-9);
    for o in [&tight_area, &easy_area] {
        equivalent(&files, "dp", o);
    }
}

// SYNTH-REQ-0554: an area limit that the prefix-carry result misses but ripple carries meet keeps
// the ripple-carry result; a limit met with prefix carries keeps them.
#[test]
fn a_missed_area_limit_tries_ripple_carries() {
    let src = r#"module add(input logic [31:0] a, b, c, output logic [31:0] y, z);
        assign y = a + b;
        assign z = b - c;
        endmodule"#;
    let files = [("add.sv", src)];
    let synth = |area_limit: Option<f64>| {
        run(
            &files,
            SynthOptions {
                electrical: Some(rustsys::electrical::Options {
                    period_ns: 50.0,
                    area_limit,
                    ..Default::default()
                }),
                ..SynthOptions::default()
            },
        )
        .unwrap()
    };
    let prefix = synth(None);
    let roomy = synth(Some(prefix.report.area * 2.0));
    assert!((roomy.report.area - prefix.report.area).abs() < 1e-9);
    let tight = synth(Some(prefix.report.area * 0.9));
    let e = tight.report.electrical.as_ref().unwrap();
    assert!(
        e.area_limit_met,
        "{} > {}",
        tight.report.area,
        prefix.report.area * 0.9
    );
    assert!(tight.report.area < prefix.report.area);
    assert!(!tight.messages.iter().any(|m| m.contains("S0532")));
    equivalent(&files, "add", &tight);
}
