// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Randomized agreement of synthesis and the independent checker on case statements under the
//! complete-case and parallel-case assumptions (SYNTH-REQ-0224, SYNTH-REQ-0225, CHECK-REQ-0012,
//! CHECK-REQ-0013): overlapping and duplicate labels, signal labels, partial writes, nested
//! statements and default items, each synthesized and proved with the same options.

mod common;

use rustsys::{Sources, SynthOptions, synthesize};
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_fixtures::liberty::Variant;

struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "x",
    "y",
    "x ^ y",
    "x & y",
    "~x",
    "{y[1:0], x[1:0]}",
    "4'd5",
    "x + y",
];
const BITS: &[&str] = &["a", "b ^ c", "d", "x[0]", "&y", "a | d"];

fn body(r: &mut Lcg) -> String {
    let mut out = String::new();
    for _ in 0..1 + r.below(2) {
        out += &match r.below(5) {
            0 => format!("o = {};", r.pick(WORDS)),
            1 => format!("o[{}] = {};", r.below(4), r.pick(BITS)),
            2 => format!("p = {};", r.pick(BITS)),
            3 => format!(
                "if ({}) o = {}; else p = {};",
                r.pick(BITS),
                r.pick(WORDS),
                r.pick(BITS)
            ),
            _ => String::new(),
        };
        out.push(' ');
    }
    format!("begin {out}end")
}

/// A module with one marked case statement in a combinational block.
fn module(r: &mut Lcg) -> String {
    let mark = r.pick(&[
        "(* parallel_case *)",
        "unique",
        "unique0",
        "(* parallel_case, full_case *)",
        "priority",
        "",
    ]);
    let signal_labels = r.below(2) == 0;
    let mut items = String::new();
    for _ in 0..2 + r.below(4) {
        let label = if signal_labels {
            r.pick(&["a", "b", "c", "d", "a & b", "~c"]).to_string()
        } else {
            // Three-bit patterns; `?` bits and repeats make items overlap.
            (0..3)
                .map(|_| match r.below(4) {
                    0 => '?',
                    1 => '1',
                    _ => '0',
                })
                .collect::<String>()
        };
        let label = if signal_labels {
            label
        } else {
            format!("3'b{label}")
        };
        items += &format!("{label}: {}\n", body(r));
    }
    if r.below(3) == 0 {
        items += &format!("default: {}\n", body(r));
    }
    let (kind, sel) = if signal_labels {
        ("case", "1'b1")
    } else {
        ("casez", "s")
    };
    format!(
        "module m(input [2:0] s, input a, b, c, d, input [3:0] x, y,
          output reg [3:0] o, output reg p);
  always @* begin
    o = y; p = c;
    {mark} {kind} ({sel})
{items}    endcase
  end
endmodule"
    )
}

#[test]
fn synthesis_and_checker_agree_on_random_case_statements() {
    let lib = common::lib(Variant::Rich);
    let mut r = Lcg(20_261_013);
    for i in 0..40 {
        let src = module(&mut r);
        let full = r.below(2) == 0;
        let texts = vec![("m.sv".to_string(), src.clone())];
        let out = synthesize(
            &Sources::Texts(texts.clone()),
            std::slice::from_ref(&lib),
            &SynthOptions {
                assume_full_case: full,
                assume_parallel_case: true,
                ..SynthOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{i}: {:?}\n{src}", e.messages));
        let rep = check(
            &CheckOptions {
                rtl: Input::Texts(texts),
                top: Some("m".into()),
                netlist: Input::Texts(vec![("m.v".into(), out.text.clone())]),
                assume_full_case: full,
                assume_parallel_case: true,
                ..CheckOptions::default()
            },
            std::slice::from_ref(&lib),
        );
        assert_eq!(
            rep.verdict,
            Verdict::Equivalent,
            "{i} full={full}: {:?} {:?}\n{src}",
            rep.mismatches,
            rep.errors
        );
    }
}
