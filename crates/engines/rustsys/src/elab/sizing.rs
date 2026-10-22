// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Type resolution and self-determined types of expressions (SYNTH-REQ-0202, SYNTH-REQ-0208).

use std::rc::Rc;

use rusteda_formats::verilog::ast::{self, BinOp, DataType, Dim, Expr, ExprKind, TypeKind, UnOp};

use super::types::{EnumInfo, Range, Ty, layout_struct};
use super::{EResult, Elaborator, ScopeId, Symbol, TVal};
use crate::bits;

impl<'a> Elaborator<'a> {
    // ------------------------------------------------------------------ types

    /// Resolves a data type. Enumeration constants are declared in `scope` when `declare_enums`.
    pub(crate) fn resolve_type(
        &mut self,
        scope: ScopeId,
        dt: &DataType,
        declare_enums: bool,
    ) -> EResult<Ty> {
        let signing = dt.signing;
        let base = match &dt.kind {
            TypeKind::Implicit | TypeKind::Logic | TypeKind::Reg | TypeKind::Bit => Ty::Bit {
                signed: signing.unwrap_or(false) && dt.packed.is_empty(),
            },
            TypeKind::Integer | TypeKind::Int => Ty::vec(32, signing.unwrap_or(true)),
            TypeKind::Shortint => Ty::vec(16, signing.unwrap_or(true)),
            TypeKind::Longint => Ty::vec(64, signing.unwrap_or(true)),
            TypeKind::Byte => Ty::vec(8, signing.unwrap_or(true)),
            TypeKind::Time => Ty::vec(64, signing.unwrap_or(false)),
            TypeKind::Enum { base, members } => {
                let bt = match base {
                    Some(b) => self.resolve_type(scope, b, false)?,
                    None => Ty::int(),
                };
                let w = bt.width();
                let mut vals = Vec::new();
                let mut next: u64 = 0;
                let mut prev_set = false;
                for m in members {
                    if m.range.is_some() {
                        return Err(self.err(
                            "S0111",
                            m.name.span,
                            "enumeration name ranges are not supported",
                        ));
                    }
                    let v = match &m.value {
                        Some(e) => {
                            let tv = self.eval_assign(scope, e, &bt)?;
                            let TVal::Packed(bv, _) = tv else {
                                unreachable!("packed base")
                            };
                            bits::const_u64(&bits::resize(&bv, w.min(64) as usize, false))
                                .ok_or_else(|| {
                                    self.err("S0112", e.span, "enumeration value is not constant")
                                })?
                        }
                        None => {
                            if prev_set && w < 64 && next >= 1u64 << w {
                                return Err(self.err(
                                    "S0113",
                                    m.name.span,
                                    "enumeration value overflows the base type",
                                ));
                            }
                            next
                        }
                    };
                    prev_set = true;
                    next = v.wrapping_add(1);
                    vals.push((m.name.name.clone(), v));
                }
                let info = Rc::new(EnumInfo {
                    members: vals.clone(),
                });
                let ty = Ty::Enum {
                    base: Rc::new(bt),
                    info,
                };
                if declare_enums {
                    for (m, (name, v)) in members.iter().zip(&vals) {
                        let bv = bits::constant(*v, w as usize);
                        self.declare(
                            scope,
                            name,
                            Symbol::Const(Rc::new(TVal::Packed(bv, ty.clone()))),
                            m.name.span,
                        )?;
                    }
                }
                ty
            }
            TypeKind::Struct {
                union,
                packed,
                members,
            } => {
                if !packed {
                    return Err(self.err(
                        "S0114",
                        dt.span,
                        "unpacked structures and unions are not supported",
                    ));
                }
                let mut fs = Vec::new();
                for m in members {
                    let t = self.resolve_type(scope, &m.ty, declare_enums)?;
                    for (n, dims) in &m.names {
                        if !dims.is_empty() {
                            return Err(self.err(
                                "S0115",
                                n.span,
                                "unpacked dimensions are not allowed in packed structures",
                            ));
                        }
                        fs.push((n.name.clone(), t.clone()));
                    }
                }
                Ty::Struct {
                    fields: Rc::new(layout_struct(fs, *union)),
                    signed: signing.unwrap_or(false),
                    union: *union,
                }
            }
            TypeKind::Named(path) => match self.lookup_path(scope, path)? {
                Symbol::Type(t) => t,
                _ => {
                    return Err(self.err(
                        "S0116",
                        path.name.span,
                        format!("'{}' is not a type", path.name.name),
                    ));
                }
            },
            TypeKind::TypeOf(e) => match &e.kind {
                ExprKind::Type(t) => self.resolve_type(scope, t, false)?,
                _ => self.expr_ty(scope, e)?,
            },
            other => {
                return Err(self.err(
                    "S0117",
                    dt.span,
                    format!("type {other:?} is not supported for synthesis"),
                ));
            }
        };
        let mut t = base;
        for (k, d) in dt.packed.iter().enumerate().rev() {
            let range = self.packed_range(scope, d)?;
            // Signedness written with a keyword type applies to the whole packed array.
            let signed = k == 0
                && signing.unwrap_or(false)
                && matches!(
                    dt.kind,
                    TypeKind::Implicit | TypeKind::Logic | TypeKind::Reg | TypeKind::Bit
                );
            t = Ty::Vector {
                elem: Rc::new(t),
                range,
                signed,
            };
        }
        if t.width() > 1 << 24 {
            return Err(self.err("S0118", dt.span, "packed type is too wide"));
        }
        Ok(t)
    }

    pub(crate) fn packed_range(&mut self, scope: ScopeId, d: &Dim) -> EResult<Range> {
        match d {
            Dim::Range(l, r) => Ok(Range {
                left: self.eval_const_int(scope, l)?,
                right: self.eval_const_int(scope, r)?,
            }),
            Dim::Size(n) => {
                let v = self.eval_const_int(scope, n)?;
                if v <= 0 {
                    return Err(self.err("S0119", n.span, "dimension size must be positive"));
                }
                Ok(Range {
                    left: v - 1,
                    right: 0,
                })
            }
            Dim::Unsupported(sp) => Err(self.err(
                "S0110",
                *sp,
                "dynamic, queue and associative dimensions are not supported",
            )),
        }
    }

    /// Applies unpacked dimensions to a type.
    pub(crate) fn apply_unpacked(
        &mut self,
        scope: ScopeId,
        mut t: Ty,
        dims: &[Dim],
    ) -> EResult<Ty> {
        for d in dims.iter().rev() {
            let range = match d {
                Dim::Size(n) => {
                    let v = self.eval_const_int(scope, n)?;
                    if v <= 0 {
                        return Err(self.err("S0119", n.span, "dimension size must be positive"));
                    }
                    Range::up(v as u32)
                }
                other => self.packed_range(scope, other)?,
            };
            t = Ty::Unpacked {
                elem: Rc::new(t),
                range,
            };
        }
        Ok(t)
    }

    // --------------------------------------------------------- static types

    /// Self-determined type of an expression.
    pub(crate) fn expr_ty(&mut self, scope: ScopeId, e: &Expr) -> EResult<Ty> {
        Ok(match &e.kind {
            ExprKind::Number(l) => {
                let w = l.width().map_err(|m| self.err("S0130", e.span, m))?;
                Ty::vec(w, l.is_signed())
            }
            ExprKind::UnbasedUnsized(_) => Ty::Bit { signed: false },
            ExprKind::Str(s) => Ty::vec(8 * (s.len().max(1) as u32), false),
            ExprKind::Real(_) => {
                return Err(self.err("S0131", e.span, "real values are not supported"));
            }
            ExprKind::Ident(p) => match self.lookup_path(scope, p)? {
                Symbol::Signal(s) => self.signals[s as usize].ty.clone(),
                Symbol::Local(l) => self.locals[l as usize].ty.clone(),
                Symbol::Const(v) => v.ty().clone(),
                Symbol::Genvar => {
                    return Err(self.err("S0132", e.span, "genvar used outside of its loop"));
                }
                Symbol::Type(_) => {
                    return Err(self.err("S0133", e.span, "a type is used as a value"));
                }
                Symbol::Func(_) | Symbol::Task(_) => {
                    return Err(self.err("S0134", e.span, "subroutine name used without a call"));
                }
                Symbol::Scope(_) => {
                    return Err(self.err(
                        "S0135",
                        e.span,
                        "hierarchical references are not supported",
                    ));
                }
            },
            ExprKind::Index(..) | ExprKind::Slice { .. } | ExprKind::Member(..) => {
                self.select_ty(scope, e)?
            }
            ExprKind::Unary(op, a) => match op {
                UnOp::Plus | UnOp::Minus | UnOp::BitNot => {
                    let t = self.packed_ty(scope, a)?;
                    Ty::vec(t.width(), t.is_signed())
                }
                _ => Ty::Bit { signed: false },
            },
            ExprKind::Binary(op, a, b) => match op {
                BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::BitXnor => {
                    let ta = self.packed_ty(scope, a)?;
                    let tb = self.packed_ty(scope, b)?;
                    Ty::vec(ta.width().max(tb.width()), ta.is_signed() && tb.is_signed())
                }
                BinOp::Pow | BinOp::Shl | BinOp::Shr | BinOp::AShl | BinOp::AShr => {
                    let ta = self.packed_ty(scope, a)?;
                    Ty::vec(ta.width(), ta.is_signed())
                }
                _ => Ty::Bit { signed: false },
            },
            ExprKind::Cond(_, a, b) => {
                let ta = self.packed_ty(scope, a)?;
                let tb = self.packed_ty(scope, b)?;
                if ta == tb && !matches!(ta, Ty::Vector { .. }) {
                    ta
                } else {
                    Ty::vec(ta.width().max(tb.width()), ta.is_signed() && tb.is_signed())
                }
            }
            ExprKind::Inside(..) => Ty::Bit { signed: false },
            ExprKind::Concat(items) => {
                let mut w = 0u32;
                for i in items {
                    // SYNTH-REQ-0221: a zero replication operand has size zero.
                    if let ExprKind::Repl(n, _) = &i.kind {
                        if self.eval_const_int(scope, n)? == 0 {
                            continue;
                        }
                    }
                    w += self.packed_ty(scope, i)?.width();
                }
                if w == 0 {
                    return Err(self.err(
                        "S0137",
                        e.span,
                        "a concatenation needs an operand of positive size (SYNTH-REQ-0221)",
                    ));
                }
                Ty::vec(w, false)
            }
            ExprKind::Repl(n, items) => {
                let k = self.eval_const_int(scope, n)?;
                if k < 0 {
                    return Err(self.err("S0136", n.span, "negative replication count"));
                }
                let mut w = 0u32;
                for i in items {
                    w += self.packed_ty(scope, i)?.width();
                }
                if k == 0 {
                    return Err(self.err("S0137", n.span, "zero replication is only allowed inside a concatenation with other operands"));
                }
                Ty::vec(w.saturating_mul(k as u32), false)
            }
            ExprKind::Pattern { ty: Some(t), .. } => self.resolve_type(scope, t, false)?,
            ExprKind::Pattern { ty: None, .. } => {
                return Err(self.err(
                    "S0138",
                    e.span,
                    "assignment pattern needs a type from its context",
                ));
            }
            ExprKind::Call { func, .. } => match self.lookup_path(scope, func)? {
                Symbol::Func(f) => match &f.decl.ret {
                    Some(rt) => self.resolve_type(f.scope, rt, false)?,
                    None => return Err(self.err("S0139", e.span, "void function used as a value")),
                },
                _ => {
                    return Err(self.err(
                        "S0140",
                        func.name.span,
                        format!("'{}' is not a function", func.name.name),
                    ));
                }
            },
            ExprKind::SysCall { name, args } => self.syscall_ty(scope, name, args, e)?,
            ExprKind::Cast { target, expr } => self.cast_ty(scope, target, expr)?,
            ExprKind::Type(_) => {
                return Err(self.err("S0133", e.span, "a type is used as a value"));
            }
            ExprKind::Range(..) => {
                return Err(self.err("S0141", e.span, "range is only allowed in inside lists"));
            }
            ExprKind::Unsupported(w) => {
                return Err(self.err(
                    "S0142",
                    e.span,
                    format!("'{w}' is not supported for synthesis"),
                ));
            }
        })
    }

    /// Type of an operand that must be packed.
    pub(crate) fn packed_ty(&mut self, scope: ScopeId, e: &Expr) -> EResult<Ty> {
        let t = self.expr_ty(scope, e)?;
        if !t.is_packed() {
            return Err(self.err(
                "S0143",
                e.span,
                "an unpacked array is used where a packed value is required",
            ));
        }
        Ok(t)
    }

    pub(crate) fn syscall_ty(
        &mut self,
        scope: ScopeId,
        name: &str,
        args: &[Expr],
        e: &Expr,
    ) -> EResult<Ty> {
        Ok(match name {
            "$signed" | "$unsigned" => {
                let a = args.first().ok_or_else(|| {
                    self.err("S0150", e.span, format!("{name} needs an argument"))
                })?;
                let t = self.packed_ty(scope, a)?;
                Ty::vec(t.width(), name == "$signed")
            }
            "$clog2" | "$bits" | "$size" | "$left" | "$right" | "$high" | "$low" | "$countones"
            | "$dimensions" | "$increment" => Ty::int(),
            "$onehot" | "$onehot0" | "$isunknown" => Ty::Bit { signed: false },
            _ => {
                return Err(self.err(
                    "S0151",
                    e.span,
                    format!("system function {name} is not supported"),
                ));
            }
        })
    }

    pub(crate) fn cast_ty(
        &mut self,
        scope: ScopeId,
        target: &ast::CastTarget,
        expr: &Expr,
    ) -> EResult<Ty> {
        Ok(match target {
            ast::CastTarget::Type(t) => self.resolve_type(scope, t, false)?,
            ast::CastTarget::Expr(x) => match self.cast_type_name(scope, x) {
                Some(t) => t,
                None => {
                    let n = self.eval_const_int(scope, x)?;
                    if n <= 0 {
                        return Err(self.err("S0152", x.span, "cast size must be positive"));
                    }
                    let s = self.packed_ty(scope, expr)?.is_signed();
                    Ty::vec(n as u32, s)
                }
            },
            ast::CastTarget::Signed | ast::CastTarget::Unsigned => {
                let t = self.packed_ty(scope, expr)?;
                Ty::vec(t.width(), matches!(target, ast::CastTarget::Signed))
            }
            ast::CastTarget::Const => self.expr_ty(scope, expr)?,
        })
    }

    /// If a cast prefix names a type, returns it.
    pub(crate) fn cast_type_name(&self, scope: ScopeId, x: &Expr) -> Option<Ty> {
        if let ExprKind::Ident(p) = &x.kind {
            if let Ok(Symbol::Type(t)) = self.lookup_path(scope, p) {
                return Some(t);
            }
        }
        None
    }
}
