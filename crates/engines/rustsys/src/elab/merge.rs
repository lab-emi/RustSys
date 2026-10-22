// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Merging of the environments of alternative paths (SYNTH-REQ-0210, SYNTH-REQ-0211): two paths
//! under a condition, or mutually exclusive paths under their conditions.

use rustc_hash::FxHashSet;

use super::{Elaborator, Env, VarKey};
use crate::aig::Lit;
use crate::bits::{self, Bv};

impl Elaborator<'_> {
    /// Merges two environments: `c ? t : e`, key by key.
    pub(crate) fn merge_env(&mut self, c: Lit, t: Env, e: Env) -> Env {
        match c.const_value() {
            Some(true) => return t,
            Some(false) => return e,
            None => {}
        }
        let mut out = t.clone();
        let keys: FxHashSet<VarKey> = t.vals.keys().chain(e.vals.keys()).copied().collect();
        for k in keys {
            let tv = t
                .vals
                .get(&k)
                .cloned()
                .unwrap_or_else(|| self.initial_value(k));
            let ev = e
                .vals
                .get(&k)
                .cloned()
                .unwrap_or_else(|| self.initial_value(k));
            let v = if tv == ev {
                tv
            } else {
                bits::mux(&mut self.aig, c, &tv, &ev)
            };
            out.vals.insert(k, v);
        }
        let keys: FxHashSet<VarKey> = t
            .assigned
            .keys()
            .chain(e.assigned.keys())
            .copied()
            .collect();
        for k in keys {
            let w = self.initial_value(k).len();
            let ta = t
                .assigned
                .get(&k)
                .cloned()
                .unwrap_or_else(|| vec![Lit::FALSE; w]);
            let ea = e
                .assigned
                .get(&k)
                .cloned()
                .unwrap_or_else(|| vec![Lit::FALSE; w]);
            out.assigned
                .insert(k, bits::mux(&mut self.aig, c, &ta, &ea));
        }
        let keys: FxHashSet<VarKey> = t.nba.keys().chain(e.nba.keys()).copied().collect();
        for k in keys {
            let init = self.initial_value(k);
            let w = init.len();
            let (tv, ta) = t
                .nba
                .get(&k)
                .cloned()
                .unwrap_or_else(|| (init.clone(), vec![Lit::FALSE; w]));
            let (ev, ea) = e
                .nba
                .get(&k)
                .cloned()
                .unwrap_or_else(|| (init.clone(), vec![Lit::FALSE; w]));
            let v = bits::mux(&mut self.aig, c, &tv, &ev);
            let a = bits::mux(&mut self.aig, c, &ta, &ea);
            out.nba.insert(k, (v, a));
        }
        out.live = self.aig.mux(c, t.live, e.live);
        out.broke = self.aig.mux(c, t.broke, e.broke);
        out.continued = self.aig.mux(c, t.continued, e.continued);
        out.returned = self.aig.mux(c, t.returned, e.returned);
        for (s, sp) in e.blocking_sigs {
            out.blocking_sigs.entry(s).or_insert(sp);
        }
        for (s, sp) in e.nba_sigs {
            out.nba_sigs.entry(s).or_insert(sp);
        }
        out
    }

    /// Merges the environments of mutually exclusive paths taken under `conds` with `dflt`, the
    /// environment when none is taken: every bit becomes `OR_i (c_i & x_i) | (!OR_i c_i & x_d)`
    /// over the paths whose bit differs from the default's.
    pub(crate) fn merge_parallel(&mut self, conds: &[Lit], envs: Vec<Env>, dflt: Env) -> Env {
        self.merge_alternatives(conds, envs, dflt, false)
    }

    /// Merges paths whose conditions may overlap (SYNTH-REQ-0225): every bit becomes
    /// `OR_i (c_i & x_i) | (!OR_i c_i & x_d)` over all paths, so that two taken paths give the OR
    /// of their values whatever the representation of those values.
    pub(crate) fn merge_or(&mut self, conds: &[Lit], envs: Vec<Env>, dflt: Env) -> Env {
        self.merge_alternatives(conds, envs, dflt, true)
    }

    fn merge_alternatives(&mut self, conds: &[Lit], envs: Vec<Env>, dflt: Env, all: bool) -> Env {
        let mut out = dflt.clone();
        let keys: FxHashSet<VarKey> = envs
            .iter()
            .flat_map(|e| e.vals.keys())
            .chain(dflt.vals.keys())
            .copied()
            .collect();
        for k in keys {
            let init = self.initial_value(k);
            let d = dflt.vals.get(&k).cloned().unwrap_or_else(|| init.clone());
            let vs: Vec<Bv> = envs
                .iter()
                .map(|e| e.vals.get(&k).cloned().unwrap_or_else(|| init.clone()))
                .collect();
            let v = self.par_bits(conds, &vs, &d, all);
            out.vals.insert(k, v);
        }
        let keys: FxHashSet<VarKey> = envs
            .iter()
            .flat_map(|e| e.assigned.keys())
            .chain(dflt.assigned.keys())
            .copied()
            .collect();
        for k in keys {
            let zero = vec![Lit::FALSE; self.initial_value(k).len()];
            let d = dflt
                .assigned
                .get(&k)
                .cloned()
                .unwrap_or_else(|| zero.clone());
            let vs: Vec<Bv> = envs
                .iter()
                .map(|e| e.assigned.get(&k).cloned().unwrap_or_else(|| zero.clone()))
                .collect();
            let v = self.par_bits(conds, &vs, &d, all);
            out.assigned.insert(k, v);
        }
        let keys: FxHashSet<VarKey> = envs
            .iter()
            .flat_map(|e| e.nba.keys())
            .chain(dflt.nba.keys())
            .copied()
            .collect();
        for k in keys {
            let init = self.initial_value(k);
            let none = (init.clone(), vec![Lit::FALSE; init.len()]);
            let (dv, da) = dflt.nba.get(&k).cloned().unwrap_or_else(|| none.clone());
            let (vs, as_): (Vec<Bv>, Vec<Bv>) = envs
                .iter()
                .map(|e| e.nba.get(&k).cloned().unwrap_or_else(|| none.clone()))
                .unzip();
            let v = self.par_bits(conds, &vs, &dv, all);
            let a = self.par_bits(conds, &as_, &da, all);
            out.nba.insert(k, (v, a));
        }
        let flag = |this: &mut Self, f: fn(&Env) -> Lit| {
            let vs: Vec<Bv> = envs.iter().map(|e| vec![f(e)]).collect();
            this.par_bits(conds, &vs, &[f(&dflt)], all)[0]
        };
        out.live = flag(self, |e| e.live);
        out.broke = flag(self, |e| e.broke);
        out.continued = flag(self, |e| e.continued);
        out.returned = flag(self, |e| e.returned);
        for e in envs {
            for (s, sp) in e.blocking_sigs {
                out.blocking_sigs.entry(s).or_insert(sp);
            }
            for (s, sp) in e.nba_sigs {
                out.nba_sigs.entry(s).or_insert(sp);
            }
        }
        out
    }

    /// Parallel selection of every bit (see `merge_parallel`); with `all`, paths whose bit equals
    /// the default's take part too (see `merge_or`).
    fn par_bits(&mut self, conds: &[Lit], vs: &[Bv], d: &[Lit], all: bool) -> Bv {
        (0..d.len())
            .map(|b| {
                if all && vs.iter().all(|v| v[b] == d[b]) {
                    return d[b];
                }
                let mut sel = Vec::new();
                let mut groups: Vec<(Lit, Vec<Lit>)> = Vec::new();
                for (c, v) in conds.iter().zip(vs) {
                    if all || v[b] != d[b] {
                        sel.push(*c);
                        if let Some((_, cs)) = groups.iter_mut().find(|(value, _)| *value == v[b]) {
                            cs.push(*c);
                        } else {
                            groups.push((v[b], vec![*c]));
                        }
                    }
                }
                if sel.is_empty() {
                    return d[b];
                }
                let any = bits::red_or(&mut self.aig, &sel);
                let mut terms: Vec<Lit> = groups
                    .into_iter()
                    .map(|(value, cs)| {
                        let c = bits::red_or(&mut self.aig, &cs);
                        self.aig.and(c, value)
                    })
                    .collect();
                terms.push(self.aig.and(!any, d[b]));
                bits::red_or(&mut self.aig, &terms)
            })
            .collect()
    }
}
