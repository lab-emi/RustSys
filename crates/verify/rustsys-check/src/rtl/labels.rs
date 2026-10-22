// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Preserve four-state label structure before constructing comparison masks.
//! A label is capped at 1M bits, depth 64 and 8M total copied bits/nodes;
//! transient storage <16 MiB. This does not use synthesis's mask evaluator.

use super::size::Sz;
use super::{Builder, R};
use rusteda_formats::verilog::ast::{CastTarget, Expr, ExprKind, TypeKind};
use rusteda_formats::verilog::number::Bit4;

impl Builder<'_> {
    pub(crate) fn label_bits(
        &mut self,
        sc: usize,
        e: &Expr,
        sz: &Sz,
        w: u32,
        signed: bool,
    ) -> R<Vec<Bit4>> {
        self.label_value(sc, e, sz, (w, signed), 0, &mut 8_000_000)
    }

    #[allow(clippy::too_many_lines)]
    fn label_value(
        &mut self,
        sc: usize,
        e: &Expr,
        sz: &Sz,
        (width, signed): (u32, bool),
        depth: usize,
        left: &mut usize,
    ) -> R<Vec<Bit4>> {
        let work = sz.w as usize + width as usize + 1;
        if width > 1 << 20 || sz.w > 1 << 20 || depth >= 64 || work > *left {
            return self.fail(e.span, "wildcard label memory/work budget reached");
        }
        *left -= work;
        if width == 0 {
            return Ok(Vec::new());
        }
        let mut raw = match &e.kind {
            ExprKind::Number(n) => n.bits().map_err(|m| format!("{}: {m}", self.loc(e.span)))?,
            ExprKind::UnbasedUnsized(ch) => {
                return Ok(vec![
                    match ch {
                        'x' => Bit4::X,
                        'z' => Bit4::Z,
                        '1' => Bit4::One,
                        _ => Bit4::Zero,
                    };
                    width as usize
                ]);
            }
            ExprKind::Concat(items) | ExprKind::Repl(_, items) => {
                let mut unit = Vec::new();
                for (part, ps) in items.iter().zip(&sz.k).rev() {
                    if ps.w == 0 {
                        continue;
                    }
                    unit.extend(self.label_value(sc, part, ps, (ps.w, ps.s), depth + 1, left)?);
                    if unit.len() > 1 << 20 {
                        return self.fail(e.span, "wildcard concatenation width budget reached");
                    }
                }
                if matches!(&e.kind, ExprKind::Repl(..)) {
                    let count = sz.w as usize / unit.len().max(1);
                    if unit.is_empty() || count * unit.len() != sz.w as usize {
                        return self.fail(e.span, "invalid wildcard replication");
                    }
                    let mut repeated = Vec::with_capacity(sz.w as usize);
                    for _ in 0..count {
                        repeated.extend_from_slice(&unit);
                    }
                    repeated
                } else {
                    unit
                }
            }
            ExprKind::Cast { target, expr } => {
                let child = &sz.k[0];
                let mut v = self.label_value(sc, expr, child, (sz.w, child.s), depth + 1, left)?;
                if matches!(target,CastTarget::Type(t) if matches!(&t.kind,TypeKind::Bit)) {
                    for b in &mut v {
                        if b.is_unknown() {
                            *b = Bit4::Zero;
                        }
                    }
                }
                v
            }
            ExprKind::SysCall { name, args }
                if matches!(name.as_str(), "$signed" | "$unsigned") && args.len() == 1 =>
            {
                let child = &sz.k[0];
                self.label_value(sc, &args[0], child, (sz.w, child.s), depth + 1, left)?
            }
            ExprKind::Call { .. } | ExprKind::Pattern { .. } => {
                return self.fail(
                    e.span,
                    "function/assignment-pattern wildcard labels are unsupported",
                );
            }
            _ => {
                let operands: Vec<&Expr> = match &e.kind {
                    ExprKind::Unary(_, a) | ExprKind::Member(a, _) => vec![a],
                    ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Range(a, b) => {
                        vec![a, b]
                    }
                    ExprKind::Cond(a, b, c) => vec![a, b, c],
                    ExprKind::Slice {
                        base, left, right, ..
                    } => vec![base, left, right],
                    ExprKind::SysCall { args, .. } => args.iter().collect(),
                    _ => Vec::new(),
                };
                for operand in operands {
                    let os = self.size(sc, operand)?;
                    if self
                        .label_value(sc, operand, &os, (os.w, os.s), depth + 1, left)?
                        .iter()
                        .any(|b| b.is_unknown())
                    {
                        return self.fail(
                            e.span,
                            "unknown bits in this wildcard-label operation are unsupported",
                        );
                    }
                }
                vec![Bit4::Zero; sz.w as usize]
            }
        };
        let fill = if signed && sz.s {
            raw.last().copied().unwrap_or(Bit4::Zero)
        } else {
            Bit4::Zero
        };
        raw.resize(width as usize, fill);
        Ok(raw)
    }
}
