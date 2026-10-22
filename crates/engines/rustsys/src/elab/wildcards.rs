// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Structural wildcard masks, independent of the two-state value lowering.
//! <=1M bits/result, depth <=64, <=8M copied bits/nodes per label (<16 MiB
//! transient masks). No graph paths or combinations are enumerated.

use rusteda_formats::verilog::ast::{CastTarget, Expr, ExprKind, TypeKind};
use rusteda_formats::verilog::number::Bit4;

use super::{EResult, Elaborator, ScopeId};

impl Elaborator<'_> {
    pub(crate) fn wildcard_bits(
        &mut self,
        scope: ScopeId,
        e: &Expr,
        context: (u32, bool),
        ignore: (bool, bool),
    ) -> EResult<Vec<bool>> {
        if !ignore.0 && !ignore.1 {
            return Ok(vec![false; context.0 as usize]);
        }
        self.wild_bits_inner(scope, e, context, ignore, &mut 8_000_000, 0)
    }

    #[allow(clippy::too_many_lines)]
    fn wild_bits_inner(
        &mut self,
        scope: ScopeId,
        e: &Expr,
        (w, cs): (u32, bool),
        ignore: (bool, bool),
        left: &mut usize,
        depth: usize,
    ) -> EResult<Vec<bool>> {
        let ty = self.packed_ty(scope, e)?;
        let own = ty.width() as usize;
        let cost = own.saturating_add(w as usize).saturating_add(1);
        if own > 1 << 20 || w > 1 << 20 || depth >= 64 || cost > *left {
            return Err(self.err(
                "S0130",
                e.span,
                "wildcard pattern memory/work budget reached",
            ));
        }
        *left -= cost;
        let wildcard = |b: Bit4| (b == Bit4::X && ignore.0) || (b == Bit4::Z && ignore.1);
        let mut mask = match &e.kind {
            ExprKind::Number(n) => n
                .bits()
                .map_err(|m| self.err("S0130", e.span, m))?
                .into_iter()
                .map(wildcard)
                .collect(),
            ExprKind::UnbasedUnsized(c) => {
                return Ok(vec![
                    (ignore.0 && *c == 'x') || (ignore.1 && *c == 'z');
                    w as usize
                ]);
            }
            ExprKind::Concat(items) | ExprKind::Repl(_, items) => {
                let mut one = Vec::new();
                for item in items.iter().rev() {
                    if let ExprKind::Repl(n, _) = &item.kind
                        && self.eval_const_int(scope, n)? == 0
                    {
                        continue;
                    }
                    let t = self.packed_ty(scope, item)?;
                    let part = self.wild_bits_inner(
                        scope,
                        item,
                        (t.width(), t.is_signed()),
                        ignore,
                        left,
                        depth + 1,
                    )?;
                    one.extend(part);
                    if one.len() > 1 << 20 {
                        return Err(self.err(
                            "S0130",
                            e.span,
                            "wildcard concatenation width budget reached",
                        ));
                    }
                }
                if matches!(&e.kind, ExprKind::Repl(..)) {
                    if one.is_empty() || !own.is_multiple_of(one.len()) {
                        return Err(self.err("S0130", e.span, "invalid wildcard replication"));
                    }
                    let mut result = Vec::with_capacity(own);
                    for _ in 0..own / one.len() {
                        result.extend_from_slice(&one);
                    }
                    result
                } else {
                    one
                }
            }
            ExprKind::Cast { target, expr } => {
                let inner = self.packed_ty(scope, expr)?;
                let mut m = self.wild_bits_inner(
                    scope,
                    expr,
                    (own as u32, inner.is_signed()),
                    ignore,
                    left,
                    depth + 1,
                )?;
                if matches!(target,CastTarget::Type(t) if matches!(&t.kind,TypeKind::Bit)) {
                    m.fill(false);
                }
                m
            }
            ExprKind::SysCall { name, args }
                if matches!(name.as_str(), "$signed" | "$unsigned") && args.len() == 1 =>
            {
                let inner = self.packed_ty(scope, &args[0])?;
                self.wild_bits_inner(
                    scope,
                    &args[0],
                    (own as u32, inner.is_signed()),
                    ignore,
                    left,
                    depth + 1,
                )?
            }
            ExprKind::Call { .. } | ExprKind::Pattern { .. } => {
                return Err(self.err(
                    "S0130",
                    e.span,
                    "function/assignment-pattern wildcard labels are not supported",
                ));
            }
            _ => {
                let children: Vec<&Expr> = match &e.kind {
                    ExprKind::Unary(_, a) | ExprKind::Member(a, _) => vec![a],
                    ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Range(a, b) => {
                        vec![a, b]
                    }
                    ExprKind::Cond(a, b, c) => vec![a, b, c],
                    ExprKind::Slice {
                        base, left, right, ..
                    } => vec![base, left, right],
                    ExprKind::SysCall { args, .. } => args.iter().collect(),
                    _ => vec![],
                };
                for child in children {
                    let t = self.packed_ty(scope, child)?;
                    if self
                        .wild_bits_inner(
                            scope,
                            child,
                            (t.width(), t.is_signed()),
                            ignore,
                            left,
                            depth + 1,
                        )?
                        .iter()
                        .any(|x| *x)
                    {
                        return Err(self.err(
                            "S0130",
                            e.span,
                            "wildcard literals in this label operation are not supported",
                        ));
                    }
                }
                vec![false; own]
            }
        };
        let fill = cs && ty.is_signed() && mask.last().copied().unwrap_or(false);
        mask.resize(w as usize, fill);
        Ok(mask)
    }
}
