// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The AIGER reader (`docs/specs/aiger/`) on hand-written graphs in both forms with known
//! answers, malformed files and a deterministic mutation suite.

use rusteda_formats::aiger::{Aig, Diag, Limits, Options, Reset, lit, read_bytes};

fn read(b: &[u8]) -> Result<Aig, Vec<Diag>> {
    read_bytes(b, "t.aig", &Options::default())
}

fn ok(b: &[u8]) -> Aig {
    read(b).unwrap_or_else(|e| panic!("{e:?}"))
}

/// The code and the line of the first error.
fn fails(b: &[u8]) -> (String, u32) {
    let e = read(b).expect_err("an error");
    let line = e[0].location.as_ref().map_or(0, |l| l.line);
    (e[0].code.clone(), line)
}

/// An unsigned number in 7-bit groups, least significant first (AIGER-REQ-0013).
fn varint(mut x: u32, out: &mut Vec<u8>) {
    while x >= 0x80 {
        out.push((x & 0x7f) as u8 | 0x80);
        x >>= 7;
    }
    out.push(x as u8);
}

/// The binary form of a graph whose inputs, latches and gates define variables 1, 2, ... in
/// order: header, latch lines, output lines, binary gates and `tail`.
fn binary(
    m: u32,
    i: u32,
    latches: &[(u32, Option<u32>)],
    outs: &[u32],
    ands: &[(u32, u32)],
    tail: &str,
) -> Vec<u8> {
    let l = latches.len() as u32;
    let a = ands.len() as u32;
    let mut b = format!("aig {m} {i} {l} {} {a}\n", outs.len()).into_bytes();
    for (next, reset) in latches {
        match reset {
            Some(r) => b.extend(format!("{next} {r}\n").bytes()),
            None => b.extend(format!("{next}\n").bytes()),
        }
    }
    for o in outs {
        b.extend(format!("{o}\n").bytes());
    }
    for (k, (r0, r1)) in ands.iter().enumerate() {
        let lhs = 2 * (i + l + k as u32 + 1);
        let (r0, r1) = if r0 >= r1 { (*r0, *r1) } else { (*r1, *r0) };
        varint(lhs - r0, &mut b);
        varint(r0 - r1, &mut b);
    }
    b.extend(tail.bytes());
    b
}

/// A half adder with a latch: inputs x (2) and y (4), latch q (6) taking the sum, sum
/// (gates 8, 10, 12: `!(x & y) & !(!x & !y)`) and carry (gate 14).
const HALF: &str = "aag 7 2 1 3 4\n2\n4\n6 12 1\n12\n14\n6\n8 2 4\n10 3 5\n12 9 11\n14 2 4\n\
                    i0 x\ni1 y\nl0 q\no0 sum\no1 carry\nc\nfree text\n";

fn check_half(g: &Aig) {
    assert_eq!(g.max_var, 7);
    assert_eq!(g.inputs, [2, 4]);
    assert_eq!(g.latches.len(), 1);
    assert_eq!(g.latches[0].reset, Reset::One);
    assert_eq!(g.outputs, [12, 14, 6]);
    for m in 0..8u32 {
        let mut v = vec![false; 8];
        v[1] = m & 1 != 0;
        v[2] = m & 2 != 0;
        v[3] = m & 4 != 0;
        g.eval(&mut v);
        assert_eq!(lit(&v, g.outputs[0]), v[1] ^ v[2], "{m}");
        assert_eq!(lit(&v, g.outputs[1]), v[1] && v[2], "{m}");
        assert_eq!(lit(&v, g.latches[0].next), v[1] ^ v[2], "{m}");
    }
}

// AIGER-REQ-0001, AIGER-REQ-0002, AIGER-REQ-0012, AIGER-REQ-0021, AIGER-REQ-0023,
// AIGER-REQ-0024: an ASCII graph with a latch, symbols and comments.
#[test]
fn ascii_graph() {
    let g = ok(HALF.as_bytes());
    check_half(&g);
    assert!(!g.binary);
    assert_eq!(g.symbols.inputs.get(&1).map(String::as_str), Some("y"));
    assert_eq!(g.symbols.latches.get(&0).map(String::as_str), Some("q"));
    assert_eq!(g.symbols.outputs.get(&1).map(String::as_str), Some("carry"));
    assert!(!g.symbols.outputs.contains_key(&2));
}

// AIGER-REQ-0013: the same graph in the binary form.
#[test]
fn binary_graph() {
    let b = binary(
        7,
        2,
        &[(12, Some(1))],
        &[12, 14, 6],
        &[(2, 4), (3, 5), (9, 11), (2, 4)],
        "i0 x\nl0 q\nc\n",
    );
    let g = ok(&b);
    assert!(g.binary);
    check_half(&g);
    assert_eq!(g.ands[2], [12, 11, 9]);
    // Differences of several bytes.
    let mut b = b"aig 300 2 0 1 298\n600\n".to_vec();
    for k in 0..298u32 {
        let lhs = 2 * (k + 3);
        let (r0, r1) = if k == 0 { (4, 2) } else { (lhs - 2, 2) };
        varint(lhs - r0, &mut b);
        varint(r0 - r1, &mut b);
    }
    let g = ok(&b);
    assert_eq!(g.ands.len(), 298);
    assert_eq!(g.ands[297], [600, 598, 2]);
}

// AIGER-REQ-0012: gates in any order are returned in an order where every gate follows the
// gates it reads; cycles are found.
#[test]
fn ascii_order_and_cycles() {
    let g = ok(b"aag 5 2 0 1 3\n2\n4\n10\n10 8 6\n8 2 4\n6 3 5\n");
    let pos = |lhs: u32| g.ands.iter().position(|a| a[0] == lhs).unwrap();
    assert!(pos(8) < pos(10) && pos(6) < pos(10));
    assert_eq!(
        fails(b"aag 4 1 0 1 3\n2\n8\n4 2 8\n6 4 2\n8 6 2\n"),
        ("G0005".into(), 4)
    );
    assert_eq!(fails(b"aag 2 1 0 1 1\n2\n4\n4 4 2\n").0, "G0005");
}

// AIGER-REQ-0010: the header.
#[test]
fn headers() {
    for h in [
        &b""[..],
        b"aiger 1 1 0 0 0\n2\n",
        b"aag 1 1 0 0\n2\n",
        b"aag 1 1 0 0 0 0 0 0 0 0\n2\n",
        b"aag 01 1 0 0 0\n2\n",
        b"aag -1 1 0 0 0\n2\n",
        b"aag  1 1 0 0 0\n2\n",
        b"aag 1 1 0 0 0 \n2\n",
        b"aag 4294967296 1 0 0 0\n2\n",
        b"aag 1 2 0 0 0\n2\n4\n",
        b"aig 3 1 0 0 0\n",
    ] {
        assert_eq!(
            fails(h),
            ("G0002".into(), 1),
            "{}",
            String::from_utf8_lossy(h)
        );
    }
    // Missing trailing numbers are 0; unused variables are allowed in the ASCII form.
    let g = ok(b"aag 5 1 0 1 0 0 0\n2\n3\n");
    assert_eq!((g.max_var, g.outputs.clone()), (5, vec![3]));
}

// AIGER-REQ-0011: definitions and references.
#[test]
fn definitions() {
    for (t, want, line) in [
        (&b"aag 1 1 0 0 0\n3\n"[..], "G0003", 2),
        (b"aag 1 1 0 0 0\n0\n", "G0003", 2),
        (b"aag 2 2 0 0 0\n2\n2\n", "G0003", 3),
        (b"aag 1 1 0 1 0\n2\n4\n", "G0003", 3),
        (b"aag 2 1 0 1 1\n2\n4\n4 2 6\n", "G0003", 4),
        (b"aag 3 1 0 1 1\n2\n6\n4 2 2\n", "G0004", 3),
        (b"aag 3 1 0 1 1\n2\n4\n4 2 6\n", "G0004", 4),
        (b"aag 2 1 1 0 0\n2\n4 6\n", "G0003", 3),
    ] {
        assert_eq!(
            fails(t),
            (want.into(), line),
            "{}",
            String::from_utf8_lossy(t)
        );
    }
}

// AIGER-REQ-0012, AIGER-REQ-0020: line forms; carriage returns and a missing final line feed.
#[test]
fn lines() {
    let g = ok(b"aag 3 2 0 1 1\r\n2\r\n4\r\n6\r\n6 2 4");
    assert_eq!(g.ands, [[6, 2, 4]]);
    for t in [
        &b"aag 3 2 0 1 1\n2\n4\n6\n6 2\n"[..],
        b"aag 3 2 0 1 1\n2\n4\n6\n6 2 4 4\n",
        b"aag 3 2 0 1 1\n2\n4\n6\n6 2 x\n",
        b"aag 3 2 0 1 1\n2\n4\n6\n\n",
    ] {
        assert_eq!(fails(t).0, "G0009", "{}", String::from_utf8_lossy(t));
    }
}

// AIGER-REQ-0021: reset values 0 (also by default), 1 and uninitialized; anything else.
#[test]
fn latch_resets() {
    let g = ok(b"aag 4 1 3 0 0\n2\n4 2\n6 2 1\n8 2 8\n");
    let r: Vec<Reset> = g.latches.iter().map(|l| l.reset).collect();
    assert_eq!(r, [Reset::Zero, Reset::One, Reset::Uninitialized]);
    assert_eq!(fails(b"aag 2 1 1 0 0\n2\n4 2 6\n"), ("G0003".into(), 3));
    let b = binary(2, 1, &[(2, Some(4))], &[], &[], "");
    assert_eq!(ok(&b).latches[0].reset, Reset::Uninitialized);
}

// AIGER-REQ-0022: properties are rejected unless allowed, and then returned.
#[test]
fn properties() {
    let t = b"aag 2 1 1 0 0 1 1 1 1\n2\n4 3\n4\n2\n2\n3\n4\n5\n";
    assert_eq!(fails(t), ("G0010".into(), 1));
    let g = read_bytes(
        t,
        "p.aag",
        &Options {
            allow_properties: true,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(g.bad, [4]);
    assert_eq!(g.constraints, [2]);
    assert_eq!(g.justice, [vec![3, 4]]);
    assert_eq!(g.fairness, [5]);
}

// AIGER-REQ-0013: malformed binary gates.
#[test]
fn binary_errors() {
    // A zero first difference, a difference above the left-hand side, data that ends inside the
    // gates, a number of six bytes.
    let mut zero = b"aig 2 1 0 1 1\n4\n".to_vec();
    zero.extend([0, 0]);
    let mut above = b"aig 2 1 0 1 1\n4\n".to_vec();
    above.extend([5, 0]);
    let mut short = b"aig 3 1 0 1 2\n6\n".to_vec();
    short.extend([2]);
    let mut long = b"aig 2 1 0 1 1\n4\n".to_vec();
    long.extend([0x81, 0x80, 0x80, 0x80, 0x80, 0x00, 0]);
    for (b, want) in [
        (zero, "G0006"),
        (above, "G0006"),
        (short, "G0008"),
        (long, "G0006"),
    ] {
        let e = read(&b).unwrap_err();
        assert_eq!(e[0].code, want, "{e:?}");
    }
    let mut late = b"aig 3 1 0 1 2\n6\n".to_vec();
    late.extend([2, 0, 0x82]);
    let e = read(&late).unwrap_err();
    assert_eq!(e[0].code, "G0006");
    assert_eq!(e[0].location.as_ref().unwrap().line, 3);
    assert!(e[0].message.contains("AND gate 1"), "{e:?}");
}

// AIGER-REQ-0023, AIGER-REQ-0024: symbol errors and trailing text.
#[test]
fn symbol_errors() {
    let base = "aag 3 2 0 1 1\n2\n4\n6\n6 2 4\n";
    for s in [
        "i2 z\n",
        "i0 a\ni0 b\n",
        "x0 a\n",
        "i0\n",
        "i0 \n",
        "\n",
        "o 1\n",
        "c0 a\n",
    ] {
        let t = format!("{base}{s}");
        assert_eq!(fails(t.as_bytes()).0, "G0007", "{s:?}");
    }
    let g = ok(format!("{base}i0 a name with blanks\nc\nanything \u{1} at all\n").as_bytes());
    assert_eq!(g.symbols.inputs[&0], "a name with blanks");
}

// AIGER-REQ-0030: limits checked against the header before allocation.
#[test]
fn limits() {
    let small = Options {
        limits: Limits {
            max_vars: 10,
            max_items: 2,
        },
        ..Options::default()
    };
    let e = read_bytes(b"aag 11 0 0 0 0\n", "t", &small).unwrap_err();
    assert_eq!(e[0].code, "G0008");
    let e = read_bytes(b"aag 1 1 0 3 0\n2\n2\n2\n2\n", "t", &small).unwrap_err();
    assert_eq!(e[0].code, "G0008");
    // Headers that announce more than the data holds.
    assert_eq!(fails(b"aig 16000000 0 0 0 16000000\n").0, "G0008");
    assert_eq!(fails(b"aag 16000000 16000000 0 0 0\n2\n").0, "G0008");
    assert_eq!(fails(b"aag 1 0 0 10000000 0\n").0, "G0008");
    // A binary header with many implicit inputs is bounded by `max_vars` only.
    let g = ok(b"aig 1000 1000 0 0 0\n");
    assert_eq!(g.inputs.len(), 1000);
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

// AIGER-REQ-0031, AIGER-REQ-0032: mutated, truncated and random files never panic, and every
// error is located.
#[test]
fn mutations_never_panic() {
    let bin = binary(
        7,
        2,
        &[(12, Some(1))],
        &[12, 14, 6],
        &[(2, 4), (3, 5), (9, 11), (2, 4)],
        "i0 x\nc\n",
    );
    let seeds = [HALF.as_bytes().to_vec(), bin];
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for k in 0..6000 {
        let mut b = seeds[k % 2].clone();
        match k % 3 {
            0 => {
                for _ in 0..1 + rng.below(4) {
                    let at = rng.below(b.len());
                    b[at] = rng.next() as u8;
                }
            }
            1 => b.truncate(rng.below(b.len() + 1)),
            _ => {
                let at = rng.below(b.len() + 1);
                let byte = b" 0123456789\n\x80\xffaigc"[rng.below(18)];
                b.insert(at, byte);
            }
        }
        if let Err(errs) = read(&b) {
            for d in errs {
                assert!(d.code.starts_with('G'), "{d:?}");
                assert!(d.location.is_some(), "{d:?}");
            }
        }
    }
    for n in 0..300 {
        let b: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        let _ = read(&b);
        let mut h = b"aig 9 2 1 1 6\n".to_vec();
        h.extend(&b);
        let _ = read(&h);
    }
}
