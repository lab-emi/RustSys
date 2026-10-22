// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Types of declarations and expression evaluation with the sizing rules of IEEE 1800-2017
//! clauses 11.6 to 11.8 (SYNTH-REQ-0202 to SYNTH-REQ-0206).

use rusteda_formats::verilog::ast::{self, BinOp, Expr, ExprKind, UnOp};
use rusteda_formats::verilog::number::Bit4;

use super::types::{Range, Ty};
use super::{EResult, Elaborator, ScopeId, TVal};
use crate::aig::Lit;
use crate::bits::{self, Bv};

/// Converts four-state literal bits to literals (x and z become 0, SYNTH-REQ-0201).
pub(crate) fn lit_bits(b: &[Bit4]) -> Bv {
    b.iter().map(|x| Lit::from_bool(*x == Bit4::One)).collect()
}

fn clog2(v: u64) -> u64 {
    if v <= 1 {
        0
    } else {
        64 - u64::from((v - 1).leading_zeros())
    }
}

impl<'a> Elaborator<'a> {
    // ------------------------------------------------------------ constants

    /// Evaluates a constant integral expression to an `i64`.
    pub(crate) fn eval_const_int(&mut self, scope: ScopeId, e: &Expr) -> EResult<i64> {
        let (v, t) = self.eval_self(scope, e)?;
        bits::const_i64(&v, t.is_signed())
            .ok_or_else(|| self.err("S0121", e.span, "expression is not a constant integer"))
    }

    /// Evaluates a constant expression as a typed value.
    pub(crate) fn eval_const(
        &mut self,
        scope: ScopeId,
        e: &Expr,
        target: Option<&Ty>,
    ) -> EResult<TVal> {
        let v = match target {
            Some(t) => self.eval_assign(scope, e, t)?,
            None => {
                // Unpacked values (for example an unpacked parameter passed on) keep their type.
                let t = self.expr_ty(scope, e)?;
                if t.is_packed() {
                    let (v, t) = self.eval_self(scope, e)?;
                    TVal::Packed(v, t)
                } else {
                    self.eval_assign(scope, e, &t)?
                }
            }
        };
        let is_const = match &v {
            TVal::Packed(b, _) => b.iter().all(|l| l.is_const()),
            TVal::Unpacked(es, _) => es.iter().flatten().all(|l| l.is_const()),
        };
        if !is_const {
            return Err(self.err("S0122", e.span, "expression is not constant"));
        }
        Ok(v)
    }

    // ------------------------------------------------------------ evaluation

    /// Self-determined evaluation.
    pub(crate) fn eval_self(&mut self, scope: ScopeId, e: &Expr) -> EResult<(Bv, Ty)> {
        let t = self.packed_ty(scope, e)?;
        let v = self.eval(scope, e, t.width(), t.is_signed())?;
        Ok((v, t))
    }

    /// Boolean value of an expression (non-zero).
    pub(crate) fn eval_bool(&mut self, scope: ScopeId, e: &Expr) -> EResult<Lit> {
        let (v, _) = self.eval_self(scope, e)?;
        Ok(bits::red_or(&mut self.aig, &v))
    }

    /// Context-determined evaluation to `w` bits with propagated signedness `s`.
    pub(crate) fn eval(&mut self, scope: ScopeId, e: &Expr, w: u32, s: bool) -> EResult<Bv> {
        let wu = w as usize;
        Ok(match &e.kind {
            ExprKind::Number(l) => {
                let b = l.bits().map_err(|m| self.err("S0130", e.span, m))?;
                if b.contains(&Bit4::Z) {
                    self.warn_z(e.span);
                }
                bits::resize(&lit_bits(&b), wu, s)
            }
            ExprKind::UnbasedUnsized(c) => {
                if *c == 'z' {
                    self.warn_z(e.span);
                }
                vec![Lit::from_bool(*c == '1'); wu]
            }
            ExprKind::Str(bytes) => {
                let mut v = Vec::with_capacity(bytes.len() * 8);
                for byte in bytes.iter().rev() {
                    for k in 0..8 {
                        v.push(Lit::from_bool((byte >> k) & 1 == 1));
                    }
                }
                if v.is_empty() {
                    v = vec![Lit::FALSE; 8];
                }
                bits::resize(&v, wu, s)
            }
            ExprKind::Ident(_)
            | ExprKind::Index(..)
            | ExprKind::Slice { .. }
            | ExprKind::Member(..) => match self.eval_typed(scope, e)? {
                TVal::Packed(v, _) => bits::resize(&v, wu, s),
                TVal::Unpacked(..) => {
                    return Err(self.err(
                        "S0143",
                        e.span,
                        "an unpacked array is used where a packed value is required",
                    ));
                }
            },
            ExprKind::Unary(op, a) => match op {
                UnOp::Plus => self.eval(scope, a, w, s)?,
                UnOp::Minus => {
                    let v = self.eval(scope, a, w, s)?;
                    bits::neg(&mut self.aig, &v)
                }
                UnOp::BitNot => bits::not(&self.eval(scope, a, w, s)?),
                UnOp::LogNot => {
                    let b = self.eval_bool(scope, a)?;
                    bits::resize(&[!b], wu, false)
                }
                _ => {
                    let (v, _) = self.eval_self(scope, a)?;
                    let g = &mut self.aig;
                    let r = match op {
                        UnOp::RedAnd => bits::red_and(g, &v),
                        UnOp::RedNand => !bits::red_and(g, &v),
                        UnOp::RedOr => bits::red_or(g, &v),
                        UnOp::RedNor => !bits::red_or(g, &v),
                        UnOp::RedXor => bits::red_xor(g, &v),
                        _ => !bits::red_xor(g, &v),
                    };
                    bits::resize(&[r], wu, false)
                }
            },
            ExprKind::Binary(op, a, b) => self.eval_binary(scope, *op, a, b, w, s, e)?,
            ExprKind::Cond(c, a, b) => {
                let cl = self.eval_bool(scope, c)?;
                let t = self.eval(scope, a, w, s)?;
                let f = self.eval(scope, b, w, s)?;
                self.select_sums(cl, &t, &f)
                    .unwrap_or_else(|| bits::mux(&mut self.aig, cl, &t, &f))
            }
            ExprKind::Inside(a, items) => {
                let r = self.eval_inside(scope, a, items)?;
                bits::resize(&[r], wu, false)
            }
            ExprKind::Concat(items) => {
                let v = self.eval_concat(scope, items)?;
                bits::resize(&v, wu, false)
            }
            ExprKind::Repl(n, items) => {
                let k = self.eval_const_int(scope, n)?.max(0) as usize;
                let one = self.eval_concat(scope, items)?;
                let mut v = Vec::with_capacity(one.len() * k);
                for _ in 0..k {
                    v.extend_from_slice(&one);
                }
                bits::resize(&v, wu, false)
            }
            ExprKind::Pattern { ty: Some(t), .. } => {
                let ty = self.resolve_type(scope, t, false)?;
                match self.eval_assign(scope, e, &ty)? {
                    TVal::Packed(v, _) => bits::resize(&v, wu, s),
                    TVal::Unpacked(..) => {
                        return Err(self.err(
                            "S0143",
                            e.span,
                            "unpacked pattern used as a packed value",
                        ));
                    }
                }
            }
            ExprKind::Call { func, args } => {
                let tv = self.call(scope, func, args, e.span)?;
                match tv {
                    Some(TVal::Packed(v, _)) => bits::resize(&v, wu, s),
                    _ => {
                        return Err(self.err(
                            "S0139",
                            e.span,
                            "function does not return a packed value",
                        ));
                    }
                }
            }
            ExprKind::SysCall { name, args } => {
                let v = self.eval_syscall(scope, name, args, e)?;
                bits::resize(&v, wu, s)
            }
            ExprKind::Cast { target, expr } => {
                let v = self.eval_cast(scope, target, expr)?;
                bits::resize(&v, wu, s)
            }
            _ => {
                // Report the precise reason through the type computation.
                self.expr_ty(scope, e)?;
                return Err(self.err("S0142", e.span, "expression is not supported here"));
            }
        })
    }

    fn eval_concat(&mut self, scope: ScopeId, items: &[Expr]) -> EResult<Bv> {
        let mut v = Vec::new();
        // Items are listed most significant first.
        for it in items.iter().rev() {
            // A zero replication inside a concatenation contributes nothing.
            if let ExprKind::Repl(n, _) = &it.kind {
                if self.eval_const_int(scope, n)? == 0 {
                    continue;
                }
            }
            let (x, _) = self.eval_self(scope, it)?;
            v.extend_from_slice(&x);
        }
        Ok(v)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_binary(
        &mut self,
        scope: ScopeId,
        op: BinOp,
        a: &Expr,
        b: &Expr,
        w: u32,
        s: bool,
        e: &Expr,
    ) -> EResult<Bv> {
        let wu = w as usize;
        Ok(match op {
            BinOp::Add | BinOp::Sub => {
                let x = self.eval(scope, a, w, s)?;
                let y = self.eval(scope, b, w, s)?;
                self.add_sub(&x, &y, op == BinOp::Sub)
            }
            BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::BitXnor => {
                let x = self.eval(scope, a, w, s)?;
                let y = self.eval(scope, b, w, s)?;
                let g = &mut self.aig;
                match op {
                    BinOp::Mul if s => bits::mul_signed(g, &x, &y),
                    BinOp::Mul => bits::mul(g, &x, &y),
                    BinOp::Div => bits::divmod(g, &x, &y, s).0,
                    BinOp::Mod => bits::divmod(g, &x, &y, s).1,
                    BinOp::BitAnd => bits::and(g, &x, &y),
                    BinOp::BitOr => bits::or(g, &x, &y),
                    BinOp::BitXor => bits::xor(g, &x, &y),
                    _ => bits::not(&bits::xor(g, &x, &y)),
                }
            }
            BinOp::Shl | BinOp::Shr | BinOp::AShl | BinOp::AShr => {
                let x = self.eval(scope, a, w, s)?;
                let (amt, _) = self.eval_self(scope, b)?;
                let left = matches!(op, BinOp::Shl | BinOp::AShl);
                let fill = if op == BinOp::AShr && s {
                    x.last().copied().unwrap_or(Lit::FALSE)
                } else {
                    Lit::FALSE
                };
                bits::shift(&mut self.aig, &x, &amt, left, fill)
            }
            BinOp::Pow => self.eval_pow(scope, a, b, w, s, e)?,
            BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::CaseEq
            | BinOp::CaseNe => {
                let ta = self.packed_ty(scope, a)?;
                let tb = self.packed_ty(scope, b)?;
                let cw = ta.width().max(tb.width());
                let cs = ta.is_signed() && tb.is_signed();
                let x = self.eval(scope, a, cw, cs)?;
                let y = self.eval(scope, b, cw, cs)?;
                let g = &mut self.aig;
                let r = match op {
                    BinOp::Lt => bits::lt(g, &x, &y, cs),
                    BinOp::Gt => bits::lt(g, &y, &x, cs),
                    BinOp::Le => !bits::lt(g, &y, &x, cs),
                    BinOp::Ge => !bits::lt(g, &x, &y, cs),
                    BinOp::Eq | BinOp::CaseEq => bits::eq(g, &x, &y),
                    _ => !bits::eq(g, &x, &y),
                };
                bits::resize(&[r], wu, false)
            }
            BinOp::WildEq | BinOp::WildNe => {
                let r = self.eval_wild_eq(scope, a, b)?;
                let r = if op == BinOp::WildEq { r } else { !r };
                bits::resize(&[r], wu, false)
            }
            BinOp::LogAnd | BinOp::LogOr | BinOp::Implies | BinOp::Equiv => {
                let x = self.eval_bool(scope, a)?;
                let y = self.eval_bool(scope, b)?;
                let g = &mut self.aig;
                let r = match op {
                    BinOp::LogAnd => g.and(x, y),
                    BinOp::LogOr => g.or(x, y),
                    BinOp::Implies => g.or(!x, y),
                    _ => g.xnor(x, y),
                };
                bits::resize(&[r], wu, false)
            }
        })
    }

    /// Structural mask of wildcard bits of an equality operand.
    fn wildcard_mask(
        &mut self,
        scope: ScopeId,
        e: &Expr,
        w: u32,
        s: bool,
    ) -> EResult<Option<Vec<bool>>> {
        self.wildcard_bits(scope, e, (w, s), (true, true)).map(Some)
    }

    /// `a ==? b`: bits of `b` that are x, z or ? are not compared.
    pub(crate) fn eval_wild_eq(&mut self, scope: ScopeId, a: &Expr, b: &Expr) -> EResult<Lit> {
        let ta = self.packed_ty(scope, a)?;
        let tb = self.packed_ty(scope, b)?;
        let cw = ta.width().max(tb.width());
        let cs = ta.is_signed() && tb.is_signed();
        let x = self.eval(scope, a, cw, cs)?;
        self.wildcard += 1;
        let y = self.eval(scope, b, cw, cs);
        self.wildcard -= 1;
        let y = y?;
        let mask = self.wildcard_mask(scope, b, cw, cs)?;
        let mut terms = Vec::new();
        for i in 0..cw as usize {
            if mask.as_ref().is_some_and(|m| m[i]) {
                continue;
            }
            terms.push(self.aig.xnor(x[i], y[i]));
        }
        Ok(bits::red_and(&mut self.aig, &terms))
    }

    fn eval_inside(&mut self, scope: ScopeId, a: &Expr, items: &[Expr]) -> EResult<Lit> {
        let mut any = Lit::FALSE;
        for it in items {
            let hit = match &it.kind {
                ExprKind::Range(lo, hi) => {
                    let ta = self.packed_ty(scope, a)?;
                    let tl = self.packed_ty(scope, lo)?;
                    let th = self.packed_ty(scope, hi)?;
                    let cw = ta.width().max(tl.width()).max(th.width());
                    let cs = ta.is_signed() && tl.is_signed() && th.is_signed();
                    let x = self.eval(scope, a, cw, cs)?;
                    let l = self.eval(scope, lo, cw, cs)?;
                    let h = self.eval(scope, hi, cw, cs)?;
                    let g = &mut self.aig;
                    let ge = !bits::lt(g, &x, &l, cs);
                    let le = !bits::lt(g, &h, &x, cs);
                    g.and(ge, le)
                }
                _ => self.eval_wild_eq(scope, a, it)?,
            };
            any = self.aig.or(any, hit);
        }
        Ok(any)
    }

    fn eval_pow(
        &mut self,
        scope: ScopeId,
        a: &Expr,
        b: &Expr,
        w: u32,
        s: bool,
        e: &Expr,
    ) -> EResult<Bv> {
        let wu = w as usize;
        let x = self.eval(scope, a, w, s)?;
        let (y, ty) = self.eval_self(scope, b)?;
        let bsigned = ty.is_signed();
        if let Some(ev) = bits::const_i64(&y, bsigned) {
            let xa = bits::const_i64(&x, s);
            // Constant exponent: repeated multiplication, with the standard's rules for
            // negative exponents.
            if ev < 0 {
                let r: i64 = match xa {
                    Some(1) => 1,
                    Some(-1) => {
                        if ev % 2 == 0 {
                            1
                        } else {
                            -1
                        }
                    }
                    Some(0) => 0,
                    Some(_) => 0,
                    None => {
                        return Err(self.err(
                            "S0160",
                            e.span,
                            "negative exponent needs a constant base",
                        ));
                    }
                };
                return Ok(bits::constant(r as u64, wu));
            }
            let mut acc = bits::constant(1, wu);
            let mut base = x;
            let mut k = ev as u64;
            while k > 0 {
                if k & 1 == 1 {
                    acc = bits::mul(&mut self.aig, &acc, &base);
                }
                k >>= 1;
                if k > 0 {
                    base = bits::mul(&mut self.aig, &base, &base);
                }
            }
            return Ok(acc);
        }
        if bits::const_u64(&x) == Some(2) {
            // 2 ** b is a shift of 1.
            let one = bits::constant(1, wu);
            return Ok(bits::shift(&mut self.aig, &one, &y, true, Lit::FALSE));
        }
        Err(self.err(
            "S0161",
            e.span,
            "power operator needs a constant exponent or a base of 2 (SYNTH-REQ-0205)",
        ))
    }

    fn eval_syscall(&mut self, scope: ScopeId, name: &str, args: &[Expr], e: &Expr) -> EResult<Bv> {
        let arg = |k: usize| args.get(k);
        let need = |this: &Self, k: usize| -> EResult<&Expr> {
            args.get(k).ok_or_else(|| {
                this.err(
                    "S0150",
                    e.span,
                    format!("{name} needs {} argument(s)", k + 1),
                )
            })
        };
        Ok(match name {
            "$signed" | "$unsigned" => {
                let (v, _) = self.eval_self(scope, need(self, 0)?)?;
                v
            }
            "$clog2" => {
                let x = need(self, 0)?;
                let (v, _) = self.eval_self(scope, x)?;
                let n = bits::const_u64(&v)
                    .ok_or_else(|| self.err("S0162", x.span, "$clog2 needs a constant argument"))?;
                bits::constant(clog2(n), 32)
            }
            "$bits" => {
                let x = need(self, 0)?;
                let w = self.bits_of(scope, x)?;
                bits::constant(u64::from(w), 32)
            }
            "$size" | "$left" | "$right" | "$high" | "$low" | "$increment" => {
                let x = need(self, 0)?;
                let dim = match arg(1) {
                    Some(d) => self.eval_const_int(scope, d)?,
                    None => 1,
                };
                let r = self.dim_of(scope, x, dim)?;
                let v: i64 = match name {
                    "$size" => i64::from(r.len()),
                    "$left" => r.left,
                    "$right" => r.right,
                    "$high" => r.hi(),
                    "$low" => r.lo(),
                    _ => {
                        if r.left >= r.right {
                            1
                        } else {
                            -1
                        }
                    }
                };
                bits::constant(v as u64, 32)
            }
            "$countones" => {
                let (v, _) = self.eval_self(scope, need(self, 0)?)?;
                bits::popcount(&mut self.aig, &v, 32)
            }
            "$onehot" | "$onehot0" => {
                let (v, _) = self.eval_self(scope, need(self, 0)?)?;
                let w = (64 - (v.len() as u64).leading_zeros()) as usize + 1;
                let c = bits::popcount(&mut self.aig, &v, w);
                let target = bits::constant(u64::from(name == "$onehot"), w);
                let eq = bits::eq(&mut self.aig, &c, &target);
                let r = if name == "$onehot0" {
                    let zero = bits::constant(0, w);
                    let z = bits::eq(&mut self.aig, &c, &zero);
                    let one = bits::constant(1, w);
                    let o = bits::eq(&mut self.aig, &c, &one);
                    self.aig.or(z, o)
                } else {
                    eq
                };
                vec![r]
            }
            "$isunknown" => vec![Lit::FALSE],
            _ => {
                return Err(self.err(
                    "S0151",
                    e.span,
                    format!("system function {name} is not supported"),
                ));
            }
        })
    }

    /// `$bits` of an expression or type argument.
    fn bits_of(&mut self, scope: ScopeId, x: &Expr) -> EResult<u32> {
        let t = match &x.kind {
            ExprKind::Type(t) => self.resolve_type(scope, t, false)?,
            _ => match self.cast_type_name(scope, x) {
                Some(t) => t,
                None => self.expr_ty(scope, x)?,
            },
        };
        let (_, elem) = t.unpacked_dims();
        Ok(elem.width() * t.element_count())
    }

    /// Range of dimension `dim` (1-based, outermost unpacked first, then packed).
    fn dim_of(&mut self, scope: ScopeId, x: &Expr, dim: i64) -> EResult<Range> {
        let t = match &x.kind {
            ExprKind::Type(t) => self.resolve_type(scope, t, false)?,
            _ => match self.cast_type_name(scope, x) {
                Some(t) => t,
                None => self.expr_ty(scope, x)?,
            },
        };
        let mut ranges = Vec::new();
        let mut cur = t;
        loop {
            match cur {
                Ty::Unpacked { elem, range } | Ty::Vector { elem, range, .. } => {
                    ranges.push(range);
                    cur = (*elem).clone();
                }
                Ty::Enum { .. } | Ty::Struct { .. } => {
                    ranges.push(Range::down(cur.width()));
                    break;
                }
                Ty::Bit { .. } => {
                    if ranges.is_empty() {
                        ranges.push(Range { left: 0, right: 0 });
                    }
                    break;
                }
            }
        }
        usize::try_from(dim - 1)
            .ok()
            .and_then(|i| ranges.get(i).copied())
            .ok_or_else(|| self.err("S0163", x.span, "dimension out of range"))
    }

    fn eval_cast(&mut self, scope: ScopeId, target: &ast::CastTarget, expr: &Expr) -> EResult<Bv> {
        let t = self.cast_ty(scope, target, expr)?;
        match target {
            ast::CastTarget::Signed | ast::CastTarget::Unsigned | ast::CastTarget::Const => {
                let (v, _) = self.eval_self(scope, expr)?;
                Ok(v)
            }
            _ => match self.eval_assign(scope, expr, &t)? {
                TVal::Packed(v, _) => Ok(v),
                TVal::Unpacked(..) => Err(self.err(
                    "S0164",
                    expr.span,
                    "casts to unpacked types are not supported",
                )),
            },
        }
    }

    /// `x + y` or `x - y` of equal-width operands (SYNTH-REQ-0407). An operand that is itself a
    /// sum built here contributes its terms, so that chained additions become one carry-save sum
    /// with a single carry-propagate adder; two plain operands use the plain adder.
    pub(crate) fn add_sub(&mut self, x: &[Lit], y: &[Lit], sub: bool) -> Bv {
        let sx = self.sums.get(x).cloned();
        let sy = self.sums.get(y).cloned();
        let mut sum = sx.as_deref().cloned().unwrap_or_else(|| bits::Sum::row(x));
        let ty = match &sy {
            Some(t) => (**t).clone(),
            None => bits::Sum::row(y),
        };
        let ty = if sub { ty.negate() } else { ty };
        sum.rows.extend(ty.rows);
        sum.add_konst(&ty.konst);
        let r = if (sx.is_none() && sy.is_none()) || sum.size() > MAX_SUM_BITS {
            let g = &mut self.aig;
            if sub {
                bits::sub(g, x, y)
            } else {
                bits::add(g, x, y, Lit::FALSE).0
            }
        } else {
            let cols = sum.columns();
            bits::add_columns(&mut self.aig, cols)
        };
        if sum.size() <= MAX_SUM_BITS && !r.iter().all(|l| l.is_const()) {
            self.sums.insert(r.clone(), std::rc::Rc::new(sum));
        }
        r
    }

    /// `c ? t : f` where both are sums built here: one sum of row-wise selections, since
    /// `c ? (t1 + .. + tn) : (f1 + .. + fn) = (c ? t1 : f1) + .. + (c ? tn : fn)` (SYNTH-REQ-0407).
    /// Rows present in both stay unselected.
    fn select_sums(&mut self, c: Lit, t: &[Lit], f: &[Lit]) -> Option<Bv> {
        let st = self.sums.get(t).cloned()?;
        let sf = self.sums.get(f).cloned()?;
        if st.konst.len() != sf.konst.len() || c.is_const() {
            return None;
        }
        let w = st.konst.len();
        let mut rest_f: Vec<Option<&Bv>> = sf.rows.iter().map(Some).collect();
        let mut out = bits::Sum {
            rows: Vec::new(),
            konst: vec![false; w],
        };
        let mut rest_t = Vec::new();
        for r in &st.rows {
            match rest_f.iter_mut().find(|q| q.is_some_and(|q| q == r)) {
                Some(q) => {
                    *q = None;
                    out.rows.push(r.clone());
                }
                None => rest_t.push(r.clone()),
            }
        }
        let rest_f: Vec<Bv> = rest_f.into_iter().flatten().cloned().collect();
        let kt: Bv = st.konst.iter().map(|&b| Lit::from_bool(b)).collect();
        let kf: Bv = sf.konst.iter().map(|&b| Lit::from_bool(b)).collect();
        let zero = bits::constant(0, w);
        let n = rest_t.len().max(rest_f.len());
        for i in 0..n {
            let a = rest_t.get(i).unwrap_or(&zero);
            let b = rest_f.get(i).unwrap_or(&zero);
            out.rows.push(bits::mux(&mut self.aig, c, a, b));
        }
        if kt != kf {
            out.rows.push(bits::mux(&mut self.aig, c, &kt, &kf));
        } else {
            out.konst = st.konst.clone();
        }
        if out.size() > MAX_SUM_BITS {
            return None;
        }
        let r = bits::add_columns(&mut self.aig, out.columns());
        if !r.iter().all(|l| l.is_const()) {
            self.sums.insert(r.clone(), std::rc::Rc::new(out));
        }
        Some(r)
    }

    /// Evaluates an expression in an assignment-like context with target type `target`.
    pub(crate) fn eval_assign(&mut self, scope: ScopeId, e: &Expr, target: &Ty) -> EResult<TVal> {
        if let ExprKind::Pattern { .. } = &e.kind {
            return self.eval_pattern(scope, e, target);
        }
        // The operands of a conditional operator in an assignment-like context take the target
        // type, so that untyped assignment patterns may appear there (SYNTH-REQ-0222).
        if let ExprKind::Cond(c, a, b) = &e.kind {
            if needs_target(a) || needs_target(b) || !target.is_packed() {
                let sel = self.eval_bool(scope, c)?;
                let va = self.eval_assign(scope, a, target)?;
                let vb = self.eval_assign(scope, b, target)?;
                return Ok(match (va, vb) {
                    (TVal::Packed(x, t), TVal::Packed(y, _)) => {
                        TVal::Packed(bits::mux(&mut self.aig, sel, &x, &y), t)
                    }
                    (TVal::Unpacked(xs, t), TVal::Unpacked(ys, _)) => TVal::Unpacked(
                        xs.iter()
                            .zip(&ys)
                            .map(|(x, y)| bits::mux(&mut self.aig, sel, x, y))
                            .collect(),
                        t,
                    ),
                    _ => unreachable!("both operands converted to the target type"),
                });
            }
        }
        if !target.is_packed() {
            let v = self.eval_typed(scope, e)?;
            return match v {
                TVal::Unpacked(elems, _) if elems.len() as u32 == target.element_count() => {
                    let (_, et) = target.unpacked_dims();
                    let ew = et.width() as usize;
                    let elems = elems
                        .into_iter()
                        .map(|x| bits::resize(&x, ew, false))
                        .collect();
                    Ok(TVal::Unpacked(elems, target.clone()))
                }
                _ => Err(self.err(
                    "S0165",
                    e.span,
                    "value does not match the unpacked array shape of the target",
                )),
            };
        }
        let tw = target.width();
        let et = self.packed_ty(scope, e)?;
        let w = tw.max(et.width());
        let v = self.eval(scope, e, w, et.is_signed())?;
        Ok(TVal::Packed(v[..tw as usize].to_vec(), target.clone()))
    }
}

/// Largest number of row bits a sum keeps as terms; larger sums are added plainly.
const MAX_SUM_BITS: usize = 1 << 13;

/// True for an untyped assignment pattern, possibly inside nested conditional operators.
fn needs_target(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Pattern { ty: None, .. } => true,
        ExprKind::Cond(_, a, b) => needs_target(a) || needs_target(b),
        _ => false,
    }
}
