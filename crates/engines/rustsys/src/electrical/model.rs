// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use rustc_hash::FxHashMap;
use rusteda_formats::liberty::{Cell, Direction, Library, Table};

use crate::map::library::CellLib;

/// Resolve table orientation and timing-arc metadata once, outside the sizing hot path.
#[derive(Debug, Clone, Copy)]
struct Lookup<'a> {
    table: &'a Table,
    swap: bool,
}
impl Lookup<'_> {
    fn value(&self, a: f64, b: f64) -> f64 {
        if self.swap {
            self.table.lookup(b, a)
        } else {
            self.table.lookup(a, b)
        }
    }
}
#[derive(Debug, Clone, Copy)]
struct Arc<'a> {
    from: usize,
    to: usize,
    delay: Lookup<'a>,
    transition: Option<Lookup<'a>>,
}

fn load_table(t: &Table) -> Option<Lookup<'_>> {
    t.is_consistent().then(|| Lookup {
        table: t,
        swap: t
            .variable_1
            .as_deref()
            .is_some_and(|v| v.contains("capacitance"))
            && !t
                .variable_2
                .as_deref()
                .is_some_and(|v| v.contains("capacitance")),
    })
}
fn compiled_arcs(c: &Cell) -> Vec<Vec<Arc<'_>>> {
    let mut arcs = vec![Vec::new(); c.pins.len() * c.pins.len()];
    for (output, pin) in c.pins.iter().enumerate() {
        for raw in &pin.arcs {
            if !raw.timing_type.is_empty()
                && raw.timing_type != "combinational"
                && !raw.timing_type.starts_with("combinational_")
                && raw.timing_type != "rising_edge"
                && raw.timing_type != "falling_edge"
            {
                continue;
            }
            let Some(input) = c.pins.iter().position(|p| p.name == raw.related_pin) else {
                continue;
            };
            for from in 0..2 {
                for to in 0..2 {
                    let allowed = match raw.timing_sense.as_deref() {
                        Some("positive_unate") => from == to,
                        Some("negative_unate") => from != to,
                        _ => true,
                    };
                    if !allowed {
                        continue;
                    }
                    let dt = if to == 0 {
                        &raw.cell_rise
                    } else {
                        &raw.cell_fall
                    };
                    let st = if to == 0 {
                        &raw.rise_transition
                    } else {
                        &raw.fall_transition
                    };
                    if let Some(delay) = dt.as_ref().and_then(load_table) {
                        arcs[output * c.pins.len() + input].push(Arc {
                            from,
                            to,
                            delay,
                            transition: st.as_ref().and_then(load_table),
                        });
                    }
                }
            }
        }
    }
    arcs
}
fn compiled_setup(c: &Cell) -> Vec<Vec<(usize, Lookup<'_>)>> {
    c.pins
        .iter()
        .map(|pin| {
            let mut out = Vec::new();
            for a in &pin.arcs {
                if a.timing_type.starts_with("setup_") {
                    for (tr, t) in [&a.rise_constraint, &a.fall_constraint]
                        .into_iter()
                        .enumerate()
                    {
                        if let Some(t) = t.as_ref().filter(|t| t.is_consistent()) {
                            out.push((
                                tr,
                                Lookup {
                                    table: t,
                                    swap: t
                                        .variable_1
                                        .as_deref()
                                        .is_some_and(|s| s.contains("related_pin")),
                                },
                            ));
                        }
                    }
                }
            }
            out
        })
        .collect()
}

#[derive(Debug)]
pub(super) struct CellModel<'a> {
    pub raw: &'a Cell,
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
    /// All synchronous inputs, including load-enable and external hold-data pins.
    pub data: Vec<usize>,
    pub clock: Option<usize>,
    pub q_pins: [Option<usize>; 2],
    pub comb: bool,
    pub buffer: bool,
    pub time: f64,
    pub cap: f64,
    pin_caps: Vec<f64>,
    cap_limits: Vec<f64>,
    slew_limits: Vec<f64>,
    fanout_limits: Vec<f64>,
    fanout_loads: Vec<f64>,
    arcs: Vec<Vec<Arc<'a>>>,
    setups: Vec<Vec<(usize, Lookup<'a>)>>,
    pub family: usize,
    pub truth: Option<u64>,
}

impl CellModel<'_> {
    pub fn input_cap(&self, pin: usize) -> f64 {
        self.pin_caps[pin]
    }
    pub fn max_cap(&self, pin: usize) -> f64 {
        self.cap_limits[pin]
    }
    pub fn max_slew(&self, pin: usize) -> f64 {
        self.slew_limits[pin]
    }
    pub fn max_fanout(&self, pin: usize) -> f64 {
        self.fanout_limits[pin]
    }
    pub fn input_fanout(&self, pin: usize) -> f64 {
        self.fanout_loads[pin]
    }

    /// Conservative rise/fall arc values in ns, at the current slew and load.
    pub fn arc(
        &self,
        input: usize,
        output: usize,
        slew: [f64; 2],
        load: f64,
    ) -> [(usize, usize, f64, f64); 4] {
        let mut values = [(0, 0, 0.0_f64, 0.0_f64); 4];
        for ti in 0..2 {
            for to in 0..2 {
                values[ti * 2 + to] = (ti, to, -1.0, 0.0);
            }
        }
        let list = &self.arcs[output * self.raw.pins.len() + input];
        if list.is_empty() {
            for v in &mut values {
                v.2 = 1e-3;
                v.3 = slew[v.0];
            }
            return values;
        }
        let cap_load = load / self.cap;
        for arc in list {
            let s_in = slew[arc.from] / self.time;
            let delay = arc.delay.value(s_in, cap_load).max(0.0) * self.time;
            let transition = arc.transition.map_or(slew[arc.from], |t| {
                t.value(s_in, cap_load).max(0.0) * self.time
            });
            let value = &mut values[arc.from * 2 + arc.to];
            value.2 = value.2.max(delay);
            value.3 = value.3.max(transition);
        }
        values
    }

    pub fn arc_delay(
        &self,
        input: usize,
        output: usize,
        slew: [f64; 2],
        load: f64,
    ) -> [(usize, usize, f64); 4] {
        let mut values = [(0, 0, -1.0_f64); 4];
        for ti in 0..2 {
            for to in 0..2 {
                values[ti * 2 + to] = (ti, to, -1.0);
            }
        }
        let list = &self.arcs[output * self.raw.pins.len() + input];
        if list.is_empty() {
            for v in &mut values {
                v.2 = 1e-3;
            }
            return values;
        }
        let cap_load = load / self.cap;
        for arc in list {
            let delay = arc
                .delay
                .value(slew[arc.from] / self.time, cap_load)
                .max(0.0)
                * self.time;
            let value = &mut values[arc.from * 2 + arc.to];
            value.2 = value.2.max(delay);
        }
        values
    }

    pub fn arc_slew(
        &self,
        input: usize,
        output: usize,
        slew: [f64; 2],
        load: f64,
    ) -> [(usize, usize, bool, f64); 4] {
        let mut values = [(0, 0, false, 0.0_f64); 4];
        for ti in 0..2 {
            for to in 0..2 {
                values[ti * 2 + to] = (ti, to, false, 0.0);
            }
        }
        let list = &self.arcs[output * self.raw.pins.len() + input];
        if list.is_empty() {
            for v in &mut values {
                v.2 = true;
                v.3 = slew[v.0];
            }
            return values;
        }
        let cap_load = load / self.cap;
        for arc in list {
            let transition = arc.transition.map_or(slew[arc.from], |t| {
                t.value(slew[arc.from] / self.time, cap_load).max(0.0) * self.time
            });
            let value = &mut values[arc.from * 2 + arc.to];
            value.2 = true;
            value.3 = value.3.max(transition);
        }
        values
    }

    pub fn setup(&self, input: usize, slew: [f64; 2], clock_slew: f64) -> [f64; 2] {
        let mut out = [0.0_f64; 2];
        for &(tr, table) in &self.setups[input] {
            let value = table.value(slew[tr] / self.time, clock_slew / self.time) * self.time;
            out[tr] = out[tr].max(value);
        }
        out
    }
}

#[derive(Debug)]
pub(super) struct Model<'a> {
    pub cells: Vec<CellModel<'a>>,
    pub by_name: FxHashMap<String, usize>,
    pub families: Vec<Vec<usize>>,
    pub buffers: Vec<usize>,
    pub matching: FxHashMap<(usize, u64), Vec<(usize, [u8; 6])>>,
}

impl<'a> Model<'a> {
    pub fn corner(base: &Self, lib: &'a Library, eligible: &CellLib) -> Result<Self, String> {
        for c in &base.cells {
            let alt = lib
                .cells
                .iter()
                .find(|p| p.name == c.raw.name)
                .ok_or_else(|| {
                    format!("electrical corner {} lacks cell {}", lib.name, c.raw.name)
                })?;
            if alt.dont_use
                || !alt.problems.is_empty()
                || (alt.area - c.raw.area).abs() > 1e-8
                || alt.sequentials != c.raw.sequentials
                || alt.pins.len() != c.raw.pins.len()
                || alt.pins.iter().zip(&c.raw.pins).any(|(a, b)| {
                    a.name != b.name || a.direction != b.direction || a.function != b.function
                })
            {
                return Err(format!(
                    "electrical corner {} has incompatible cell {}",
                    lib.name, c.raw.name
                ));
            }
        }
        Ok(Self::build(std::slice::from_ref(lib), eligible, false))
    }

    pub fn new(libs: &'a [Library], eligible: &CellLib) -> Self {
        Self::build(libs, eligible, true)
    }

    fn build(libs: &'a [Library], eligible: &CellLib, with_matching: bool) -> Self {
        let mut raw = FxHashMap::default();
        for l in libs {
            for c in &l.cells {
                raw.entry(c.name.as_str()).or_insert((c, l));
            }
        }
        let mut m = Self {
            cells: Vec::new(),
            by_name: FxHashMap::default(),
            families: Vec::new(),
            buffers: Vec::new(),
            matching: FxHashMap::default(),
        };
        let mut families = FxHashMap::default();
        for c in &eligible.comb {
            let (cell, lib) = raw[c.name.as_str()];
            let key = (c.inputs.len(), c.tt);
            let next = families.len();
            let family = *families.entry(key).or_insert(next);
            while m.families.len() <= family {
                m.families.push(Vec::new());
            }
            let id = m.add(cell, lib, true, family, None, None);
            m.cells[id].truth = Some(c.tt);
            m.families[family].push(id);
            if c.inputs.len() == 1 && c.tt == 0b10 {
                m.cells[id].buffer = true;
                m.buffers.push(id);
            }
        }
        let mut seq_prototypes: Vec<usize> = Vec::new();
        for s in &eligible.seq {
            // A selector can have several equivalent mapper role assignments.
            // Electrical analysis describes the physical cell only once.
            if m.by_name.contains_key(&s.name) {
                continue;
            }
            let (cell, lib) = raw[s.name.as_str()];
            let data = cell.pins.iter().position(|p| p.name == s.data);
            let clock = cell.pins.iter().position(|p| p.name == s.clock.pin);
            let same = seq_prototypes.iter().copied().find(|&id| {
                let old = m.cells[id].raw;
                old.sequentials == cell.sequentials
                    && old.pins.len() == cell.pins.len()
                    && old.pins.iter().zip(&cell.pins).all(|(a, b)| {
                        a.name == b.name && a.direction == b.direction && a.function == b.function
                    })
            });
            let family = if let Some(id) = same {
                m.cells[id].family
            } else {
                m.families.push(Vec::new());
                m.families.len() - 1
            };
            let id = m.add(cell, lib, false, family, data, clock);
            for pin in [&s.enable, &s.hold].into_iter().flatten() {
                let index = cell
                    .pins
                    .iter()
                    .position(|p| p.name == pin.pin)
                    .expect("classified synchronous pin");
                m.cells[id].data.push(index);
            }
            m.cells[id].data.sort_unstable();
            m.cells[id].data.dedup();
            m.cells[id].q_pins = [
                s.q.as_ref()
                    .and_then(|q| cell.pins.iter().position(|p| &p.name == q)),
                s.qn.as_ref()
                    .and_then(|q| cell.pins.iter().position(|p| &p.name == q)),
            ];
            if same.is_none() {
                seq_prototypes.push(id);
            }
            m.families[family].push(id);
        }
        for (name, _) in [&eligible.tie0, &eligible.tie1].into_iter().flatten() {
            let (cell, lib) = raw[name.as_str()];
            m.add(cell, lib, false, usize::MAX, None, None);
        }
        for f in &mut m.families {
            f.sort_by(|&a, &b| {
                m.cells[a]
                    .raw
                    .area
                    .total_cmp(&m.cells[b].raw.area)
                    .then(a.cmp(&b))
            });
        }
        m.buffers.sort_by(|&a, &b| {
            m.cells[a]
                .raw
                .area
                .total_cmp(&m.cells[b].raw.area)
                .then(a.cmp(&b))
        });
        if with_matching {
            for (&(n, tt), matches) in &eligible.table {
                let mut v: Vec<_> = matches
                    .iter()
                    .filter(|x| x.phase == 0)
                    .map(|x| (x.cell as usize, x.perm))
                    .collect();
                v.sort_by(|a, b| {
                    m.cells[a.0]
                        .raw
                        .area
                        .total_cmp(&m.cells[b.0].raw.area)
                        .then(a.0.cmp(&b.0))
                });
                if !v.is_empty() {
                    m.matching.insert((n as usize, tt), v);
                }
            }
        }
        m
    }

    fn add(
        &mut self,
        c: &'a Cell,
        l: &Library,
        comb: bool,
        family: usize,
        data: Option<usize>,
        clock: Option<usize>,
    ) -> usize {
        let id = self.cells.len();
        self.by_name.insert(c.name.clone(), id);
        self.cells.push(CellModel {
            raw: c,
            inputs: c
                .pins
                .iter()
                .enumerate()
                .filter(|(_, p)| p.direction == Direction::Input)
                .map(|(i, _)| i)
                .collect(),
            outputs: c
                .pins
                .iter()
                .enumerate()
                .filter(|(_, p)| p.direction == Direction::Output)
                .map(|(i, _)| i)
                .collect(),
            data: data.into_iter().collect(),
            clock,
            q_pins: [None, None],
            comb,
            buffer: false,
            time: l.time_unit_ns,
            cap: l.cap_unit_pf,
            pin_caps: c
                .pins
                .iter()
                .map(|p| {
                    let rise = p
                        .rise_capacitance_range
                        .map_or_else(|| p.rise_capacitance.unwrap_or(p.capacitance), |r| r.1);
                    let fall = p
                        .fall_capacitance_range
                        .map_or_else(|| p.fall_capacitance.unwrap_or(p.capacitance), |r| r.1);
                    rise.max(fall) * l.cap_unit_pf
                })
                .collect(),
            cap_limits: c
                .pins
                .iter()
                .map(|p| {
                    p.max_capacitance
                        .or(l.default_max_capacitance)
                        .map_or(f64::INFINITY, |v| v * l.cap_unit_pf)
                })
                .collect(),
            slew_limits: c
                .pins
                .iter()
                .map(|p| {
                    p.max_transition
                        .or(l.default_max_transition)
                        .map_or(f64::INFINITY, |v| v * l.time_unit_ns)
                })
                .collect(),
            fanout_limits: c
                .pins
                .iter()
                .map(|p| {
                    p.max_fanout
                        .or(l.default_max_fanout)
                        .unwrap_or(f64::INFINITY)
                })
                .collect(),
            fanout_loads: c
                .pins
                .iter()
                .map(|p| p.fanout_load.unwrap_or(l.default_fanout_load))
                .collect(),
            arcs: compiled_arcs(c),
            setups: compiled_setup(c),
            family,
            truth: None,
        });
        id
    }
}
