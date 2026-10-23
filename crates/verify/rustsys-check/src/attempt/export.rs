// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! CHECK-REQ-0021: standard binary AIGER export of the independently modeled obligations.
//! At most 64M ANDs *8 B in capped 1M-node chunks, 8M outputs *4 B, one dense
//! u32 image per existing circuit definition and bounded topological scratch.
//! A side is written and released before the next. No paths/cuts or clauses are
//! enumerated; the external CEC verdict is deliberately outside this exporter.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};

use super::Obligation;
use crate::circ::{B, Circuit, Def};
use crate::{ObligationExport, Stop};

struct Ands {
    chunks: Vec<Vec<[u32; 2]>>,
    len: usize,
    max: usize,
    inputs: usize,
}

impl Ands {
    fn and(&mut self, a: u32, b: u32) -> Result<u32, Stop> {
        if a == 0 || b == 0 || a == (b ^ 1) {
            return Ok(0);
        }
        if a == 1 || a == b {
            return Ok(b);
        }
        if b == 1 {
            return Ok(a);
        }
        if self.len >= self.max
            || self.inputs.saturating_add(self.len) + 1 >= (u32::MAX / 2) as usize
        {
            return Err(Stop::Undecided("AIGER AND-node budget reached".into()));
        }
        if self.chunks.last().is_none_or(|c| c.len() == c.capacity()) {
            self.chunks
                .push(Vec::with_capacity((self.max - self.len).min(1_000_000)));
        }
        self.chunks
            .last_mut()
            .expect("chunk")
            .push([a.max(b), a.min(b)]);
        self.len += 1;
        Ok(2 * (self.inputs + self.len) as u32)
    }
    fn or(&mut self, a: u32, b: u32) -> Result<u32, Stop> {
        Ok(self.and(a ^ 1, b ^ 1)? ^ 1)
    }
}

fn io(e: std::io::Error) -> Stop {
    Stop::Error(format!("AIGER obligation export: {e}"))
}

fn variable(output: &mut impl Write, mut v: u32) -> Result<(), Stop> {
    let mut bytes = [0u8; 5];
    let mut count = 0;
    loop {
        bytes[count] = (v & 127) as u8;
        v >>= 7;
        count += 1;
        if v == 0 {
            break;
        }
        bytes[count - 1] |= 128;
    }
    output.write_all(&bytes[..count]).map_err(io)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn write(
    c: &Circuit,
    obs: &[Obligation],
    frees: &[u32],
    options: &ObligationExport,
    outputs: usize,
    pairs: usize,
    rtl_states: usize,
    gate_states: usize,
    model: &crate::rtl::RtlModel,
    netlist: &crate::netlist::NetModel,
    corr: &crate::corr::Corr,
) -> Result<(), Stop> {
    if obs.is_empty() || obs.len() > options.max_outputs || frees.len() >= (u32::MAX / 2) as usize {
        return Err(Stop::Undecided(
            "AIGER interface count budget reached".into(),
        ));
    }
    fs::create_dir(&options.directory).map_err(io)?;
    #[derive(serde::Serialize)]
    struct Relation<'a> {
        rtl_index: usize,
        gate_index: usize,
        rtl_name: &'a str,
        gate_instance: &'a str,
        rtl_variable: u32,
        gate_variable: u32,
        inverted: bool,
    }
    let mut relations = BufWriter::with_capacity(
        1024 * 1024,
        File::create_new(options.directory.join("state-relation.jsonl")).map_err(io)?,
    );
    let mut records = 0usize;
    for (si, state) in model.states.iter().enumerate() {
        let Some((ci, inverted)) = corr.paired[si] else {
            continue;
        };
        let gate = &netlist.seq[ci];
        if crate::netlist::trace(c, gate.iq) != Some((state.q, inverted)) {
            return Err(Stop::Internal(
                "exported state relation is not the actual circuit binding".into(),
            ));
        }
        let relation = Relation {
            rtl_index: si,
            gate_index: ci,
            rtl_name: &state.name,
            gate_instance: &gate.inst,
            rtl_variable: state.q,
            gate_variable: gate.iq,
            inverted,
        };
        serde_json::to_writer(&mut relations, &relation).map_err(|e| Stop::Error(e.to_string()))?;
        relations.write_all(b"\n").map_err(io)?;
        records += 1;
    }
    relations.flush().map_err(io)?;
    if records != gate_states {
        return Err(Stop::Internal(
            "state relation does not cover every implementation element".into(),
        ));
    }
    let mut input_variables = BufWriter::new(
        File::create_new(options.directory.join("input-variables.u32le")).map_err(io)?,
    );
    for &v in frees {
        input_variables.write_all(&v.to_le_bytes()).map_err(io)?;
    }
    input_variables.flush().map_err(io)?;
    let mut kinds = BTreeMap::<&str, usize>::new();
    for obligation in obs {
        *kinds.entry(obligation.kind).or_default() += 1;
    }
    let mut sides = Vec::new();
    for side in 0..2 {
        let order = {
            let roots: Vec<B> = obs
                .iter()
                .flat_map(|o| [if side == 0 { o.a } else { o.b }, o.assume])
                .collect();
            c.order(&roots)
                .map_err(|cy| Stop::Internal(format!("cycle in AIGER export through {}", cy.0)))?
        };
        let mut image = vec![u32::MAX; c.defs.len()];
        for (index, &v) in frees.iter().enumerate() {
            image[v as usize] = 2 * (index + 1) as u32;
        }
        let mut ands = Ands {
            chunks: Vec::new(),
            len: 0,
            max: options.max_and_nodes,
            inputs: frees.len(),
        };
        let bit = |b: B, image: &[u32]| -> u32 {
            match b {
                B::F => 0,
                B::T => 1,
                B::V(v) => image[v as usize],
            }
        };
        for v in order {
            let get = |x: u32| image[x as usize];
            let lit = match c.defs[v as usize] {
                Def::Free => {
                    let l = get(v);
                    if l == u32::MAX {
                        return Err(Stop::Internal("unbound AIGER free variable".into()));
                    }
                    l
                }
                Def::Alias(b) => bit(b, &image),
                Def::Not(x) => get(x) ^ 1,
                Def::And(a, b) => ands.and(get(a), get(b))?,
                Def::Or(a, b) => ands.or(get(a), get(b))?,
                Def::Xor(a, b) => {
                    let x = ands.and(get(a), get(b) ^ 1)?;
                    let y = ands.and(get(a) ^ 1, get(b))?;
                    ands.or(x, y)?
                }
                Def::Mux(s, a, b) => {
                    let x = ands.and(get(s), get(a))?;
                    let y = ands.and(get(s) ^ 1, get(b))?;
                    ands.or(x, y)?
                }
            };
            if lit == u32::MAX {
                return Err(Stop::Internal("unordered AIGER definition".into()));
            }
            image[v as usize] = lit;
        }
        let mut out = Vec::with_capacity(obs.len());
        for o in obs {
            out.push(ands.and(
                bit(if side == 0 { o.a } else { o.b }, &image),
                bit(o.assume, &image),
            )?);
        }
        drop(image);
        let path = options
            .directory
            .join(if side == 0 { "gold.aig" } else { "gate.aig" });
        let mut writer =
            BufWriter::with_capacity(1024 * 1024, File::create_new(&path).map_err(io)?);
        writeln!(
            writer,
            "aig {} {} 0 {} {}",
            frees.len() + ands.len,
            frees.len(),
            out.len(),
            ands.len
        )
        .map_err(io)?;
        for literal in out {
            writeln!(writer, "{literal}").map_err(io)?;
        }
        for (index, [a, b]) in ands.chunks.iter().flat_map(|c| c.iter()).enumerate() {
            let lhs = 2 * (frees.len() + index + 1) as u32;
            if *a >= lhs || b > a {
                return Err(Stop::Internal("AIGER node order is invalid".into()));
            }
            variable(&mut writer, lhs - a)?;
            variable(&mut writer, a - b)?;
        }
        writer.flush().map_err(io)?;
        sides.push(serde_json::json!({"file":path.file_name().unwrap().to_string_lossy(),"inputs":frees.len(),"outputs":obs.len(),"and_nodes":ands.len,"bytes":fs::metadata(&path).map_err(io)?.len()}));
    }
    let mut first = obs.len()
        - model
            .observations
            .iter()
            .map(|(_, b)| b.len())
            .sum::<usize>();
    let observations: Vec<_> = model.observations.iter().map(|(p, b)| {
        let item = serde_json::json!({"name":p.name,"offset":p.offset,"width":p.width,"first_output":first});
        first += b.len();
        item
    }).collect();
    let manifest = serde_json::json!({"schema":1,"method":"independent-rtl-event-obligations-v1",
        "proof_status":"not_run","format":"binary-aiger-combinational",
        "sides":sides,"obligation_kinds":kinds,"output_bits":outputs,"state_pairs":pairs,
        "observations":observations,
        "rtl_state_bits":rtl_states,"gate_state_elements":gate_states,
        "primary_input_bits":model.inputs.iter().map(|(_,bits,_)|bits.len()).sum::<usize>(),
        "all_gate_initial_states_covered":pairs==gate_states,"reset_assumptions":[],
        "initial_state_choices":[],"case_assumptions":[],
        "rtl_unknown_literals":"X/Z-as-zero; distinct from four-state equality",
        "state_relation":"one-to-one, named or proposed, parity checked; every original event/output obligation requires external CEC",
        "proposed_state_bindings":corr.discovered.len(),
        "state_discovery_limits":{"max_definitions":options.max_discovery_definitions,
            "max_candidates":options.max_discovery_candidates,"max_support_entries":options.max_discovery_support_entries,
            "max_sketch_bytes":options.max_discovery_sketch_bytes,"max_rounds":options.max_discovery_rounds,
            "max_evaluation_work":options.max_discovery_work},
        "state_relation_file":"state-relation.jsonl","state_relation_records":records,
        "input_variables_file":"input-variables.u32le","all_rtl_inputs_and_states_quantified":true,
        "comparison":"each obligation compares (reference & guard) with (implementation & guard); guards are original clock/reset event semantics",
        "max_and_nodes":options.max_and_nodes,"max_outputs":options.max_outputs});
    let mut writer =
        BufWriter::new(File::create_new(options.directory.join("manifest.json")).map_err(io)?);
    serde_json::to_writer_pretty(&mut writer, &manifest).map_err(|e| Stop::Error(e.to_string()))?;
    writer.flush().map_err(io)?;
    Ok(())
}
