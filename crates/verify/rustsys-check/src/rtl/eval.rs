// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Expression evaluation, selects and assignment targets of the checker's RTL model.
//!
//! Selects are expressed as "locations": for every selected bit, a list of alternatives, each a
//! source (a variable bit or a value bit) with the condition under which it is the one selected.
//! Reads OR the alternatives together; writes update every alternative under its condition.

use rusteda_formats::verilog::ast::{BinOp, CastTarget, Expr, ExprKind, SliceKind, UnOp};
use rusteda_formats::verilog::number::Bit4;

use super::size::{Sz, konst_i64, konst_u64};
use super::ty::{Packed, Ty, index_at, offset};
use super::{Builder, Name, R, Val};
use crate::circ::B;

/// Source of a selected bit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Src {
    /// Bit of a variable: signal or local, flat bit index.
    Var(bool, usize, usize),
    /// A value bit.
    Bit(B),
}

/// Selected bits with alternatives.
pub(crate) type Loc = Vec<Vec<(Src, B)>>;

impl<'a> Builder<'a> {
    pub(crate) fn truthy(&mut self, sc: usize, e: &Expr) -> R<B> {
        let s = self.size(sc, e)?;
        let v = self.ev(sc, e, &s, s.w, s.s)?;
        Ok(self.reduce(&v, 1))
    }

    // ------------------------------------------------------------ evaluation

    /// Evaluates `e` (annotated by `sz`) in a context of width `cw` and signedness `cs`.
    pub(crate) fn ev(&mut self, sc: usize, e: &Expr, sz: &Sz, cw: u32, cs: bool) -> R<Vec<B>> {
        let w = cw as usize;
        Ok(match &e.kind {
            ExprKind::Number(l) => {
                let bits = l.bits().map_err(|m| format!("{}: {m}", self.loc(e.span)))?;
                let v: Vec<B> = bits.iter().map(|b| B::from_bool(*b == Bit4::One)).collect();
                self.ext(&v, w, cs)
            }
            ExprKind::UnbasedUnsized(ch) => vec![B::from_bool(*ch == '1'); w],
            ExprKind::Str(s) => {
                let mut v = Vec::new();
                for byte in s.iter().rev() {
                    for k in 0..8 {
                        v.push(B::from_bool((byte >> k) & 1 == 1));
                    }
                }
                if v.is_empty() {
                    v = vec![B::F; 8];
                }
                self.ext(&v, w, cs)
            }
            ExprKind::Ident(_)
            | ExprKind::Index(..)
            | ExprKind::Slice { .. }
            | ExprKind::Member(..) => {
                let (loc, t) = self.locate(sc, e)?;
                if t.is_array() {
                    return self.fail(e.span, "unpacked array used as a value");
                }
                let v = self.read_loc(&loc);
                self.ext(&v, w, cs)
            }
            ExprKind::Unary(op, a) => {
                let sa = &sz.k[0];
                match op {
                    UnOp::Plus => self.ev(sc, a, sa, cw, cs)?,
                    UnOp::Minus => {
                        let v = self.ev(sc, a, sa, cw, cs)?;
                        let z = vec![B::F; w];
                        self.w_sub(&z, &v)
                    }
                    UnOp::BitNot => {
                        let v = self.ev(sc, a, sa, cw, cs)?;
                        self.w_not(&v)
                    }
                    UnOp::LogNot => {
                        let b = self.truthy(sc, a)?;
                        let nb = self.c.not(b);
                        self.ext(&[nb], w, false)
                    }
                    _ => {
                        let v = self.ev(sc, a, sa, sa.w, sa.s)?;
                        let r = match op {
                            UnOp::RedAnd => self.reduce(&v, 0),
                            UnOp::RedNand => {
                                let x = self.reduce(&v, 0);
                                self.c.not(x)
                            }
                            UnOp::RedOr => self.reduce(&v, 1),
                            UnOp::RedNor => {
                                let x = self.reduce(&v, 1);
                                self.c.not(x)
                            }
                            UnOp::RedXor => self.reduce(&v, 2),
                            _ => {
                                let x = self.reduce(&v, 2);
                                self.c.not(x)
                            }
                        };
                        self.ext(&[r], w, false)
                    }
                }
            }
            ExprKind::Binary(op, a, b) => self.ev_binary(sc, *op, a, b, sz, cw, cs, e)?,
            ExprKind::Cond(c, a, b) => {
                let cb = self.truthy(sc, c)?;
                let x = self.ev(sc, a, &sz.k[1], cw, cs)?;
                let y = self.ev(sc, b, &sz.k[2], cw, cs)?;
                (0..w).map(|i| self.c.mux(cb, x[i], y[i])).collect()
            }
            ExprKind::Inside(a, items) => {
                let mut any = B::F;
                for (k, it) in items.iter().enumerate() {
                    let isz = &sz.k[k + 1];
                    let hit = match &it.kind {
                        ExprKind::Range(lo, hi) => {
                            let asz = &sz.k[0];
                            let cw2 = asz.w.max(isz.w);
                            let cs2 = asz.s && isz.s;
                            let x = self.ev(sc, a, asz, cw2, cs2)?;
                            let l = self.ev(sc, lo, &isz.k[0], cw2, cs2)?;
                            let h = self.ev(sc, hi, &isz.k[1], cw2, cs2)?;
                            let below = self.w_lt(&x, &l, cs2);
                            let above = self.w_lt(&h, &x, cs2);
                            let nb = self.c.not(below);
                            let na = self.c.not(above);
                            self.c.and(nb, na)
                        }
                        _ => self.wild_eq(sc, a, &sz.k[0], it, isz)?,
                    };
                    any = self.c.or(any, hit);
                }
                self.ext(&[any], w, false)
            }
            ExprKind::Concat(items) => {
                let mut v = Vec::new();
                for (it, isz) in items.iter().zip(&sz.k).rev() {
                    if isz.w == 0 {
                        continue;
                    }
                    let x = self.ev(sc, it, isz, isz.w, isz.s)?;
                    v.extend(x);
                }
                self.ext(&v, w, false)
            }
            ExprKind::Repl(n, items) => {
                let cnt = self.cint(sc, n)? as usize;
                let mut one = Vec::new();
                for (it, isz) in items.iter().zip(&sz.k).rev() {
                    let x = self.ev(sc, it, isz, isz.w, isz.s)?;
                    one.extend(x);
                }
                let mut v = Vec::new();
                for _ in 0..cnt {
                    v.extend_from_slice(&one);
                }
                self.ext(&v, w, false)
            }
            ExprKind::Pattern { .. } => {
                let t = self.type_of(sc, e)?;
                let Val::P(v) = self.assign_val(sc, e, &t)? else {
                    return self.fail(e.span, "unpacked pattern");
                };
                self.ext(&v, w, cs)
            }
            ExprKind::Call { func, args } => {
                let r = self.call(sc, func, args, e.span)?;
                match r {
                    Some(Val::P(v)) => self.ext(&v, w, cs),
                    _ => return self.fail(e.span, "function value is not packed"),
                }
            }
            ExprKind::SysCall { name, args } => {
                let v = self.ev_sys(sc, name, args, sz, e)?;
                self.ext(&v, w, cs)
            }
            ExprKind::Cast { target, expr } => {
                let v = match target {
                    CastTarget::Signed | CastTarget::Unsigned | CastTarget::Const => {
                        let s = &sz.k[0];
                        self.ev(sc, expr, s, s.w, s.s)?
                    }
                    _ => {
                        let t = Ty::bits(u64::from(sz.w), sz.s);
                        let t = if let CastTarget::Type(dt) = target {
                            self.resolve(sc, dt, None)?
                        } else if let CastTarget::Expr(x) = target {
                            if self.is_type_name(sc, x) {
                                self.type_of(sc, e)?
                            } else {
                                t
                            }
                        } else {
                            t
                        };
                        match self.assign_val(sc, expr, &t)? {
                            Val::P(v) => v,
                            Val::U(_) => return self.fail(e.span, "cast to an unpacked type"),
                        }
                    }
                };
                self.ext(&v, w, cs)
            }
            _ => return self.fail(e.span, "expression not supported"),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn ev_binary(
        &mut self,
        sc: usize,
        op: BinOp,
        a: &Expr,
        b: &Expr,
        sz: &Sz,
        cw: u32,
        cs: bool,
        e: &Expr,
    ) -> R<Vec<B>> {
        let w = cw as usize;
        let (sa, sb) = (&sz.k[0], &sz.k[1]);
        Ok(match op {
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::BitXnor => {
                let x = self.ev(sc, a, sa, cw, cs)?;
                let y = self.ev(sc, b, sb, cw, cs)?;
                match op {
                    BinOp::Add => self.w_add(&x, &y, B::F),
                    BinOp::Sub => self.w_sub(&x, &y),
                    BinOp::Mul if cs => self.w_mul_signed(&x, &y),
                    BinOp::Mul => self.w_mul(&x, &y),
                    BinOp::Div => self.w_divmod(&x, &y, cs).0,
                    BinOp::Mod => self.w_divmod(&x, &y, cs).1,
                    BinOp::BitAnd => (0..w).map(|i| self.c.and(x[i], y[i])).collect(),
                    BinOp::BitOr => (0..w).map(|i| self.c.or(x[i], y[i])).collect(),
                    BinOp::BitXor => (0..w).map(|i| self.c.xor(x[i], y[i])).collect(),
                    _ => (0..w)
                        .map(|i| {
                            let t = self.c.xor(x[i], y[i]);
                            self.c.not(t)
                        })
                        .collect(),
                }
            }
            BinOp::Shl | BinOp::AShl | BinOp::Shr | BinOp::AShr => {
                let x = self.ev(sc, a, sa, cw, cs)?;
                let amt = self.ev(sc, b, sb, sb.w, sb.s)?;
                let left = matches!(op, BinOp::Shl | BinOp::AShl);
                let fill = if op == BinOp::AShr && cs {
                    x[w - 1]
                } else {
                    B::F
                };
                self.w_shift(&x, &amt, left, fill)
            }
            BinOp::Pow => {
                let x = self.ev(sc, a, sa, cw, cs)?;
                let y = self.ev(sc, b, sb, sb.w, sb.s)?;
                match konst_i64(&y, sb.s) {
                    Some(k) if k < 0 => {
                        let xv = konst_i64(&x, cs).ok_or_else(|| {
                            format!(
                                "{}: negative exponent needs a constant base",
                                self.loc(e.span)
                            )
                        })?;
                        let r: i64 = match xv {
                            1 => 1,
                            -1 => {
                                if k % 2 == 0 {
                                    1
                                } else {
                                    -1
                                }
                            }
                            _ => 0,
                        };
                        (0..w)
                            .map(|i| B::from_bool(if i < 64 { (r >> i) & 1 == 1 } else { r < 0 }))
                            .collect()
                    }
                    Some(k) => {
                        let mut acc: Vec<B> = (0..w).map(|i| B::from_bool(i == 0)).collect();
                        for _ in 0..k.min(4096) {
                            acc = self.w_mul(&acc, &x);
                            if acc.iter().all(|b| *b == B::F) {
                                break;
                            }
                        }
                        acc
                    }
                    None if konst_u64(&x) == Some(2) => {
                        let one: Vec<B> = (0..w).map(|i| B::from_bool(i == 0)).collect();
                        self.w_shift(&one, &y, true, B::F)
                    }
                    None => {
                        return self.fail(e.span, "power needs a constant exponent or a base of 2");
                    }
                }
            }
            BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::CaseEq
            | BinOp::CaseNe => {
                let cw2 = sa.w.max(sb.w);
                let cs2 = sa.s && sb.s;
                let x = self.ev(sc, a, sa, cw2, cs2)?;
                let y = self.ev(sc, b, sb, cw2, cs2)?;
                let r = match op {
                    BinOp::Lt => self.w_lt(&x, &y, cs2),
                    BinOp::Gt => self.w_lt(&y, &x, cs2),
                    BinOp::Le => {
                        let t = self.w_lt(&y, &x, cs2);
                        self.c.not(t)
                    }
                    BinOp::Ge => {
                        let t = self.w_lt(&x, &y, cs2);
                        self.c.not(t)
                    }
                    BinOp::Eq | BinOp::CaseEq => self.w_eq(&x, &y),
                    _ => {
                        let t = self.w_eq(&x, &y);
                        self.c.not(t)
                    }
                };
                self.ext(&[r], w, false)
            }
            BinOp::WildEq | BinOp::WildNe => {
                let r = self.wild_eq(sc, a, sa, b, sb)?;
                let r = if op == BinOp::WildNe {
                    self.c.not(r)
                } else {
                    r
                };
                self.ext(&[r], w, false)
            }
            BinOp::LogAnd | BinOp::LogOr | BinOp::Implies | BinOp::Equiv => {
                let x = self.truthy(sc, a)?;
                let y = self.truthy(sc, b)?;
                let r = match op {
                    BinOp::LogAnd => self.c.and(x, y),
                    BinOp::LogOr => self.c.or(x, y),
                    BinOp::Implies => {
                        let nx = self.c.not(x);
                        self.c.or(nx, y)
                    }
                    _ => {
                        let t = self.c.xor(x, y);
                        self.c.not(t)
                    }
                };
                self.ext(&[r], w, false)
            }
        })
    }

    /// `a ==? b` with wildcard bits (x, z, ?) of a literal `b` ignored.
    pub(crate) fn wild_eq(&mut self, sc: usize, a: &Expr, sa: &Sz, b: &Expr, sb: &Sz) -> R<B> {
        let cw = sa.w.max(sb.w);
        let cs = sa.s && sb.s;
        let x = self.ev(sc, a, sa, cw, cs)?;
        let y = self.ev(sc, b, sb, cw, cs)?;
        let mask: Vec<bool> = self
            .label_bits(sc, b, sb, cw, cs)?
            .into_iter()
            .map(|bit| bit.is_unknown())
            .collect();
        let mut r = B::T;
        for i in 0..cw as usize {
            if mask[i] {
                continue;
            }
            let d = self.c.xor(x[i], y[i]);
            let nd = self.c.not(d);
            r = self.c.and(r, nd);
        }
        Ok(r)
    }

    fn ev_sys(&mut self, sc: usize, name: &str, args: &[Expr], sz: &Sz, e: &Expr) -> R<Vec<B>> {
        let arg = |i: usize| args.get(i);
        let num = |v: i64| -> Vec<B> { (0..32).map(|i| B::from_bool((v >> i) & 1 == 1)).collect() };
        Ok(match name {
            "$signed" | "$unsigned" => {
                let s = &sz.k[0];
                let a = arg(0).expect("sized");
                self.ev(sc, a, s, s.w, s.s)?
            }
            "$clog2" => {
                let a = arg(0).ok_or_else(|| format!("{}: missing argument", self.loc(e.span)))?;
                let s = self.size(sc, a)?;
                let v = self.ev(sc, a, &s, s.w, s.s)?;
                let n = konst_u64(&v)
                    .ok_or_else(|| format!("{}: $clog2 needs a constant", self.loc(e.span)))?;
                let mut k = 0i64;
                while (1u128 << k) < u128::from(n) {
                    k += 1;
                }
                num(k)
            }
            "$bits" => {
                let a = arg(0).ok_or_else(|| format!("{}: missing argument", self.loc(e.span)))?;
                let t = self.arg_type(sc, a)?;
                num((t.width() * t.count()) as i64)
            }
            "$size" | "$left" | "$right" | "$high" | "$low" | "$increment" => {
                let a = arg(0).ok_or_else(|| format!("{}: missing argument", self.loc(e.span)))?;
                let t = self.arg_type(sc, a)?;
                let d = match arg(1) {
                    Some(x) => self.cint(sc, x)?,
                    None => 1,
                };
                // Dimensions: unpacked first, then packed.
                let mut dims: Vec<(i64, i64)> = t.unpacked.clone();
                let mut p = t.packed.clone();
                loop {
                    match p {
                        Packed::Arr { elem, left, right } => {
                            dims.push((left, right));
                            p = (*elem).clone();
                        }
                        Packed::Bit => {
                            if dims.is_empty() {
                                dims.push((0, 0));
                            }
                            break;
                        }
                        other => {
                            dims.push((other.width() as i64 - 1, 0));
                            break;
                        }
                    }
                }
                let Some((l, r)) = usize::try_from(d - 1)
                    .ok()
                    .and_then(|i| dims.get(i).copied())
                else {
                    return self.fail(e.span, "dimension out of range");
                };
                num(match name {
                    "$size" => (l - r).abs() + 1,
                    "$left" => l,
                    "$right" => r,
                    "$high" => l.max(r),
                    "$low" => l.min(r),
                    _ => {
                        if l >= r {
                            1
                        } else {
                            -1
                        }
                    }
                })
            }
            "$countones" | "$onehot" | "$onehot0" => {
                let a = arg(0).ok_or_else(|| format!("{}: missing argument", self.loc(e.span)))?;
                let s = self.size(sc, a)?;
                let v = self.ev(sc, a, &s, s.w, s.s)?;
                // Count with a 32-bit adder chain.
                let mut acc = vec![B::F; 32];
                for b in &v {
                    let mut one = vec![B::F; 32];
                    one[0] = *b;
                    acc = self.w_add(&acc, &one, B::F);
                }
                match name {
                    "$countones" => acc,
                    "$onehot" => vec![self.eq_const(&acc, false, 1)],
                    _ => {
                        let z = self.eq_const(&acc, false, 0);
                        let o = self.eq_const(&acc, false, 1);
                        vec![self.c.or(z, o)]
                    }
                }
            }
            "$isunknown" => vec![B::F],
            _ => return self.fail(e.span, format!("{name} not supported")),
        })
    }

    fn arg_type(&mut self, sc: usize, a: &Expr) -> R<Ty> {
        match &a.kind {
            ExprKind::Type(t) => self.resolve(sc, t, None),
            ExprKind::Ident(p) => match self.find_path(sc, p)? {
                Name::Type(t) => Ok(t),
                _ => self.type_of(sc, a),
            },
            _ => self.type_of(sc, a),
        }
    }

    // --------------------------------------------------------------- locations

    /// Location and type of a name or select expression.
    pub(crate) fn locate(&mut self, sc: usize, e: &Expr) -> R<(Loc, Ty)> {
        match &e.kind {
            ExprKind::Ident(p) => match self.find_path(sc, p)? {
                Name::Sig(s) => {
                    let n = self.sigs[s].vars.len();
                    Ok((
                        (0..n).map(|i| vec![(Src::Var(true, s, i), B::T)]).collect(),
                        self.sigs[s].ty.clone(),
                    ))
                }
                Name::Local(l) => {
                    let t = self.locals[l].clone();
                    let n = (t.width() * t.count()) as usize;
                    Ok((
                        (0..n)
                            .map(|i| vec![(Src::Var(false, l, i), B::T)])
                            .collect(),
                        t,
                    ))
                }
                Name::Value(v) => {
                    let bits: Vec<B> = match &v.0 {
                        Val::P(b) => b.clone(),
                        Val::U(es) => es.iter().flatten().copied().collect(),
                    };
                    Ok((
                        bits.into_iter()
                            .map(|b| vec![(Src::Bit(b), B::T)])
                            .collect(),
                        v.1.clone(),
                    ))
                }
                _ => self.fail(e.span, "not a value"),
            },
            ExprKind::Index(b, idx) => {
                let (base, t) = self.locate(sc, b)?;
                let Some((l, r, et)) = t.index_type() else {
                    return self.fail(e.span, "cannot index");
                };
                let n = (l - r).unsigned_abs() + 1;
                let is = self.size(sc, idx)?;
                let iv = self.ev(sc, idx, &is, is.w, is.s)?;
                let ew = (et.width() * et.count()) as usize;
                let mut out: Loc = vec![Vec::new(); ew];
                for p in 0..n {
                    // Position p from the left for unpacked, offset from the right for packed.
                    let (index, chunk) = if t.is_array() {
                        (index_at(l, r, p), p)
                    } else {
                        (index_at(l, r, n - 1 - p), n - 1 - p)
                    };
                    let _ = chunk;
                    let cond = match konst_i64(&iv, is.s) {
                        Some(k) => B::from_bool(k == index),
                        None => self.eq_const(&iv, is.s, index),
                    };
                    if cond == B::F {
                        continue;
                    }
                    // Chunk position within the base location.
                    let slot = if t.is_array() {
                        p
                    } else {
                        offset(l, r, index).expect("in range")
                    };
                    for (j, o) in out.iter_mut().enumerate() {
                        for (src, c) in &base[slot as usize * ew + j] {
                            let cc = self.c.and(*c, cond);
                            if cc != B::F {
                                o.push((*src, cc));
                            }
                        }
                    }
                }
                Ok((out, et))
            }
            ExprKind::Slice {
                base,
                kind,
                left,
                right,
            } => {
                let (bl, t) = self.locate(sc, base)?;
                let Some((l, r, et)) = t.index_type() else {
                    return self.fail(e.span, "cannot slice");
                };
                let ew = (et.width() * et.count()) as usize;
                let desc = l >= r;
                // Indices of the result elements, least significant (packed) or leftmost
                // (unpacked) first, as functions of a base value.
                let (count, sel_base): (u64, Option<(Vec<B>, bool)>) = match kind {
                    SliceKind::Const => (
                        (self.cint(sc, left)? - self.cint(sc, right)?).unsigned_abs() + 1,
                        None,
                    ),
                    _ => {
                        let s = self.size(sc, left)?;
                        let v = self.ev(sc, left, &s, s.w, s.s)?;
                        (self.cint(sc, right)? as u64, Some((v, s.s)))
                    }
                };
                let (lc, rc) = match kind {
                    SliceKind::Const => (self.cint(sc, left)?, self.cint(sc, right)?),
                    _ => (0, 0),
                };
                let mut out: Loc = vec![Vec::new(); count as usize * ew];
                for j in 0..count as i64 {
                    // Result element j (from the right for packed; from the left for unpacked).
                    let n = count as i64;
                    let from_right = if t.is_array() { n - 1 - j } else { j };
                    let candidates: Vec<(i64, B)> = match (&sel_base, kind) {
                        (None, _) => {
                            let idx = if lc >= rc {
                                rc + from_right
                            } else {
                                rc - from_right
                            };
                            vec![(idx, B::T)]
                        }
                        (Some((bv, bs)), k) => {
                            // Selected index as base + delta.
                            let delta = match (k, desc) {
                                (SliceKind::Up, true) => from_right,
                                (SliceKind::Up, false) => n - 1 - from_right,
                                (SliceKind::Down, true) => from_right - (n - 1),
                                _ => -from_right,
                            };
                            let (lo, hi) = (l.min(r), l.max(r));
                            let mut cands = Vec::new();
                            for idx in lo..=hi {
                                let c = match konst_i64(bv, *bs) {
                                    Some(k) => B::from_bool(k + delta == idx),
                                    None => {
                                        let bvc = bv.clone();
                                        self.eq_const(&bvc, *bs, idx - delta)
                                    }
                                };
                                if c != B::F {
                                    cands.push((idx, c));
                                }
                            }
                            cands
                        }
                    };
                    for (idx, c) in candidates {
                        let Some(o) = offset(l, r, idx) else { continue };
                        let slot = if t.is_array() {
                            (l - r).unsigned_abs() - o
                        } else {
                            o
                        };
                        for k in 0..ew {
                            for (src, cc) in &bl[slot as usize * ew + k] {
                                let x = self.c.and(*cc, c);
                                if x != B::F {
                                    let pos = if t.is_array() {
                                        j as usize * ew + k
                                    } else {
                                        from_right as usize * ew + k
                                    };
                                    out[pos].push((*src, x));
                                }
                            }
                        }
                    }
                }
                let rt = if t.is_array() {
                    let mut u = t.clone();
                    u.unpacked[0] = (0, count as i64 - 1);
                    u
                } else {
                    Ty {
                        packed: Packed::Arr {
                            elem: std::rc::Rc::new(et.packed),
                            left: count as i64 - 1,
                            right: 0,
                        },
                        signed: false,
                        unpacked: vec![],
                    }
                };
                Ok((out, rt))
            }
            ExprKind::Member(b, f) => {
                let (bl, t) = self.locate(sc, b)?;
                let Some((off, p)) = t.packed.member(&f.name) else {
                    return self.fail(f.span, format!("no member {}", f.name));
                };
                let w = p.width();
                let out = bl[off as usize..(off + w) as usize].to_vec();
                Ok((
                    out,
                    Ty {
                        packed: p,
                        signed: false,
                        unpacked: vec![],
                    },
                ))
            }
            _ => {
                let s = self.size(sc, e)?;
                let v = self.ev(sc, e, &s, s.w, s.s)?;
                Ok((
                    v.into_iter().map(|b| vec![(Src::Bit(b), B::T)]).collect(),
                    Ty::bits(u64::from(s.w), s.s),
                ))
            }
        }
    }

    /// Reads a location.
    pub(crate) fn read_loc(&mut self, loc: &Loc) -> Vec<B> {
        let mut out = Vec::with_capacity(loc.len());
        for alts in loc {
            let mut r = B::F;
            for (src, c) in alts {
                let v = self.read_src(*src);
                let t = self.c.and(*c, v);
                r = self.c.or(r, t);
            }
            out.push(r);
        }
        out
    }

    /// Current value of one source bit.
    pub(crate) fn read_src(&mut self, s: Src) -> B {
        match s {
            Src::Bit(b) => b,
            Src::Var(is_sig, id, bit) => {
                if let Some(ps) = &self.ps {
                    if let Some(v) = ps.cur.get(&(is_sig, id, bit)) {
                        return *v;
                    }
                }
                if is_sig {
                    let v = self.sigs[id].vars[bit];
                    match self.forced.get(&v) {
                        Some(k) => B::from_bool(*k),
                        None => B::V(v),
                    }
                } else {
                    B::F
                }
            }
        }
    }
}
