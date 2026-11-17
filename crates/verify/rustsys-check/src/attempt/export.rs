// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! CHECK-REQ-0021: standard binary AIGER export of the independently modeled obligations.
//! At most 64M ANDs *8 B in capped 1M-node chunks, 8M outputs *4 B, one dense
//! u32 image per existing circuit definition and bounded topological scratch.
//! A side is written and released before the next. No paths/cuts or clauses are
//! enumerated; the external CEC verdict is deliberately outside this exporter.

use std::collections::{BTreeMap, HashMap};
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
    canonical: &[Vec<u32>],
    names: &HashMap<u32, String>,
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
        // A state sharing another state's element is a state-map record below.
        if corr.owners[ci] != Some(si) {
            continue;
        }
        let gate = &netlist.seq[ci];
        if !binds(c, gate.iq, state.q, inverted) {
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
    let encoded: usize = corr.encodings.iter().map(|e| e.gate.len()).sum();
    if records + encoded != gate_states {
        return Err(Stop::Internal(
            "state relation does not cover every implementation element".into(),
        ));
    }
    // The implementation state map: RTL bits defined from an owner's value or a constant.
    // Each record is checked against the actual circuit definition that the obligations use.
    let state_map = options.initial_state_map;
    let mut shared = 0usize;
    let mut constants = 0usize;
    if state_map {
        let mut map = BufWriter::with_capacity(
            1024 * 1024,
            File::create_new(options.directory.join("state-map.jsonl")).map_err(io)?,
        );
        let mut equivalents = corr.equivalents.clone();
        equivalents.sort_unstable();
        for &(si, ri, inv) in &equivalents {
            let (state, rep) = (&model.states[si], &model.states[ri]);
            let Some((ci, parity)) = corr.paired[si] else {
                return Err(Stop::Internal("shared RTL state has no element".into()));
            };
            if corr.owners[ci] != Some(ri)
                || corr.paired[ri].map(|(x, p)| (x, p ^ inv)) != Some((ci, parity))
                || crate::netlist::trace(c, state.q) != Some((rep.q, inv))
            {
                return Err(Stop::Internal(
                    "exported shared state is not the actual circuit binding".into(),
                ));
            }
            let record = serde_json::json!({"kind":"shares","rtl_index":si,"rtl_name":state.name,
                "rtl_variable":state.q,"representative_index":ri,"representative_name":rep.name,
                "representative_variable":rep.q,"gate_index":ci,"gate_instance":netlist.seq[ci].inst,
                "inverted":inv});
            serde_json::to_writer(&mut map, &record).map_err(|e| Stop::Error(e.to_string()))?;
            map.write_all(b"\n").map_err(io)?;
            shared += 1;
        }
        for (si, state) in model.states.iter().enumerate() {
            let Some(value) = corr.constants[si] else {
                continue;
            };
            if c.defs[state.q as usize] != Def::Alias(B::from_bool(value))
                || corr.paired[si].is_some()
            {
                return Err(Stop::Internal(
                    "exported constant state is not the actual circuit binding".into(),
                ));
            }
            let record = serde_json::json!({"kind":"constant","rtl_index":si,"rtl_name":state.name,
                "rtl_variable":state.q,"value":value});
            serde_json::to_writer(&mut map, &record).map_err(|e| Stop::Error(e.to_string()))?;
            map.write_all(b"\n").map_err(io)?;
            constants += 1;
        }
        // Re-encoded registers: each element is its bit of the element code of the RTL code,
        // checked by evaluating the actual definitions on every valid code.
        for (enc, inputs) in corr.encodings.iter().zip(canonical) {
            if !super::encoded::check(c, netlist, enc, inputs) {
                return Err(Stop::Internal(
                    "exported re-encoded register is not the actual circuit binding".into(),
                ));
            }
            let reset = super::encoded::reset(model, enc).map(|(_, code)| code);
            let record = serde_json::json!({"kind":"encoding","variable":enc.variable,
                "rtl_indices":enc.rtl,
                "rtl_names":enc.rtl.iter().map(|&si| model.states[si].name.as_str()).collect::<Vec<_>>(),
                "rtl_variables":enc.rtl.iter().map(|&si| model.states[si].q).collect::<Vec<_>>(),
                "gate_indices":enc.gate,
                "gate_instances":enc.gate.iter().map(|&ci| netlist.seq[ci].inst.as_str()).collect::<Vec<_>>(),
                "gate_variables":enc.gate.iter().map(|&ci| netlist.seq[ci].iq).collect::<Vec<_>>(),
                "codes":enc.codes,"rtl_reset_code":reset,"input_variables":inputs,
                "rtl_definition":"bit b = bit b of (valid(input) ? input : rtl_reset_code); the export quantifies the inputs, so every obligation ranges over exactly the valid codes",
                "element_definition":"element i = OR over codes (rtl, gate) with bit i of gate set of (RTL register == rtl)"});
            serde_json::to_writer(&mut map, &record).map_err(|e| Stop::Error(e.to_string()))?;
            map.write_all(b"\n").map_err(io)?;
        }
        map.flush().map_err(io)?;
    } else if !corr.equivalents.is_empty()
        || corr.constants.iter().any(Option::is_some)
        || !corr.encodings.is_empty()
    {
        return Err(Stop::Internal(
            "state map relations without explicit permission".into(),
        ));
    }
    if pairs + encoded != gate_states + shared {
        return Err(Stop::Internal(
            "exported state pairs differ from owners and shared states".into(),
        ));
    }
    let mut input_variables = BufWriter::new(
        File::create_new(options.directory.join("input-variables.u32le")).map_err(io)?,
    );
    for &v in frees {
        input_variables.write_all(&v.to_le_bytes()).map_err(io)?;
    }
    input_variables.flush().map_err(io)?;
    // Under a state map: the name of each AIGER input, so that a counterexample can be read.
    if state_map {
        let mut index = BufWriter::new(
            File::create_new(options.directory.join("input-names.txt")).map_err(io)?,
        );
        for v in frees {
            let name = names.get(v).map_or("", String::as_str);
            writeln!(index, "{}", name.replace('\n', " ")).map_err(io)?;
        }
        index.flush().map_err(io)?;
    }
    let mut kinds = BTreeMap::<&str, usize>::new();
    for obligation in obs {
        *kinds.entry(obligation.kind).or_default() += 1;
    }
    // Under a state map: the obligation of each AIGER output, so that a failing output names
    // the relation or invariant it refutes.
    if state_map {
        let mut index = BufWriter::with_capacity(
            1024 * 1024,
            File::create_new(options.directory.join("obligations.jsonl")).map_err(io)?,
        );
        for (output, o) in obs.iter().enumerate() {
            let record = serde_json::json!({"output":output,"kind":o.kind,"name":o.name});
            serde_json::to_writer(&mut index, &record).map_err(|e| Stop::Error(e.to_string()))?;
            index.write_all(b"\n").map_err(io)?;
        }
        index.flush().map_err(io)?;
    }
    let sides = write_pair(c, obs, frees, options, ["gold.aig", "gate.aig"])?;
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
    let registers = corr.encodings.len();
    let recoded = corr.encodings.iter().filter(|e| !e.gate.is_empty()).count();
    let mapped = shared + constants + registers;
    let choices = if mapped == 0 {
        serde_json::json!([])
    } else {
        serde_json::json!({"file":"state-map.jsonl","records":mapped,"shared":shared,"constant":constants,
            "encoded_registers":recoded,"invariant_only_registers":registers - recoded,
            "chosen_by":"implementation state map; proved by the exported obligations, not assumed"})
    };
    // Re-encoded registers restrict the claim to runs from reset (see `encoded`).
    let reset_reachable = registers > 0;
    let invariants: Vec<_> = corr.encodings.iter().map(|enc| {
        let bits = enc.rtl.len();
        // A register whose elements copy its bits has only the invariant (no element map).
        let copies = enc.gate.is_empty();
        let elements: Vec<&str> = if copies {
            enc.rtl.iter().filter_map(|&si| corr.paired[si]).map(|(ci, _)| netlist.seq[ci].inst.as_str()).collect()
        } else {
            enc.gate.iter().map(|&ci| netlist.seq[ci].inst.as_str()).collect()
        };
        let element_kinds = |base: bool| if copies {
            serde_json::json!({})
        } else if base {
            serde_json::json!({"encoded-async-clear":format!("{} element *", enc.variable),
                "encoded-async-preset":format!("{} element *", enc.variable)})
        } else {
            serde_json::json!({"encoded-next-state":format!("{} element *", enc.variable),
                "encoded-clock":format!("{} element *", enc.variable)})
        };
        let mut base = serde_json::json!({"invariant-base":[enc.variable.as_str()]});
        let mut step = serde_json::json!({"invariant-step":[enc.variable.as_str()]});
        if let (Some(b), Some(extra)) = (base.as_object_mut(), element_kinds(true).as_object()) {
            b.extend(extra.clone());
        }
        if let (Some(s), Some(extra)) = (step.as_object_mut(), element_kinds(false).as_object()) {
            s.extend(extra.clone());
        }
        let unreachable: Vec<u64> = if bits <= 10 {
            (0..1u64 << bits).filter(|code| enc.encode(*code).is_none()).collect()
        } else {
            Vec::new()
        };
        serde_json::json!({"kind":if copies {"valid_code"} else {"valid_fsm_encoding"},"variable":enc.variable,
            "registers":elements,
            "element_map":if copies {"named or proposed element relations (elements copy the bits)"} else {"element i = OR over codes with bit i of the element code set of (RTL register == RTL code)"},
            "rtl_bits":enc.rtl.iter().map(|&si| model.states[si].name.as_str()).collect::<Vec<_>>(),
            "codes":enc.codes.len(),"rtl_codes":enc.codes.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            "rtl_codes_outside_invariant":unreachable,
            "base":"exported","step":"exported",
            "base_obligations":base,"step_obligations":step})
    }).collect();
    let semantics = if reset_reachable { "reset_reachable" } else { "arbitrary_initial" };
    let scope = if reset_reachable {
        "equivalence from reset only: both models start from their deterministic asynchronous reset; implementation states outside the image of the state map (invalid element codes) and power-up without reset are not covered"
    } else {
        "every implementation initial state is covered"
    };
    let claim = if reset_reachable {
        "refinement from reset: every implementation run from reset is the image under the proved state map of the RTL run from reset with the same inputs, so outputs agree; both reset deterministically, so this is equivalence from reset. The valid-code invariants are proved (base and step obligations), never assumed; RTL codes outside them are unreachable from reset"
    } else if mapped == 0 {
        "equivalence: every RTL state and every implementation state has an equal-behavior counterpart"
    } else {
        "implementation refinement: every implementation initial state behaves as the RTL started from its mapped state; RTL bits outside the map stay arbitrary"
    };
    let manifest = serde_json::json!({"schema":1,"method":"independent-rtl-event-obligations-v1",
        "proof_status":"not_run","format":"binary-aiger-combinational",
        "sides":sides,"obligation_kinds":kinds,"output_bits":outputs,"state_pairs":gate_states,
        "shared_state_pairs":shared,"constant_states":constants,
        "observations":observations,
        "rtl_state_bits":rtl_states,"gate_state_elements":gate_states,
        "primary_input_bits":model.inputs.iter().map(|(_,bits,_)|bits.len()).sum::<usize>(),
        "all_gate_initial_states_covered":!reset_reachable && pairs==gate_states+shared,"reset_assumptions":[],
        "semantics":semantics,"invariants":invariants,"scope":scope,
        "encoded_state_elements":encoded,
        "obligation_index_file":if state_map {serde_json::json!("obligations.jsonl")} else {serde_json::Value::Null},
        "input_names_file":if state_map {serde_json::json!("input-names.txt")} else {serde_json::Value::Null},
        "initial_state_choices":choices,"case_assumptions":[],
        "claim":claim,
        "rtl_states_defined_by_map":mapped,
        "rtl_unknown_literals":"X/Z-as-zero; distinct from four-state equality",
        "state_relation":if state_map {"one owner RTL state per implementation element, named or proposed, parity checked; state map of shared/constant RTL bits in state_map_file; every original event/output obligation requires external CEC"} else {"one-to-one, named or proposed, parity checked; every original event/output obligation requires external CEC"},
        "state_map_file":if state_map {serde_json::json!("state-map.jsonl")} else {serde_json::Value::Null},
        "proposed_state_bindings":corr.discovered.len(),
        "state_discovery_limits":{"max_definitions":options.max_discovery_definitions,
            "max_candidates":options.max_discovery_candidates,"max_support_entries":options.max_discovery_support_entries,
            "max_sketch_bytes":options.max_discovery_sketch_bytes,"max_rounds":options.max_discovery_rounds,
            "max_evaluation_work":options.max_discovery_work},
        "state_relation_file":"state-relation.jsonl","state_relation_records":records,
        "input_variables_file":"input-variables.u32le","all_rtl_inputs_and_states_quantified":mapped==0,
        "comparison":"each obligation compares (reference & guard) with (implementation & guard); guards are original clock/reset event semantics",
        "max_and_nodes":options.max_and_nodes,"max_outputs":options.max_outputs});
    let mut writer =
        BufWriter::new(File::create_new(options.directory.join("manifest.json")).map_err(io)?);
    serde_json::to_writer_pretty(&mut writer, &manifest).map_err(|e| Stop::Error(e.to_string()))?;
    writer.flush().map_err(io)?;
    Ok(())
}

/// Whether element value `iq` is RTL state value `q` (inverted when `inverted`) through aliases
/// and inversions; `q` itself may be defined (a canonical code, see `encoded`).
fn binds(c: &Circuit, mut v: u32, q: u32, inverted: bool) -> bool {
    let mut parity = false;
    for _ in 0..1_000_000 {
        if v == q {
            return parity == inverted;
        }
        match c.defs[v as usize] {
            Def::Alias(B::V(x)) => v = x,
            Def::Not(x) => {
                parity = !parity;
                v = x;
            }
            _ => return false,
        }
    }
    false
}

/// Writes the reference (`a`) and implementation (`b`) sides of `obs`, each guarded by its
/// assumption, as binary AIGER files over the inputs `frees`.
fn write_pair(
    c: &Circuit,
    obs: &[Obligation],
    frees: &[u32],
    options: &ObligationExport,
    files: [&str; 2],
) -> Result<Vec<serde_json::Value>, Stop> {
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
        let path = options.directory.join(files[side]);
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
    Ok(sides)
}
