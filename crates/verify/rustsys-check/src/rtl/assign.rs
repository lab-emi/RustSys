// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Assignment values, patterns and targets of the checker's RTL model; subroutine calls in expressions.

use rusteda_formats::verilog::ast::{self, Expr, ExprKind, PatternItem, PatternKey};

use super::eval::Loc;
use super::eval::Src;
use super::ty::{Packed, Ty};
use super::{Builder, Name, R, Val};
use crate::circ::B;

impl<'a> Builder<'a> {
    // ---------------------------------------------------------- assignments

    /// Value of `e` assigned to a target of type `t`.
    pub(crate) fn assign_val(&mut self, sc: usize, e: &Expr, t: &Ty) -> R<Val> {
        if let ExprKind::Pattern { .. } = &e.kind {
            return self.pattern(sc, e, t);
        }
        // `c ? x : y` in an assignment context: both operands are assigned to the target type,
        // which is what gives untyped patterns among them a type (SYNTH-REQ-0222).
        if let ExprKind::Cond(c, x, y) = &e.kind {
            fn untyped(z: &Expr) -> bool {
                match &z.kind {
                    ExprKind::Pattern { ty: None, .. } => true,
                    ExprKind::Cond(_, p, q) => untyped(p) || untyped(q),
                    _ => false,
                }
            }
            if t.is_array() || untyped(x) || untyped(y) {
                let cb = self.truthy(sc, c)?;
                let vx = self.assign_val(sc, x, t)?;
                let vy = self.assign_val(sc, y, t)?;
                return Ok(match (vx, vy) {
                    (Val::P(a), Val::P(b)) => Val::P(
                        a.iter()
                            .zip(&b)
                            .map(|(p, q)| self.c.mux(cb, *p, *q))
                            .collect(),
                    ),
                    (Val::U(a), Val::U(b)) => Val::U(
                        a.iter()
                            .zip(&b)
                            .map(|(pa, pb)| {
                                pa.iter()
                                    .zip(pb)
                                    .map(|(p, q)| self.c.mux(cb, *p, *q))
                                    .collect()
                            })
                            .collect(),
                    ),
                    _ => return self.fail(e.span, "conditional operands of different shapes"),
                });
            }
        }
        if t.is_array() {
            let (loc, st) = self.locate(sc, e)?;
            if st.count() != t.count() || !st.is_array() {
                return self.fail(e.span, "array shape mismatch");
            }
            let bits = self.read_loc(&loc);
            let ew = st.width() as usize;
            let tw = t.width() as usize;
            let elems: Vec<Vec<B>> = bits
                .chunks(ew.max(1))
                .map(|c| self.ext(c, tw, false))
                .collect();
            return Ok(Val::U(elems));
        }
        let s = self.size(sc, e)?;
        let tw = t.width() as u32;
        let w = tw.max(s.w);
        let v = self.ev(sc, e, &s, w, s.s)?;
        Ok(Val::P(v[..tw as usize].to_vec()))
    }

    pub(crate) fn pattern(&mut self, sc: usize, e: &Expr, t: &Ty) -> R<Val> {
        let ExprKind::Pattern { ty, repl, items } = &e.kind else {
            unreachable!()
        };
        let t = match ty {
            Some(dt) => self.resolve(sc, dt, None)?,
            None => t.clone(),
        };
        let items: Vec<PatternItem> = match repl {
            Some(n) => {
                let k = self.cint(sc, n)?.max(0) as usize;
                (0..k).flat_map(|_| items.iter().cloned()).collect()
            }
            None => items.clone(),
        };
        // Structures.
        if !t.is_array() {
            if let Packed::Struct(ms) = &t.packed {
                let ms = ms.clone();
                let mut vals: Vec<Option<Vec<B>>> = vec![None; ms.len()];
                let mut default: Option<&Expr> = None;
                let mut next = 0usize;
                for it in &items {
                    match it {
                        PatternItem::Positional(x) => {
                            let Some((_, mt)) = ms.get(next) else {
                                return self.fail(x.span, "too many items");
                            };
                            let mt = Ty {
                                packed: mt.clone(),
                                signed: false,
                                unpacked: vec![],
                            };
                            let Val::P(v) = self.assign_val(sc, x, &mt)? else {
                                unreachable!()
                            };
                            vals[next] = Some(v);
                            next += 1;
                        }
                        PatternItem::Keyed(PatternKey::Default, x) => default = Some(x),
                        PatternItem::Keyed(PatternKey::Expr(k), x) => {
                            let Some(id) = k.as_ident() else {
                                return self.fail(k.span, "member name expected");
                            };
                            let Some(i) = ms.iter().position(|(n, _)| *n == id.name) else {
                                return self.fail(k.span, "no such member");
                            };
                            let mt = Ty {
                                packed: ms[i].1.clone(),
                                signed: false,
                                unpacked: vec![],
                            };
                            let Val::P(v) = self.assign_val(sc, x, &mt)? else {
                                unreachable!()
                            };
                            vals[i] = Some(v);
                        }
                        PatternItem::Keyed(PatternKey::Type(_), x) => {
                            return self.fail(x.span, "type keys not supported");
                        }
                    }
                }
                let mut out = Vec::new();
                // Least significant member last in declaration order.
                for (i, (_, mt)) in ms.iter().enumerate().rev() {
                    let v = match &vals[i] {
                        Some(v) => v.clone(),
                        None => {
                            let Some(d) = default else {
                                return self.fail(e.span, "member not set");
                            };
                            let mt = Ty {
                                packed: mt.clone(),
                                signed: false,
                                unpacked: vec![],
                            };
                            let Val::P(v) = self.fill_default(sc, d, &mt)? else {
                                unreachable!()
                            };
                            v
                        }
                    };
                    out.extend(v);
                }
                return Ok(Val::P(out));
            }
        }
        let Some((l, r, et)) = t.index_type() else {
            return self.fail(e.span, "pattern target");
        };
        let n = ((l - r).unsigned_abs() + 1) as usize;
        let mut vals: Vec<Option<Val>> = vec![None; n];
        let mut default: Option<&Expr> = None;
        let mut next = 0usize;
        for it in &items {
            match it {
                PatternItem::Positional(x) => {
                    if next >= n {
                        return self.fail(x.span, "too many items");
                    }
                    vals[next] = Some(self.assign_val(sc, x, &et)?);
                    next += 1;
                }
                PatternItem::Keyed(PatternKey::Default, x) => default = Some(x),
                PatternItem::Keyed(PatternKey::Expr(k), x) => {
                    let i = self.cint(sc, k)?;
                    let Some(p) = super::ty::position(l, r, i) else {
                        return self.fail(k.span, "index out of range");
                    };
                    vals[p as usize] = Some(self.assign_val(sc, x, &et)?);
                }
                PatternItem::Keyed(PatternKey::Type(_), x) => {
                    return self.fail(x.span, "type keys not supported");
                }
            }
        }
        if next != 0 && next != n {
            return self.fail(e.span, "wrong number of pattern items");
        }
        let mut elems = Vec::with_capacity(n);
        for v in vals {
            elems.push(match v {
                Some(v) => v,
                None => {
                    let Some(d) = default else {
                        return self.fail(e.span, "element not set");
                    };
                    self.fill_default(sc, d, &et)?
                }
            });
        }
        Ok(join(&t, elems))
    }

    /// A `default:` item applied to an element or member of type `t` left unset by the other
    /// items (SYNTH-REQ-0223): bits, bit vectors and unions take the value converted to their
    /// type, as does any element whose type is the value's own; structures and arrays of
    /// non-bit elements receive the default once per member or element.
    fn fill_default(&mut self, sc: usize, d: &Expr, t: &Ty) -> R<Val> {
        let nested = t.is_array()
            || match &t.packed {
                Packed::Struct(_) => true,
                Packed::Arr { elem, .. } => **elem != Packed::Bit,
                Packed::Bit | Packed::Union(_) => false,
            };
        let whole = !nested
            || matches!(d.kind, ExprKind::Pattern { ty: None, .. })
            || self
                .type_of(sc, d)
                .is_ok_and(|dt| dt.packed == t.packed && dt.unpacked == t.unpacked);
        if whole {
            return self.assign_val(sc, d, t);
        }
        if let (false, Packed::Struct(ms)) = (t.is_array(), &t.packed) {
            let ms = ms.clone();
            let mut out = Vec::new();
            // Least significant member last in declaration order.
            for (_, mt) in ms.iter().rev() {
                let mt = Ty {
                    packed: mt.clone(),
                    signed: false,
                    unpacked: vec![],
                };
                let Val::P(v) = self.fill_default(sc, d, &mt)? else {
                    unreachable!()
                };
                out.extend(v);
            }
            return Ok(Val::P(out));
        }
        let Some((l, r, et)) = t.index_type() else {
            return self.fail(d.span, "pattern target");
        };
        let n = (l - r).unsigned_abs() + 1;
        let mut elems = Vec::with_capacity(n as usize);
        for _ in 0..n {
            elems.push(self.fill_default(sc, d, &et)?);
        }
        Ok(join(t, elems))
    }

    /// Target location of an assignment and its type (concatenations are flattened, most
    /// significant part first).
    pub(crate) fn target(&mut self, sc: usize, e: &Expr) -> R<(Loc, Ty)> {
        if let ExprKind::Concat(items) = &e.kind {
            let mut loc: Loc = Vec::new();
            for it in items.iter().rev() {
                let (l, t) = self.target(sc, it)?;
                if t.is_array() {
                    return self.fail(it.span, "array in a concatenation target");
                }
                loc.extend(l);
            }
            let w = loc.len() as u64;
            return Ok((loc, Ty::bits(w, false)));
        }
        let (loc, t) = self.locate(sc, e)?;
        for alts in &loc {
            if alts.iter().any(|(s, _)| matches!(s, Src::Bit(_))) {
                return self.fail(e.span, "not an assignable target");
            }
        }
        Ok((loc, t))
    }

    /// Subroutine call used from an expression.
    pub(crate) fn call(
        &mut self,
        sc: usize,
        func: &ast::Path,
        args: &[ast::Arg],
        sp: rusteda_formats::verilog::Span,
    ) -> R<Option<Val>> {
        match self.find_path(sc, func)? {
            Name::Func(f, fsc) => self.invoke(
                sc,
                fsc,
                &f.name,
                f.ret.as_ref(),
                &f.ports,
                &f.decls,
                &f.body,
                args,
                sp,
            ),
            Name::Task(t, tsc) => self.invoke(
                sc, tsc, &t.name, None, &t.ports, &t.decls, &t.body, args, sp,
            ),
            _ => self.fail(sp, "not a subroutine"),
        }
    }
}

/// Array value from its elements, leftmost first.
fn join(t: &Ty, elems: Vec<Val>) -> Val {
    if t.is_array() {
        let mut out = Vec::new();
        for v in elems {
            match v {
                Val::P(b) => out.push(b),
                Val::U(bs) => out.extend(bs),
            }
        }
        Val::U(out)
    } else {
        // Leftmost element is the most significant.
        let mut out = Vec::new();
        for v in elems.into_iter().rev() {
            let Val::P(b) = v else { unreachable!() };
            out.extend(b);
        }
        Val::P(out)
    }
}
