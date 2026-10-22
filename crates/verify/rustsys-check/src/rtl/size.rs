// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Types and the sizing pass: every expression is annotated with its self-determined width and
//! signedness before it is evaluated (IEEE 1800-2017 11.6 to 11.8; SYNTH-REQ-0202).

use std::rc::Rc;

use rusteda_formats::verilog::ast::{
    BinOp, DataType, Dim, Expr, ExprKind, SliceKind, TypeKind, UnOp,
};

use super::ty::{Packed, Ty};
use super::{Builder, Name, R, Val};
use crate::circ::B;

/// Size annotation of an expression node and of its operands.
#[derive(Debug, Clone)]
pub(crate) struct Sz {
    pub w: u32,
    pub s: bool,
    pub k: Vec<Sz>,
}

fn leaf(w: u64, s: bool) -> Sz {
    Sz {
        w: w as u32,
        s,
        k: Vec::new(),
    }
}

impl<'a> Builder<'a> {
    /// Resolves a data type; enumeration constants are declared in `enums` if given.
    pub(crate) fn resolve(&mut self, sc: usize, dt: &DataType, enums: Option<usize>) -> R<Ty> {
        let sgn = dt.signing;
        let (base, signed) = match &dt.kind {
            TypeKind::Implicit | TypeKind::Logic | TypeKind::Reg | TypeKind::Bit => {
                (Packed::Bit, sgn.unwrap_or(false))
            }
            TypeKind::Int | TypeKind::Integer => (Ty::bits(32, true).packed, sgn.unwrap_or(true)),
            TypeKind::Shortint => (Ty::bits(16, true).packed, sgn.unwrap_or(true)),
            TypeKind::Longint => (Ty::bits(64, true).packed, sgn.unwrap_or(true)),
            TypeKind::Byte => (Ty::bits(8, true).packed, sgn.unwrap_or(true)),
            TypeKind::Time => (Ty::bits(64, false).packed, sgn.unwrap_or(false)),
            TypeKind::Enum { base, members } => {
                let bt = match base {
                    Some(b) => self.resolve(sc, b, None)?,
                    None => Ty::bits(32, true),
                };
                let w = bt.width();
                let mut next: u64 = 0;
                for m in members {
                    if m.range.is_some() {
                        return self.fail(m.name.span, "enumeration ranges are not supported");
                    }
                    let v = match &m.value {
                        Some(e) => {
                            let bits = self.assign_val(sc, e, &bt)?;
                            let Val::P(bits) = bits else {
                                return self.fail(e.span, "bad enumeration value");
                            };
                            konst_u64(&bits).ok_or_else(|| {
                                format!("{}: enumeration value is not constant", self.loc(e.span))
                            })?
                        }
                        None => next,
                    };
                    next = v.wrapping_add(1);
                    if let Some(esc) = enums {
                        let bits: Vec<B> = (0..w)
                            .map(|i| B::from_bool(i < 64 && (v >> i) & 1 == 1))
                            .collect();
                        self.put(
                            esc,
                            &m.name.name,
                            Name::Value(Rc::new((Val::P(bits), bt.clone()))),
                            m.name.span,
                        )?;
                    }
                }
                (bt.packed, bt.signed)
            }
            TypeKind::Struct {
                union,
                packed,
                members,
            } => {
                if !packed {
                    return self.fail(dt.span, "unpacked structures are not supported");
                }
                let mut ms = Vec::new();
                for m in members {
                    let t = self.resolve(sc, &m.ty, enums)?;
                    for (n, d) in &m.names {
                        if !d.is_empty() {
                            return self.fail(n.span, "unpacked member");
                        }
                        ms.push((n.name.clone(), t.packed.clone()));
                    }
                }
                let p = if *union {
                    Packed::Union(Rc::new(ms))
                } else {
                    Packed::Struct(Rc::new(ms))
                };
                (p, sgn.unwrap_or(false))
            }
            TypeKind::Named(p) => match self.find_path(sc, p)? {
                Name::Type(t) => {
                    if !t.unpacked.is_empty() && !dt.packed.is_empty() {
                        return self.fail(dt.span, "packed dimensions on an unpacked type");
                    }
                    if dt.packed.is_empty() {
                        return Ok(t);
                    }
                    (t.packed, t.signed)
                }
                _ => return self.fail(p.name.span, "not a type"),
            },
            TypeKind::TypeOf(e) => {
                let t = match &e.kind {
                    ExprKind::Type(t) => self.resolve(sc, t, None)?,
                    _ => self.type_of(sc, e)?,
                };
                return Ok(t);
            }
            _ => return self.fail(dt.span, "type not supported"),
        };
        let mut p = base;
        for d in dt.packed.iter().rev() {
            let (l, r) = self.range(sc, d, true)?;
            p = Packed::Arr {
                elem: Rc::new(p),
                left: l,
                right: r,
            };
        }
        // `signed` on a keyword type with packed dimensions applies to the whole vector; a
        // named type keeps its own signedness only without extra dimensions.
        let signed = match &dt.kind {
            TypeKind::Named(_) if !dt.packed.is_empty() => false,
            _ => signed,
        };
        Ok(Ty {
            packed: p,
            signed,
            unpacked: Vec::new(),
        })
    }

    /// Range of a dimension; `[n]` is `[n-1:0]` when packed and `[0:n-1]` when unpacked.
    pub(crate) fn range(&mut self, sc: usize, d: &Dim, packed: bool) -> R<(i64, i64)> {
        match d {
            Dim::Range(l, r) => Ok((self.cint(sc, l)?, self.cint(sc, r)?)),
            Dim::Size(n) => {
                let v = self.cint(sc, n)?;
                if v <= 0 {
                    return self.fail(n.span, "size must be positive");
                }
                Ok(if packed { (v - 1, 0) } else { (0, v - 1) })
            }
            Dim::Unsupported(sp) => self.fail(*sp, "unsupported dimension"),
        }
    }

    pub(crate) fn with_unpacked(&mut self, sc: usize, mut t: Ty, dims: &[Dim]) -> R<Ty> {
        let mut u = Vec::new();
        for d in dims {
            u.push(self.range(sc, d, false)?);
        }
        u.extend(t.unpacked);
        t.unpacked = u;
        Ok(t)
    }

    /// Constant integer value of an expression.
    pub(crate) fn cint(&mut self, sc: usize, e: &Expr) -> R<i64> {
        let sz = self.size(sc, e)?;
        let v = self.ev(sc, e, &sz, sz.w, sz.s)?;
        konst_i64(&v, sz.s).ok_or_else(|| format!("{}: not a constant integer", self.loc(e.span)))
    }

    /// Type of an identifier, select or member expression; other expressions get a plain
    /// vector type of their self-determined size.
    pub(crate) fn type_of(&mut self, sc: usize, e: &Expr) -> R<Ty> {
        match &e.kind {
            ExprKind::Ident(p) => match self.find_path(sc, p)? {
                Name::Sig(s) => Ok(self.sigs[s].ty.clone()),
                Name::Local(l) => Ok(self.locals[l].clone()),
                Name::Value(v) => Ok(v.1.clone()),
                _ => self.fail(e.span, "not a value"),
            },
            ExprKind::Index(b, _) => {
                let t = self.type_of(sc, b)?;
                match t.index_type() {
                    Some((_, _, et)) => Ok(et),
                    None => self.fail(e.span, "cannot index"),
                }
            }
            ExprKind::Slice {
                base,
                kind,
                left,
                right,
            } => {
                let t = self.type_of(sc, base)?;
                let n = match kind {
                    SliceKind::Const => {
                        (self.cint(sc, left)? - self.cint(sc, right)?).unsigned_abs() + 1
                    }
                    _ => self.cint(sc, right)? as u64,
                };
                let Some((_, _, et)) = t.index_type() else {
                    return self.fail(e.span, "cannot slice");
                };
                if t.is_array() {
                    let mut r = t.clone();
                    r.unpacked[0] = (0, n as i64 - 1);
                    Ok(r)
                } else {
                    Ok(Ty {
                        packed: Packed::Arr {
                            elem: Rc::new(et.packed),
                            left: n as i64 - 1,
                            right: 0,
                        },
                        signed: false,
                        unpacked: Vec::new(),
                    })
                }
            }
            ExprKind::Member(b, f) => {
                let t = self.type_of(sc, b)?;
                if t.is_array() {
                    return self.fail(e.span, "member of an array");
                }
                match t.packed.member(&f.name) {
                    Some((_, p)) => Ok(Ty {
                        packed: p,
                        signed: false,
                        unpacked: Vec::new(),
                    }),
                    None => self.fail(f.span, format!("no member {}", f.name)),
                }
            }
            ExprKind::Pattern { ty: Some(t), .. } => self.resolve(sc, t, None),
            ExprKind::Cast {
                target: rusteda_formats::verilog::ast::CastTarget::Type(t),
                ..
            } => self.resolve(sc, t, None),
            ExprKind::Cast {
                target: rusteda_formats::verilog::ast::CastTarget::Expr(x),
                ..
            } if self.is_type_name(sc, x) => {
                let ExprKind::Ident(p) = &x.kind else {
                    unreachable!()
                };
                match self.find_path(sc, p)? {
                    Name::Type(t) => Ok(t),
                    _ => unreachable!(),
                }
            }
            ExprKind::Call { func, .. } => match self.find_path(sc, func)? {
                Name::Func(f, fsc) => match &f.ret {
                    Some(rt) => self.resolve(fsc, rt, None),
                    None => self.fail(e.span, "void function used as a value"),
                },
                _ => self.fail(e.span, "not a function"),
            },
            _ => {
                let s = self.size(sc, e)?;
                Ok(Ty::bits(u64::from(s.w), s.s))
            }
        }
    }

    pub(crate) fn is_type_name(&self, sc: usize, x: &Expr) -> bool {
        match &x.kind {
            ExprKind::Ident(p) => matches!(self.find_path(sc, p), Ok(Name::Type(_))),
            _ => false,
        }
    }

    /// Sizing pass.
    pub(crate) fn size(&mut self, sc: usize, e: &Expr) -> R<Sz> {
        self.depth += 1;
        if self.depth > 2000 {
            self.depth -= 1;
            return self.fail(e.span, "expression too deep");
        }
        let r = self.size_inner(sc, e);
        self.depth -= 1;
        r
    }

    fn size_inner(&mut self, sc: usize, e: &Expr) -> R<Sz> {
        Ok(match &e.kind {
            ExprKind::Number(l) => leaf(
                u64::from(
                    l.width()
                        .map_err(|m| format!("{}: {m}", self.loc(e.span)))?,
                ),
                l.is_signed(),
            ),
            ExprKind::UnbasedUnsized(_) => leaf(1, false),
            ExprKind::Str(s) => leaf(8 * s.len().max(1) as u64, false),
            ExprKind::Ident(_)
            | ExprKind::Index(..)
            | ExprKind::Slice { .. }
            | ExprKind::Member(..) => {
                let t = self.type_of(sc, e)?;
                if t.is_array() {
                    return self.fail(e.span, "unpacked array used as a value");
                }
                leaf(t.width(), t.signed)
            }
            ExprKind::Unary(op, a) => {
                let sa = self.size(sc, a)?;
                match op {
                    UnOp::Plus | UnOp::Minus | UnOp::BitNot => Sz {
                        w: sa.w,
                        s: sa.s,
                        k: vec![sa],
                    },
                    _ => Sz {
                        w: 1,
                        s: false,
                        k: vec![sa],
                    },
                }
            }
            ExprKind::Binary(op, a, b) => {
                let sa = self.size(sc, a)?;
                let sb = self.size(sc, b)?;
                let (w, s) = match op {
                    BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Mod
                    | BinOp::BitAnd
                    | BinOp::BitOr
                    | BinOp::BitXor
                    | BinOp::BitXnor => (sa.w.max(sb.w), sa.s && sb.s),
                    BinOp::Pow | BinOp::Shl | BinOp::Shr | BinOp::AShl | BinOp::AShr => {
                        (sa.w, sa.s)
                    }
                    _ => (1, false),
                };
                Sz {
                    w,
                    s,
                    k: vec![sa, sb],
                }
            }
            ExprKind::Cond(c, a, b) => {
                let sc_ = self.size(sc, c)?;
                let sa = self.size(sc, a)?;
                let sb = self.size(sc, b)?;
                Sz {
                    w: sa.w.max(sb.w),
                    s: sa.s && sb.s,
                    k: vec![sc_, sa, sb],
                }
            }
            ExprKind::Inside(a, items) => {
                let mut k = vec![self.size(sc, a)?];
                for it in items {
                    k.push(match &it.kind {
                        ExprKind::Range(lo, hi) => {
                            let l = self.size(sc, lo)?;
                            let h = self.size(sc, hi)?;
                            Sz {
                                w: l.w.max(h.w),
                                s: l.s && h.s,
                                k: vec![l, h],
                            }
                        }
                        _ => self.size(sc, it)?,
                    });
                }
                Sz { w: 1, s: false, k }
            }
            ExprKind::Concat(items) => {
                let mut k = Vec::new();
                let mut w = 0u32;
                for it in items {
                    // A zero replication operand contributes nothing (SYNTH-REQ-0221).
                    let zero = match &it.kind {
                        ExprKind::Repl(n, _) => self.cint(sc, n)? == 0,
                        _ => false,
                    };
                    let s = if zero {
                        Sz {
                            w: 0,
                            s: false,
                            k: Vec::new(),
                        }
                    } else {
                        self.size(sc, it)?
                    };
                    w += s.w;
                    k.push(s);
                }
                if w == 0 {
                    return self.fail(e.span, "concatenation without an operand of positive size");
                }
                Sz { w, s: false, k }
            }
            ExprKind::Repl(n, items) => {
                let cnt = self.cint(sc, n)?;
                if cnt <= 0 {
                    return self.fail(n.span, "replication count must be positive");
                }
                let mut k = Vec::new();
                let mut w = 0u32;
                for it in items {
                    let s = self.size(sc, it)?;
                    w += s.w;
                    k.push(s);
                }
                Sz {
                    w: w * cnt as u32,
                    s: false,
                    k,
                }
            }
            ExprKind::Pattern { ty: Some(_), .. } | ExprKind::Call { .. } => {
                let t = self.type_of(sc, e)?;
                if t.is_array() {
                    return self.fail(e.span, "unpacked value used as a packed value");
                }
                leaf(t.width(), t.signed)
            }
            ExprKind::Pattern { ty: None, .. } => {
                return self.fail(e.span, "assignment pattern without a type context");
            }
            ExprKind::SysCall { name, args } => match name.as_str() {
                "$signed" | "$unsigned" => {
                    let a = args
                        .first()
                        .ok_or_else(|| format!("{}: missing argument", self.loc(e.span)))?;
                    let s = self.size(sc, a)?;
                    Sz {
                        w: s.w,
                        s: name == "$signed",
                        k: vec![s],
                    }
                }
                "$clog2" | "$bits" | "$size" | "$left" | "$right" | "$high" | "$low"
                | "$countones" | "$increment" => leaf(32, true),
                "$onehot" | "$onehot0" | "$isunknown" => leaf(1, false),
                _ => return self.fail(e.span, format!("{name} not supported")),
            },
            ExprKind::Cast { target, expr } => {
                use rusteda_formats::verilog::ast::CastTarget as C;
                let inner = self.size(sc, expr)?;
                match target {
                    C::Type(t) => {
                        let ty = self.resolve(sc, t, None)?;
                        Sz {
                            w: ty.width() as u32,
                            s: ty.signed,
                            k: vec![inner],
                        }
                    }
                    C::Expr(x) if self.is_type_name(sc, x) => {
                        let ty = self.type_of(sc, e)?;
                        Sz {
                            w: ty.width() as u32,
                            s: ty.signed,
                            k: vec![inner],
                        }
                    }
                    C::Expr(x) => {
                        let n = self.cint(sc, x)?;
                        if n <= 0 {
                            return self.fail(x.span, "cast size must be positive");
                        }
                        Sz {
                            w: n as u32,
                            s: inner.s,
                            k: vec![inner],
                        }
                    }
                    C::Signed => Sz {
                        w: inner.w,
                        s: true,
                        k: vec![inner],
                    },
                    C::Unsigned => Sz {
                        w: inner.w,
                        s: false,
                        k: vec![inner],
                    },
                    C::Const => Sz {
                        w: inner.w,
                        s: inner.s,
                        k: vec![inner],
                    },
                }
            }
            _ => return self.fail(e.span, "expression not supported"),
        })
    }
}

/// Constant value of bits (zero-extended) if all constant and within 64 bits.
pub(crate) fn konst_u64(v: &[B]) -> Option<u64> {
    let mut r = 0u64;
    for (i, b) in v.iter().enumerate() {
        match b.konst()? {
            true if i >= 64 => return None,
            true => r |= 1 << i,
            false => {}
        }
    }
    Some(r)
}

/// Constant signed or unsigned value if it fits an `i64`.
pub(crate) fn konst_i64(v: &[B], signed: bool) -> Option<i64> {
    let bits: Vec<bool> = v.iter().map(|b| b.konst()).collect::<Option<_>>()?;
    let top = signed && bits.last().copied().unwrap_or(false);
    let mut r: i128 = 0;
    for (i, b) in bits.iter().enumerate() {
        if *b {
            if i >= 64 {
                if !(top) {
                    return None;
                }
                continue;
            }
            r |= 1i128 << i;
        }
    }
    if top {
        // Two's complement of the full width.
        if bits.len() < 64 {
            r -= 1i128 << bits.len();
        } else {
            r = i128::from((r as u64) as i64);
        }
    }
    i64::try_from(r).ok()
}
