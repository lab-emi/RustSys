// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Procedures of the checker's RTL model: `always` blocks interpreted into combinational
//! functions, flip-flops and latches.

use std::collections::{HashMap, HashSet};

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{
    self, AlwaysKind, Edge, Expr, ExprKind, Stmt, StmtKind, TimingCtrl,
};

use super::proc::PState;
use super::{Builder, R, RtlState};
use crate::circ::B;
use crate::prove::{Proof, Rng, always, equal};

/// Conflict budget of each proof that the model needs (CHECK-REQ-0002).
pub(crate) const PROOF_BUDGET: u64 = 200_000;

/// Names referenced by a statement (for clock identification).
fn names_in(s: &Stmt, out: &mut HashSet<String>) {
    fn ex(e: &Expr, out: &mut HashSet<String>) {
        match &e.kind {
            ExprKind::Ident(p) => {
                out.insert(p.name.name.clone());
            }
            ExprKind::Unary(_, a) | ExprKind::Member(a, _) | ExprKind::Cast { expr: a, .. } => {
                ex(a, out)
            }
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) => {
                ex(a, out);
                ex(b, out);
            }
            ExprKind::Slice {
                base, left, right, ..
            } => {
                ex(base, out);
                ex(left, out);
                ex(right, out);
            }
            ExprKind::Cond(a, b, c) => {
                ex(a, out);
                ex(b, out);
                ex(c, out);
            }
            ExprKind::Inside(a, v) => {
                ex(a, out);
                v.iter().for_each(|x| ex(x, out));
            }
            ExprKind::Concat(v) | ExprKind::SysCall { args: v, .. } => {
                v.iter().for_each(|x| ex(x, out))
            }
            ExprKind::Repl(n, v) => {
                ex(n, out);
                v.iter().for_each(|x| ex(x, out));
            }
            ExprKind::Pattern { items, .. } => {
                for it in items {
                    match it {
                        ast::PatternItem::Positional(x) | ast::PatternItem::Keyed(_, x) => {
                            ex(x, out)
                        }
                    }
                }
            }
            ExprKind::Call { args, .. } => {
                for a in args {
                    if let ast::Arg::Positional(Some(x)) | ast::Arg::Named(_, Some(x)) = a {
                        ex(x, out);
                    }
                }
            }
            _ => {}
        }
    }
    match &s.kind {
        StmtKind::Block { stmts, decls, .. } => {
            for d in decls {
                if let ast::BlockDecl::Data(dd) = d {
                    dd.decls
                        .iter()
                        .filter_map(|x| x.init.as_ref())
                        .for_each(|x| ex(x, out));
                }
            }
            stmts.iter().for_each(|x| names_in(x, out));
        }
        StmtKind::Assign { lhs, rhs, .. } => {
            ex(lhs, out);
            ex(rhs, out);
        }
        StmtKind::IncDec { target, .. } => ex(target, out),
        StmtKind::If {
            cond, then, els, ..
        } => {
            ex(cond, out);
            names_in(then, out);
            if let Some(e) = els {
                names_in(e, out);
            }
        }
        StmtKind::Case { expr, items, .. } => {
            ex(expr, out);
            for it in items {
                it.labels.iter().for_each(|l| ex(l, out));
                names_in(&it.body, out);
            }
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            init.iter().for_each(|i| ex(&i.rhs, out));
            if let Some(c) = cond {
                ex(c, out);
            }
            step.iter().for_each(|x| names_in(x, out));
            names_in(body, out);
        }
        StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
            ex(cond, out);
            names_in(body, out);
        }
        StmtKind::Repeat { count, body } => {
            ex(count, out);
            names_in(body, out);
        }
        StmtKind::Forever(b) | StmtKind::Timed { body: b, .. } => names_in(b, out),
        StmtKind::Foreach { array, body, .. } => {
            ex(array, out);
            names_in(body, out);
        }
        StmtKind::Return(Some(e)) | StmtKind::Call(e) => ex(e, out),
        _ => {}
    }
}

impl<'a> Builder<'a> {
    pub(super) fn always(&mut self, sc: usize, a: &'a ast::Always) -> R<()> {
        let (events, body): (Option<&Vec<ast::EventExpr>>, &Stmt) = match &a.body.kind {
            StmtKind::Timed {
                ctrl: TimingCtrl::Events(e),
                body,
            } => (Some(e), body),
            StmtKind::Timed {
                ctrl: TimingCtrl::Star,
                body,
            } => (None, body),
            _ => (None, &a.body),
        };
        let edge = match (a.kind, events) {
            (AlwaysKind::Comb | AlwaysKind::Latch, Some(_)) => {
                return self.fail(a.span, "event control on always_comb/always_latch");
            }
            (AlwaysKind::Comb | AlwaysKind::Latch, None) => false,
            (_, Some(evs)) => {
                let n = evs
                    .iter()
                    .filter(|e| matches!(e.edge, Edge::Pos | Edge::Neg))
                    .count();
                if n == evs.len() {
                    true
                } else if n == 0 && a.kind == AlwaysKind::Always {
                    false
                } else {
                    return self.fail(a.span, "mixed event control");
                }
            }
            (AlwaysKind::Ff, None) => return self.fail(a.span, "always_ff without edges"),
            (AlwaysKind::Always, None) => {
                if matches!(
                    a.body.kind,
                    StmtKind::Timed {
                        ctrl: TimingCtrl::Star,
                        ..
                    }
                ) {
                    false
                } else {
                    return self.fail(a.span, "always without event control");
                }
            }
        };
        let pr = self.proc_id();
        if !edge {
            let ps = self.interpret(sc, body)?;
            return self.finish_comb(ps, pr, a.span);
        }
        let evs = events.expect("edge procedure");
        self.finish_edge(sc, evs, body, pr, a.span)
    }

    fn interpret(&mut self, sc: usize, body: &Stmt) -> R<PState> {
        self.ps = Some(PState::default());
        let r = self.exec(sc, body, B::T);
        let ps = self.ps.take().expect("state");
        r?;
        for s in &ps.nonblocking {
            if ps.blocking.contains(s) {
                return self.fail(
                    self.sigs[*s].span,
                    format!(
                        "{} assigned with blocking and nonblocking assignments",
                        self.sigs[*s].name
                    ),
                );
            }
        }
        Ok(ps)
    }

    fn final_value(ps: &PState, s: usize, bit: usize) -> Option<B> {
        ps.nba
            .get(&(true, s, bit))
            .or_else(|| ps.cur.get(&(true, s, bit)))
            .copied()
    }

    /// Whether bit `bit` of signal `s`, written under condition `w`, is written on every path
    /// (`Some(true)`), on none (`Some(false)`) or on some (`None`, a latch). A proof that runs
    /// out of its budget leaves the question open: a latch in its place would be cut free and
    /// could report a difference that does not exist (CHECK-REQ-0002).
    fn classify(&mut self, w: B, s: usize, bit: usize, sp: Span) -> R<Option<bool>> {
        let mut rng = Rng(0x5eed);
        if w == B::T {
            return Ok(Some(true));
        }
        if w == B::F {
            return Ok(Some(false));
        }
        let every = always(self.c, w, B::T, self.proof_budget, &mut rng)?;
        if every == Proof::Holds {
            return Ok(Some(true));
        }
        let nw = self.c.not(w);
        let none = always(self.c, nw, B::T, self.proof_budget, &mut rng)?;
        if none == Proof::Holds {
            return Ok(Some(false));
        }
        if every == Proof::Unknown || none == Proof::Unknown {
            return self.undecided(
                sp,
                format!(
                    "cannot decide whether {} is written on every path of the procedure: the RTL \
                     model's conflict budget of {} per proof ran out",
                    self.bit_name(s, bit),
                    self.proof_budget
                ),
            );
        }
        Ok(None)
    }

    fn finish_comb(&mut self, ps: PState, pr: usize, sp: Span) -> R<()> {
        let mut keys: Vec<(usize, usize)> = ps.wrote.keys().copied().collect();
        keys.sort_unstable();
        for (s, bit) in keys {
            let w = ps.wrote[&(s, bit)];
            let v = Self::final_value(&ps, s, bit).unwrap_or(B::F);
            match self.classify(w, s, bit, sp)? {
                Some(false) => {}
                Some(true) => {
                    // The bit's own earlier value is irrelevant when it is always written; it
                    // may only appear on paths that never execute (SYNTH-REQ-0212).
                    let own = self.sigs[s].vars[bit];
                    let cyc = |c: crate::circ::Cycle| {
                        format!("combinational cycle through variable {}", c.0)
                    };
                    let v0 = self.c.subst(v, own, B::F).map_err(cyc)?;
                    let v1 = self.c.subst(v, own, B::T).map_err(cyc)?;
                    let mut rng = Rng(0x10);
                    if v0 != v1 {
                        match equal(self.c, v0, v1, B::T, self.proof_budget, &mut rng)? {
                            Proof::Holds => {}
                            Proof::Unknown => {
                                return self.undecided(sp, format!("cannot decide whether {} depends on its own value: the RTL model's conflict budget of {} per proof ran out", self.bit_name(s, bit), self.proof_budget));
                            }
                            Proof::Fails(_) => {
                                return self.fail(sp, format!("combinational loop: {} depends on its own value (SYNTH-REQ-0214)", self.bit_name(s, bit)));
                            }
                        }
                    }
                    self.set(s, bit, v0, pr, sp)?
                }
                None => {
                    let q = self.c.free();
                    let name = self.bit_name(s, bit);
                    self.states.push(RtlState {
                        name,
                        latch: true,
                        q,
                        next: v,
                        clk: w,
                        arst: None,
                    });
                    self.set(s, bit, B::V(q), pr, sp)?;
                }
            }
        }
        Ok(())
    }

    fn finish_edge(
        &mut self,
        sc: usize,
        evs: &[ast::EventExpr],
        body: &Stmt,
        pr: usize,
        sp: Span,
    ) -> R<()> {
        let mut refs = HashSet::new();
        names_in(body, &mut refs);
        // Event signals: (edge, bit, referenced by the body).
        let mut ev = Vec::new();
        for e in evs {
            let base = {
                let mut x = &e.expr;
                loop {
                    match &x.kind {
                        ExprKind::Index(b, _)
                        | ExprKind::Member(b, _)
                        | ExprKind::Slice { base: b, .. } => x = b,
                        ExprKind::Ident(p) => break Some(p.name.name.clone()),
                        _ => break None,
                    }
                }
            };
            let s = self.size(sc, &e.expr)?;
            let v = self.ev(sc, &e.expr, &s, s.w, s.s)?;
            ev.push((e.edge, v[0], base.is_some_and(|n| refs.contains(&n))));
        }
        let clocks: Vec<usize> = (0..ev.len()).filter(|i| !ev[*i].2).collect();
        if clocks.len() != 1 {
            return self.fail(sp, "cannot identify the clock of the procedure");
        }
        let ci = clocks[0];
        let clk = if ev[ci].0 == Edge::Pos {
            ev[ci].1
        } else {
            self.c.not(ev[ci].1)
        };
        // Controls: forced variable, its active value, active-high condition.
        let mut ctrls = Vec::new();
        for (i, (edge, b, _)) in ev.iter().enumerate() {
            if i == ci {
                continue;
            }
            let active = *edge == Edge::Pos;
            let var = match b {
                B::V(v) => *v,
                _ => return self.fail(sp, "constant asynchronous control"),
            };
            let cond = if active { *b } else { self.c.not(*b) };
            ctrls.push((var, active, cond));
        }
        let full = self.interpret(sc, body)?;
        let mut bits: Vec<(usize, usize)> = full
            .wrote
            .iter()
            .filter(|(_, w)| **w != B::F)
            .map(|(k, _)| *k)
            .collect();
        bits.sort_unstable();
        // Values with each control active and the others inactive.
        let mut rng = Rng(0xa5a5);
        let mut actives: Vec<HashMap<(usize, usize), B>> = Vec::new();
        for k in 0..ctrls.len() {
            self.forced.clear();
            for (j, (v, a, _)) in ctrls.iter().enumerate() {
                self.forced.insert(*v, if j == k { *a } else { !*a });
            }
            let ps = self.interpret(sc, body);
            self.forced.clear();
            let ps = ps?;
            actives.push(
                bits.iter()
                    .map(|k| {
                        (
                            *k,
                            Self::final_value(&ps, k.0, k.1)
                                .unwrap_or_else(|| B::V(self.sigs[k.0].vars[k.1])),
                        )
                    })
                    .collect(),
            );
        }
        let mut arst: HashMap<(usize, usize), (usize, bool)> = HashMap::new();
        let mut used = vec![false; ctrls.len()];
        for key in &bits {
            let cur = B::V(self.sigs[key.0].vars[key.1]);
            for (k, act) in actives.iter().enumerate() {
                let v = act[key];
                // A proof that ran out of budget leaves the question open (CHECK-REQ-0002).
                let mut open = false;
                let konst = match v.konst() {
                    Some(x) => Some(x),
                    None => {
                        let one = always(self.c, v, B::T, self.proof_budget, &mut rng)?;
                        if one == Proof::Holds {
                            Some(true)
                        } else {
                            let nv = self.c.not(v);
                            let zero = always(self.c, nv, B::T, self.proof_budget, &mut rng)?;
                            open = one == Proof::Unknown || zero == Proof::Unknown;
                            (zero == Proof::Holds).then_some(false)
                        }
                    }
                };
                match konst {
                    Some(x) => {
                        if arst.contains_key(key) {
                            return self.fail(
                                sp,
                                format!(
                                    "{} has two asynchronous controls",
                                    self.bit_name(key.0, key.1)
                                ),
                            );
                        }
                        arst.insert(*key, (k, x));
                        used[k] = true;
                    }
                    None => match equal(self.c, v, cur, B::T, self.proof_budget, &mut rng)? {
                        Proof::Holds => {}
                        Proof::Fails(_) if !open => {
                            return self.fail(
                                sp,
                                format!(
                                    "asynchronous value of {} is not constant",
                                    self.bit_name(key.0, key.1)
                                ),
                            );
                        }
                        _ => {
                            return self.undecided(
                                sp,
                                format!(
                                    "cannot decide whether the asynchronous value of {} is \
                                     constant: the RTL model's conflict budget of {} per proof \
                                     ran out",
                                    self.bit_name(key.0, key.1),
                                    self.proof_budget
                                ),
                            );
                        }
                    },
                }
            }
        }
        if !bits.is_empty() && used.iter().any(|u| !u) {
            return self.fail(sp, "asynchronous control that controls no state bit");
        }
        // Synchronous next state: the bit's asynchronous control inactive.
        let mut sync: HashMap<(usize, usize), B> = HashMap::new();
        for k in 0..ctrls.len() {
            if !bits.iter().any(|b| arst.get(b).is_some_and(|x| x.0 == k)) {
                continue;
            }
            self.forced.clear();
            self.forced.insert(ctrls[k].0, !ctrls[k].1);
            let ps = self.interpret(sc, body);
            self.forced.clear();
            let ps = ps?;
            for b in &bits {
                if arst.get(b).is_some_and(|x| x.0 == k) {
                    sync.insert(
                        *b,
                        Self::final_value(&ps, b.0, b.1).unwrap_or(B::V(self.sigs[b.0].vars[b.1])),
                    );
                }
            }
        }
        for key in bits {
            let next = match sync.get(&key) {
                Some(v) => *v,
                None => Self::final_value(&full, key.0, key.1)
                    .unwrap_or(B::V(self.sigs[key.0].vars[key.1])),
            };
            let q = self.c.free();
            let name = self.bit_name(key.0, key.1);
            let a = arst.get(&key).map(|(k, v)| (ctrls[*k].2, *v));
            self.states.push(RtlState {
                name,
                latch: false,
                q,
                next,
                clk,
                arst: a,
            });
            self.set(key.0, key.1, B::V(q), pr, sp)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};

    use crate::circ::Circuit;
    use crate::rtl::{BuildError, RtlModel, RtlOptions, build_with};

    fn build(src: &str, budget: u64) -> Result<RtlModel, BuildError> {
        let mut sm = SourceMap::new();
        let (units, _) = read_texts(&mut sm, &[("m.sv", src)], PpOptions::default());
        let mut c = Circuit::new();
        build_with(&sm, &units, &mut c, &RtlOptions::default(), budget)
    }

    fn undecided(r: Result<RtlModel, BuildError>) -> String {
        match r {
            Err(BuildError::Undecided(m)) => m,
            other => panic!("{other:?}"),
        }
    }

    // CHECK-REQ-0002: a proof that the model needs and that runs out of its conflict budget
    // stops the build as undecided and names the budget; with enough budget it is decided.
    #[test]
    fn exhausted_model_proofs_are_undecided() {
        // Whether y depends on its own value is whether a * b and b * a can differ.
        let own = "module m(input logic [5:0] a, b, output logic y);
  always_comb if (a * b != b * a) y = y; else y = 1'b0;
endmodule";
        assert!(build(own, super::PROOF_BUDGET).is_ok());
        let m = undecided(build(own, 0));
        assert!(
            m.starts_with("m.sv:2:")
                && m.contains("y depends on its own value")
                && m.contains("conflict budget of 0 per proof"),
            "{m}"
        );
        // The asynchronous value (a ^ b) ^ (b ^ a) is the constant 0.
        let arst = "module m(input logic clk, r, a, b, d, output logic q);
  always_ff @(posedge clk or posedge r) if (r) q <= (a ^ b) ^ (b ^ a); else q <= d;
endmodule";
        assert_eq!(build(arst, super::PROOF_BUDGET).unwrap().states.len(), 1);
        let m = undecided(build(arst, 0));
        assert!(m.contains("asynchronous value of q"), "{m}");
        // Whether y is written on every path is whether a * b and b * a are always equal, and
        // whether the complete case over a ^ b matches. Decided, y is combinational; undecided,
        // it is not taken for a latch, which would be cut free and differ from the netlist.
        let every = "module m(input logic [5:0] a, b, input logic d, output logic y);
  always @* if (a * b == b * a) y = d;
endmodule";
        let case = "module m(input logic [3:0] a, b, input logic d, output logic y);
  always_comb case (a ^ b)
    4'd0, 4'd1, 4'd2, 4'd3, 4'd4, 4'd5, 4'd6, 4'd7: y = d;
    4'd8, 4'd9, 4'd10, 4'd11, 4'd12, 4'd13, 4'd14, 4'd15: y = !d;
  endcase
endmodule";
        for src in [every, case] {
            assert!(build(src, super::PROOF_BUDGET).unwrap().states.is_empty());
            let m = undecided(build(src, 0));
            assert!(
                m.starts_with("m.sv:2:")
                    && m.contains("whether y is written on every path")
                    && m.contains("conflict budget of 0 per proof"),
                "{m}"
            );
        }
        // A bit written on some paths only is still a latch.
        let latch = "module m(input logic en, d, output logic y);
  always_latch if (en) y = d;
endmodule";
        assert_eq!(build(latch, 0).unwrap().states.len(), 1);
        // A value that is not constant stays an error of the RTL.
        let load = "module m(input logic clk, r, a, d, output logic q);
  always_ff @(posedge clk or posedge r) if (r) q <= a; else q <= d;
endmodule";
        match build(load, super::PROOF_BUDGET) {
            Err(BuildError::Input(m)) => assert!(m.contains("is not constant"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
}
