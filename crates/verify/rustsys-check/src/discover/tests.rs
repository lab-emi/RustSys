// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use std::collections::{HashMap, HashSet};

use rusteda_formats::verilog::{PpOptions, SourceMap};
use rustsys_fixtures::liberty::{Variant, library};

use super::Search;
use crate::circ::Circuit;
use crate::corr::{self, Setting};
use crate::{Input, netlist, read, reset, rtl};

#[test]
fn random_words_go_to_the_same_variables_in_every_search() {
    // The order of the free variables decides which random word each gets; it must not
    // depend on hashing (CHECK-REQ-0043).
    let rtl_text = "module m(input clk, rn, en, d, output logic q, r);
  logic a, b, k;
  always_ff @(posedge clk or negedge rn)
if (!rn) begin a <= 1; b <= 1; k <= 0; end
else begin if (en) begin a <= d; b <= d; end k <= k & d; end
  assign q = a ^ k;
  assign r = b & en;
endmodule";
    let net = "module m(input clk, rn, en, d, output q, r);
  wire st, nd, ndn, iq;
  wtf_mux2_1 u1 (.A0(st), .A1(d), .S(en), .X(nd));
  wtf_inv_1 u3 (.A(nd), .Y(ndn));
  wtf_dffr_1 u2 (.D(ndn), .CLK(clk), .RESET_B(rn), .Q(iq), .Q_N(st));
  wtf_buf_1 u4 (.A(st), .X(q));
  wtf_and2_1 u5 (.A(st), .B(en), .X(r));
endmodule";
    let lib =
        rusteda_formats::liberty::read_text(&library(Variant::Full), "fixture").expect("fixture");
    let text = |name: &str, t: &str| Input::Texts(vec![(name.into(), t.into())]);
    let mut sm = SourceMap::new();
    let units = read(&mut sm, &text("m.sv", rtl_text), PpOptions::default()).expect("rtl");
    let mut c = Circuit::new();
    let model = rtl::build(&sm, &units, &mut c, &rtl::RtlOptions::default()).expect("model");
    let mut sm2 = SourceMap::new();
    let nunits = read(&mut sm2, &text("m.v", net), PpOptions::default()).expect("netlist");
    let inputs: HashMap<String, Vec<u32>> = model
        .inputs
        .iter()
        .map(|(n, v, _)| (n.clone(), v.clone()))
        .collect();
    let nm = netlist::build(
        &mut c,
        &sm2,
        &nunits,
        &[lib],
        &model.top,
        Some(&inputs),
        &[],
        &[],
    )
    .expect("nm");
    let pulse = reset::pulse(&model, &[("rn".into(), false)]).expect("pulse");
    let control_vars = HashSet::new();
    let s = Setting {
        c: &c,
        model: &model,
        nm: &nm,
        pulse: &pulse,
        control_vars: &control_vars,
        uninitialized_dont_care: false,
    };
    let named = corr::by_name(&s).expect("names");
    assert!(!named.unused.is_empty() && named.unmatched.len() == 3);
    let first = Search::new(&s, &named, 1).expect("search").frees;
    for _ in 0..16 {
        assert_eq!(Search::new(&s, &named, 1).expect("search").frees, first);
    }
}
