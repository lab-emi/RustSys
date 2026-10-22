// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The BLIF reader (`docs/specs/blif/`) on hand-written texts with known answers, malformed
//! texts and a deterministic mutation suite.

use rusteda_formats::blif::{Diag, Init, LatchType, Limits, Network, Options, read_texts};

fn read(text: &str) -> Result<Network, Vec<Diag>> {
    read_named(&[("m.blif", text)], &Options::default())
}

fn read_named(texts: &[(&str, &str)], opts: &Options) -> Result<Network, Vec<Diag>> {
    read_texts(texts, opts).map(|r| r.network)
}

fn ok(text: &str) -> Network {
    read(text).unwrap_or_else(|e| panic!("{e:?}"))
}

/// The code of the first error, with its location.
fn fails(text: &str) -> (String, String) {
    let e = read(text).expect_err("an error");
    let d = &e[0];
    let at = d.location.as_ref().map_or(String::new(), |l| {
        format!("{}:{}:{}", l.file, l.line, l.column)
    });
    (d.code.clone(), at)
}

fn code(text: &str) -> String {
    fails(text).0
}

/// The value of output `o` of a combinational network for input values `x`, by evaluating the
/// nodes in an order where every node follows its inputs.
fn eval(n: &Network, x: &[bool]) -> Vec<bool> {
    let mut v: Vec<Option<bool>> = vec![None; n.signals.len()];
    for (s, b) in n.inputs.iter().zip(x) {
        v[*s as usize] = Some(*b);
    }
    for _ in 0..=n.nodes.len() {
        for nd in &n.nodes {
            let ins: Option<Vec<bool>> = nd.inputs.iter().map(|s| v[*s as usize]).collect();
            if let Some(ins) = ins {
                v[nd.output as usize] = Some(nd.cover.eval(&ins));
            }
        }
    }
    n.outputs.iter().map(|o| v[*o as usize].unwrap()).collect()
}

// BLIF-REQ-0001, BLIF-REQ-0003, BLIF-REQ-0030: covers of the on-set and the off-set, constants,
// an empty cover, `-` literals and a signal read twice, with their truth tables.
#[test]
fn covers_and_their_functions() {
    let n = ok(".model top\n\
         .inputs a b c\n\
         .outputs y z k1 k0 e t\n\
         .names a b c y\n\
         11- 1\n\
         --1 1\n\
         .names a b z\n\
         11 0\n\
         .names k1\n\
         1\n\
         .names k0\n\
         0\n\
         .names e\n\
         .names a a t\n\
         10 1\n\
         .end\n");
    assert_eq!(n.name, "top");
    assert_eq!(n.inputs.len(), 3);
    assert_eq!(n.outputs.len(), 6);
    assert_eq!(n.nodes.len(), 6);
    for m in 0..8u32 {
        let (a, b, c) = (m & 1 != 0, m & 2 != 0, m & 4 != 0);
        let got = eval(&n, &[a, b, c]);
        assert_eq!(
            got,
            [a && b || c, !(a && b), true, false, false, false],
            "{m}"
        );
    }
    let y = &n.nodes[0];
    assert!(y.cover.onset && y.cover.len() == 2 && y.cover.row(1) == b"--1");
    assert!(!n.nodes[1].cover.onset);
}

// BLIF-REQ-0010, BLIF-REQ-0011: comments, continuation lines, carriage returns and empty lines;
// a name with a control character is rejected.
#[test]
fn text_rules() {
    let n = ok(
        "# header\r\n.model top # name\r\n.inputs a \\\r\n b\r\n\r\n.outputs y\r\n\
         .names a \\\n b y\n11 1\n.end\n",
    );
    assert_eq!(n.inputs.len(), 2);
    assert_eq!(eval(&n, &[true, true]), [true]);
    assert_eq!(eval(&n, &[true, false]), [false]);
    assert_eq!(
        fails(".model top\n.inputs a\u{1}b\n.end\n"),
        ("B0002".into(), "m.blif:2:9".into())
    );
}

// BLIF-REQ-0020, BLIF-REQ-0021: default model name, `.end` optional, repeated port lists,
// clocks after inputs; ports and models declared twice; commands outside a model.
#[test]
fn models_and_ports() {
    let n = read_named(
        &[(
            "dir/design.blif",
            ".model\n.inputs a\n.clock ck\n.inputs b\n.outputs y\n.names a b y\n11 1\n",
        )],
        &Options::default(),
    )
    .unwrap();
    assert_eq!(n.name, "design");
    let names: Vec<&str> = n.inputs.iter().map(|&s| n.name(s)).collect();
    assert_eq!(names, ["a", "b", "ck"]);
    assert_eq!(n.clocks.len(), 1);
    assert_eq!(n.name(n.clocks[0]), "ck");
    assert_eq!(code(".model m\n.inputs a a\n.end\n"), "B0005");
    assert_eq!(code(".model m\n.inputs a\n.clock a\n.end\n"), "B0005");
    assert_eq!(code(".model m\n.outputs y\n.outputs y\n.end\n"), "B0005");
    assert_eq!(
        code(".model m\n.inputs a\n.outputs a\n.end\n.model m\n.end\n"),
        "B0005"
    );
    assert_eq!(fails(".inputs a\n"), ("B0002".into(), "m.blif:1:1".into()));
    assert_eq!(code("# nothing\n"), "B0002");
    assert_eq!(code(".model m\n.end\n.end\n"), "B0002");
    // A signal may be both an input and an output.
    let n = ok(".model m\n.inputs a\n.outputs a\n.end\n");
    assert_eq!((n.inputs[0], n.outputs[0]), (n.inputs[0], n.inputs[0]));
}

// BLIF-REQ-0022, BLIF-REQ-0023: one driver per signal; undriven reads and outputs; unused
// nodes are kept.
#[test]
fn drivers() {
    assert_eq!(
        fails(".model m\n.inputs a\n.outputs y\n.names a y\n1 1\n.names a y\n0 1\n.end\n"),
        ("B0006".into(), "m.blif:6:1".into())
    );
    assert_eq!(
        code(".model m\n.inputs a\n.outputs y\n.names a a\n1 1\n.names a y\n1 1\n.end\n"),
        "B0006"
    );
    // Signals without a driver are kept, with one warning that names them.
    let undriven = |t: &str| {
        let r = read_texts(&[("m.blif", t)], &Options::default()).unwrap();
        let w = r
            .warnings
            .iter()
            .find(|d| d.code == "B0007")
            .map(|d| d.render_first_line())
            .unwrap_or_default();
        (r.network, w)
    };
    let (n, w) = undriven(".model m\n.inputs a\n.outputs y\n.names a b y\n11 1\n.end\n");
    assert!(
        w.starts_with("warning[B0007]: m.blif:4:1: 1 signals"),
        "{w}"
    );
    assert!(w.ends_with(": b"), "{w}");
    assert_eq!(n.nodes.len(), 1);
    let (_, w) = undriven(".model m\n.inputs a\n.outputs y z w\n.names a y\n1 1\n.end\n");
    assert!(
        w.starts_with("warning[B0007]: m.blif:3:12: 2 signals"),
        "{w}"
    );
    assert!(w.ends_with(": z, w"), "{w}");
    let n = ok(".model m\n.inputs a\n.outputs y\n.names a y\n1 1\n.names a u\n0 1\n.end\n");
    assert_eq!(n.nodes.len(), 2);
}

// BLIF-REQ-0030: malformed cover rows.
#[test]
fn bad_rows() {
    for t in [
        ".model m\n.inputs a b\n.outputs y\n.names a b y\n1 1\n.end\n",
        ".model m\n.inputs a b\n.outputs y\n.names a b y\n1x 1\n.end\n",
        ".model m\n.inputs a b\n.outputs y\n.names a b y\n11 2\n.end\n",
        ".model m\n.inputs a b\n.outputs y\n.names a b y\n11 1\n00 0\n.end\n",
        ".model m\n.inputs a b\n.outputs y\n.names a b y\n11\n.end\n",
        ".model m\n.outputs y\n.names y\n1 1\n.end\n",
        ".model m\n.outputs y\n11 1\n.end\n",
    ] {
        assert_eq!(code(t), "B0003", "{t}");
    }
}

// BLIF-REQ-0031, BLIF-REQ-0032: the forms of `.latch`, `NIL`, initial values and their
// defaults; malformed latches; the type `as`.
#[test]
fn latches() {
    let n = ok(".model m\n.inputs d c\n.outputs q1 q2 q3 q4 q5\n\
         .latch d q1\n.latch d q2 1\n.latch d q3 re c\n.latch d q4 fe NIL 0\n\
         .latch d q5 al c 2\n.end\n");
    let l = &n.latches;
    assert_eq!(l.len(), 5);
    assert_eq!(
        (l[0].kind, l[0].control, l[0].init),
        (None, None, Init::Unknown)
    );
    assert_eq!((l[1].kind, l[1].init), (None, Init::One));
    assert_eq!(l[2].kind, Some(LatchType::RisingEdge));
    assert_eq!(n.name(l[2].control.unwrap()), "c");
    assert_eq!(
        (l[3].kind, l[3].control, l[3].init),
        (Some(LatchType::FallingEdge), None, Init::Zero)
    );
    assert_eq!(
        (l[4].kind, l[4].init),
        (Some(LatchType::ActiveLow), Init::DontCare)
    );
    assert_eq!(Init::One.value(), Some(true));
    assert_eq!(Init::Unknown.value(), None);
    for t in [
        ".model m\n.inputs d\n.outputs q\n.latch d\n.end\n",
        ".model m\n.inputs d\n.outputs q\n.latch d q 4\n.end\n",
        ".model m\n.inputs d c\n.outputs q\n.latch d q xx c\n.end\n",
        ".model m\n.inputs d c\n.outputs q\n.latch d q re c 0 9\n.end\n",
    ] {
        assert_eq!(code(t), "B0004", "{t}");
    }
    assert_eq!(
        code(".model m\n.inputs d c\n.outputs q\n.latch d q as c\n.end\n"),
        "B0010"
    );
    // A latch output has one driver; its control is read.
    assert_eq!(
        code(".model m\n.inputs d\n.outputs q\n.latch d q 0\n.names d q\n1 1\n.end\n"),
        "B0006"
    );
    let r = read_texts(
        &[(
            "m.blif",
            ".model m\n.inputs d\n.outputs q\n.latch d q re c\n.end\n",
        )],
        &Options::default(),
    )
    .unwrap();
    assert!(r.warnings.iter().any(|d| d.code == "B0007"));
}

// BLIF-REQ-0033, BLIF-REQ-0034: `.gate` and `.mlatch` are recorded with their connections; signals
// that cells connect count as driven.
#[test]
fn cells() {
    let n = ok(".model m\n.inputs a b ck\n.outputs y q\n\
         .gate NAND2 A=a B=b Y=n1\n.gate INV A=n1 Y=y\n.mlatch DFF D=y Q=q ck 1\n.end\n");
    assert_eq!(n.cells.len(), 3);
    assert_eq!(n.cells[0].cell, "NAND2");
    let conns: Vec<(&str, &str)> = n.cells[0]
        .conns
        .iter()
        .map(|(p, s)| (p.as_str(), n.name(*s)))
        .collect();
    assert_eq!(conns, [("A", "a"), ("B", "b"), ("Y", "n1")]);
    let (ctl, init) = n.cells[2].mlatch.unwrap();
    assert_eq!((n.name(ctl.unwrap()), init), ("ck", Init::One));
    let n = ok(".model m\n.inputs d\n.outputs q\n.mlatch DFF D=d Q=q NIL\n.end\n");
    assert_eq!(n.cells[0].mlatch, Some((None, Init::Unknown)));
    for t in [
        ".model m\n.inputs a\n.outputs y\n.gate INV A=a A=a Y=y\n.end\n",
        ".model m\n.inputs a\n.outputs y\n.gate INV a Y=y\n.end\n",
        ".model m\n.inputs a\n.outputs y\n.gate INV =a Y=y\n.end\n",
        ".model m\n.inputs a\n.outputs y\n.gate\n.end\n",
        ".model m\n.inputs a\n.outputs y\n.mlatch DFF D=a Q=y\n.end\n",
        ".model m\n.inputs a\n.outputs y\n.gate INV A=a Y=y extra\n.end\n",
    ] {
        assert_eq!(code(t), "B0002", "{t}");
    }
}

const HIER: &str = ".model top\n.inputs a b\n.outputs y\n\
     .subckt half x=a y=b s=t\n.subckt inv i=t o=y\n.end\n\
     .model half\n.inputs x y\n.outputs s c\n.names x y n\n11 0\n.names n s\n1 1\n\
     .names x y c\n11 1\n.end\n\
     .model inv\n.inputs i\n.outputs o\n.latch i q 0\n.names q o\n0 1\n.end\n";

// BLIF-REQ-0002, BLIF-REQ-0040, BLIF-REQ-0041, BLIF-REQ-0042: flattening, instance names and
// the order of the depth-first walk; unconnected formal outputs.
#[test]
fn hierarchy() {
    let n = ok(HIER);
    assert_eq!(n.name, "top");
    let outs: Vec<&str> = n.nodes.iter().map(|nd| n.name(nd.output)).collect();
    assert_eq!(outs, ["half_0.n", "t", "half_0.c", "y"]);
    assert_eq!(n.name(n.latches[0].output), "inv_1.q");
    assert_eq!(n.name(n.latches[0].input), "t");
    assert_eq!(n.latches[0].init, Init::Zero);
    // Another top model.
    let n = read_named(
        &[("h.blif", HIER)],
        &Options {
            top: Some("half".into()),
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(n.name, "half");
    assert_eq!(n.nodes.len(), 3);
    let e = read_named(
        &[("h.blif", HIER)],
        &Options {
            top: Some("nope".into()),
            ..Options::default()
        },
    )
    .unwrap_err();
    assert_eq!(e[0].code, "B0013");
    // Nested instances and models spread over files.
    let n = read_named(
        &[
            (
                "a.blif",
                ".model top\n.inputs a\n.outputs y\n.subckt mid i=a o=y\n.end\n",
            ),
            (
                "b.blif",
                ".model mid\n.inputs i\n.outputs o\n.subckt leaf i=i o=o\n.end\n\
                 .model leaf\n.inputs i\n.outputs o\n.names i w\n1 1\n.names w o\n1 1\n.end\n",
            ),
        ],
        &Options::default(),
    )
    .unwrap();
    let outs: Vec<&str> = n.nodes.iter().map(|nd| n.name(nd.output)).collect();
    assert_eq!(outs, ["mid_0.leaf_0.w", "y"]);
}

// BLIF-REQ-0040, BLIF-REQ-0041: instance errors, recursion and name collisions.
#[test]
fn hierarchy_errors() {
    let leaf = ".model leaf\n.inputs i\n.outputs o\n.names i o\n1 1\n.end\n";
    for (top, want) in [
        (
            ".model top\n.inputs a\n.outputs y\n.subckt nope i=a o=y\n.end\n",
            "B0008",
        ),
        (
            ".model top\n.inputs a\n.outputs y\n.subckt leaf j=a o=y\n.end\n",
            "B0008",
        ),
        (
            ".model top\n.inputs a\n.outputs y\n.subckt leaf o=y\n.end\n",
            "B0008",
        ),
        (
            ".model top\n.inputs a\n.outputs y\n.subckt leaf i=a i=a o=y\n.end\n",
            "B0002",
        ),
        (
            ".model top\n.inputs a\n.outputs y\n.subckt leaf i=a o=y\n.subckt leaf i=a o=y\n.end\n",
            "B0006",
        ),
        (
            ".model top\n.inputs a\n.outputs y leaf_0.o\n.subckt leaf i=a\n.names a y\n1 1\n.names a leaf_0.o\n1 1\n.end\n",
            "B0012",
        ),
    ] {
        let text = format!("{top}{leaf}");
        assert_eq!(code(&text), want, "{top}");
    }
    let rec = ".model top\n.inputs a\n.outputs y\n.subckt r i=a o=y\n.end\n\
               .model r\n.inputs i\n.outputs o\n.subckt s i=i o=o\n.end\n\
               .model s\n.inputs i\n.outputs o\n.subckt r i=i o=o\n.end\n";
    assert_eq!(fails(rec), ("B0009".into(), "m.blif:14:1".into()));
    let bb = ".model top\n.inputs a\n.outputs y\n.subckt b i=a o=y\n.end\n\
              .model b\n.inputs i\n.outputs o\n.blackbox\n.end\n";
    assert_eq!(code(bb), "B0010");
    assert_eq!(
        code(".model b\n.inputs i\n.outputs o\n.blackbox\n.end\n"),
        "B0010"
    );
}

// BLIF-REQ-0050: constructs that would change the function are errors.
#[test]
fn unsupported_constructs() {
    for c in [
        ".search other.blif",
        ".start_kiss",
        ".latch_order a",
        ".code s0 00",
        ".conn a b",
        ".frobnicate",
    ] {
        let t = format!(".model m\n.inputs a\n.outputs y\n{c}\n.names a y\n1 1\n.end\n");
        assert_eq!(fails(&t), ("B0010".into(), "m.blif:4:1".into()), "{c}");
    }
}

// BLIF-REQ-0051, BLIF-REQ-0052: timing, clock-constraint and annotation constructs are skipped
// with one warning per command and file; `.exdc` is skipped with a warning.
#[test]
fn skipped_constructs_warn() {
    let t = ".model m\n.inputs a b\n.outputs y\n.wire_load_slope 0.00\n.input_arrival a 1 1\n\
             .input_arrival b 2 2\n.names a b y\n11 1\n.exdc\n.inputs a\n.names a b y\n00 1\n\
             .end\n";
    let r = read_texts(&[("m.blif", t)], &Options::default()).unwrap();
    let w: Vec<(String, String)> = r
        .warnings
        .iter()
        .map(|d| (d.code.clone(), d.render_first_line()))
        .collect();
    assert_eq!(w.len(), 3, "{w:?}");
    assert_eq!(w[0].0, "B0021");
    assert!(w[1].1.contains(".wire_load_slope (1 occurrence)"), "{w:?}");
    assert!(
        w[2].1
            .contains("m.blif:5:1: .input_arrival (2 occurrences)"),
        "{w:?}"
    );
    // The don't-care network is not part of the function.
    assert_eq!(r.network.nodes.len(), 1);
    assert_eq!(eval(&r.network, &[false, false]), [false]);
}

// BLIF-REQ-0060: a hierarchy that doubles at every level stops at the limit.
#[test]
fn limits() {
    let mut t = String::from(".model l0\n.inputs a\n.outputs y\n.subckt l1 i=a o=y\n.end\n");
    for k in 1..40 {
        t.push_str(&format!(
            ".model l{k}\n.inputs i\n.outputs o\n.subckt l{} i=i o=w\n.subckt l{} i=w o=o\n.end\n",
            k + 1,
            k + 1
        ));
    }
    t.push_str(".model l40\n.inputs i\n.outputs o\n.names i o\n0 1\n.end\n");
    let opts = Options {
        limits: Limits {
            max_items: 1000,
            max_refs: 1 << 20,
        },
        ..Options::default()
    };
    let e = read_named(&[("m.blif", &t)], &opts).unwrap_err();
    assert_eq!(e[0].code, "B0011", "{e:?}");
    let opts = Options {
        limits: Limits {
            max_items: 1 << 20,
            max_refs: 3000,
        },
        ..Options::default()
    };
    let e = read_named(&[("m.blif", &t)], &opts).unwrap_err();
    assert_eq!(e[0].code, "B0011", "{e:?}");
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

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

// BLIF-REQ-0061, BLIF-REQ-0062: mutated texts never panic, and every error is located.
#[test]
fn mutations_never_panic() {
    let seeds = [
        HIER,
        ".model m\n.inputs a b c\n.outputs y\n.names a b c y\n1-1 1\n-11 1\n.latch y q re c 1\n.end\n",
        ".model m\n.inputs a\n.outputs y\n.gate INV A=a Y=y\n.mlatch DFF D=y Q=q a 0\n.end\n",
    ];
    let alphabet: &[u8] = b" .\n\\#=-01nameslatchsubcktinputsoutputs\r\x01";
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for k in 0..4000 {
        let mut b = seeds[k % seeds.len()].as_bytes().to_vec();
        for _ in 0..1 + rng.below(6) {
            let at = rng.below(b.len() + 1);
            match rng.below(3) {
                0 if at < b.len() => {
                    b.remove(at);
                }
                1 => b.insert(at, alphabet[rng.below(alphabet.len())]),
                _ if at < b.len() => b[at] = alphabet[rng.below(alphabet.len())],
                _ => {}
            }
        }
        let Ok(text) = String::from_utf8(b) else {
            continue;
        };
        if let Err(errs) = read(&text) {
            assert!(!errs.is_empty());
            for d in errs {
                assert!(d.code.starts_with('B'), "{d:?}");
                assert!(d.location.is_some(), "{d:?}\n{text}");
            }
        }
    }
}
