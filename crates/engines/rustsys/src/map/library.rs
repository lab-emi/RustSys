// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Cell classification and the matching table (SYNTH-REQ-0501, -0502, -0511, -0513, -0520).

use rustc_hash::FxHashMap;
use rusteda_formats::liberty::{Cell, Direction, Func, Library, SeqKind};

mod selector_costs;
pub(crate) use selector_costs::prefer_narrow_muxes;

/// Glob match with `*` and `?`.
pub fn glob_match(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

/// A usable single-output combinational cell.
#[derive(Debug, Clone)]
pub struct CombCell {
    pub name: String,
    pub inputs: Vec<String>,
    pub output: String,
    /// Truth table over `inputs` (LIBERTY-REQ-0022).
    pub tt: u64,
    pub area: f64,
    /// Estimated delay from each input (SYNTH-REQ-0513).
    pub delay: Vec<f64>,
    /// `area` in fixed-point mapping units (SYNTH-REQ-0514).
    pub area_q: u32,
    pub n_inputs: u8,
    pub delay_arr: [i32; 6],
    /// `delay` in fixed-point mapping units (SYNTH-REQ-0514).
    pub delay_q: Vec<i32>,
}

/// Fixed-point cost units of mapping (SYNTH-REQ-0514): delays in 1e-5 library time units and
/// areas in 1e-2 library area units. Integer costs make every mapping backend compute exactly
/// the same values, so their netlists are identical.
pub const DELAY_SCALE: f64 = 1e5;
pub const AREA_SCALE: f64 = 1e2;
/// Arrival of an unmapped (node, phase).
pub const ARR_INF: i32 = i32::MAX;
/// Saturation value of area flows; also the area flow of an unmapped (node, phase).
pub const AF_MAX: u32 = 1 << 30;

/// Delay in fixed-point units (at least one unit).
pub fn quantize_delay(d: f64) -> i32 {
    (d * DELAY_SCALE).round().clamp(1.0, 1.0e9) as i32
}

/// Area in fixed-point units.
pub fn quantize_area(a: f64) -> u32 {
    (a * AREA_SCALE).round().clamp(0.0, f64::from(AF_MAX)) as u32
}

/// A pin connection with polarity: the pin is active (or passes data) when the signal driven
/// onto it is `!active_low`.
#[derive(Debug, Clone)]
pub struct PolPin {
    pub pin: String,
    pub active_low: bool,
}

/// A usable flip-flop or latch.
#[derive(Debug, Clone)]
pub struct SeqCell {
    pub name: String,
    pub latch: bool,
    /// Data pin; `data_inverted` if the stored value is the complement of the pin.
    pub data: String,
    pub data_inverted: bool,
    /// Optional data-load control. Inactive means hold the same stored state.
    pub enable: Option<PolPin>,
    /// Optional physical hold-data input of a selector (for example scan data).
    /// The mapper must wire it to that cell's stored state, compensating polarity.
    pub hold: Option<PolPin>,
    /// Clock (edge-triggered: `active_low` = falling edge) or enable (latch).
    pub clock: PolPin,
    pub clear: Option<PolPin>,
    pub preset: Option<PolPin>,
    /// Output pin carrying the state, and output pin carrying its complement.
    pub q: Option<String>,
    pub qn: Option<String>,
    pub area: f64,
    pub clk_to_q: f64,
}

/// A match of a cut function to a cell: cell input `i` connects to leaf `perm[i]`, inverted when
/// bit `i` of `phase` is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub cell: u32,
    pub perm: [u8; 6],
    pub phase: u8,
    pub leaf_phase: u8,
}

/// Classified library.
#[derive(Debug, Clone)]
pub struct CellLib {
    pub comb: Vec<CombCell>,
    pub seq: Vec<SeqCell>,
    pub inverter: usize,
    pub buffer: Option<usize>,
    pub tie0: Option<(String, String)>,
    pub tie1: Option<(String, String)>,
    /// Area of the tie cells (constant 0, constant 1).
    pub tie_area: [f64; 2],
    /// (number of variables, truth table) -> matches, cheapest first.
    pub table: FxHashMap<(u8, u64), Vec<Match>>,
    pub max_inputs: usize,
    pub time_unit_ns: f64,
    pub warnings: Vec<String>,
    /// Cells rejected with the reason (for reporting).
    pub rejected: Vec<(String, String)>,
}

fn tt_mask(n: usize) -> u64 {
    if n >= 6 {
        u64::MAX
    } else {
        (1u64 << (1u32 << n)) - 1
    }
}

/// Single pin (possibly complemented) of a function, if it is one.
fn single_pin(f: &Func) -> Option<(String, bool)> {
    match f {
        Func::Var(v) => Some((v.clone(), false)),
        Func::Not(a) => single_pin(a).map(|(v, c)| (v, !c)),
        _ => None,
    }
}

fn arc_delay(cell: &Cell, out: &str, input: &str, load: f64, slew: Option<f64>) -> f64 {
    let Some(pin) = cell.pin(out) else { return 1.0 };
    let mut best: Option<f64> = None;
    for a in pin.arcs.iter().filter(|a| a.related_pin == input) {
        for t in [&a.cell_rise, &a.cell_fall].into_iter().flatten() {
            if t.is_consistent() {
                let v = t.lookup_slew_load(slew.unwrap_or_else(|| t.mid_slew()), load);
                best = Some(best.map_or(v, |b: f64| b.max(v)));
            }
        }
    }
    best.unwrap_or(1.0).max(1e-6)
}

/// Reason a cell is excluded as an isolation, level-shifter, or always-on cell (SYNTH-REQ-0501).
fn special_cell_reason(c: &Cell) -> Option<&'static str> {
    if c.is_isolation_cell
        || c.pins
            .iter()
            .any(|p| p.isolation_cell_enable_pin || p.isolation_cell_data_pin)
    {
        return Some("isolation cell");
    }
    if c.is_level_shifter
        || c.pins
            .iter()
            .any(|p| p.level_shifter_enable_pin || p.level_shifter_data_pin)
    {
        return Some("level-shifter cell");
    }
    if c.always_on || c.pins.iter().any(|p| p.always_on) {
        return Some("always-on cell");
    }
    None
}

impl CellLib {
    /// Classifies the cells of the libraries (first definition of a name wins).
    pub fn build(libs: &[Library], dont_use: &[String]) -> Result<CellLib, String> {
        Self::build_for_mapping(libs, dont_use, false)
    }

    /// Retaining distinct complemented-leaf choices lets covering price shared phases.
    pub fn build_for_mapping(
        libs: &[Library],
        dont_use: &[String],
        phase_choices: bool,
    ) -> Result<CellLib, String> {
        Self::build_profile(libs, dont_use, phase_choices, None)
    }

    /// Use a specified transition and FO4 of the smallest eligible inverter for
    /// constrained covering. This proxy must not depend on unused large drive variants.
    pub fn build_profile(
        libs: &[Library],
        dont_use: &[String],
        phase_choices: bool,
        slew_ns: Option<f64>,
    ) -> Result<CellLib, String> {
        let mut seen: FxHashMap<&str, ()> = FxHashMap::default();
        let mut cells: Vec<&Cell> = Vec::new();
        let mut warnings = Vec::new();
        for l in libs {
            for c in &l.cells {
                if seen.insert(&c.name, ()).is_some() {
                    warnings.push(format!(
                        "cell '{}' is defined more than once; the first definition is used",
                        c.name
                    ));
                    continue;
                }
                cells.push(c);
            }
        }
        let time_unit_ns = libs.first().map_or(1.0, |l| l.time_unit_ns);
        let mut lib = CellLib {
            comb: Vec::new(),
            seq: Vec::new(),
            inverter: usize::MAX,
            buffer: None,
            tie0: None,
            tie1: None,
            tie_area: [0.0; 2],
            table: FxHashMap::default(),
            max_inputs: 0,
            time_unit_ns,
            warnings,
            rejected: Vec::new(),
        };
        // Untimed mapping retains the historical estimate. With electrical constraints,
        // characterize at the supplied slew and a reproducible small-cell reference load.
        let mut inv_caps = Vec::new();
        let mut small = None;
        for c in &cells {
            if special_cell_reason(c).is_some() {
                continue;
            }
            let ins: Vec<_> = c.inputs().collect();
            let outs: Vec<_> = c.outputs().collect();
            if ins.len() == 1
                && outs.len() == 1
                && outs[0]
                    .function
                    .as_ref()
                    .is_some_and(|f| f.truth_table(&[ins[0].name.as_str()]) == Some(1))
            {
                inv_caps.push(ins[0].capacitance);
                if !c.dont_use
                    && !dont_use.iter().any(|p| glob_match(p, &c.name))
                    && small.is_none_or(|(a, _)| c.area < a)
                {
                    let pin = ins[0];
                    let cap = pin
                        .rise_capacitance_range
                        .map(|v| v.1)
                        .unwrap_or(pin.rise_capacitance.unwrap_or(pin.capacitance))
                        .max(
                            pin.fall_capacitance_range
                                .map(|v| v.1)
                                .unwrap_or(pin.fall_capacitance.unwrap_or(pin.capacitance)),
                        );
                    small = Some((c.area, cap));
                }
            }
        }
        let load = if slew_ns.is_some() {
            4.0 * small.map_or(0.0, |(_, cap)| cap)
        } else if inv_caps.is_empty() {
            0.0
        } else {
            4.0 * inv_caps.iter().sum::<f64>() / inv_caps.len() as f64
        };
        let slew = slew_ns.map(|t| t / time_unit_ns);
        for c in cells {
            if let Err(reason) = lib.add_cell(c, dont_use, load, slew) {
                lib.rejected.push((c.name.clone(), reason));
            }
        }
        // Inverter and buffer: smallest area.
        let mut inv = None;
        for (i, c) in lib.comb.iter().enumerate() {
            if c.inputs.len() == 1
                && c.tt == 0b01
                && inv.is_none_or(|j: usize| c.area < lib.comb[j].area)
            {
                inv = Some(i);
            }
            if c.inputs.len() == 1
                && c.tt == 0b10
                && lib.buffer.is_none_or(|j| c.area < lib.comb[j].area)
            {
                lib.buffer = Some(i);
            }
        }
        lib.inverter = inv.ok_or("the library has no usable inverter (SYNTH-REQ-0502)")?;
        lib.build_table(phase_choices);
        Ok(lib)
    }

    fn add_cell(
        &mut self,
        c: &Cell,
        dont_use: &[String],
        load: f64,
        slew: Option<f64>,
    ) -> Result<(), String> {
        if c.dont_use {
            return Err("dont_use".into());
        }
        if dont_use.iter().any(|g| glob_match(g, &c.name)) {
            return Err("excluded by --dont-use".into());
        }
        if !c.problems.is_empty() {
            return Err(c.problems.join("; "));
        }
        if let Some(reason) = special_cell_reason(c) {
            return Err(reason.into());
        }
        if c.has_bus || c.has_statetable || c.is_clock_gating {
            return Err("bus, state table or clock gating cell".into());
        }
        // A test view is not an additional functional state element. Accept
        // only a complete primary sequential model, with every input role
        // checked below; never assume that a scan/control input is inactive.
        if c.has_test_cell && c.sequentials.len() != 1 {
            return Err("test view without one complete primary sequential model".into());
        }
        if c.pins
            .iter()
            .any(|p| matches!(p.direction, Direction::Inout))
        {
            return Err("inout pin".into());
        }
        let ins: Vec<String> = c.inputs().map(|p| p.name.clone()).collect();
        let outs: Vec<&rusteda_formats::liberty::Pin> = c.outputs().collect();
        if outs.iter().any(|p| p.three_state.is_some()) {
            return Err("three-state output".into());
        }
        if outs.is_empty() || outs.iter().any(|p| p.function.is_none()) {
            return Err("output without function".into());
        }
        match c.sequentials.len() {
            0 => {
                if ins.is_empty() {
                    let mut consts = Vec::with_capacity(outs.len());
                    for p in &outs {
                        let f = p.function.as_ref().expect("checked");
                        let tt = f
                            .truth_table(&[])
                            .ok_or("function refers to unknown names")?;
                        consts.push((p.name.as_str(), tt & 1 == 1));
                    }
                    for (pin, v) in consts {
                        let idx = usize::from(v);
                        let slot = if v { &mut self.tie1 } else { &mut self.tie0 };
                        if slot.is_none() || c.area < self.tie_area[idx] {
                            *slot = Some((c.name.clone(), pin.to_string()));
                            self.tie_area[idx] = c.area;
                        }
                    }
                    return Ok(());
                }
                if outs.len() != 1 {
                    return Err("more than one output".into());
                }
                if ins.len() > 6 {
                    return Err("more than 6 inputs (SYNTH-REQ-0502)".into());
                }
                let f = outs[0].function.as_ref().expect("checked");
                let names: Vec<&str> = ins.iter().map(String::as_str).collect();
                let tt = f
                    .truth_table(&names)
                    .ok_or("function refers to unknown names")?;
                // Every input must influence the function.
                for k in 0..ins.len() {
                    let m = crate::map::cuts::var_mask(k) & tt_mask(ins.len());
                    let lo = tt & !m;
                    let hi = (tt & m) >> (1u32 << k);
                    if lo & !m == hi & !m {
                        return Err(format!("input {} does not affect the output", ins[k]));
                    }
                }
                let delay: Vec<f64> = ins
                    .iter()
                    .map(|i| arc_delay(c, &outs[0].name, i, load, slew))
                    .collect();
                let delay_q: Vec<i32> = delay.iter().map(|d| quantize_delay(*d)).collect();
                let mut delay_arr = [0i32; 6];
                delay_arr[..delay_q.len()].copy_from_slice(&delay_q);
                self.max_inputs = self.max_inputs.max(ins.len());
                self.comb.push(CombCell {
                    n_inputs: ins.len() as u8,
                    name: c.name.clone(),
                    inputs: ins,
                    output: outs[0].name.clone(),
                    tt,
                    area: c.area,
                    area_q: quantize_area(c.area),
                    delay_arr,
                    delay_q,
                    delay,
                });
                Ok(())
            }
            1 => self.add_seq(c, &ins, &outs, load, slew),
            _ => Err("more than one sequential group".into()),
        }
    }

    fn add_seq(
        &mut self,
        c: &Cell,
        ins: &[String],
        outs: &[&rusteda_formats::liberty::Pin],
        load: f64,
        slew: Option<f64>,
    ) -> Result<(), String> {
        let s = &c.sequentials[0];
        if s.clocked_on_also.is_some() {
            return Err("clocked_on_also".into());
        }
        let modes = if let Some((data, inverted)) = single_pin(&s.data) {
            vec![super::seq_modes::DataMode {
                data,
                inverted,
                enable: None,
                hold: None,
            }]
        } else {
            super::seq_modes::infer(s, ins)?
        };
        let (clk, clk_low) = single_pin(&s.clock).ok_or("clock is not a single pin")?;
        let pol = |f: &Option<Func>| -> Result<Option<PolPin>, String> {
            match f {
                None => Ok(None),
                Some(f) => {
                    let (p, low) = single_pin(f).ok_or("clear/preset is not a single pin")?;
                    Ok(Some(PolPin {
                        pin: p,
                        active_low: low,
                    }))
                }
            }
        };
        let clear = pol(&s.clear)?;
        let preset = pol(&s.preset)?;
        // Every physical input has one explicit role. Validate all views
        // before publishing any of them; no extra test/control input is ignored.
        for mode in &modes {
            let mut roles = vec![mode.data.as_str(), clk.as_str()];
            roles.extend(clear.iter().map(|p| p.pin.as_str()));
            roles.extend(preset.iter().map(|p| p.pin.as_str()));
            roles.extend(mode.enable.iter().map(|p| p.pin.as_str()));
            roles.extend(mode.hold.iter().map(|p| p.pin.as_str()));
            for (i, pin) in roles.iter().enumerate() {
                if roles[..i].contains(pin) {
                    return Err(format!(
                        "input {pin} has multiple sequential roles (SYNTH-REQ-0523)"
                    ));
                }
                if !ins.iter().any(|p| p == pin) {
                    return Err(format!("'{pin}' is not an input pin"));
                }
            }
            for pin in ins {
                if !roles.contains(&pin.as_str()) {
                    return Err(format!(
                        "input {pin} is not a supported sequential data/control pin (SYNTH-REQ-0523)"
                    ));
                }
            }
        }
        let mut q = None;
        let mut qn = None;
        for o in outs {
            match o.function.as_ref().and_then(single_pin) {
                Some((v, false)) if v == s.state => q = Some(o.name.clone()),
                Some((v, true)) if v == s.state => qn = Some(o.name.clone()),
                Some((v, false)) if v == s.state_n && !s.state_n.is_empty() => {
                    qn = Some(o.name.clone())
                }
                Some((v, true)) if v == s.state_n && !s.state_n.is_empty() => {
                    q = Some(o.name.clone())
                }
                _ => {
                    return Err(format!(
                        "output {} is not the state or its complement",
                        o.name
                    ));
                }
            }
        }
        if q.is_none() && qn.is_none() {
            return Err("no state output".into());
        }
        let out_pin = q.clone().or_else(|| qn.clone()).expect("checked");
        let clk_to_q = arc_delay(c, &out_pin, &clk, load, slew);
        for mode in modes {
            self.seq.push(SeqCell {
                name: c.name.clone(),
                latch: s.kind == SeqKind::Latch,
                data: mode.data,
                data_inverted: mode.inverted,
                enable: mode.enable,
                hold: mode.hold,
                clock: PolPin {
                    pin: clk.clone(),
                    active_low: clk_low,
                },
                clear: clear.clone(),
                preset: preset.clone(),
                q: q.clone(),
                qn: qn.clone(),
                area: c.area,
                clk_to_q,
            });
        }
        Ok(())
    }

    /// Expands every combinational cell into all input permutations and polarities.
    fn build_table(&mut self, phase_choices: bool) {
        let mut table: FxHashMap<(u8, u64), Vec<(u32, Match)>> = FxHashMap::default();
        for (ci, c) in self.comb.iter().enumerate() {
            let n = c.inputs.len();
            let perms = permutations(n);
            for p in &perms {
                for phase in 0..(1u32 << n) {
                    let mut tt = 0u64;
                    for m in 0..(1u32 << n) {
                        // Leaf assignment m -> cell input assignment y.
                        let mut y = 0u32;
                        for (i, &leaf) in p.iter().enumerate() {
                            let b = ((m >> leaf) & 1) ^ ((phase >> i) & 1);
                            y |= b << i;
                        }
                        if (c.tt >> y) & 1 == 1 {
                            tt |= 1 << m;
                        }
                    }
                    let mut perm = [0u8; 6];
                    perm[..n].copy_from_slice(p);
                    let leaf_phase =
                        (0..n).fold(0u8, |v, i| v | (((phase as u8 >> i) & 1) << perm[i]));
                    let cost = phase.count_ones();
                    table.entry((n as u8, tt)).or_default().push((
                        cost,
                        Match {
                            cell: ci as u32,
                            perm,
                            phase: phase as u8,
                            leaf_phase,
                        },
                    ));
                }
            }
        }
        for (k, mut v) in table {
            // Keep, per cell, the variant with the fewest complemented inputs.
            v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cell.cmp(&b.1.cell)));
            let mut out: Vec<Match> = Vec::new();
            for (_, m) in v {
                if !out
                    .iter()
                    .any(|x| x.cell == m.cell && (!phase_choices || x.leaf_phase == m.leaf_phase))
                {
                    out.push(m);
                }
            }
            out.sort_by(|a, b| {
                let ca = self.comb[a.cell as usize].area;
                let cb = self.comb[b.cell as usize].area;
                a.phase
                    .count_ones()
                    .cmp(&b.phase.count_ones())
                    .then(ca.total_cmp(&cb))
                    .then(a.cell.cmp(&b.cell))
            });
            // A later match cannot win if an earlier match has the same referenced
            // leaf phases, no greater cell area, and no slower pin for every leaf.
            // Preserve the first match on ties, including its physical implementation.
            let mut kept = Vec::new();
            let mut costs: Vec<(u8, u32, [i32; 6])> = Vec::new();
            for m in out {
                let cell = &self.comb[m.cell as usize];
                let phases = m.leaf_phase;
                let mut delays = [0i32; 6];
                for i in 0..k.0 as usize {
                    let leaf = m.perm[i] as usize;
                    delays[leaf] = cell.delay_arr[i];
                }
                let dominated = costs.iter().any(|(p, a, d)| {
                    *p == phases
                        && *a <= cell.area_q
                        && (0..k.0 as usize).all(|i| d[i] <= delays[i])
                });
                if !dominated {
                    costs.push((phases, cell.area_q, delays));
                    kept.push(m);
                }
            }
            self.table.insert(k, kept);
        }
    }

    /// Matches of a function of `n` variables.
    pub fn matches(&self, n: usize, tt: u64) -> &[Match] {
        self.table
            .get(&(n as u8, tt & tt_mask(n)))
            .map_or(&[], Vec::as_slice)
    }
}

/// All permutations of `0..n` (n ≤ 6).
fn permutations(n: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = (0..n as u8).collect();
    fn rec(k: usize, cur: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
        if k == cur.len() {
            out.push(cur.clone());
            return;
        }
        for i in k..cur.len() {
            cur.swap(k, i);
            rec(k + 1, cur, out);
            cur.swap(k, i);
        }
    }
    rec(0, &mut cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_phase_choice_matches_the_declared_function() {
        let raw = rusteda_formats::liberty::read_text(
            &rustsys_fixtures::liberty::library(rustsys_fixtures::liberty::Variant::Rich),
            "rich.lib",
        )
        .unwrap();
        let lib = CellLib::build_for_mapping(&[raw], &[], true).unwrap();
        {
            for (&(n, tt), choices) in &lib.table {
                assert!(!choices.is_empty());
                for m in choices {
                    let cell = &lib.comb[m.cell as usize];
                    for x in 0..1u64 << n {
                        let pins = (0..n as usize).fold(0u64, |v, i| {
                            v | ((((x >> m.perm[i]) ^ (u64::from(m.phase) >> i)) & 1) << i)
                        });
                        assert_eq!((cell.tt >> pins) & 1, (tt >> x) & 1, "{} {m:?}", cell.name);
                    }
                }
            }
        }
    }

    #[test]
    fn globs() {
        assert!(glob_match("*_x1", "nand2_x1"));
        assert!(glob_match("n?nd*", "nand2_x1"));
        assert!(!glob_match("*_x2", "nand2_x1"));
        assert!(glob_match("*", ""));
        assert_eq!(permutations(3).len(), 6);
    }

    // SYNTH-REQ-0501, SYNTH-REQ-0522: isolation, level-shifter, and always-on cells (marked on
    // the cell or on a pin) are excluded from general mapping with their rejection reasons, and
    // multi-output tie cells (such as sky130 conb_1 HI/LO) are selected by smallest area.
    #[test]
    fn excludes_special_cells_and_selects_smallest_multi_output_tie() {
        let src = r#"library(wp3) {
            cell(inv_1) {
                area : 1.0;
                pin(A) { direction : input; capacitance : 0.01; }
                pin(Y) { direction : output; function : "!A"; }
            }
            cell(iso_1) {
                area : 0.5;
                is_isolation_cell : true;
                pin(A) { direction : input; }
                pin(EN) { direction : input; }
                pin(Y) { direction : output; function : "A & EN"; }
            }
            cell(lvl_1) {
                area : 0.5;
                is_level_shifter : true;
                pin(A) { direction : input; }
                pin(Y) { direction : output; function : "A"; }
            }
            cell(aon_buf) {
                area : 0.5;
                always_on : true;
                pin(A) { direction : input; }
                pin(Y) { direction : output; function : "A"; }
            }
            cell(aon_pin_inv) {
                area : 0.25;
                pin(A) { direction : input; always_on : true; }
                pin(Y) { direction : output; function : "!A"; }
            }
            cell(macro_sparecell) {
                area : 36.0;
                pin(LO) { direction : output; function : "0"; }
            }
            cell(conb_1) {
                area : 3.75;
                pin(HI) { direction : output; function : "1"; }
                pin(LO) { direction : output; function : "0"; }
            }
        }"#;
        let raw = rusteda_formats::liberty::read_text(src, "wp3.lib").unwrap();
        let lib = CellLib::build(&[raw], &[]).unwrap();
        assert_eq!(lib.comb.len(), 1);
        assert_eq!(lib.comb[0].name, "inv_1");
        assert_eq!(
            lib.rejected,
            vec![
                ("iso_1".to_string(), "isolation cell".to_string()),
                ("lvl_1".to_string(), "level-shifter cell".to_string()),
                ("aon_buf".to_string(), "always-on cell".to_string()),
                ("aon_pin_inv".to_string(), "always-on cell".to_string()),
            ]
        );
        assert_eq!(lib.tie0, Some(("conb_1".to_string(), "LO".to_string())));
        assert_eq!(lib.tie1, Some(("conb_1".to_string(), "HI".to_string())));
        assert_eq!(lib.tie_area, [3.75, 3.75]);
    }
}
