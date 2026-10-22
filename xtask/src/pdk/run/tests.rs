// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Tests of the `verify pdk` runner: command lines, reports, time limits, the summary, missing
//! libraries and whole runs with a stand-in for `rustsys`.

use super::*;
use crate::testutil::Tree;

fn names(v: &[OsString]) -> Vec<String> {
    v.iter().map(|a| a.to_string_lossy().into_owned()).collect()
}

// The synthesis run writes the netlist and the report into the run directory; the check reads
// that netlist; both take the top module, the library and the sources in order.
#[test]
fn command_lines() {
    let srcs = [PathBuf::from("/r/pkg.sv"), PathBuf::from("/r/alu.sv")];
    let (lib, dir) = (Path::new("/l/x.lib"), Path::new("/o/x/alu"));
    assert_eq!(
        names(&synth_args("alu", lib, dir, &srcs)).join(" "),
        "--progress off synth --top alu --lib /l/x.lib --out /o/x/alu/netlist.v \
         --report /o/x/alu/report.json /r/pkg.sv /r/alu.sv"
    );
    assert_eq!(
        names(&check_args("alu", lib, dir, &srcs)).join(" "),
        "--progress off check synth --top alu --lib /l/x.lib --netlist /o/x/alu/netlist.v \
         --report /o/x/alu/check.json /r/pkg.sv /r/alu.sv"
    );
}

// Reports give the cell counts, the area, the warnings and the verdict; missing members fail.
#[test]
fn reads_reports() {
    let r = synth_report(
        r#"{"format": "rustsys-synth-report", "cells": {"a": 2, "b": 5}, "area": 12.5, "warnings": 1}"#,
    );
    assert_eq!(
        r,
        Ok(SynthReport {
            counts: vec![("a".into(), 2), ("b".into(), 5)],
            area: 12.5,
            warnings: 1,
        })
    );
    assert!(synth_report(r#"{"area": 1.0}"#).is_err());
    assert!(synth_report(r#"{"cells": {"a": 1.5}, "area": 1.0}"#).is_err());
    assert!(synth_report(r#"{"cells": {}}"#).is_err());
    assert_eq!(
        check_verdict(r#"{"verdict": "equivalent"}"#),
        Ok("equivalent".into())
    );
    assert!(check_verdict("{}").is_err());
    assert!(check_verdict("not json").is_err());
}

// The executable comes from the compiler-artifact message of the `rustsys` binary.
#[test]
fn executable_from_build_messages() {
    let msgs = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"rustsys_proto"},"executable":null}
{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"rustsys"},"executable":"/t/release/rustsys"}
not json
{"reason":"build-finished","success":true}"#;
    assert_eq!(executable(msgs), Some(PathBuf::from("/t/release/rustsys")));
    assert_eq!(
        executable(r#"{"reason":"build-finished","success":true}"#),
        None
    );
}

// A child ends with its status or is killed at the time limit; a missing program is an error.
#[cfg(unix)]
#[test]
fn timed_runs() {
    let limit = Duration::from_secs(20);
    let mut ok = Command::new("sh");
    ok.args(["-c", "exit 3"]);
    assert_eq!(run_timed(&mut ok, limit).map(|r| r.0), Ok(Exit::Code(3)));
    let mut slow = Command::new("sleep");
    slow.arg("20");
    let (exit, took) =
        run_timed(&mut slow, Duration::from_millis(100)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(exit, Exit::TimedOut);
    assert!(took < Duration::from_secs(10), "{took:?}");
    assert!(run_timed(&mut Command::new("/nonexistent/rustsys"), limit).is_err());
    assert_eq!(Exit::TimedOut.describe(limit), "timed out after 20 s");
}

fn passed(library: &str, design: &str, cells: u64, area: f64, hazards: Hazards) -> Run {
    Run {
        library: library.into(),
        design: design.into(),
        cells: Some(cells),
        area: Some(area),
        warnings: Some(0),
        hazards,
        ..Run::default()
    }
}

// The summary states the counts and every failure, lists the hazards per library and per run,
// and tabulates cells and area per design and library with totals over the passed runs.
#[test]
fn summary_sections() {
    let mut marks = BTreeMap::new();
    marks.insert("iso_1".to_string(), vec![Mark::Isolation, Mark::AlwaysOn]);
    let libs = vec![
        ("lib_a".to_string(), marks),
        ("lib_b".to_string(), BTreeMap::new()),
    ];
    let designs: Vec<Design> = ["counter", "alu"]
        .iter()
        .map(|n| Design {
            name: n.to_string(),
            top: n.to_string(),
            files: vec![format!("{n}.sv")],
        })
        .collect();
    let h = Hazards {
        cells: vec![
            (Mark::Isolation, "iso_1".into(), 3),
            (Mark::AlwaysOn, "iso_1".into(), 3),
        ],
        const_assigns: 2,
        const_pins: 0,
    };
    let runs = vec![
        passed("lib_a", "counter", 49, 670.64, h),
        passed("lib_a", "alu", 258, 2333.5, Hazards::default()),
        passed("lib_b", "counter", 50, 1901.04, Hazards::default()),
        Run {
            library: "lib_b".into(),
            design: "alu".into(),
            failure: Some("check verdict 'not-equivalent' (exit status 3)".into()),
            ..Run::default()
        },
    ];
    let s = summary(&libs, &designs, &runs);
    for want in [
        "4 runs: 3 passed, 1 failed",
        "FAIL lib_b/alu: check verdict 'not-equivalent' (exit status 3)",
        "  lib_a  is_isolation_cell: 1, always_on: 1\n",
        "  lib_b  none\n",
        "  lib_a/counter: is_isolation_cell: iso_1 x3; always_on: iso_1 x3; \
         literal constants: 2 assign(s), 0 pin(s)\n",
        "counter           49 / 670.6  50 / 1901.0\n",
        "alu             258 / 2333.5         FAIL\n",
        "total (passed)  307 / 3004.1  50 / 1901.0\n",
    ] {
        assert!(s.contains(want), "{want}\n{s}");
    }
    let tsv = runs_tsv(&runs);
    assert_eq!(tsv.lines().count(), 5);
    assert!(tsv.starts_with("library\tdesign\tstatus\tcells\tarea\twarnings"));
    assert!(tsv.contains("lib_a\tcounter\tpass\t49\t670.6400\t0\t"));
    assert!(tsv.contains("lib_b\talu\tfail\t"));
}

// Columns are padded to their widest cell: the first to the left, the others to the right.
#[test]
fn text_tables() {
    let rows = vec![
        vec!["design".to_string(), "a".to_string()],
        vec!["x".to_string(), "1234".to_string()],
    ];
    assert_eq!(table(&rows), "design     a\nx       1234\n");
}

fn library(name: &str) -> Library {
    Library {
        name: name.into(),
        library: format!("{name}_lib"),
        corner: "tt".into(),
        repo: "https://example.org/r".into(),
        commit: "0".repeat(40),
        license: "Apache-2.0".into(),
        source: manifest::Source::File {
            url: "https://example.org/x.lib".into(),
        },
        sha256: "0".repeat(64),
        size: 1,
    }
}

// Missing libraries skip the suite with a banner, or fail it with `--require`, before anything
// is built.
#[test]
fn missing_libraries() {
    let t = Tree::new("pdk-missing");
    let libs = [library("a_tt"), library("b_tt")];
    let missing: Vec<&Library> = libs.iter().skip(1).collect();
    let banner = skipped_banner(&missing, 2, t.root(), false);
    assert!(banner.contains("SKIPPED: verify pdk: 1 of 2 libraries missing"));
    assert!(banner.contains("  b_tt (b_tt_lib__tt.lib)\n"));
    assert!(banner.contains("`--require` makes that a failure"));
    assert!(skipped_banner(&missing, 2, t.root(), true).contains("--require: missing"));
    assert_eq!(
        run(t.root(), &libs, t.root(), &VerifyOptions::default()),
        Ok(())
    );
    let required = VerifyOptions {
        require: true,
        ..VerifyOptions::default()
    };
    assert!(run(t.root(), &libs, t.root(), &required).is_err_and(|e| e.contains("2 of 2")));
}

// A library directory at or below the output directory is refused before anything is read, built
// or deleted, with or without `--require` and whether its libraries match or are missing; a
// sibling of the output directory is not inside it.
#[test]
fn library_directory_inside_outputs() {
    let t = Tree::new("pdk-outputs");
    let mut present = library("a_tt");
    // The SHA-256 and size of a file holding "x".
    present.sha256 = "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881".into();
    present.size = 1;
    let libs = [present, library("b_tt")];
    t.file("target/verify-pdk/libs/a_tt_lib__tt.lib", "x");
    let required = VerifyOptions {
        require: true,
        ..VerifyOptions::default()
    };
    let default = VerifyOptions::default();
    for (dir, o) in [
        ("target/verify-pdk/libs", &default),
        ("target/verify-pdk/libs", &required),
        ("target/verify-pdk", &default),
        ("target/verify-pdk/libs/../other", &default),
    ] {
        let e = run(t.root(), &libs, &t.root().join(dir), o)
            .err()
            .unwrap_or_default();
        assert!(
            e.contains("refusing the library directory") && e.contains(OUT),
            "{dir}: {e}"
        );
    }
    assert_eq!(t.files(), ["target/verify-pdk/libs/a_tt_lib__tt.lib"]);
    let sibling = t.root().join("target/verify-pdk-libs");
    assert_eq!(run(t.root(), &libs, &sibling, &default), Ok(()));
}

/// A stand-in for `rustsys`: writes a netlist and the reports where the arguments say; the top
/// module `bad` fails synthesis and `neq` gets a negative verdict.
const STAND_IN: &str = r#"#!/bin/sh
mode=synth
while [ $# -gt 0 ]; do
  case "$1" in
    check) mode=check ;;
    --top) top=$2; shift ;;
    --out) out=$2; shift ;;
    --report) rep=$2; shift ;;
  esac
  shift
done
if [ "$mode" = synth ]; then
  if [ "$top" = bad ]; then echo "note: reading" >&2; echo "error[S0133]: top.sv:1:1: broken" >&2; exit 1; fi
  cat > "$out" <<'END'
module m(input a, output y, output z);
  iso_1 g0 (.A(a), .SLEEP(1'b0), .X(y));
  assign z = 1'b1;
endmodule
END
  printf '{"cells": {"iso_1": 2, "inv_1": 1}, "area": 7.5, "warnings": 1}\n' > "$rep"
else
  if [ "$top" = neq ]; then printf '{"verdict": "not-equivalent"}\n' > "$rep"; exit 3; fi
  printf '{"verdict": "equivalent"}\n' > "$rep"
fi
"#;

// A run synthesizes, reads the report and the netlist, checks and records the verdict; a failing
// synthesis or a negative verdict becomes the run's failure, with the first error line.
#[cfg(unix)]
#[test]
fn design_runs() {
    use std::os::unix::fs::PermissionsExt;
    let t = Tree::new("pdk-run");
    t.file("bin/rustsys", STAND_IN)
        .file("rtl/d/d.sv", "module d; endmodule\n");
    let bin = t.root().join("bin/rustsys");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("{e}"));
    let mut marks = BTreeMap::new();
    marks.insert("iso_1".to_string(), vec![Mark::Isolation]);
    let (lib, out, rtl) = (library("a_tt"), t.root().join("out"), t.root().join("rtl"));
    let guard = crate::guard::Guard {
        scope: false,
        memory_max: "16G".to_string(),
        timeout_s: 60,
    };
    let s = Setup {
        bin: &bin,
        guard: &guard,
        lib: &lib,
        lib_file: Path::new("/l/a.lib"),
        marks: &marks,
        rtl: &rtl,
        out: &out,
        limit: Duration::from_secs(60),
    };
    let design = |top: &str| Design {
        name: "d".into(),
        top: top.into(),
        files: vec!["d.sv".into()],
    };
    let ok = design_run(&s, &design("d"));
    assert_eq!(ok.failure, None);
    assert_eq!(
        (ok.cells, ok.area, ok.warnings),
        (Some(3), Some(7.5), Some(1))
    );
    assert_eq!(ok.hazards.instances(Mark::Isolation), 2);
    assert_eq!((ok.hazards.const_assigns, ok.hazards.const_pins), (1, 1));
    let cmd = std::fs::read_to_string(out.join("a_tt/d/command.txt")).unwrap_or_default();
    assert_eq!(cmd.lines().count(), 2);
    assert!(
        cmd.contains(" check synth --top d --lib /l/a.lib --netlist "),
        "{cmd}"
    );
    let bad = design_run(&s, &design("bad"));
    assert_eq!(
        bad.failure.as_deref(),
        Some("synth exit status 1: error[S0133]: top.sv:1:1: broken")
    );
    let neq = design_run(&s, &design("neq"));
    assert_eq!(
        neq.failure.as_deref(),
        Some("check verdict 'not-equivalent' (exit status 3)")
    );
}
