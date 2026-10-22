// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

use super::Options;
use super::model::Model;
use super::network::Network;

pub(super) const ABSENT: f64 = -1e20;

#[derive(Debug, Clone, Copy)]
pub(super) struct Value {
    pub arrival: [f64; 2],
    pub slew: [f64; 2],
}

impl Default for Value {
    fn default() -> Self {
        Self {
            arrival: [ABSENT; 2],
            slew: [0.0; 2],
        }
    }
}

#[derive(Debug)]
pub(super) struct Timing {
    pub load: Vec<f64>,
    pub value: Vec<Value>,
    pub required: Vec<[f64; 2]>,
    pub slope: Vec<f64>,
    pub worst_slack: f64,
    pub violation: f64,
    pub endpoints: usize,
}

impl Timing {
    pub fn launch(lib: &Model<'_>, cell: usize, op: usize, load: f64, clock_slew: f64) -> Value {
        let c = &lib.cells[cell];
        let mut v = Value::default();
        if let Some(clk) = c.clock {
            for (_, to, d, s) in c.arc(clk, op, [clock_slew; 2], load) {
                if d >= 0.0 {
                    v.arrival[to] = v.arrival[to].max(d);
                    v.slew[to] = v.slew[to].max(s);
                }
            }
        }
        v
    }
    pub fn new(net: &Network, lib: &Model<'_>, opts: &Options) -> Self {
        Self::build(net, lib, opts, true)
    }

    /// Electrical-rule checks need only loads and forward transitions, not endpoint
    /// requirements or load derivatives. The forward values are identical to full timing.
    pub fn rules(net: &Network, lib: &Model<'_>, opts: &Options) -> Self {
        Self::build(net, lib, opts, false)
    }

    fn build(net: &Network, lib: &Model<'_>, opts: &Options, full: bool) -> Self {
        let n = net.nodes.len();
        let mut t = Self {
            load: vec![0.0; n],
            value: vec![Value::default(); n],
            required: vec![[f64::INFINITY; 2]; n],
            slope: vec![0.0; n],
            worst_slack: f64::INFINITY,
            violation: 0.0,
            endpoints: 0,
        };
        let parallel = super::parallel_timing::enabled(net, opts);
        if parallel {
            super::parallel_timing::initialize(&mut t, net, lib, opts);
            super::parallel_timing::forward(&mut t, net, lib, opts);
        } else {
            for (i, node) in net.nodes.iter().enumerate() {
                t.load[i] = opts.output_load_pf * node.outputs as f64;
                if node.primary {
                    t.value[i] = Value {
                        arrival: [opts.input_delay_ns; 2],
                        slew: [opts.input_slew_ns; 2],
                    };
                }
            }
            for gate in &net.gates {
                if !gate.active {
                    continue;
                }
                let c = &lib.cells[gate.cell];
                for (&n, &pin) in gate.inputs.iter().zip(&c.inputs) {
                    t.load[n] += c.input_cap(pin);
                }
            }
            for gate in &net.gates {
                if !gate.active {
                    continue;
                }
                let c = &lib.cells[gate.cell];
                if c.comb {
                    continue;
                }
                if let Some(clk) = c.clock {
                    for &(op, n) in &gate.outputs {
                        let mut v = Value::default();
                        for (_, to, d, s) in c.arc(clk, op, [opts.clock_slew_ns; 2], t.load[n]) {
                            if d >= 0.0 {
                                v.arrival[to] = v.arrival[to].max(d);
                                v.slew[to] = v.slew[to].max(s);
                            }
                        }
                        t.value[n] = v;
                    }
                }
            }
            for &g in &net.order {
                let n = net.gates[g].outputs[0].1;
                t.value[n] = t.candidate(net, lib, g, net.gates[g].cell, false);
            }
        }
        if !full {
            t.worst_slack = 0.0;
            return t;
        }
        for (n, node) in net.nodes.iter().enumerate() {
            if node.outputs > 0 {
                t.endpoint(
                    n,
                    [opts.period_ns - opts.output_delay_ns - opts.margin_ns; 2],
                );
            }
        }
        for gate in &net.gates {
            if !gate.active {
                continue;
            }
            let c = &lib.cells[gate.cell];
            for &pin in &c.data {
                let i = c
                    .inputs
                    .iter()
                    .position(|&p| p == pin)
                    .expect("data is an input");
                let n = gate.inputs[i];
                let setup = c.setup(pin, t.value[n].slew, opts.clock_slew_ns);
                t.endpoint(
                    n,
                    [
                        opts.period_ns - opts.margin_ns - setup[0],
                        opts.period_ns - opts.margin_ns - setup[1],
                    ],
                );
            }
        }
        if parallel {
            super::parallel_timing::backward(&mut t, net, lib, opts);
        } else {
            for &g in net.order.iter().rev() {
                let gate = &net.gates[g];
                let c = &lib.cells[gate.cell];
                let (op, n) = gate.outputs[0];
                let dc = (t.load[n] * 0.02).max(1e-5);
                for (&ip, &a) in c.inputs.iter().zip(&gate.inputs) {
                    let slew = t.value[a].slew;
                    let base = c.arc_delay(ip, op, slew, t.load[n]);
                    let plus = c.arc_delay(ip, op, slew, t.load[n] + dc);
                    for (&(ti, to, d), &(_, _, d_plus)) in base.iter().zip(&plus) {
                        if d >= 0.0 {
                            t.required[a][ti] = t.required[a][ti].min(t.required[n][to] - d);
                            t.slope[n] = t.slope[n].max((d_plus - d) / dc);
                        }
                    }
                }
            }
        }
        for (n, node) in net.nodes.iter().enumerate() {
            let Some((g, op)) = node.driver else { continue };
            let gate = &net.gates[g];
            let c = &lib.cells[gate.cell];
            if c.comb {
                continue;
            }
            let Some(ip) = c.clock else { continue };
            let dc = (t.load[n] * 0.02).max(1e-5);
            let slew = [opts.clock_slew_ns; 2];
            let a = c.arc_delay(ip, op, slew, t.load[n]);
            let b = c.arc_delay(ip, op, slew, t.load[n] + dc);
            for (a, b) in a.iter().zip(&b) {
                if a.2 >= 0.0 {
                    t.slope[n] = t.slope[n].max((b.2 - a.2) / dc);
                }
            }
        }
        if !t.worst_slack.is_finite() {
            t.worst_slack = 0.0;
        }
        t
    }

    fn endpoint(&mut self, n: usize, req: [f64; 2]) {
        let mut worst = f64::INFINITY;
        for tr in 0..2 {
            self.required[n][tr] = self.required[n][tr].min(req[tr]);
            if self.value[n].arrival[tr] > ABSENT / 2.0 {
                worst = worst.min(req[tr] - self.value[n].arrival[tr]);
            }
        }
        if worst.is_finite() {
            self.worst_slack = self.worst_slack.min(worst);
            if worst < 0.0 {
                self.violation -= worst;
                self.endpoints += 1;
            }
        }
    }

    /// Latest transition at one consumer pin, used to keep critical sinks near the root.
    pub fn sink_required(
        &self,
        net: &Network,
        lib: &Model<'_>,
        g: usize,
        i: usize,
        input: usize,
        opts: &Options,
    ) -> f64 {
        let gate = &net.gates[g];
        let c = &lib.cells[gate.cell];
        let pin = c.inputs[i];
        if c.comb {
            let (op, out) = gate.outputs[0];
            c.arc_delay(pin, op, self.value[input].slew, self.load[out])
                .into_iter()
                .filter(|(_, _, d)| *d >= 0.0)
                .map(|(_, to, d)| self.required[out][to] - d)
                .fold(f64::INFINITY, f64::min)
        } else if c.data.contains(&pin) {
            let setup = c.setup(pin, self.value[input].slew, opts.clock_slew_ns);
            opts.period_ns - opts.margin_ns - setup[0].max(setup[1])
        } else {
            f64::INFINITY
        }
    }

    pub fn slack(&self, n: usize) -> f64 {
        (self.required[n][0] - self.value[n].arrival[0])
            .min(self.required[n][1] - self.value[n].arrival[1])
    }

    /// Transition at a particular driver/pin with an explicitly updated load.
    pub fn slew_at_load(
        &self,
        net: &Network,
        lib: &Model<'_>,
        driver: (usize, usize, usize),
        load: f64,
        clock_slew: f64,
    ) -> [f64; 2] {
        let (g, cell, op) = driver;
        self.slew_at_load_inputs(lib, cell, op, load, clock_slew, &net.gates[g].inputs)
    }

    pub fn slew_at_load_inputs(
        &self,
        lib: &Model<'_>,
        cell: usize,
        op: usize,
        load: f64,
        clock_slew: f64,
        inputs: &[usize],
    ) -> [f64; 2] {
        let c = &lib.cells[cell];
        let mut slew = [0.0_f64; 2];
        if c.comb {
            for (&pin, &n) in c.inputs.iter().zip(inputs) {
                for (ti, to, present, s) in c.arc_slew(pin, op, self.value[n].slew, load) {
                    if present && self.value[n].arrival[ti] > ABSENT / 2.0 {
                        slew[to] = slew[to].max(s);
                    }
                }
            }
        } else if let Some(clk) = c.clock {
            for (_, to, present, s) in c.arc_slew(clk, op, [clock_slew; 2], load) {
                if present {
                    slew[to] = slew[to].max(s);
                }
            }
        }
        slew
    }

    /// Candidate arrival includes the first-order delay change at its input drivers.
    pub fn candidate(
        &self,
        net: &Network,
        lib: &Model<'_>,
        g: usize,
        cell: usize,
        upstream: bool,
    ) -> Value {
        self.candidate_inputs(net, lib, g, cell, upstream, &net.gates[g].inputs)
    }

    pub fn candidate_inputs(
        &self,
        net: &Network,
        lib: &Model<'_>,
        g: usize,
        cell: usize,
        upstream: bool,
        inputs: &[usize],
    ) -> Value {
        let gate = &net.gates[g];
        let c = &lib.cells[cell];
        let old = &lib.cells[gate.cell];
        let n = gate.outputs[0].1;
        let op = c.outputs[0];
        let mut out = Value::default();
        for (i, (&ip, &a)) in c.inputs.iter().zip(inputs).enumerate() {
            let extra = if upstream {
                self.slope[a] * (c.input_cap(ip) - old.input_cap(old.inputs[i]))
            } else {
                0.0
            };
            for (ti, to, d, s) in c.arc(ip, op, self.value[a].slew, self.load[n]) {
                if d < 0.0 || self.value[a].arrival[ti] < ABSENT / 2.0 {
                    continue;
                }
                out.arrival[to] = out.arrival[to].max(self.value[a].arrival[ti] + extra + d);
                out.slew[to] = out.slew[to].max(s);
            }
        }
        out
    }
}
