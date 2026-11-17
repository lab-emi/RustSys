// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Implementation state map for a re-encoded FSM (CHECK-REQ-0021): the export derives the
//! encoding, proves its valid-code invariant with base and step obligations, and states
//! `reset_reachable` semantics; a wrong encoding or an implementation that leaves the valid
//! codes gives refutable obligations, never a quietly weaker claim. A design whose state is
//! related one-to-one keeps `arbitrary_initial` semantics with the same option.
//! The exported miters are checked here by random simulation (a refutation is a definite
//! difference; agreement is not a proof). External ABC controls are recorded by the gate.
//! <40 circuit entries per case, one shared small library; <1 MiB scratch per test.

use std::path::Path;
use std::sync::OnceLock;

use rusteda_formats::liberty::Library;
use rustsys_check::{CheckOptions, EncodingHint, Input, ObligationExport, Verdict, check};

fn library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        rusteda_formats::liberty::read_text(
            &rustsys_fixtures::liberty::library(rustsys_fixtures::liberty::Variant::Rich),
            "fixture",
        )
        .unwrap()
    })
}

/// Four reachable codes of eight; none of the one-hot elements below equals one RTL bit.
const RTL: &str = "module m(input clk,rn,a,output y,z);
  localparam IDLE=3'd0, A=3'd3, B=3'd5, C=3'd6;
  reg [2:0] s;
  always @(posedge clk or negedge rn)
    if (!rn) s <= IDLE;
    else case (s)
      IDLE: s <= a ? A : IDLE;
      A: s <= B;
      B: s <= a ? C : IDLE;
      C: s <= IDLE;
      default: s <= IDLE;
    endcase
  assign y = s == C;
  assign z = s == B;
endmodule";

/// The same machine recoded as synthesis tools do: element `s0` is set in every non-reset
/// state, `s1..s3` are one-hot for A, B and C (element codes 0, 3, 5, 9).
fn netlist(next_a: &str) -> String {
    format!(
        "module m(input clk,rn,a,output y,z);
  wire n,fa,fb,fc,nn,da,dc,dn;
  wt_inv_x1 i0(.A(n),.Y(nn));
  {next_a}
  wt_and2_x1 g1(.A(fb),.B(a),.Y(dc));
  wt_or3_x1 g2(.A(da),.B(fa),.C(dc),.Y(dn));
  wt_dffr_x1 s0(.CK(clk),.RN(rn),.D(dn),.Q(n));
  wt_dffr_x1 s1(.CK(clk),.RN(rn),.D(da),.Q(fa));
  wt_dffr_x1 s2(.CK(clk),.RN(rn),.D(fa),.Q(fb));
  wt_dffr_x1 s3(.CK(clk),.RN(rn),.D(dc),.Q(fc));
  assign y=fc; assign z=fb;
endmodule"
    )
}

const NEXT_A: &str = "wt_and2_x1 g0(.A(nn),.B(a),.Y(da));";
/// Enters A from any state: from A with `a` set both A and B are on, an invalid code.
const NEXT_A_FROM_ANY: &str = "wt_and2_x1 g0(.A(a),.B(a),.Y(da));";
const ELEMENTS: [&str; 4] = ["s0", "s1", "s2", "s3"];
const TRUE_CODES: [(u64, u64); 4] = [(0, 0), (3, 3), (5, 5), (6, 9)];

fn export(case: &str, net: &str, hints: Vec<EncodingHint>) -> (std::path::PathBuf, Verdict) {
    let directory =
        std::env::temp_dir().join(format!("rustsys-state-map-{}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut options = ObligationExport::new(directory.clone());
    options.initial_state_map = true;
    options.encoding_hints = hints;
    let report = check(
        &CheckOptions {
            rtl: Input::Texts(vec![("rtl.v".into(), RTL.into())]),
            netlist: Input::Texts(vec![("net.v".into(), net.into())]),
            obligation_export: Some(options),
            ..CheckOptions::default()
        },
        std::slice::from_ref(library()),
    );
    (directory, report.verdict)
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Binary AIGER without latches: inputs, outputs as literals, and the AND gates.
struct Aig {
    inputs: usize,
    ands: Vec<(u32, u32)>,
    outputs: Vec<u32>,
}

fn read_aig(path: &Path) -> Aig {
    let bytes = std::fs::read(path).unwrap();
    let line_end = |from: usize| from + bytes[from..].iter().position(|&b| b == b'\n').unwrap();
    let end = line_end(0);
    let header: Vec<usize> = std::str::from_utf8(&bytes[..end])
        .unwrap()
        .split_whitespace()
        .skip(1)
        .map(|t| t.parse().unwrap())
        .collect();
    let (inputs, latches, count, and_count) = (header[1], header[2], header[3], header[4]);
    assert_eq!(latches, 0);
    let mut pos = end + 1;
    let mut outputs = Vec::with_capacity(count);
    for _ in 0..count {
        let end = line_end(pos);
        outputs.push(
            std::str::from_utf8(&bytes[pos..end])
                .unwrap()
                .parse()
                .unwrap(),
        );
        pos = end + 1;
    }
    let mut delta = || {
        let (mut value, mut shift) = (0u32, 0);
        loop {
            let byte = bytes[pos];
            pos += 1;
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return value;
            }
            shift += 7;
        }
    };
    let mut ands = Vec::with_capacity(and_count);
    for k in 0..and_count {
        let lhs = u32::try_from(2 * (inputs + 1 + k)).unwrap();
        let rhs0 = lhs - delta();
        let rhs1 = rhs0 - delta();
        ands.push((rhs0, rhs1));
    }
    Aig {
        inputs,
        ands,
        outputs,
    }
}

/// Outputs of 64 parallel input patterns.
fn simulate(aig: &Aig, patterns: &[u64]) -> Vec<u64> {
    let mut values = vec![0u64; 1 + aig.inputs + aig.ands.len()];
    values[1..=aig.inputs].copy_from_slice(patterns);
    let literal = |values: &[u64], lit: u32| {
        let value = values[(lit >> 1) as usize];
        if lit & 1 == 1 { !value } else { value }
    };
    for (k, &(a, b)) in aig.ands.iter().enumerate() {
        values[1 + aig.inputs + k] = literal(&values, a) & literal(&values, b);
    }
    aig.outputs
        .iter()
        .map(|&lit| literal(&values, lit))
        .collect()
}

/// Obligation kinds whose gold and gate outputs differ on some random input pattern.
fn refuted(directory: &Path) -> Vec<String> {
    let (gold, gate) = (
        read_aig(&directory.join("gold.aig")),
        read_aig(&directory.join("gate.aig")),
    );
    assert_eq!(
        (gold.inputs, gold.outputs.len()),
        (gate.inputs, gate.outputs.len())
    );
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut differ = vec![false; gold.outputs.len()];
    for _ in 0..256 {
        let patterns: Vec<u64> = (0..gold.inputs)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            })
            .collect();
        for (k, (g, i)) in simulate(&gold, &patterns)
            .into_iter()
            .zip(simulate(&gate, &patterns))
            .enumerate()
        {
            differ[k] |= g != i;
        }
    }
    let index = std::fs::read_to_string(directory.join("obligations.jsonl")).unwrap();
    let kinds: Vec<serde_json::Value> = index
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(kinds.len(), differ.len());
    kinds
        .iter()
        .zip(differ)
        .filter(|(_, d)| *d)
        .map(|(record, _)| record["kind"].as_str().unwrap().to_owned())
        .collect()
}

fn encoding(directory: &Path) -> serde_json::Value {
    let records = std::fs::read_to_string(directory.join("state-map.jsonl")).unwrap();
    records
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["kind"] == "encoding" && r["variable"] == "s")
        .unwrap()
}

#[test]
fn recoded_fsm_is_mapped_from_reset_with_a_proved_valid_code_invariant() {
    let (directory, verdict) = export("discovered", &netlist(NEXT_A), Vec::new());
    assert_eq!(
        verdict,
        Verdict::Undecided,
        "an export never decides equivalence"
    );
    let manifest = json(&directory.join("manifest.json"));
    assert_eq!(manifest["semantics"], "reset_reachable");
    assert_eq!(manifest["all_gate_initial_states_covered"], false);
    assert_eq!(manifest["reset_assumptions"], serde_json::json!([]));
    assert_eq!(manifest["encoded_state_elements"], 4);
    let invariant = &manifest["invariants"][0];
    assert_eq!(invariant["kind"], "valid_fsm_encoding");
    assert_eq!(invariant["variable"], "s");
    assert_eq!(invariant["registers"], serde_json::json!(ELEMENTS));
    assert_eq!(invariant["rtl_codes"], serde_json::json!([0, 3, 5, 6]));
    assert_eq!(
        invariant["rtl_codes_outside_invariant"],
        serde_json::json!([1, 2, 4, 7])
    );
    let kinds = &manifest["obligation_kinds"];
    assert!(kinds["invariant-base"].as_u64().unwrap() >= 1);
    assert!(kinds["invariant-step"].as_u64().unwrap() >= 1);
    assert_eq!(kinds["encoded-next-state"], 4);
    let map = encoding(&directory);
    assert_eq!(map["gate_instances"], serde_json::json!(ELEMENTS));
    assert_eq!(map["codes"], serde_json::json!(TRUE_CODES));
    assert_eq!(refuted(&directory), Vec::<String>::new());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_mislabelled_encoding_hint_gives_refutable_obligations() {
    let mut codes = TRUE_CODES.to_vec();
    codes[1].1 = 5;
    codes[2].1 = 3;
    let hint = EncodingHint {
        variable: "s".into(),
        elements: ELEMENTS.iter().map(|&e| e.into()).collect(),
        codes,
    };
    let (directory, verdict) = export("mislabelled", &netlist(NEXT_A), vec![hint.clone()]);
    assert_eq!(verdict, Verdict::Undecided);
    assert_eq!(
        encoding(&directory)["codes"],
        serde_json::json!(hint.codes),
        "hints are used as given"
    );
    assert!(!refuted(&directory).is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn an_implementation_that_leaves_the_valid_codes_is_never_mapped_quietly() {
    // With the true table forced, the invariant step (or the element next states) fails.
    let hint = EncodingHint {
        variable: "s".into(),
        elements: ELEMENTS.iter().map(|&e| e.into()).collect(),
        codes: TRUE_CODES.to_vec(),
    };
    let (directory, verdict) = export("invalid-forced", &netlist(NEXT_A_FROM_ANY), vec![hint]);
    assert_eq!(verdict, Verdict::Undecided);
    let failed = refuted(&directory);
    assert!(
        failed
            .iter()
            .any(|k| k.starts_with("encoded-") || k.starts_with("invariant-")),
        "{failed:?}"
    );
    std::fs::remove_dir_all(directory).unwrap();
    // Without a hint, discovery either finds no map or exports refutable obligations.
    let (directory, verdict) = export("invalid-discovered", &netlist(NEXT_A_FROM_ANY), Vec::new());
    assert_eq!(verdict, Verdict::Undecided);
    if directory.join("manifest.json").exists() {
        assert!(!refuted(&directory).is_empty());
    }
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn one_to_one_state_keeps_arbitrary_initial_semantics_under_the_same_option() {
    let directory =
        std::env::temp_dir().join(format!("rustsys-state-map-{}-plain", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut options = ObligationExport::new(directory.clone());
    options.initial_state_map = true;
    let report = check(
        &CheckOptions {
            rtl: Input::Texts(vec![("rtl.v".into(),
                "module m(input clk,rn,d,output y); reg q; always @(posedge clk or negedge rn) if(!rn) q<=0; else q<=d; assign y=q; endmodule".into())]),
            netlist: Input::Texts(vec![("net.v".into(),
                "module m(input clk,rn,d,output y); wire h; wt_dffr_x1 f(.CK(clk),.RN(rn),.D(d),.Q(h)); assign y=h; endmodule".into())]),
            obligation_export: Some(options),
            ..CheckOptions::default()
        },
        std::slice::from_ref(library()),
    );
    assert_eq!(report.verdict, Verdict::Undecided);
    let manifest = json(&directory.join("manifest.json"));
    assert_eq!(manifest["semantics"], "arbitrary_initial");
    assert_eq!(manifest["all_gate_initial_states_covered"], true);
    assert_eq!(manifest["all_rtl_inputs_and_states_quantified"], true);
    assert_eq!(manifest["invariants"], serde_json::json!([]));
    assert_eq!(refuted(&directory), Vec::<String>::new());
    std::fs::remove_dir_all(directory).unwrap();
}
