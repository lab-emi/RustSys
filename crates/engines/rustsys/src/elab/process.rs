// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Procedures, register and latch inference (SYNTH-REQ-0212, SYNTH-REQ-0301 to -0306) and
//! subroutine calls (SYNTH-REQ-0132).

use rustc_hash::FxHashSet;
use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{
    self, Always, AlwaysKind, Arg, BlockDecl, Edge, Expr, ExprKind, Stmt, StmtKind, TimingCtrl,
};

use super::{EResult, Elaborator, Env, ProcId, ScopeId, SigId, StateRec, VarRef};
use crate::aig::Lit;
use crate::bits::Bv;

/// Conflict limit of the proofs used for classification.
const PROOF_LIMIT: u64 = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcClass {
    Comb,
    Latch,
    Edge,
}

/// Collects simple identifier names referenced in a statement.
fn collect_names_stmt(s: &Stmt, out: &mut FxHashSet<String>) {
    match &s.kind {
        StmtKind::Block { stmts, decls, .. } => {
            for d in decls {
                if let BlockDecl::Data(dd) = d {
                    for dc in &dd.decls {
                        if let Some(i) = &dc.init {
                            collect_names_expr(i, out);
                        }
                    }
                }
            }
            for st in stmts {
                collect_names_stmt(st, out);
            }
        }
        StmtKind::Assign { lhs, rhs, .. } => {
            collect_names_expr(lhs, out);
            collect_names_expr(rhs, out);
        }
        StmtKind::IncDec { target, .. } => collect_names_expr(target, out),
        StmtKind::If {
            cond, then, els, ..
        } => {
            collect_names_expr(cond, out);
            collect_names_stmt(then, out);
            if let Some(e) = els {
                collect_names_stmt(e, out);
            }
        }
        StmtKind::Case { expr, items, .. } => {
            collect_names_expr(expr, out);
            for it in items {
                for l in &it.labels {
                    collect_names_expr(l, out);
                }
                collect_names_stmt(&it.body, out);
            }
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            for i in init {
                collect_names_expr(&i.rhs, out);
            }
            if let Some(c) = cond {
                collect_names_expr(c, out);
            }
            for st in step {
                collect_names_stmt(st, out);
            }
            collect_names_stmt(body, out);
        }
        StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
            collect_names_expr(cond, out);
            collect_names_stmt(body, out);
        }
        StmtKind::Repeat { count, body } => {
            collect_names_expr(count, out);
            collect_names_stmt(body, out);
        }
        StmtKind::Forever(b) | StmtKind::Timed { body: b, .. } => collect_names_stmt(b, out),
        StmtKind::Foreach { array, body, .. } => {
            collect_names_expr(array, out);
            collect_names_stmt(body, out);
        }
        StmtKind::Return(Some(e)) | StmtKind::Call(e) => collect_names_expr(e, out),
        _ => {}
    }
}

fn collect_names_expr(e: &Expr, out: &mut FxHashSet<String>) {
    match &e.kind {
        ExprKind::Ident(p) => {
            out.insert(p.name.name.clone());
        }
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) => {
            collect_names_expr(a, out);
            collect_names_expr(b, out);
        }
        ExprKind::Slice {
            base, left, right, ..
        } => {
            collect_names_expr(base, out);
            collect_names_expr(left, out);
            collect_names_expr(right, out);
        }
        ExprKind::Member(a, _) | ExprKind::Unary(_, a) => collect_names_expr(a, out),
        ExprKind::Cond(a, b, c) => {
            collect_names_expr(a, out);
            collect_names_expr(b, out);
            collect_names_expr(c, out);
        }
        ExprKind::Inside(a, items) => {
            collect_names_expr(a, out);
            for i in items {
                collect_names_expr(i, out);
            }
        }
        ExprKind::Concat(items) | ExprKind::SysCall { args: items, .. } => {
            for i in items {
                collect_names_expr(i, out);
            }
        }
        ExprKind::Repl(n, items) => {
            collect_names_expr(n, out);
            for i in items {
                collect_names_expr(i, out);
            }
        }
        ExprKind::Pattern { items, .. } => {
            for it in items {
                match it {
                    ast::PatternItem::Positional(x) | ast::PatternItem::Keyed(_, x) => {
                        collect_names_expr(x, out)
                    }
                }
            }
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                match a {
                    Arg::Positional(Some(x)) | Arg::Named(_, Some(x)) => collect_names_expr(x, out),
                    _ => {}
                }
            }
        }
        ExprKind::Cast { expr, .. } => collect_names_expr(expr, out),
        _ => {}
    }
}

/// Base identifier name of an event expression.
fn base_name(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Ident(p) => Some(&p.name.name),
        ExprKind::Index(b, _) | ExprKind::Member(b, _) | ExprKind::Slice { base: b, .. } => {
            base_name(b)
        }
        _ => None,
    }
}

impl<'a> Elaborator<'a> {
    /// Elaborates an always procedure.
    pub(crate) fn elab_always(&mut self, scope: ScopeId, a: &'a Always) -> EResult<()> {
        let (ctrl, body) = match &a.body.kind {
            StmtKind::Timed { ctrl, body } if !matches!(ctrl, TimingCtrl::Delay) => {
                (Some(ctrl), &**body)
            }
            _ => (None, &a.body),
        };
        let class = match (a.kind, ctrl) {
            (AlwaysKind::Comb, None) => ProcClass::Comb,
            (AlwaysKind::Latch, None) => ProcClass::Latch,
            (AlwaysKind::Comb | AlwaysKind::Latch, Some(_)) => {
                return Err(self.err(
                    "S0240",
                    a.span,
                    "always_comb and always_latch cannot have an event control",
                ));
            }
            (AlwaysKind::Always | AlwaysKind::Ff, Some(TimingCtrl::Events(evs))) => {
                let edges = evs
                    .iter()
                    .filter(|e| matches!(e.edge, Edge::Pos | Edge::Neg))
                    .count();
                if edges == evs.len() {
                    ProcClass::Edge
                } else if edges == 0 && a.kind == AlwaysKind::Always {
                    if evs.iter().any(|e| e.edge == Edge::Both) {
                        return Err(self.err(
                            "S0241",
                            a.span,
                            "'edge' events cannot be synthesized",
                        ));
                    }
                    ProcClass::Comb
                } else {
                    return Err(self.err(
                        "S0242",
                        a.span,
                        "event control mixes edge and level events",
                    ));
                }
            }
            (AlwaysKind::Always, Some(TimingCtrl::Star)) => ProcClass::Comb,
            (AlwaysKind::Ff, _) => {
                return Err(self.err("S0243", a.span, "always_ff needs an edge event control"));
            }
            _ => {
                return Err(self.err(
                    "S0244",
                    a.span,
                    "always procedure without an event control cannot be synthesized",
                ));
            }
        };
        let what = match class {
            ProcClass::Comb => "combinational procedure",
            ProcClass::Latch => "latch procedure",
            ProcClass::Edge => "edge procedure",
        };
        let proc = self.new_proc(what, a.span);
        self.env = Some(Env::new());
        self.exits.clear();
        let r = self.exec(scope, body);
        let env = self.env.take().expect("env");
        r?;
        match class {
            ProcClass::Comb | ProcClass::Latch => {
                self.finish_comb(env, proc, class == ProcClass::Latch, a)
            }
            ProcClass::Edge => {
                let Some(TimingCtrl::Events(evs)) = ctrl else {
                    unreachable!("edge procedure")
                };
                self.finish_edge(scope, env, evs, body, proc, a.span)
            }
        }
    }

    /// Signal elements written by a procedure: key, value, written mask. Signals written by both
    /// blocking and nonblocking assignments are rejected (SYNTH-REQ-0211).
    fn written(&mut self, env: &Env) -> EResult<Vec<((SigId, u32), Bv, Bv)>> {
        for (s, sp) in &env.nba_sigs {
            if env.blocking_sigs.contains_key(s) {
                let name = self.signals[*s as usize].name.clone();
                return Err(self.err(
                    "S0245",
                    *sp,
                    format!("'{name}' is assigned with both blocking and nonblocking assignments"),
                ));
            }
        }
        let mut out = Vec::new();
        let mut keys: Vec<_> = env.assigned.keys().copied().collect();
        keys.sort_unstable_by_key(|k| match k.0 {
            VarRef::Sig(s) => (s, k.1),
            VarRef::Local(l) => (u32::MAX - l, k.1),
        });
        for k in keys {
            if let VarRef::Sig(s) = k.0 {
                let v = env
                    .vals
                    .get(&k)
                    .cloned()
                    .unwrap_or_else(|| self.initial_value(k));
                out.push(((s, k.1), v, env.assigned[&k].clone()));
            }
        }
        let mut keys: Vec<_> = env.nba.keys().copied().collect();
        keys.sort_unstable_by_key(|k| match k.0 {
            VarRef::Sig(s) => (s, k.1),
            VarRef::Local(l) => (u32::MAX - l, k.1),
        });
        for k in keys {
            if let VarRef::Sig(s) = k.0 {
                let (v, m) = env.nba[&k].clone();
                out.push(((s, k.1), v, m));
            }
        }
        Ok(out)
    }

    /// Write condition class (SYNTH-REQ-0212): `Some(true)` always, `Some(false)` never, `None`
    /// sometimes.
    fn write_class(&self, mk: Lit) -> Option<bool> {
        if let Some(c) = mk.const_value() {
            return Some(c);
        }
        if crate::sat::prove_const(&self.aig, mk, true, PROOF_LIMIT) == Some(true) {
            return Some(true);
        }
        if crate::sat::prove_const(&self.aig, mk, false, PROOF_LIMIT) == Some(true) {
            return Some(false);
        }
        None
    }

    /// Value of an always-written bit without its (unreachable) dependence on its own earlier
    /// value; a real dependence is a combinational loop (SYNTH-REQ-0212, SYNTH-REQ-0214).
    fn drop_self(&mut self, sig: SigId, flat: u32, v: Lit, span: Span) -> EResult<Lit> {
        let own = self.signals[sig as usize].ph[flat as usize].node();
        if !self.aig.depends_on(v, own) {
            return Ok(v);
        }
        let v0 = self.aig.cofactor(&[v], &[(own, false)])[0];
        let v1 = self.aig.cofactor(&[v], &[(own, true)])[0];
        if v0 == v1 || crate::sat::prove_equal(&self.aig, v0, v1, PROOF_LIMIT) == Some(true) {
            return Ok(v0);
        }
        let ew = self.signals[sig as usize].elem_width;
        let name = self.state_name(sig, flat / ew, flat % ew);
        Err(self.err(
            "S0214",
            span,
            format!("combinational loop: '{name}' depends on its own value (SYNTH-REQ-0214)"),
        ))
    }

    /// Input literals whose conjunction is `w`, if `w` is such a product.
    fn cube_of(&self, w: Lit) -> Option<Vec<(u32, bool)>> {
        let mut out = Vec::new();
        let mut stack = vec![w];
        while let Some(l) = stack.pop() {
            if self.aig.is_input(l.node()) {
                out.push((l.node(), !l.is_compl()));
            } else if self.aig.is_and(l.node()) && !l.is_compl() {
                let (a, b) = self.aig.fanins(l.node());
                stack.push(a);
                stack.push(b);
            } else {
                return None;
            }
            if out.len() > 16 {
                return None;
            }
        }
        Some(out)
    }

    /// Data of a latch bit with enable `w`: only its value while `w` is 1 matters
    /// (SYNTH-REQ-0305). When `w` is a product of input literals, the data is cofactored on that
    /// product; otherwise the hold path through the latch's own value is dropped when it provably
    /// does not matter while `w` is 1.
    fn latch_data(&mut self, sig: SigId, flat: u32, v: Lit, w: Lit) -> Lit {
        let own = self.signals[sig as usize].ph[flat as usize].node();
        let v = match self.cube_of(w) {
            Some(cube) if !cube.iter().any(|(n, _)| *n == own) => self.aig.cofactor(&[v], &cube)[0],
            _ => v,
        };
        if !self.aig.depends_on(v, own) {
            return v;
        }
        let v0 = self.aig.cofactor(&[v], &[(own, false)])[0];
        let v1 = self.aig.cofactor(&[v], &[(own, true)])[0];
        let x = self.aig.xor(v0, v1);
        let diff = self.aig.and(x, w);
        if diff == Lit::FALSE
            || crate::sat::prove_const(&self.aig, diff, false, PROOF_LIMIT) == Some(true)
        {
            v0
        } else {
            v
        }
    }

    fn finish_comb(&mut self, env: Env, proc: ProcId, latch_proc: bool, a: &Always) -> EResult<()> {
        let written = self.written(&env)?;
        let mut latched: Vec<SigId> = Vec::new();
        let mut comb_in_latch = false;
        for ((sig, elem), v, m) in written {
            let ew = self.signals[sig as usize].elem_width;
            for b in 0..ew {
                let mk = m[b as usize];
                let class = self.write_class(mk);
                if class == Some(false) {
                    continue;
                }
                let flat = elem * ew + b;
                if class == Some(true) {
                    comb_in_latch |= latch_proc;
                    let d = self.drop_self(sig, flat, v[b as usize], a.span)?;
                    self.drive(sig, flat, d, proc, a.span)?;
                } else {
                    let q = self.aig.input();
                    let d = self.latch_data(sig, flat, v[b as usize], mk);
                    self.states.push(StateRec {
                        sig,
                        elem,
                        bit: b,
                        latch: true,
                        q,
                        d,
                        clk: mk,
                        arst: None,
                        span: a.span,
                    });
                    self.drive(sig, flat, q, proc, a.span)?;
                    if !latched.contains(&sig) {
                        latched.push(sig);
                    }
                }
            }
        }
        if !latch_proc {
            for s in latched {
                let name = self.signals[s as usize].name.clone();
                self.warn(
                    "S0305",
                    a.span,
                    format!("latch inferred for '{name}' (not assigned on every path)"),
                );
            }
        } else if comb_in_latch {
            self.warn(
                "S0306",
                a.span,
                "always_latch procedure assigns some bits on every path; they are combinational",
            );
        }
        Ok(())
    }

    fn finish_edge(
        &mut self,
        scope: ScopeId,
        env: Env,
        evs: &[ast::EventExpr],
        body: &Stmt,
        proc: ProcId,
        span: Span,
    ) -> EResult<()> {
        let mut referenced = FxHashSet::default();
        collect_names_stmt(body, &mut referenced);
        // Event literals.
        let mut events = Vec::new();
        self.env = None;
        for ev in evs {
            if ev.iff.is_some() {
                return Err(self.err(
                    "S0246",
                    ev.expr.span,
                    "'iff' in event controls is not supported",
                ));
            }
            let (v, _) = self.eval_self(scope, &ev.expr)?;
            if v.len() != 1 {
                self.warn(
                    "S0247",
                    ev.expr.span,
                    "event expression is wider than one bit; its least significant bit is used",
                );
            }
            let l = v[0];
            let used = base_name(&ev.expr).is_some_and(|n| referenced.contains(n));
            events.push((ev.edge, l, used, ev.expr.span));
        }
        let clocks: Vec<usize> = (0..events.len()).filter(|i| !events[*i].2).collect();
        if clocks.len() != 1 {
            return Err(self.err(
                "S0301",
                span,
                "cannot identify the clock: exactly one event must not be referenced in the procedure body (SYNTH-REQ-0302)",
            ));
        }
        let ci = clocks[0];
        let clk = if events[ci].0 == Edge::Pos {
            events[ci].1
        } else {
            !events[ci].1
        };
        // Asynchronous controls: (node, node value when active, active-high condition).
        let mut controls = Vec::new();
        for (i, (edge, l, _, sp)) in events.iter().enumerate() {
            if i == ci {
                continue;
            }
            if l.is_const() {
                return Err(self.err("S0302", *sp, "asynchronous control is a constant"));
            }
            let active_level = *edge == Edge::Pos;
            let node_val = active_level ^ l.is_compl();
            let cond = if active_level { *l } else { !*l };
            controls.push((l.node(), node_val, cond, *sp));
        }
        if controls.len() > 8 {
            return Err(self.err("S0302", span, "too many asynchronous controls"));
        }
        let written = self.written(&env)?;
        // Flatten state bits.
        let mut bitsv = Vec::new();
        for ((sig, elem), v, m) in &written {
            let ew = self.signals[*sig as usize].elem_width;
            for b in 0..ew {
                if m[b as usize] != Lit::FALSE {
                    bitsv.push((*sig, *elem, b, v[b as usize]));
                }
            }
        }
        let roots: Vec<Lit> = bitsv.iter().map(|x| x.3).collect();
        // Value of every bit with control k active and the others inactive (SYNTH-REQ-0303).
        let inactive: Vec<(u32, bool)> = controls.iter().map(|c| (c.0, !c.1)).collect();
        let mut actives = Vec::new();
        for k in 0..controls.len() {
            let mut fix = inactive.clone();
            fix[k].1 = controls[k].1;
            actives.push(self.aig.cofactor(&roots, &fix));
        }
        // Per bit: its asynchronous control (if any); controls under which it holds stay in the
        // synchronous function.
        let mut used = vec![false; controls.len()];
        let mut arsts: Vec<Option<usize>> = Vec::with_capacity(bitsv.len());
        for (i, (sig, elem, b, _)) in bitsv.iter().enumerate() {
            let ew = self.signals[*sig as usize].elem_width;
            let current = self.signals[*sig as usize].ph[(elem * ew + b) as usize];
            let mut ctrl = None;
            for k in 0..controls.len() {
                let mut v = actives[k][i];
                // Semantic constants count as constants (SYNTH-REQ-0303).
                if !v.is_const() {
                    if crate::sat::prove_const(&self.aig, v, true, PROOF_LIMIT) == Some(true) {
                        v = Lit::TRUE;
                        actives[k][i] = v;
                    } else if crate::sat::prove_const(&self.aig, v, false, PROOF_LIMIT)
                        == Some(true)
                    {
                        v = Lit::FALSE;
                        actives[k][i] = v;
                    }
                }
                let holds = !v.is_const()
                    && (v == current
                        || crate::sat::prove_equal(&self.aig, v, current, PROOF_LIMIT)
                            == Some(true));
                if v.is_const() {
                    if ctrl.is_some() {
                        let name = self.state_name(*sig, *elem, *b);
                        return Err(self.err("S0304", span, format!("state bit '{name}' has more than one asynchronous control (SYNTH-REQ-0304)")));
                    }
                    ctrl = Some(k);
                    used[k] = true;
                } else if !holds {
                    let name = self.state_name(*sig, *elem, *b);
                    return Err(self.err(
                        "S0303",
                        span,
                        format!("asynchronous value of '{name}' is not constant (SYNTH-REQ-0303)"),
                    ));
                }
            }
            arsts.push(ctrl);
        }
        // Synchronous next state: the bit's own asynchronous control inactive.
        let mut dsync = roots.clone();
        for k in 0..controls.len() {
            let idx: Vec<usize> = (0..bitsv.len()).filter(|i| arsts[*i] == Some(k)).collect();
            if idx.is_empty() {
                continue;
            }
            let rs: Vec<Lit> = idx.iter().map(|i| roots[*i]).collect();
            let cof = self.aig.cofactor(&rs, &[inactive[k]]);
            for (j, i) in idx.iter().enumerate() {
                dsync[*i] = cof[j];
            }
        }
        for (i, (sig, elem, b, _)) in bitsv.iter().enumerate() {
            let arst = arsts[i].map(|k| {
                (
                    controls[k].2,
                    actives[k][i].const_value().expect("constant"),
                )
            });
            let q = self.aig.input();
            self.states.push(StateRec {
                sig: *sig,
                elem: *elem,
                bit: *b,
                latch: false,
                q,
                d: dsync[i],
                clk,
                arst,
                span,
            });
            let ew = self.signals[*sig as usize].elem_width;
            self.drive(*sig, elem * ew + b, q, proc, span)?;
        }
        for (k, u) in used.iter().enumerate() {
            if !u && !bitsv.is_empty() {
                return Err(self.err(
                    "S0305",
                    controls[k].3,
                    "asynchronous control is not tested directly by the procedure body (SYNTH-REQ-0302)",
                ));
            }
        }
        Ok(())
    }
}
