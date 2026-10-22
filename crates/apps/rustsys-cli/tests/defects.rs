// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Suite `defects` (SYNTH-REQ-0802, CHECK-REQ-0070): planted defects in synthesized netlists are
//! reported by the checker.

use std::collections::HashMap;

use rusteda_formats::liberty::{Direction, Library};
use rusteda_formats::verilog::write::{NlDir, NlModule, NlRef, write_netlist};
use rustsys::{Sources, SynthOptions, synthesize};
use rustsys_check::{CheckOptions, Input, Verdict, check};
use rustsys_defects::{DefectClass, plant, read_netlist};
use rustsys_fixtures::liberty::{Variant, library};
use rustsys_fixtures::rtl::CORPUS;

fn key(r: &NlRef) -> String {
    match r {
        NlRef::Net(n) => n.clone(),
        NlRef::Bit(n, k) => format!("{n}[{k}]"),
        NlRef::Const(v) => format!("#{}", u8::from(*v)),
    }
}

/// Independent evaluation of a netlist: primary inputs and the internal state of every
/// sequential instance are given; returns the value of every net.
fn eval(
    nl: &NlModule,
    lib: &Library,
    given: &HashMap<String, bool>,
    iq: &HashMap<String, bool>,
) -> HashMap<String, bool> {
    let mut val = given.clone();
    val.insert("#0".into(), false);
    val.insert("#1".into(), true);
    for _ in 0..10_000 {
        let mut changed = false;
        for inst in &nl.instances {
            let cell = lib.cells.iter().find(|c| c.name == inst.cell).unwrap();
            let state = cell
                .sequentials
                .first()
                .map(|s| (s.state.clone(), s.state_n.clone(), iq[&inst.name]));
            for o in cell
                .pins
                .iter()
                .filter(|p| p.direction == Direction::Output)
            {
                let Some((_, r)) = inst.conns.iter().find(|(p, _)| *p == o.name) else {
                    continue;
                };
                let k = key(r);
                if val.contains_key(&k) {
                    continue;
                }
                let get = |n: &str| -> Option<bool> {
                    if let Some((s, sn, q)) = &state {
                        if n == s {
                            return Some(*q);
                        }
                        if n == sn {
                            return Some(!*q);
                        }
                    }
                    inst.conns
                        .iter()
                        .find(|(p, _)| p == n)
                        .and_then(|(_, r)| val.get(&key(r)).copied())
                };
                if let Some(v) = o.function.as_ref().unwrap().eval(&get) {
                    val.insert(k, v);
                    changed = true;
                }
            }
        }
        for (l, r) in &nl.assigns {
            if !val.contains_key(&key(l)) {
                if let Some(v) = val.get(&key(r)).copied() {
                    val.insert(key(l), v);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    val
}

/// True if the two netlists agree on outputs and on every sequential pin for random inputs and
/// states (sequential instances are matched by name).
fn same_function(a: &NlModule, b: &NlModule, lib: &Library) -> bool {
    let mut seed = 0x1234_5678u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed & 1 == 1
    };
    let seqs: Vec<String> = a
        .instances
        .iter()
        .filter(|i| {
            lib.cells
                .iter()
                .any(|c| c.name == i.cell && !c.sequentials.is_empty())
        })
        .map(|i| i.name.clone())
        .collect();
    for _ in 0..512 {
        let mut given = HashMap::new();
        for p in a.ports.iter().filter(|p| p.dir == NlDir::Input) {
            match p.width {
                None => {
                    given.insert(p.name.clone(), rnd());
                }
                Some(w) => {
                    for k in 0..w {
                        given.insert(format!("{}[{k}]", p.name), rnd());
                    }
                }
            }
        }
        let iq: HashMap<String, bool> = seqs.iter().map(|s| (s.clone(), rnd())).collect();
        let (va, vb) = (eval(a, lib, &given, &iq), eval(b, lib, &given, &iq));
        for p in a.ports.iter().filter(|p| p.dir == NlDir::Output) {
            let keys: Vec<String> = match p.width {
                None => vec![p.name.clone()],
                Some(w) => (0..w).map(|k| format!("{}[{k}]", p.name)).collect(),
            };
            for k in keys {
                if va.get(&k) != vb.get(&k) {
                    return false;
                }
            }
        }
        for s in &seqs {
            let ia = a.instances.iter().find(|i| i.name == *s).unwrap();
            let Some(ib) = b.instances.iter().find(|i| i.name == *s) else {
                return false;
            };
            let cell = lib.cells.iter().find(|c| c.name == ia.cell).unwrap();
            let sq = &cell.sequentials[0];
            let pin_val = |v: &HashMap<String, bool>,
                           inst: &rusteda_formats::verilog::write::NlInstance,
                           n: &str| {
                inst.conns
                    .iter()
                    .find(|(p, _)| p == n)
                    .and_then(|(_, r)| v.get(&key(r)).copied())
            };
            // The data pin matters only while the element can load it: latch enabled, or no
            // asynchronous control active.
            let f = |func: &Option<rusteda_formats::liberty::Func>| {
                func.as_ref()
                    .and_then(|x| x.eval(&|n| pin_val(&va, ia, n)))
                    .unwrap_or(false)
            };
            let loads = if cell.sequentials[0].kind == rusteda_formats::liberty::SeqKind::Latch {
                sq.clock.eval(&|n| pin_val(&va, ia, n)).unwrap_or(false)
                    && !f(&sq.clear)
                    && !f(&sq.preset)
            } else {
                !f(&sq.clear) && !f(&sq.preset)
            };
            let data_pins = sq.data.vars();
            for (pin, r) in &ia.conns {
                let is_input = cell
                    .pin(pin)
                    .is_some_and(|p| p.direction == Direction::Input);
                if !is_input || (!loads && data_pins.contains(pin)) {
                    continue;
                }
                let Some((_, rb)) = ib.conns.iter().find(|(p, _)| p == pin) else {
                    return false;
                };
                if va.get(&key(r)) != vb.get(&key(rb)) {
                    return false;
                }
            }
        }
    }
    true
}

#[test]
fn every_defect_class_is_detected() {
    let lib = rusteda_formats::liberty::read_text(&library(Variant::Rich), "rich").unwrap();
    let mut stats: HashMap<DefectClass, (usize, usize)> = HashMap::new();
    let mut missed = Vec::new();
    for d in CORPUS {
        let files: Vec<(String, String)> = d
            .files
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        let sopts = SynthOptions {
            top: Some(d.top.to_string()),
            ..SynthOptions::default()
        };
        let out = synthesize(
            &Sources::Texts(files.clone()),
            std::slice::from_ref(&lib),
            &sopts,
        )
        .unwrap();
        let nl = read_netlist(&out.text).unwrap();
        for class in DefectClass::ALL {
            for seed in 0..3u64 {
                let Some(bad) = plant(&nl, &lib, class, seed + 7 * d.name.len() as u64) else {
                    continue;
                };
                let copts = CheckOptions {
                    rtl: Input::Texts(files.clone()),
                    top: Some(d.top.to_string()),
                    netlist: Input::Texts(vec![("bad.v".into(), write_netlist(&bad))]),
                    ..CheckOptions::default()
                };
                let rep = check(&copts, std::slice::from_ref(&lib));
                let e = stats.entry(class).or_default();
                e.1 += 1;
                // Only `not-equivalent` reports a planted defect (CHECK-REQ-0070); `undecided`
                // and `error` are neither a detection nor an acceptable miss.
                match rep.verdict {
                    Verdict::NotEquivalent => e.0 += 1,
                    Verdict::Equivalent => {
                        // A miss is acceptable only when the planted change is functionally
                        // benign.
                        assert!(
                            same_function(&nl, &bad, &lib),
                            "{} {class:?} seed {seed}: the checker missed a real difference",
                            d.name
                        );
                        missed.push(format!("{} {class:?} seed {seed}", d.name));
                    }
                    v => panic!(
                        "{} {class:?} seed {seed}: verdict {} ({:?} {:?})",
                        d.name,
                        v.as_str(),
                        rep.errors,
                        rep.undecided
                    ),
                }
            }
        }
    }
    let mut report = Vec::new();
    for class in DefectClass::ALL {
        let (det, tot) = stats.get(&class).copied().unwrap_or((0, 0));
        report.push(format!("{class:?}: {det}/{tot}"));
        assert!(tot > 0, "class {class:?} was never planted");
        assert!(det > 0, "class {class:?} was never detected");
        // Some planted changes are functionally benign (for example a rewire to an equal net);
        // the large majority must be reported.
        assert!(
            det * 10 >= tot * 8,
            "class {class:?} detected {det}/{tot}; missed: {missed:?}"
        );
    }
    eprintln!("{}", report.join("\n"));
}
