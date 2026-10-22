// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Sources other than RTL (CHECK-REQ-0100 to CHECK-REQ-0107): BLIF and AIGER sources and
//! gate-level references against hand-written netlists of the synthetic library, with known
//! verdicts and planted defects.

use std::sync::Mutex;

use rusteda_core::job::{Ctx, Diag, Event, Observer};
use rusteda_formats::liberty::Library;
use rustsys_check::{CheckOptions, CheckReport, InitialValues, Input, Verdict, check, check_with};
use rustsys_fixtures::liberty::{Variant, library};

fn rich() -> Library {
    rusteda_formats::liberty::read_text(&library(Variant::Rich), "wt_rich.lib").unwrap()
}

fn opts(src: (&str, &str), netlist: &str) -> CheckOptions {
    CheckOptions {
        rtl: Input::Texts(vec![(src.0.to_string(), src.1.to_string())]),
        netlist: Input::Texts(vec![("netlist.v".into(), netlist.to_string())]),
        ..CheckOptions::default()
    }
}

fn run(o: &CheckOptions) -> CheckReport {
    check(o, &[rich()])
}

fn verdict(src: (&str, &str), netlist: &str) -> Verdict {
    let r = run(&opts(src, netlist));
    assert!(r.errors.is_empty() || r.verdict == Verdict::Error, "{r:?}");
    r.verdict
}

const AND_NETLIST: &str = "module m(a, b, y);\n  input a, b; output y;\n  wire n;\n\
     wt_nand2_x1 g0 (.A(a), .B(b), .Y(n));\n  wt_inv_x1 g1 (.A(n), .Y(y));\nendmodule\n";

// CHECK-REQ-0100, CHECK-REQ-0101, CHECK-REQ-0106: a BLIF cover against a netlist, an output
// without a driver, and a changed row of the cover.
#[test]
fn blif_covers() {
    let ok = ".model m\n.inputs a b\n.outputs y\n.names a b y\n11 1\n.end\n";
    assert_eq!(verdict(("m.blif", ok), AND_NETLIST), Verdict::Equivalent);
    let offset = ".model m\n.inputs a b\n.outputs y\n.names a b y\n0- 0\n-0 0\n.end\n";
    assert_eq!(
        verdict(("m.blif", offset), AND_NETLIST),
        Verdict::Equivalent
    );
    // An output without a driver reads as 0, with a warning.
    let seen = Messages::default();
    let undriven = ".model m\n.inputs a b\n.outputs y z\n.names a b y\n11 1\n.end\n";
    let netlist = AND_NETLIST
        .replace("module m(a, b, y);", "module m(a, b, y, z);")
        .replace("output y;", "output y, z;")
        .replace("endmodule", "assign z = 1'b0;\nendmodule");
    let r = check_with(
        &opts(("m.blif", undriven), &netlist),
        &[rich()],
        Ctx::new(None, Some(&seen)),
    )
    .unwrap();
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    let codes: Vec<String> = seen
        .0
        .lock()
        .map(|v| v.iter().map(|d| d.code.clone()).collect())
        .unwrap_or_default();
    assert!(codes.contains(&"C0903".to_string()), "{codes:?}");
    let wrong = ".model m\n.inputs a b\n.outputs y\n.names a b y\n10 1\n.end\n";
    let r = run(&opts(("m.blif", wrong), AND_NETLIST));
    assert_eq!(r.verdict, Verdict::NotEquivalent);
    assert_eq!(r.mismatches[0].name, "y");
    assert_eq!(r.source_format.as_deref(), Some("blif"));
}

const REG_NETLIST: &str = "module s(d, clk, init, q);\n  input d, clk, init; output q;\n\
     wt_dffrp_x1 r0 (.D(d), .CK(clk), .R(init), .Q(q));\nendmodule\n";

// CHECK-REQ-0101, CHECK-REQ-0105, CHECK-REQ-0106: a latch on the global clock with an initial
// value made by the generated init port; a swapped initial value; the arbitrary mode.
#[test]
fn latch_initial_values() {
    let zero = ".model s\n.inputs d\n.outputs q\n.latch d q 0\n.end\n";
    let r = run(&opts(("s.blif", zero), REG_NETLIST));
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.initial_values, [("q".to_string(), false)]);
    assert_eq!(r.initial_values_mode.as_deref(), Some("reset"));
    assert_eq!(r.state_pairs_checked, 1);
    let one = ".model s\n.inputs d\n.outputs q\n.latch d q 1\n.end\n";
    let r = run(&opts(("s.blif", one), REG_NETLIST));
    assert_eq!(r.verdict, Verdict::NotEquivalent, "{r:?}");
    // Under `arbitrary` there is no init port and the state has no asynchronous control.
    let plain = "module s(d, clk, q);\n  input d, clk; output q;\n\
         wt_dff_x1 r0 (.D(d), .CK(clk), .Q(q), .QN());\nendmodule\n";
    let mut o = opts(("s.blif", one), plain);
    o.initial_values = InitialValues::Arbitrary;
    let r = run(&o);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.initial_values_mode.as_deref(), Some("arbitrary"));
    // The netlist of `reset` has an input that the model of `arbitrary` lacks.
    o.netlist = Input::Texts(vec![("n.v".into(), REG_NETLIST.into())]);
    assert_ne!(run(&o).verdict, Verdict::Equivalent);
}

// CHECK-REQ-0101: latch types, the clock name when `clk` is taken, hierarchy and the names of
// flattened states.
#[test]
fn latch_types_and_hierarchy() {
    let src = ".model top\n.inputs d c clk\n.outputs y\n.subckt cell i=d c=c o=y\n.end\n\
               .model cell\n.inputs i c\n.outputs o\n.latch i q fe c 2\n.names q o\n0 1\n.end\n";
    let netlist = "module top(d, c, clk, y);\n  input d, c, clk; output y;\n  wire \\cell_0.q ;\n\
         wt_dffn_x1 r0 (.D(d), .CKN(c), .Q(\\cell_0.q ));\n  wt_inv_x1 g0 (.A(\\cell_0.q ), .Y(y));\n\
         endmodule\n";
    let r = run(&opts(("t.blif", src), netlist));
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.state_pairs_checked, 1);
    // An active-low latch against an active-high one is a difference.
    let lat = ".model top\n.inputs d g\n.outputs q\n.latch d q al g 3\n.end\n";
    let netlist = "module top(d, g, q);\n  input d, g; output q;\n\
         wt_lat_x1 l0 (.D(d), .G(g), .Q(q));\nendmodule\n";
    assert_eq!(verdict(("t.blif", lat), netlist), Verdict::NotEquivalent);
}

// CHECK-REQ-0102, CHECK-REQ-0106: AIGER sources in both forms, and a flipped inversion bit.
#[test]
fn aiger_sources() {
    let ok = "aag 3 2 0 1 1\n2\n4\n6\n6 2 4\ni0 a\ni1 b\no0 y\n";
    assert_eq!(verdict(("m.aag", ok), AND_NETLIST), Verdict::Equivalent);
    let flipped = "aag 3 2 0 1 1\n2\n4\n6\n6 3 4\ni0 a\ni1 b\no0 y\n";
    assert_eq!(
        verdict(("m.aag", flipped), AND_NETLIST),
        Verdict::NotEquivalent
    );
    let binary = "aig 3 2 0 1 1\n6\n\u{2}\u{2}i0 a\ni1 b\no0 y\nc\ncomment\n";
    assert_eq!(verdict(("m.aig", binary), AND_NETLIST), Verdict::Equivalent);
    // Module name from `--top`, default names, a latch on the global clock with reset 0.
    let seq = "aag 2 1 1 1 0\n2\n4 2\n4\n";
    let netlist = "module named(i0, clk, init, o0);\n  input i0, clk, init; output o0;\n\
         wire l0;\n  wt_dffrp_x1 r0 (.D(i0), .CK(clk), .R(init), .Q(l0));\n  assign o0 = l0;\n\
         endmodule\n";
    let mut o = opts(("x.aag", seq), netlist);
    o.top = Some("named".into());
    let r = run(&o);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.initial_values, [("l0".to_string(), false)]);
    // Properties are not synthesizable.
    let r = run(&opts(("p.aag", "aag 1 1 0 0 0 1\n2\n2\n"), AND_NETLIST));
    assert_eq!(r.verdict, Verdict::Error);
    assert!(r.errors[0].contains("C0900"), "{:?}", r.errors);
}

const REFERENCE: &str = "module top(a, b, ck, rn, y, q);\n  input a, b, ck, rn; output y, q;\n\
     wire n;\n  wt_nand2_x1 g0 (.A(a), .B(b), .Y(n));\n  wt_inv_x1 g1 (.A(n), .Y(y));\n\
     wt_dffr_x1 \\st_reg[0] (.D(n), .CK(ck), .RN(rn), .Q(q), .QN());\nendmodule\n";

fn gate(reference: &str, netlist: &str) -> CheckReport {
    let o = CheckOptions {
        rtl: Input::Texts(vec![("ref.v".into(), reference.into())]),
        netlist: Input::Texts(vec![("out.v".into(), netlist.into())]),
        gate_netlist: true,
        gate_libs: vec![rich()],
        ..CheckOptions::default()
    };
    check(&o, &[rich()])
}

// CHECK-REQ-0103, CHECK-REQ-0106: a gate-level reference against a netlist of other cells, and a
// reference with one cell of another function.
#[test]
fn gate_references() {
    let out = "module top(a, b, ck, rn, y, q);\n  input a, b, ck, rn; output y, q;\n\
         wire \\st_reg[0] , n;\n  wt_and2_x1 g0 (.A(a), .B(b), .Y(y));\n\
         wt_inv_x1 g1 (.A(y), .Y(n));\n\
         wt_dffr_x1 r0 (.D(n), .CK(ck), .RN(rn), .Q(\\st_reg[0] ));\n\
         assign q = \\st_reg[0] ;\nendmodule\n";
    let r = gate(REFERENCE, out);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    assert_eq!(r.state_pairs_checked, 1);
    assert_eq!(r.source_format.as_deref(), Some("gate-netlist"));
    assert!(r.initial_values_mode.is_none());
    let replaced = REFERENCE.replace("wt_nand2_x1 g0", "wt_nor2_x1 g0");
    assert_eq!(gate(&replaced, out).verdict, Verdict::NotEquivalent);
    // Both a clear and a preset of one reference instance are outside the model.
    let both = "module top(a, b, ck, y);\n  input a, b, ck; output y;\n\
         wt_dffrs_x1 r (.D(a), .CK(ck), .RN(a), .SN(b), .Q(y));\nendmodule\n";
    let r = gate(both, both);
    assert_eq!(r.verdict, Verdict::Error);
    assert!(r.errors[0].contains("C0902"), "{:?}", r.errors);
}

// CHECK-REQ-0104: `.gate` and `.mlatch` cells evaluated from their Liberty functions; without
// `--gate-lib` they are outside the model.
#[test]
fn blif_cells() {
    let src = ".model s\n.inputs a b ck\n.outputs y q\n.gate wt_nand2_x1 A=a B=b Y=n\n\
               .gate wt_inv_x1 A=n Y=y\n.mlatch wt_dff_x1 D=y Q=q ck 1\n.end\n";
    let netlist = "module s(a, b, ck, init, y, q);\n  input a, b, ck, init; output y, q;\n\
         wire n;\n  wt_and2_x1 g0 (.A(a), .B(b), .Y(y));\n\
         wt_dffs_x1 r0 (.D(y), .CK(ck), .SN(n), .Q(q));\n  wt_inv_x1 g1 (.A(init), .Y(n));\n\
         endmodule\n";
    let mut o = opts(("s.blif", src), netlist);
    o.gate_libs = vec![rich()];
    let r = run(&o);
    assert_eq!(r.verdict, Verdict::Equivalent, "{r:?}");
    o.gate_libs.clear();
    let r = run(&o);
    assert_eq!(r.verdict, Verdict::Error);
    assert!(r.errors[0].contains("C0902"), "{:?}", r.errors);
}

/// Collects the messages of a check.
#[derive(Default)]
struct Messages(Mutex<Vec<Diag>>);

impl Observer for Messages {
    fn event(&self, e: &Event) {
        if let Event::Diagnostic(d) = e {
            if let Ok(mut v) = self.0.lock() {
                v.push(d.clone());
            }
        }
    }
}

// CHECK-REQ-0101: constructs that change the function are errors; constructs without effect are
// warnings, never skipped silently.
#[test]
fn blif_constructs() {
    for c in [
        ".search x.blif",
        ".start_kiss",
        ".frobnicate",
        ".latch a y as a",
    ] {
        let t = format!(".model m\n.inputs a b\n.outputs y\n{c}\n.names a b y\n11 1\n.end\n");
        let r = run(&opts(("m.blif", &t), AND_NETLIST));
        assert_eq!(r.verdict, Verdict::Error, "{c}");
        assert!(r.errors[0].contains("C0900"), "{:?}", r.errors);
    }
    let t = ".model m\n.inputs a b\n.outputs y\n.wire_load_slope 0.00\n.names a b y\n11 1\n\
             .exdc\n.names a b y\n00 1\n.end\n";
    let seen = Messages::default();
    let r = check_with(
        &opts(("m.blif", t), AND_NETLIST),
        &[rich()],
        Ctx::new(None, Some(&seen)),
    )
    .unwrap();
    assert_eq!(r.verdict, Verdict::Equivalent);
    let codes: Vec<String> = seen
        .0
        .lock()
        .map(|v| v.iter().map(|d| d.code.clone()).collect())
        .unwrap_or_default();
    assert_eq!(
        codes.iter().filter(|c| *c == "C0901").count(),
        2,
        "{codes:?}"
    );
}

/// Xorshift64* pseudo-random numbers.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

// CHECK-REQ-0107: mutated BLIF and AIGER sources never panic the checker's readers.
#[test]
fn mutated_sources_never_panic() {
    let seeds = [
        (
            "m.blif",
            ".model m\n.inputs a b\n.outputs y\n.subckt c i=a j=b o=y\n.end\n\
                    .model c\n.inputs i j\n.outputs o\n.names i j o\n11 1\n.latch o q re i 1\n.end\n",
        ),
        ("m.aag", "aag 4 2 1 1 1\n2\n4\n6 8 1\n8\n8 2 7\ni0 a\nc\n"),
        ("m.aig", "aig 3 2 0 1 1\n6\n\u{2}\u{2}i0 a\n"),
    ];
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let lib = rich();
    for k in 0..300 {
        let (name, text) = seeds[k % seeds.len()];
        let mut b = text.as_bytes().to_vec();
        for _ in 0..1 + rng.next() % 4 {
            let at = (rng.next() as usize) % b.len();
            b[at] = b" .\n01=-\\#aig"[(rng.next() % 12) as usize];
        }
        let Ok(t) = String::from_utf8(b) else {
            continue;
        };
        let r = check(&opts((name, &t), AND_NETLIST), std::slice::from_ref(&lib));
        assert!(!r.internal_error, "{t}\n{r:?}");
    }
}

// CHECK-REQ-0100: the checker reads BLIF and AIGER with readers of its own: no source of the
// checker names the shared readers that the engine uses.
#[test]
fn readers_are_the_checkers_own() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let t = std::fs::read_to_string(&p).unwrap();
                for shared in ["rusteda_formats::blif", "rusteda_formats::aiger"] {
                    assert!(!t.contains(shared), "{} uses {shared}", p.display());
                }
                seen += 1;
            }
        }
    }
    assert!(seen > 10);
}
