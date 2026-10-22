// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `netlist_sources`: BLIF, AIGER and gate-level netlist sources on the command line, in
//! run records and in workspace mode (CLI-REQ-0220 to CLI-REQ-0224), synthesized and proven
//! equivalent by the checker, with planted defects of the sources (CHECK-REQ-0106). Every input is
//! a hand-written text with a known answer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusteda_formats::liberty;
use rusteda_workspace::{BaseRev, Hash64, Invocation, JsonMap, Upstream, Workspace, rlib};
use rustsys_cli::ws::{RtlFileEntry, RtlManifest};
use rustsys_fixtures::liberty::{Variant, library};

struct Env {
    dir: PathBuf,
    lib: PathBuf,
}

fn env(tag: &str) -> Env {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sources-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("wt_rich.lib");
    std::fs::write(&lib, library(Variant::Rich)).unwrap();
    Env { dir, lib }
}

impl Env {
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let p = self.dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.display().to_string()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rustsys"))
            .env_remove("NO_COLOR")
            .env_remove("RUSTSYS_RUNS_ROOT")
            .current_dir(&self.dir)
            .args(args)
            .output()
            .unwrap()
    }

    fn lib(&self) -> &str {
        self.lib.to_str().unwrap()
    }

    /// `synth` of `extra` to `out`; panics unless it succeeds.
    fn synth(&self, out: &str, extra: &[&str]) -> Output {
        let mut a = vec!["synth", "--lib", self.lib(), "--out", out];
        a.extend(extra);
        let o = self.run(&a);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        o
    }

    /// `check synth` of `netlist` against `extra`; returns the exit status and standard output.
    fn check(&self, netlist: &str, extra: &[&str]) -> (i32, String) {
        let mut a = vec!["check", "synth", "--lib", self.lib(), "--netlist", netlist];
        a.extend(extra);
        let o = self.run(&a);
        (
            o.status.code().unwrap_or(-1),
            format!("{}{}", text(&o.stdout), text(&o.stderr)),
        )
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A hierarchical BLIF design: a half adder in a submodel, latches of each type and library
/// cells of `--gate-lib`.
const DESIGN: &str = "# a design\n.model top\n.inputs a b c en \\\n  sel\n.outputs s co q1 q2 q3 y\n\
     .subckt half x=a y=b s=s c=co\n\
     .latch s q1 0\n.latch co q2 fe c 1\n.latch a q3 ah en 2\n\
     .gate wt_mux2_x1 A0=q1 A1=q2 S=sel Y=m\n.names m q3 y\n1- 1\n-1 1\n.end\n\
     .model half\n.inputs x y\n.outputs s c\n.names x y s\n10 1\n01 1\n.names x y c\n11 1\n.end\n";

// CLI-REQ-0220, CLI-REQ-0221, SYNTH-REQ-0900, SYNTH-REQ-0903, SYNTH-REQ-0913, CHECK-REQ-0100,
// CHECK-REQ-0101: a BLIF design synthesized and proven equivalent from the command line.
#[test]
fn blif_design() {
    let e = env("blif");
    let src = e.file("design.blif", DESIGN.as_bytes());
    let out = e.dir.join("design.v").display().to_string();
    let rep = e.dir.join("design.json").display().to_string();
    e.synth(&out, &["--gate-lib", e.lib(), "--report", &rep, &src]);
    let r: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rep).unwrap()).unwrap();
    assert_eq!(r["source_format"], "blif");
    assert_eq!(r["clock_port"], "clk");
    assert_eq!(r["init_port"], "init");
    assert_eq!(r["state_bits_rtl"], 3);
    let (code, o) = e.check(&out, &["--gate-lib", e.lib(), &src]);
    assert_eq!(code, 0, "{o}");
    assert!(
        o.contains("verdict: equivalent (outputs 6, state pairs 3"),
        "{o}"
    );
}

/// The half adder with a latch in ASCII AIGER, and the same graph in binary form.
const AAG: &str = "aag 7 2 1 3 4\n2\n4\n6 12 1\n12\n14\n6\n8 2 4\n10 3 5\n12 9 11\n14 2 4\n\
                   i0 x\ni1 y\nl0 q\no0 sum\no1 carry\nc\nhalf adder\n";

fn binary_half() -> Vec<u8> {
    let mut b = b"aig 7 2 1 3 4\n12 1\n12\n14\n6\n".to_vec();
    for (lhs, r0, r1) in [(8u8, 4u8, 2u8), (10, 5, 3), (12, 11, 9), (14, 4, 2)] {
        b.push(lhs - r0);
        b.push(r0 - r1);
    }
    b.extend(b"i0 x\ni1 y\nl0 q\no0 sum\no1 carry\n");
    b
}

// CLI-REQ-0220, SYNTH-REQ-0902, CHECK-REQ-0102: AIGER designs in both forms.
#[test]
fn aiger_designs() {
    let e = env("aiger");
    for (name, bytes) in [
        ("half.aag", AAG.as_bytes().to_vec()),
        ("half.aig", binary_half()),
    ] {
        let src = e.file(name, &bytes);
        let out = e.dir.join(format!("{name}.v")).display().to_string();
        let o = e.synth(&out, &[&src]);
        assert!(text(&o.stdout).contains("top: half"), "{}", text(&o.stdout));
        let (code, o) = e.check(&out, &[&src]);
        assert_eq!(code, 0, "{o}");
        assert!(
            o.contains("verdict: equivalent (outputs 3, state pairs 1"),
            "{o}"
        );
    }
}

// CLI-REQ-0220, CLI-REQ-0221: usage errors of sources and of the options that depend on them.
#[test]
fn usage_errors() {
    let e = env("usage");
    let blif = e.file("a.blif", DESIGN.as_bytes());
    let aag = e.file("b.aag", AAG.as_bytes());
    let aag2 = e.file("c.aag", AAG.as_bytes());
    let sv = e.file(
        "t.sv",
        b"module t(input a, output y); assign y = a; endmodule\n",
    );
    let net = e.file(
        "n.v",
        b"module n(a, y); input a; output y; assign y = a; endmodule\n",
    );
    for (args, code) in [
        (vec![blif.as_str(), sv.as_str()], "A0300"),
        (vec![aag.as_str(), aag2.as_str()], "A0300"),
        (vec!["-D", "X", blif.as_str()], "A0301"),
        (vec!["-I", ".", aag.as_str()], "A0301"),
        (vec!["--assume-full-case", aag.as_str()], "A0301"),
        (vec!["--initial-values", "reset", sv.as_str()], "A0301"),
        (
            vec![
                "--gate-netlist",
                net.as_str(),
                "--gate-lib",
                e.lib(),
                sv.as_str(),
            ],
            "A0302",
        ),
        (vec!["--gate-netlist", net.as_str()], "A0303"),
        (vec!["--gate-lib", e.lib(), aag.as_str()], "A0303"),
    ] {
        let mut a = vec!["synth", "--lib", e.lib(), "--out", "x.v"];
        a.extend(args.iter().copied());
        let o = e.run(&a);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let err = text(&o.stderr);
        assert!(err.contains(&format!("error[{code}]")), "{args:?}: {err}");
    }
}

// CLI-REQ-0222, SYNTH-REQ-0906, CHECK-REQ-0105: the two initial-value modes, which check synth
// must share with synthesis.
#[test]
fn initial_value_modes() {
    let e = env("init");
    let src = e.file(
        "r.blif",
        b".model r\n.inputs d\n.outputs q\n.latch d q 1\n.end\n",
    );
    let reset = e.dir.join("reset.v").display().to_string();
    e.synth(&reset, &[&src]);
    assert!(
        std::fs::read_to_string(&reset)
            .unwrap()
            .contains("input wire init")
    );
    assert_eq!(e.check(&reset, &[&src]).0, 0);
    assert_ne!(
        e.check(&reset, &["--initial-values", "arbitrary", &src]).0,
        0
    );
    let arb = e.dir.join("arb.v").display().to_string();
    let o = e.synth(&arb, &["--initial-values", "arbitrary", &src]);
    assert!(
        text(&o.stderr).contains("warning[S0913]"),
        "{}",
        text(&o.stderr)
    );
    assert!(!std::fs::read_to_string(&arb).unwrap().contains("init"));
    assert_eq!(e.check(&arb, &["--initial-values", "arbitrary", &src]).0, 0);
    assert_ne!(e.check(&arb, &[&src]).0, 0);
}

fn record(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("run.json")).unwrap()).unwrap()
}

/// A gate-level netlist of the synthetic library.
const GATES: &str = "module g(a, b, ck, rn, y, q);\n  input a, b, ck, rn; output y, q;\n\
     wire n, m;\n  wt_nand2_x1 g0 (.A(a), .B(b), .Y(n));\n  wt_xor2_x1 g1 (.A(n), .B(q), .Y(m));\n\
     wt_dffr_x1 \\cnt_reg[0] (.D(m), .CK(ck), .RN(rn), .Q(q), .QN());\n  assign y = n;\nendmodule\n";

// CLI-REQ-0223, SYNTH-REQ-0910, SYNTH-REQ-0912, CHECK-REQ-0103: recorded runs spell the new
// options in their canonical commands, and `check synth --from-run` takes them.
#[test]
fn records_and_from_run() {
    let e = env("records");
    let src = e.file("design.blif", DESIGN.as_bytes());
    let run = e.dir.join("run-blif");
    let o = e.run(&[
        "synth",
        "--lib",
        e.lib(),
        "--gate-lib",
        e.lib(),
        "--run-dir",
        run.to_str().unwrap(),
        &src,
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let cmd = record(&run)["command"].as_str().unwrap().to_string();
    assert!(
        cmd.contains("--gate-lib ") && cmd.contains("--initial-values reset"),
        "{cmd}"
    );
    let o = e.run(&["check", "synth", "--from-run", run.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    // A gate-level netlist resynthesized and proven against itself as the reference.
    let net = e.file("gates.v", GATES.as_bytes());
    let run = e.dir.join("run-gates");
    let o = e.run(&[
        "synth",
        "--lib",
        e.lib(),
        "--gate-netlist",
        &net,
        "--gate-lib",
        e.lib(),
        "--run-dir",
        run.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let cmd = record(&run)["command"].as_str().unwrap().to_string();
    assert!(
        cmd.contains("--gate-netlist ") && !cmd.contains("--initial-values"),
        "{cmd}"
    );
    let o = e.run(&["check", "synth", "--from-run", run.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(
        text(&o.stdout).contains("state pairs 1"),
        "{}",
        text(&o.stdout)
    );
    let v = std::fs::read_to_string(run.join("netlist.v")).unwrap();
    assert!(v.contains("\\cnt_reg[0] "), "{v}");
}

// CHECK-REQ-0106: planted defects of the sources make the verdict `not-equivalent` (exit 3)
// against the netlist synthesized from the unchanged source.
#[test]
fn planted_source_defects() {
    let e = env("defects");
    let src = e.file("design.blif", DESIGN.as_bytes());
    let out = e.dir.join("design.v").display().to_string();
    e.synth(&out, &["--gate-lib", e.lib(), &src]);
    for (k, (from, to)) in [
        (".names x y c\n11 1", ".names x y c\n1- 1"),
        (".latch s q1 0", ".latch s q1 1"),
        (".latch co q2 fe c 1", ".latch co q2 re c 1"),
        (
            ".gate wt_mux2_x1 A0=q1 A1=q2",
            ".gate wt_mux2_x1 A0=q2 A1=q1",
        ),
    ]
    .iter()
    .enumerate()
    {
        assert!(DESIGN.contains(from));
        let bad = e.file(&format!("bad{k}.blif"), DESIGN.replace(from, to).as_bytes());
        let (code, o) = e.check(&out, &["--gate-lib", e.lib(), &bad]);
        assert_eq!(code, 3, "{to}: {o}");
    }
    // A flipped inversion bit of an AIGER literal.
    let aag = e.file("half.aag", AAG.as_bytes());
    let half = e.dir.join("half.v").display().to_string();
    e.synth(&half, &[&aag]);
    for (from, to) in [
        ("12 9 11", "12 9 10"),
        ("14 2 4", "14 3 4"),
        ("6 12 1", "6 13 1"),
    ] {
        let bad = e.file("bad.aag", AAG.replace(from, to).as_bytes());
        let (code, o) = e.check(&half, &[&bad]);
        assert_eq!(code, 3, "{to}: {o}");
    }
    // A gate-level reference with one cell of another function.
    let net = e.file("gates.v", GATES.as_bytes());
    let g = e.dir.join("g.v").display().to_string();
    e.synth(&g, &["--gate-netlist", &net, "--gate-lib", e.lib()]);
    let bad = e.file(
        "bad_gates.v",
        GATES.replace("wt_xor2_x1", "wt_xnor2_x1").as_bytes(),
    );
    let (code, o) = e.check(&g, &["--gate-netlist", &bad, "--gate-lib", e.lib()]);
    assert_eq!(code, 3, "{o}");
}

/// A workspace with the synthetic library (rev1) and a snapshot of `src` in `format` (rev2).
fn workspace(e: &Env, name: &str, src: &[u8], top: &str, format: &str) -> PathBuf {
    let ws_dir = e.dir.join(format!("ws-{format}"));
    let inv = Invocation::new("rusteda", "0.1.0", "rusteda init", vec!["rusteda", "init"]);
    let ws = Workspace::init(&ws_dir, &inv).unwrap().workspace;
    let text = library(Variant::Rich);
    let parsed = liberty::read_text(&text, "wt_rich.lib").unwrap();
    let hash = Hash64::of(text.as_bytes());
    let cached = ws
        .cache()
        .put_library(
            &parsed.name,
            text.as_bytes(),
            rlib::KIND,
            rlib::VERSIONS,
            |_| rlib::encode_with_source(&parsed, hash),
        )
        .unwrap();
    let inv = Invocation::new(
        "rusteda",
        "0.1.0",
        "rusteda import",
        vec!["rusteda", "import"],
    );
    let mut t = ws.begin(BaseRev::parse("rev0").unwrap(), &inv).unwrap();
    t.add_library(cached.entry("default", e.lib.display().to_string()))
        .unwrap();
    t.commit(JsonMap::new()).unwrap();
    let mut t = ws.begin(BaseRev::parse("rev1").unwrap(), &inv).unwrap();
    let stage = t.staging_dir().join("rtl");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join(name), src).unwrap();
    let manifest = RtlManifest {
        top: top.into(),
        defines: BTreeMap::new(),
        params: BTreeMap::new(),
        files: vec![RtlFileEntry {
            path: name.into(),
            hash: Hash64::of(src),
        }],
        predefined_macros: true,
        format: Some(format.into()),
    };
    std::fs::write(
        stage.join("rtl.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    t.add_dir("rtl", "rtl", Upstream::new()).unwrap();
    t.commit(JsonMap::new()).unwrap();
    ws_dir
}

// CLI-REQ-0224, RCLI-REQ-0022: workspace synthesis and checks of BLIF and AIGER snapshots, with
// the initial-value mode of the synthesis run taken by the check.
#[test]
fn workspace_snapshots() {
    let e = env("ws");
    let blif = b".model top\n.inputs a b\n.outputs y q\n.names a b y\n11 1\n.latch y q 1\n.end\n";
    for (name, src, top, format) in [
        ("top.blif", blif.to_vec(), "top", "blif"),
        ("half.aag", AAG.as_bytes().to_vec(), "half", "aiger"),
    ] {
        let ws = workspace(&e, name, &src, top, format);
        let w = ws.to_str().unwrap();
        let o = e.run(&[
            "-w",
            w,
            "--format",
            "json",
            "synth",
            "--base-rev",
            "rev2",
            "--initial-values",
            "arbitrary",
            "--check",
        ]);
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(o.status.code(), Some(0), "{v}");
        assert_eq!(v["scope"]["equivalence"], "equivalent", "{v}");
        assert_eq!(v["report"]["initial_values_mode"], "arbitrary");
        let o = e.run(&["-w", w, "--format", "json", "check", "--base-rev", "rev3"]);
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(o.status.code(), Some(0), "{v}");
        assert_eq!(v["report"]["initial_values_mode"], "arbitrary", "{v}");
        assert_eq!(v["report"]["source_format"], format, "{v}");
    }
}
