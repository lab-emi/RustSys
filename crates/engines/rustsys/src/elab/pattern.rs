// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Assignment patterns (SYNTH-REQ-0131).

use rusteda_formats::verilog::ast::{Expr, ExprKind, PatternItem, PatternKey};

use super::types::Ty;
use super::{EResult, Elaborator, ScopeId, TVal};
use crate::aig::Lit;
use crate::bits::Bv;

impl<'a> Elaborator<'a> {
    // --------------------------------------------------------------- patterns

    /// Evaluates an assignment pattern for a target type.
    pub(crate) fn eval_pattern(&mut self, scope: ScopeId, e: &Expr, target: &Ty) -> EResult<TVal> {
        let ExprKind::Pattern { ty, repl, items } = &e.kind else {
            unreachable!("pattern")
        };
        let target = match ty {
            Some(t) => self.resolve_type(scope, t, false)?,
            None => target.clone(),
        };
        // Replicated pattern: '{n{a, b}}.
        let items: Vec<PatternItem> = match repl {
            Some(n) => {
                let k = self.eval_const_int(scope, n)?.max(0) as usize;
                let mut v = Vec::new();
                for _ in 0..k {
                    v.extend(items.iter().cloned());
                }
                v
            }
            None => items.clone(),
        };
        match &target {
            Ty::Struct { fields, .. } => {
                let fields = fields.clone();
                let mut vals: Vec<Option<Bv>> = vec![None; fields.len()];
                let mut default = None;
                let mut positional = 0usize;
                for it in &items {
                    match it {
                        PatternItem::Positional(x) => {
                            let f = fields.get(positional).ok_or_else(|| {
                                self.err("S0190", x.span, "too many pattern items")
                            })?;
                            vals[positional] = Some(self.pack(scope, x, &f.ty)?);
                            positional += 1;
                        }
                        PatternItem::Keyed(PatternKey::Default, x) => default = Some(x),
                        PatternItem::Keyed(PatternKey::Expr(k), x) => {
                            let name = k.as_ident().map(|i| i.name.clone()).ok_or_else(|| {
                                self.err(
                                    "S0191",
                                    k.span,
                                    "structure pattern keys must be member names",
                                )
                            })?;
                            let idx =
                                fields.iter().position(|f| f.name == name).ok_or_else(|| {
                                    self.err("S0170", k.span, format!("no member '{name}'"))
                                })?;
                            vals[idx] = Some(self.pack(scope, x, &fields[idx].ty)?);
                        }
                        PatternItem::Keyed(PatternKey::Type(_), x) => {
                            return Err(self.err(
                                "S0192",
                                x.span,
                                "type-keyed pattern items are not supported",
                            ));
                        }
                    }
                }
                let mut out = vec![Lit::FALSE; target.width() as usize];
                for (f, v) in fields.iter().zip(vals) {
                    let v = match v {
                        Some(v) => v,
                        None => match default {
                            Some(d) => self.pack_default(scope, d, &f.ty)?,
                            None => {
                                return Err(self.err(
                                    "S0193",
                                    e.span,
                                    format!("pattern does not set member '{}'", f.name),
                                ));
                            }
                        },
                    };
                    for (j, l) in v.into_iter().enumerate() {
                        out[f.offset as usize + j] = l;
                    }
                }
                Ok(TVal::Packed(out, target))
            }
            Ty::Vector { .. } | Ty::Unpacked { .. } | Ty::Bit { .. } | Ty::Enum { .. } => {
                let (range, et) = target
                    .index_dim()
                    .ok_or_else(|| self.err("S0194", e.span, "pattern target cannot be indexed"))?;
                let n = range.len() as usize;
                let mut vals: Vec<Option<TVal>> = vec![None; n];
                let mut default = None;
                let mut positional = 0usize;
                for it in &items {
                    match it {
                        PatternItem::Positional(x) => {
                            if positional >= n {
                                return Err(self.err("S0190", x.span, "too many pattern items"));
                            }
                            vals[positional] = Some(self.eval_assign(scope, x, &et)?);
                            positional += 1;
                        }
                        PatternItem::Keyed(PatternKey::Default, x) => default = Some(x),
                        PatternItem::Keyed(PatternKey::Expr(k), x) => {
                            let i = self.eval_const_int(scope, k)?;
                            let p = range.position(i).ok_or_else(|| {
                                self.err("S0195", k.span, "pattern index out of range")
                            })?;
                            vals[p as usize] = Some(self.eval_assign(scope, x, &et)?);
                        }
                        PatternItem::Keyed(PatternKey::Type(_), x) => {
                            return Err(self.err(
                                "S0192",
                                x.span,
                                "type-keyed pattern items are not supported",
                            ));
                        }
                    }
                }
                if positional != 0 && positional != n {
                    return Err(self.err(
                        "S0196",
                        e.span,
                        format!("pattern has {positional} items but the target has {n} elements"),
                    ));
                }
                let mut elems = Vec::with_capacity(n);
                for v in vals {
                    let v = match v {
                        Some(v) => v,
                        None => match default {
                            Some(d) => self.default_value(scope, d, &et)?,
                            None => {
                                return Err(self.err(
                                    "S0193",
                                    e.span,
                                    "pattern does not set every element",
                                ));
                            }
                        },
                    };
                    elems.push(v);
                }
                Ok(assemble(elems, target))
            }
        }
    }

    /// Value of a `default` pattern item for one element or member of type `t` that no other
    /// item sets (SYNTH-REQ-0223).
    fn default_value(&mut self, scope: ScopeId, d: &Expr, t: &Ty) -> EResult<TVal> {
        let aggregate = match t {
            Ty::Struct { union, .. } => !union,
            Ty::Vector { elem, .. } => !matches!(**elem, Ty::Bit { .. }),
            Ty::Unpacked { .. } => true,
            Ty::Bit { .. } | Ty::Enum { .. } => false,
        };
        let takes_whole = !aggregate
            || matches!(d.kind, ExprKind::Pattern { ty: None, .. })
            || self.expr_ty(scope, d).is_ok_and(|vt| vt == *t);
        if takes_whole {
            return self.eval_assign(scope, d, t);
        }
        if let Ty::Struct { fields, .. } = t {
            let mut out = vec![Lit::FALSE; t.width() as usize];
            for f in fields.iter() {
                let v = self.pack_default(scope, d, &f.ty)?;
                for (j, l) in v.into_iter().enumerate() {
                    out[f.offset as usize + j] = l;
                }
            }
            return Ok(TVal::Packed(out, t.clone()));
        }
        let (range, et) = t.index_dim().expect("arrays have an index dimension");
        let mut elems = Vec::with_capacity(range.len() as usize);
        for _ in 0..range.len() {
            elems.push(self.default_value(scope, d, &et)?);
        }
        Ok(assemble(elems, t.clone()))
    }

    fn pack_default(&mut self, scope: ScopeId, d: &Expr, t: &Ty) -> EResult<Bv> {
        match self.default_value(scope, d, t)? {
            TVal::Packed(v, _) => Ok(v),
            TVal::Unpacked(..) => {
                Err(self.err("S0197", d.span, "unpacked value in a packed pattern"))
            }
        }
    }

    pub(crate) fn pack(&mut self, scope: ScopeId, x: &Expr, t: &Ty) -> EResult<Bv> {
        match self.eval_assign(scope, x, t)? {
            TVal::Packed(v, _) => Ok(v),
            TVal::Unpacked(..) => {
                Err(self.err("S0197", x.span, "unpacked value in a packed pattern"))
            }
        }
    }
}

/// Array value of type `t` from its element values, leftmost element first.
fn assemble(elems: Vec<TVal>, t: Ty) -> TVal {
    if t.is_packed() {
        // Element at position p (from the left) has offset n-1-p.
        let mut out = Vec::with_capacity(t.width() as usize);
        for v in elems.into_iter().rev() {
            let TVal::Packed(b, _) = v else {
                unreachable!("packed element")
            };
            out.extend(b);
        }
        TVal::Packed(out, t)
    } else {
        let mut flat = Vec::new();
        for v in elems {
            match v {
                TVal::Packed(b, _) => flat.push(b),
                TVal::Unpacked(bs, _) => flat.extend(bs),
            }
        }
        TVal::Unpacked(flat, t)
    }
}
