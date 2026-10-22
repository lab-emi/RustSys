// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Explicit-width wiring expressions written by external netlist exporters.
//! One assignment holds at most 2 * MAX_NET_BITS * sizeof(B) = 16 MiB of bit
//! references; its iterative syntax walk is capped at MAX_NET_BITS entries.
//! These are wiring aliases, never an elaboration or synthesis optimization.

use std::collections::{HashMap, HashSet};

use rusteda_formats::verilog::ast::{Expr, ExprKind, SliceKind};
use rusteda_formats::verilog::number::Bit4;
use rusteda_formats::verilog::{SourceMap, Span};

use super::{MAX_NET_BITS, NetModel, UNKNOWN_BIT, int_value, located, unmodeled};
use crate::circ::B;

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn bits(
    sm: &SourceMap,
    nm: &NetModel,
    ranges: &HashMap<String, (i64, i64)>,
    supply: &HashSet<String>,
    expr: &Expr,
    bit: &impl Fn(&NetModel, &Expr, &dyn Fn() -> String) -> Result<B, String>,
    what: &dyn Fn() -> String,
) -> Result<Vec<B>, String> {
    let mut out = Vec::new();
    let mut stack = vec![expr];
    let mut visits = 0u64;
    let budget = |at: Span| {
        format!(
            "warning[C0005]: {}: netlist connection exceeds the bit/work budget",
            sm.describe(at)
        )
    };
    while let Some(e) = stack.pop() {
        visits += 1;
        if visits > MAX_NET_BITS || out.len() as u64 >= MAX_NET_BITS {
            return Err(budget(e.span));
        }
        match &e.kind {
            ExprKind::Ident(p) if p.scope.is_none() => {
                let name = &p.name.name;
                if supply.contains(name) {
                    return Err(located(
                        sm,
                        e.span,
                        format!("supply port {name} is read as a logic value (CHECK-REQ-0022)"),
                    ));
                }
                let vars = nm.nets.get(name).ok_or_else(|| {
                    located(sm, e.span, format!("{}: undeclared net {name}", what()))
                })?;
                if out.len() as u64 + vars.len() as u64 > MAX_NET_BITS {
                    return Err(budget(e.span));
                }
                out.extend(vars.iter().copied().map(B::V));
            }
            ExprKind::Concat(items) => {
                if stack.len() as u64 + items.len() as u64 > MAX_NET_BITS {
                    return Err(budget(e.span));
                }
                // Stack order visits the rightmost, least-significant item first.
                stack.extend(items);
            }
            ExprKind::Number(number) => {
                let values = number.bits()?;
                if values.iter().any(|b| b.is_unknown()) {
                    return Err(unmodeled(
                        UNKNOWN_BIT,
                        sm,
                        e.span,
                        format!(
                            "{}: literal {} has an x or z bit",
                            what(),
                            sm.snippet(e.span)
                        ),
                    ));
                }
                if out.len() as u64 + values.len() as u64 > MAX_NET_BITS {
                    return Err(budget(e.span));
                }
                out.extend(values.into_iter().map(|b| B::from_bool(b == Bit4::One)));
            }
            ExprKind::Slice {
                base,
                kind: SliceKind::Const,
                left,
                right,
            } => {
                let name = &base
                    .as_ident()
                    .ok_or_else(|| located(sm, e.span, "netlist slice must select a named vector"))?
                    .name;
                if supply.contains(name) {
                    return Err(located(
                        sm,
                        e.span,
                        format!("supply port {name} is read as a logic value (CHECK-REQ-0022)"),
                    ));
                }
                let vars = nm
                    .nets
                    .get(name)
                    .ok_or_else(|| located(sm, e.span, format!("undeclared net {name}")))?;
                let &(dl, dr) = ranges
                    .get(name)
                    .ok_or_else(|| located(sm, e.span, format!("{name} is not a vector")))?;
                let bound = |e: &Expr| {
                    let ExprKind::Number(n) = &e.kind else {
                        return Err(located(
                            sm,
                            e.span,
                            "netlist slice bound must be an integer literal",
                        ));
                    };
                    int_value(n)?.ok_or_else(|| {
                        located(sm, e.span, "netlist slice bound is outside 32 bits")
                    })
                };
                let (l, r) = (bound(left)?, bound(right)?);
                if !(dl.min(dr)..=dl.max(dr)).contains(&l)
                    || !(dl.min(dr)..=dl.max(dr)).contains(&r)
                    || (l != r && (l < r) != (dl < dr))
                {
                    return Err(located(
                        sm,
                        e.span,
                        format!("invalid slice [{l}:{r}] of {name}[{dl}:{dr}]"),
                    ));
                }
                let width = (l - r).unsigned_abs() + 1;
                if out.len() as u64 + width > MAX_NET_BITS {
                    return Err(budget(e.span));
                }
                for k in 0..width {
                    let index = if l >= r { r + k as i64 } else { r - k as i64 };
                    let offset = (index - dr).unsigned_abs() as usize;
                    out.push(B::V(vars[offset]));
                }
            }
            _ => out.push(bit(nm, e, what)?),
        }
    }
    Ok(out)
}
