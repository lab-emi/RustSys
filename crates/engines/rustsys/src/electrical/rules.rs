// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Electrical design-rule evaluation, independently of endpoint slack.

use super::{model::Model, network::Network, timing::Timing};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleReport {
    pub capacitance_nets: usize,
    pub transition_nets: usize,
    pub fanout_nets: usize,
    pub protected_nets: usize,
    pub constant_nets: usize,
    pub worst_capacitance_ratio: f64,
    pub worst_transition_ratio: f64,
    pub worst_fanout_ratio: f64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    pub cap: f64,
    pub slew: f64,
    pub fanout: f64,
    pub fanout_load: f64,
}

impl Limits {
    pub fn for_node(net: &Network, lib: &Model<'_>, n: usize) -> Self {
        let mut v = Self {
            cap: f64::INFINITY,
            slew: f64::INFINITY,
            fanout: f64::INFINITY,
            fanout_load: 0.0,
        };
        if let Some((g, p)) = net.nodes[n].driver {
            let c = &lib.cells[net.gates[g].cell];
            v.cap = c.max_cap(p);
            v.slew = c.max_slew(p);
            v.fanout = c.max_fanout(p);
        }
        for &(g, i) in &net.nodes[n].sinks {
            let c = &lib.cells[net.gates[g].cell];
            let p = c.inputs[i];
            v.slew = v.slew.min(c.max_slew(p));
            v.fanout_load += c.input_fanout(p);
        }
        v
    }

    pub fn violations(self, t: &Timing, n: usize) -> [bool; 3] {
        [
            t.load[n] > self.cap + 1e-12,
            t.value[n].slew[0].max(t.value[n].slew[1]) > self.slew + 1e-10,
            self.fanout_load > self.fanout + 1e-8,
        ]
    }
}

pub(super) fn report(net: &Network, lib: &Model<'_>, t: &Timing) -> RuleReport {
    let mut r = RuleReport::default();
    let ratio = |v: f64, limit: f64| {
        if limit.is_finite() {
            v / limit.max(1e-20)
        } else {
            0.0
        }
    };
    for (n, node) in net.nodes.iter().enumerate() {
        let l = Limits::for_node(net, lib, n);
        let bad = l.violations(t, n);
        if node.clock {
            r.protected_nets += usize::from(bad.iter().any(|x| *x));
            continue;
        }
        if node.driver.is_some_and(|(g, _)| {
            let c = &lib.cells[net.gates[g].cell];
            !c.comb && c.clock.is_none()
        }) {
            r.constant_nets += usize::from(bad.iter().any(|x| *x));
            continue;
        }
        r.capacitance_nets += usize::from(bad[0]);
        r.transition_nets += usize::from(bad[1]);
        r.fanout_nets += usize::from(bad[2]);
        r.worst_capacitance_ratio = r.worst_capacitance_ratio.max(ratio(t.load[n], l.cap));
        r.worst_transition_ratio = r
            .worst_transition_ratio
            .max(ratio(t.value[n].slew[0].max(t.value[n].slew[1]), l.slew));
        r.worst_fanout_ratio = r.worst_fanout_ratio.max(ratio(l.fanout_load, l.fanout));
    }
    r
}
