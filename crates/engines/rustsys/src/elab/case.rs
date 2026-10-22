// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Case statements: item matching and the merge of item paths (SYNTH-REQ-0220).

use rusteda_formats::verilog::ast::{self, CaseKind, Expr, ExprKind, Stmt};

use super::{EResult, Elaborator, ScopeId};
use crate::aig::Lit;
use crate::bits::{self, Bv};

/// A constant case label: the value of every compared bit, `None` for wildcard bits.
pub(crate) type Pattern = Vec<Option<bool>>;

/// True when some value matches both patterns.
fn overlaps(a: &Pattern, b: &Pattern) -> bool {
    a.iter()
        .zip(b)
        .all(|(x, y)| x.is_none() || y.is_none() || x == y)
}

impl<'a> Elaborator<'a> {
    /// Condition that a case label matches (SYNTH-REQ-0201 wildcard rules).
    pub(crate) fn case_match(
        &mut self,
        scope: ScopeId,
        kind: CaseKind,
        inside: bool,
        sel: (&Bv, u32, bool),
        label: &Expr,
        all: (u32, bool),
    ) -> EResult<(Lit, Option<Pattern>)> {
        if inside {
            if let ExprKind::Range(lo, hi) = &label.kind {
                let (cw, cs) = all;
                let x = sel.0.clone();
                let l = self.eval(scope, lo, cw, cs)?;
                let h = self.eval(scope, hi, cw, cs)?;
                let ge = !bits::lt(&mut self.aig, &x, &l, cs);
                let le = !bits::lt(&mut self.aig, &h, &x, cs);
                return Ok((self.aig.and(ge, le), None));
            }
        }
        let (cw, cs) = all;
        let x = bits::resize(sel.0, cw as usize, sel.2 && cs);
        // Labels of casez/casex and inside ranges have wildcard bits (SYNTH-REQ-0201).
        let wild_ctx = inside || kind != CaseKind::Case;
        self.wildcard += u32::from(wild_ctx);
        let y = self.eval(scope, label, cw, cs);
        self.wildcard -= u32::from(wild_ctx);
        let y = y?;
        let wild = self.wildcard_bits(
            scope,
            label,
            (cw, cs),
            (
                inside || kind == CaseKind::Casex,
                inside || kind != CaseKind::Case,
            ),
        )?;
        let mut terms = Vec::new();
        for i in 0..cw as usize {
            if !wild[i] {
                terms.push(self.aig.xnor(x[i], y[i]));
            }
        }
        // A constant label is a pattern: the value of every compared bit.
        let pat: Option<Pattern> = (0..cw as usize)
            .map(|i| {
                if wild[i] {
                    Some(None)
                } else {
                    y[i].const_value().map(Some)
                }
            })
            .collect();
        Ok((bits::red_and(&mut self.aig, &terms), pat))
    }

    /// A `case` whose item conditions are mutually exclusive: every branch runs under its own
    /// condition and the results merge in parallel form (SYNTH-REQ-0220).
    pub(crate) fn exec_case_parallel(
        &mut self,
        scope: ScopeId,
        conds: &[(Lit, &Stmt)],
        default: Option<&Stmt>,
    ) -> EResult<()> {
        let base = self.env.take().expect("procedural context");
        let cs: Vec<Lit> = conds.iter().map(|(c, _)| *c).collect();
        let mut envs = Vec::with_capacity(conds.len());
        for (c, body) in conds {
            let mut be = base.clone();
            be.live = self.aig.and(base.live, *c);
            self.env = Some(be);
            self.exec(scope, body)?;
            envs.push(self.env.take().expect("env"));
        }
        let taken = bits::red_or(&mut self.aig, &cs);
        let mut de = base;
        de.live = self.aig.and(de.live, !taken);
        self.env = Some(de);
        if let Some(d) = default {
            self.exec(scope, d)?;
        }
        let de = self.env.take().expect("env");
        self.env = Some(self.merge_parallel(&cs, envs, de));
        Ok(())
    }

    /// A case under the parallel-case assumption (SYNTH-REQ-0225): every item runs from the
    /// state before the statement under its own condition; the results merge as
    /// `OR_i (c_i & x_i) | (!OR_i c_i & x_d)`.
    fn exec_case_assumed_parallel(
        &mut self,
        scope: ScopeId,
        conds: &[(Lit, &Stmt)],
        default: Option<&Stmt>,
    ) -> EResult<()> {
        let base = self.env.take().expect("procedural context");
        let cs: Vec<Lit> = conds.iter().map(|(c, _)| *c).collect();
        let mut envs = Vec::with_capacity(conds.len());
        for (c, body) in conds {
            let mut be = base.clone();
            be.live = self.aig.and(base.live, *c);
            self.env = Some(be);
            self.exec(scope, body)?;
            envs.push(self.env.take().expect("env"));
        }
        let taken = bits::red_or(&mut self.aig, &cs);
        let mut de = base;
        de.live = self.aig.and(de.live, !taken);
        self.env = Some(de);
        if let Some(d) = default {
            self.exec(scope, d)?;
        }
        let de = self.env.take().expect("env");
        self.env = Some(self.merge_or(&cs, envs, de));
        Ok(())
    }

    pub(crate) fn exec_case(
        &mut self,
        scope: ScopeId,
        kind: CaseKind,
        inside: bool,
        expr: &Expr,
        items: &[ast::CaseItem],
        (full, parallel_marked): (bool, bool),
    ) -> EResult<()> {
        let st = self.packed_ty(scope, expr)?;
        // The case expression and all items are context-determined at the widest width
        // (SYNTH-REQ-0220).
        let mut cw = st.width();
        let mut cs = st.is_signed();
        for it in items {
            for l in &it.labels {
                let parts: Vec<&Expr> = match &l.kind {
                    ExprKind::Range(lo, hi) => vec![lo, hi],
                    _ => vec![l],
                };
                for p in parts {
                    let t = self.packed_ty(scope, p)?;
                    cw = cw.max(t.width());
                    cs = cs && t.is_signed();
                }
            }
        }
        let sel = self.eval(scope, expr, cw, cs)?;
        // Conditions in priority order; the default item is taken when none matches. Items whose
        // labels are all constant patterns that no other item's labels overlap are mutually
        // exclusive and merge in parallel form; a label equal to an earlier one never matches
        // first and is dropped.
        let mut conds: Vec<(Lit, &Stmt)> = Vec::new();
        let mut default: Option<&Stmt> = None;
        let mut earlier: Vec<Pattern> = Vec::new();
        let mut parallel = true;
        // A case marked complete without a default item takes its last item as the default
        // under the complete-case assumption (SYNTH-REQ-0224).
        let last_as_default =
            (self.assume_full_case && full && items.iter().all(|it| !it.labels.is_empty()))
                .then(|| items.len().checked_sub(1))
                .flatten();
        // A case marked parallel runs every item under its own match condition and ORs the
        // results under the parallel-case assumption (SYNTH-REQ-0225).
        if self.assume_parallel_case && parallel_marked {
            let mut conds: Vec<(Lit, &Stmt)> = Vec::new();
            let mut default: Option<&Stmt> = None;
            // Items whose labels are constant patterns that no other item's labels overlap are
            // mutually exclusive: the OR of the matching results is then the one result, and
            // the parallel merge of exclusive items computes it with less logic.
            let mut exclusive = true;
            let mut seen: Vec<Pattern> = Vec::new();
            for (i, it) in items.iter().enumerate() {
                if it.labels.is_empty() || Some(i) == last_as_default {
                    default = Some(&it.body);
                    continue;
                }
                let mut any = Lit::FALSE;
                let mut pats: Vec<Pattern> = Vec::new();
                for l in &it.labels {
                    let (m, pat) =
                        self.case_match(scope, kind, inside, (&sel, cw, cs), l, (cw, cs))?;
                    match pat {
                        Some(p) => pats.push(p),
                        None => exclusive = false,
                    }
                    any = self.aig.or(any, m);
                }
                if pats.iter().any(|p| seen.iter().any(|q| overlaps(p, q))) {
                    exclusive = false;
                }
                seen.extend(pats);
                conds.push((any, &it.body));
            }
            if exclusive {
                return self.exec_case_parallel(scope, &conds, default);
            }
            return self.exec_case_assumed_parallel(scope, &conds, default);
        }
        for (i, it) in items.iter().enumerate() {
            if it.labels.is_empty() || Some(i) == last_as_default {
                default = Some(&it.body);
                continue;
            }
            let mut any = Lit::FALSE;
            let mut pats: Vec<Pattern> = Vec::new();
            for l in &it.labels {
                let (m, pat) = self.case_match(scope, kind, inside, (&sel, cw, cs), l, (cw, cs))?;
                match pat {
                    Some(p) if earlier.contains(&p) || pats.contains(&p) => continue,
                    Some(p) => pats.push(p),
                    None => parallel = false,
                }
                any = self.aig.or(any, m);
            }
            if any == Lit::FALSE {
                continue;
            }
            if pats.iter().any(|p| earlier.iter().any(|q| overlaps(p, q))) {
                parallel = false;
            }
            earlier.extend(pats);
            let stop = any == Lit::TRUE;
            conds.push((any, &it.body));
            if stop {
                break;
            }
        }
        if parallel && conds.len() >= 2 {
            return self.exec_case_parallel(scope, &conds, default);
        }
        let base = self.env.take().expect("procedural context");
        let mut branch_envs = Vec::with_capacity(conds.len());
        // Branch environments carry their exact (priority-exclusive) path conditions.
        let mut taken = Lit::FALSE;
        for (c, body) in &conds {
            let excl = self.aig.and(*c, !taken);
            taken = self.aig.or(taken, *c);
            let mut be = base.clone();
            be.live = self.aig.and(base.live, excl);
            self.env = Some(be);
            self.exec(scope, body)?;
            branch_envs.push(self.env.take().expect("env"));
        }
        let all_covered = conds.last().is_some_and(|(c, _)| *c == Lit::TRUE);
        let mut de = base;
        de.live = self.aig.and(de.live, !taken);
        self.env = Some(de);
        if !all_covered {
            if let Some(d) = default {
                self.exec(scope, d)?;
            }
        }
        let mut acc = self.env.take().expect("env");
        for ((c, _), env) in conds.iter().zip(branch_envs).rev() {
            acc = self.merge_env(*c, env, acc);
        }
        self.env = Some(acc);
        Ok(())
    }
}
