// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Randomized edit sequences with fixed seeds (CORE-REQ-0121, 0122, 0123): after every accepted
//! step the invariants hold and the frozen view round-trips, a rejected step changes nothing,
//! and a seed always builds the same netlist (CORE-REQ-0102). Port, net and instance names have
//! segments and escapes, and some break the name rules.

use super::tests::{assert_consistent, library, snapshot};
use super::*;

/// xorshift64* generator.
pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub(super) fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n` (`n > 0`).
    pub(super) fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// True with probability `1 / n`.
    pub(super) fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    pub(super) fn pick<T: Copy>(&mut self, v: &[T]) -> Option<T> {
        (!v.is_empty()).then(|| v[self.below(v.len())])
    }
}

const NAMES: [&str; 6] = ["a", "b", "n", "u", "y", "zz"];
const PINS: [&str; 12] = [
    "A", "B", "Y", "D", "CK", "Q", "QN", "H", "IO", "EN", "X", "Z",
];
/// Names that break a rule: empty segments, stray backslashes, characters outside printable
/// ASCII.
const BAD_NAMES: [&str; 8] = ["a//b", "/a", "a/", r"a\b", r"u\", "a b", "n\u{e9}", ""];

/// A port, net or instance name: one or two segments, some with a '/' or '\' that the name
/// escapes; one in sixteen breaks a rule.
fn name(rng: &mut Rng) -> String {
    if rng.one_in(16) {
        return BAD_NAMES[rng.below(BAD_NAMES.len())].to_string();
    }
    let segment = |rng: &mut Rng| {
        let mut s = format!("{}{}", NAMES[rng.below(NAMES.len())], rng.below(20));
        if rng.one_in(6) {
            s.insert(rng.below(s.len() + 1), ['/', '\\'][rng.below(2)]);
        }
        s
    };
    let first = segment(rng);
    if rng.one_in(4) {
        names::join([first, segment(rng)])
    } else {
        names::escape_segment(&first)
    }
}

/// Runs `steps` random operations and returns the final frozen netlist.
fn run(seed: u64, steps: usize) -> FrozenNetlist<'static> {
    use PinDir::{Inout, Input, Other};
    let mut g = GateNetlist::new("random").unwrap();
    let mut cells = library(&mut g).to_vec();
    cells.push(
        g.add_cell_type("bidir", &[("IO", Inout), ("EN", Input), ("X", Other)])
            .unwrap(),
    );
    let buf = cells[1];
    let mut rng = Rng::new(seed);
    let mut dead_insts: Vec<InstId> = Vec::new();
    let mut dead_pins: Vec<PinId> = Vec::new();
    let (mut accepted, mut rejected, mut bad_names) = (0, 0, 0);
    for step in 0..steps {
        let before = snapshot(&g);
        let nets: Vec<NetId> = g.nets().map(|(id, _)| id).collect();
        let insts: Vec<InstId> = g.instances().map(|(id, _)| id).collect();
        let pins: Vec<PinId> = g.pins().map(|(id, _)| id).collect();
        let mut net = |rng: &mut Rng| match rng.pick(&nets) {
            Some(n) if rng.one_in(12) => Id::new(n.index(), n.generation() + 1),
            Some(n) => n,
            None => Id::new(0, 0),
        };
        let inst = |rng: &mut Rng| {
            let pool = if rng.one_in(10) { &dead_insts } else { &insts };
            rng.pick(pool).unwrap_or(Id::new(99, 0))
        };
        let pin = |rng: &mut Rng| {
            let pool = if rng.one_in(10) { &dead_pins } else { &pins };
            rng.pick(pool).unwrap_or(Id::new(99, 0))
        };
        let conn = |rng: &mut Rng, net: &mut dyn FnMut(&mut Rng) -> NetId| {
            if rng.one_in(5) {
                Conn::Const(rng.one_in(2))
            } else {
                Conn::Net(net(rng))
            }
        };
        let result = match rng.below(17) {
            0 | 1 => {
                let n = (!rng.one_in(4)).then(|| name(&mut rng));
                let index = (rng.one_in(2)).then(|| rng.below(4) as i32 - 1);
                g.add_net(n.as_deref(), index).map(drop)
            }
            2 | 3 => {
                let cell = rng.pick(&cells).unwrap();
                g.add_instance(&name(&mut rng), cell).map(drop)
            }
            4 => {
                let i = inst(&mut rng);
                let pins = g.instance(i).map(|x| x.pins.clone()).unwrap_or_default();
                let r = g.remove_instance(i);
                if r.is_ok() {
                    dead_insts.push(i);
                    dead_pins.extend(pins);
                }
                r
            }
            5..=7 => {
                let i = inst(&mut rng);
                let c = conn(&mut rng, &mut net);
                let own: Vec<String> = g
                    .instance(i)
                    .and_then(|x| g.cell(x.cell))
                    .map(|c| {
                        c.pins
                            .iter()
                            .map(|p| g.resolve(p.name).to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                match rng.below(4) {
                    0 => g.connect(i, rng.below(5) as u32, c).map(drop),
                    1 => g.connect(i, PINS[rng.below(PINS.len())], c).map(drop),
                    _ => {
                        let pin = own.get(rng.below(own.len().max(1))).map_or("A", |s| s);
                        g.connect(i, pin, c).map(drop)
                    }
                }
            }
            8 => g.disconnect(pin(&mut rng)).map(drop),
            9 => g.swap_cell(inst(&mut rng), rng.pick(&cells).unwrap()),
            10 => {
                let n = net(&mut rng);
                let mut sinks: Vec<PinId> = g
                    .net(n)
                    .map(|x| x.pins.iter().copied().filter(|_| rng.one_in(2)).collect())
                    .unwrap_or_default();
                if rng.one_in(4) {
                    sinks.push(pin(&mut rng));
                }
                let net_name = (!rng.one_in(3)).then(|| name(&mut rng));
                let inst_name = name(&mut rng);
                match g.insert_buffer(n, &sinks, buf, "A", "Y", &inst_name, net_name.as_deref()) {
                    // Removing the buffer at once gives back the tables before the insertion.
                    Ok((b, _)) if rng.one_in(3) => {
                        assert_eq!(g.remove_buffer(b), Ok(n), "step {step}");
                        assert_eq!(g.freeze(), before.0, "step {step}: buffer removal");
                        Ok(())
                    }
                    r => r.map(drop),
                }
            }
            11 => {
                let lhs = net(&mut rng);
                let rhs = conn(&mut rng, &mut net);
                g.add_assign(lhs, rhs)
            }
            12 => {
                let dir = [PortDir::Input, PortDir::Output, PortDir::Inout][rng.below(3)];
                let range = rng
                    .one_in(2)
                    .then(|| (rng.below(4) as i32 - 1, rng.below(4) as i32 - 1));
                g.add_port(&name(&mut rng), dir, range).map(drop)
            }
            13 => {
                let p = pin(&mut rng);
                let c = conn(&mut rng, &mut net);
                g.move_pin(p, c).map(drop)
            }
            14 => g.remove_net(net(&mut rng)),
            15 => g.remove_assign(rng.below(g.assign_count() + 2)).map(drop),
            _ => {
                let i = inst(&mut rng);
                let pins = g.instance(i).map(|x| x.pins.clone()).unwrap_or_default();
                let r = g.remove_buffer(i).map(drop);
                if r.is_ok() {
                    dead_insts.push(i);
                    dead_pins.extend(pins);
                }
                r
            }
        };
        match result {
            Ok(()) => {
                accepted += 1;
                assert_consistent(&g);
            }
            Err(e) => {
                rejected += 1;
                if matches!(
                    e,
                    NetlistError::InvalidName { .. } | NetlistError::EmptyName { .. }
                ) {
                    bad_names += 1;
                }
                assert!(!e.to_string().is_empty());
                assert_eq!(
                    snapshot(&g),
                    before,
                    "step {step}: rejected '{e}' changed state"
                );
            }
        }
    }
    assert!(
        accepted > steps / 3 && rejected > steps / 10 && bad_names > 0,
        "{accepted} accepted, {rejected} rejected, {bad_names} for their names"
    );
    g.freeze()
}

#[test]
fn random_edit_sequences() {
    for seed in 1..=4 {
        let f = run(seed, 1200);
        assert!(
            f.instances.len() > 10 && f.pin_net.len() > 20,
            "seed {seed}"
        );
        // Some names have two segments, some escape a '/' or a '\'.
        let names: Vec<&str> = (1..f.str_offs.len() as u32 - 1)
            .map(|k| f.string(k).unwrap())
            .collect();
        for part in ["/", r"\/", r"\\"] {
            assert!(
                names.iter().any(|n| n.contains(part)),
                "seed {seed}: {part}"
            );
        }
        assert_eq!(run(seed, 1200), f, "seed {seed} builds the same netlist");
    }
}
