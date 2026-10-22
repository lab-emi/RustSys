// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Deterministic 100,000-input fuzz / mutation suite for `rusteda_formats::sdc`
//! (SDC-REQ-0001, SDC-REQ-0015).

use rusteda_formats::sdc::{self, Env, Limits, Source};

/// Xorshift64* deterministic PRNG.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn gen_range(&mut self, upper: usize) -> usize {
        if upper == 0 {
            0
        } else {
            (self.next_u64() as usize) % upper
        }
    }
}

const SEEDS: &[&str] = &[
    "set_units -time ns -capacitance pF\ncreate_clock -name clk -period 2.5 [get_ports clk]\n",
    "set p [expr {1.5 + 2.5 * 0.5}]\nif {[info exists ::env(X)]} { set p $::env(X) }\n",
    "foreach p {a b c} { set_input_delay -clock [get_clocks clk] 0.2 [get_ports $p] }\n",
    "set_false_path -from [get_pins -hierarchical -filter \"direction == in\" *] -to [all_outputs]\n",
    "set_clock_groups -asynchronous -group [get_clocks a] -group [get_clocks b]\n",
    "set ports [list a b clk]\nset idx [lsearch $ports clk]\nset ports [lreplace $ports $idx $idx]\n",
    "set_driving_cell -lib_cell BUF_X2 -pin Z [all_inputs]\nset_load -pin_load 0.05 [all_outputs]\n",
    "set_multicycle_path -setup 2 -from [all_registers] -to [all_registers]\n",
];

const TOKENS: &[&str] = &[
    "{",
    "}",
    "[",
    "]",
    "\"",
    "$",
    "${",
    "::",
    "{*}",
    ";",
    "\\\n",
    "#comment\n",
    "-period",
    "-waveform",
    "-filter",
    "expr",
    "if",
    "elseif",
    "else",
    "foreach",
    "set",
    "unset",
    "lsearch",
    "lreplace",
    "get_ports",
    "get_pins",
    "create_clock",
    "set_input_delay",
    "set_false_path",
    "==",
    "&&",
    "||",
    "0.0",
    "-1",
];

// SDC-REQ-0001, SDC-REQ-0015: 100,000 deterministically mutated SDC inputs must never panic in
// `sdc::read`.
#[test]
fn fuzz_100k_mutated_sdc_inputs_never_panic() {
    let mut rng = Rng(0x5DC0_2026_1017_0001);
    let env = Env::default();
    let limits = Limits {
        max_steps: 256,
        max_depth: 16,
        ..Limits::default()
    };

    for _ in 0..100_000 {
        let seed = SEEDS[rng.gen_range(SEEDS.len())];
        let mut bytes = seed.as_bytes().to_vec();
        let mutations = 1 + rng.gen_range(4);
        for _ in 0..mutations {
            if bytes.is_empty() {
                break;
            }
            let pos = rng.gen_range(bytes.len());
            match rng.gen_range(5) {
                0 => {
                    bytes.truncate(pos);
                }
                1 => {
                    let end = (pos + 1 + rng.gen_range(6)).min(bytes.len());
                    bytes.drain(pos..end);
                }
                2 => {
                    let tok = TOKENS[rng.gen_range(TOKENS.len())].as_bytes();
                    bytes.splice(pos..pos, tok.iter().copied());
                }
                3 => {
                    let ch = b"[]{}()$\"\\;#-019aZ\n "[rng.gen_range(19)];
                    bytes[pos] = ch;
                }
                _ => {
                    let end = (pos + 1 + rng.gen_range(8)).min(bytes.len());
                    let slice = bytes[pos..end].to_vec();
                    bytes.splice(pos..pos, slice);
                }
            }
        }
        let s = String::from_utf8_lossy(&bytes).into_owned();
        let src = [Source {
            name: "<fuzz>".to_string(),
            text: s,
            xxh3: 0,
        }];
        let (sdc, _diags) = sdc::read(&src, &env, &limits);
        if !sdc.cmds.is_empty() {
            let _ = sdc::write(&sdc);
        }
    }
}
