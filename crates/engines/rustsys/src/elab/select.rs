// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Selects, assignment targets and assignment patterns (SYNTH-REQ-0206, SYNTH-REQ-0208).

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{Expr, ExprKind, SliceKind};

use super::types::{Range, Ty};
use super::{EResult, Elaborator, ScopeId, Symbol, TVal, VarRef};
use crate::aig::{Aig, Lit};
use crate::bits::{self, Bv};

/// A position that is either a constant or a vector of literals (unsigned; values at or above
/// the number of positions mean "out of range").
#[derive(Debug, Clone)]
pub(crate) enum Pos {
    Const(Option<u32>),
    Dyn(Bv),
}

/// One part of an assignment target.
#[derive(Debug, Clone)]
pub(crate) struct LvPiece {
    pub var: VarRef,
    /// Element positions (left bound first) of an unpacked variable: all elements, or one.
    pub elem: ElemSel,
    /// Offset of the least significant selected bit within the element.
    pub lo: Pos,
    pub width: u32,
    pub ty: Ty,
    pub span: Span,
}

/// Selected elements of a variable, as flat element positions (left bound first).
#[derive(Debug, Clone)]
pub(crate) enum ElemSel {
    /// Elements `base .. base + len`.
    Static { base: u32, len: u32 },
    /// Elements `base + pos * stride .. base + (pos + 1) * stride` for a position `pos` among
    /// `count`, when `valid`.
    Dyn {
        base: u32,
        count: u32,
        stride: u32,
        pos: Bv,
        valid: Lit,
    },
}

/// True when a one-hot decoded selection of one of `n` elements of `w` bits (position of `k`
/// bits) needs fewer AND nodes than a multiplexer tree: the decoder has about `2n` nodes shared by
/// all bits, then `2n - 1` per bit, against `3 (2^k - 1)` per bit for the tree.
fn decoded_is_smaller(n: usize, k: usize, w: usize) -> bool {
    n >= 4 && 2 * n + (2 * n - 1) * w < 3 * ((1usize << k) - 1) * w
}

/// Smallest number of bits to hold `n` distinct values.
fn bits_for(n: u64) -> usize {
    (64 - n.leading_zeros()) as usize
}

/// Preserve a net-array read until driver resolution can identify an actual constant ROM.
#[derive(Debug, Clone)]
pub(crate) struct DeferredSelect {
    pub chunks: Vec<Bv>,
    pub pos: Bv,
    pub outputs: Bv,
}

/// Select at a dynamic position. Constant tables use shared Shannon cofactors: a decoder's
/// generic data-input cost does not describe a ROM whose entries simplify to zero and one.
pub(crate) fn lower_select(aig: &mut Aig, chunks: &[Bv], p: &[Lit]) -> Bv {
    let w = chunks.first().map_or(0, Vec::len);
    let k = bits_for(chunks.len().saturating_sub(1) as u64);
    let constant = chunks.iter().flatten().all(|l| l.is_const());
    if !constant && decoded_is_smaller(chunks.len(), k, w) {
        return lower_decoded(aig, chunks, p, k);
    }
    let mut layer: Vec<Bv> = chunks.to_vec();
    layer.resize(1 << k, vec![Lit::FALSE; w]);
    for bit in 0..k {
        let s = p.get(bit).copied().unwrap_or(Lit::FALSE);
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            next.push(bits::mux(aig, s, &pair[1], &pair[0]));
        }
        layer = next;
    }
    let high: Vec<Lit> = p.iter().skip(k).copied().collect();
    let over = bits::red_or(aig, &high);
    let zero = vec![Lit::FALSE; w];
    let v = layer.pop().unwrap_or_default();
    bits::mux(aig, over, &zero, &v)
}

fn lower_decoded(aig: &mut Aig, chunks: &[Bv], p: &[Lit], k: usize) -> Bv {
    let n = chunks.len();
    let high: Vec<Lit> = p.iter().skip(k).copied().collect();
    let over = bits::red_or(aig, &high);
    let mut terms = vec![!over];
    for bit in 0..k {
        let s = p.get(bit).copied().unwrap_or(Lit::FALSE);
        let span = 1usize << bit;
        let mut next = vec![Lit::FALSE; (2 * span).min(n)];
        for (i, t) in terms.iter().enumerate() {
            next[i] = aig.and(*t, !s);
            if i + span < n {
                next[i + span] = aig.and(*t, s);
            }
        }
        terms = next;
    }
    let w = chunks.first().map_or(0, Vec::len);
    (0..w)
        .map(|b| {
            let prods: Vec<Lit> = chunks
                .iter()
                .zip(&terms)
                .map(|(c, t)| aig.and(*t, c[b]))
                .filter(|l| *l != Lit::FALSE)
                .collect();
            bits::red_or(aig, &prods)
        })
        .collect()
}

impl<'a> Elaborator<'a> {
    /// Selects `chunks[pos]`; zero if the position is out of range.
    pub(crate) fn select_chunk(&mut self, chunks: &[Bv], pos: &Pos) -> Bv {
        let w = chunks.first().map_or(0, Vec::len);
        match pos {
            Pos::Const(Some(p)) => chunks
                .get(*p as usize)
                .cloned()
                .unwrap_or_else(|| vec![Lit::FALSE; w]),
            Pos::Const(None) => vec![Lit::FALSE; w],
            Pos::Dyn(p) => {
                let net_array = (16..=1024).contains(&chunks.len())
                    && chunks.iter().flatten().all(|l| {
                        self.placeholder_of
                            .get(&l.node())
                            .is_some_and(|(s, _)| self.signals[*s as usize].is_net)
                    });
                if net_array {
                    let outputs: Bv = (0..w).map(|_| self.aig.input()).collect();
                    let id = self.deferred_selects.len();
                    for l in &outputs {
                        self.select_of.insert(l.node(), id);
                    }
                    self.deferred_selects.push(DeferredSelect {
                        chunks: chunks.to_vec(),
                        pos: p.clone(),
                        outputs: outputs.clone(),
                    });
                    outputs
                } else {
                    lower_select(&mut self.aig, chunks, p)
                }
            }
        }
    }

    /// Offset (from the right bound) of an index expression within `range`.
    pub(crate) fn index_offset(
        &mut self,
        scope: ScopeId,
        idx: &Expr,
        range: Range,
        add: i64,
    ) -> EResult<Pos> {
        let (v, t) = self.eval_self(scope, idx)?;
        if let Some(i) = bits::const_i64(&v, t.is_signed()) {
            return Ok(Pos::Const(range.offset(i + add)));
        }
        Ok(Pos::Dyn(self.dyn_offset(
            &v,
            t.is_signed(),
            range,
            add,
            false,
        )))
    }

    /// Position (from the left bound) of an index expression within `range`.
    pub(crate) fn index_position(
        &mut self,
        scope: ScopeId,
        idx: &Expr,
        range: Range,
    ) -> EResult<Pos> {
        let (v, t) = self.eval_self(scope, idx)?;
        if let Some(i) = bits::const_i64(&v, t.is_signed()) {
            return Ok(Pos::Const(range.position(i)));
        }
        Ok(Pos::Dyn(self.dyn_offset(&v, t.is_signed(), range, 0, true)))
    }

    /// Dynamic offset or position of `v + add` in `range`; negative results become large
    /// unsigned values (out of range).
    fn dyn_offset(
        &mut self,
        v: &[Lit],
        signed: bool,
        range: Range,
        add: i64,
        position: bool,
    ) -> Bv {
        let mag =
            range.left.unsigned_abs().max(range.right.unsigned_abs()) + add.unsigned_abs() + 1;
        let w = v.len().max(bits_for(mag)) + 2;
        let x = bits::resize(v, w, signed);
        let x = if add != 0 {
            bits::add_const(&mut self.aig, &x, add)
        } else {
            x
        };
        // offset = x - right (descending) or right - x (ascending); position from the left bound
        // is the mirror: left - x (descending) or x - left (ascending).
        let (base, negate) = match (position, range.left >= range.right) {
            (false, true) => (range.right, false),
            (false, false) => (range.right, true),
            (true, true) => (range.left, true),
            (true, false) => (range.left, false),
        };
        let b = bits::constant(base as u64, w);
        if negate {
            bits::sub(&mut self.aig, &b, &x)
        } else {
            bits::sub(&mut self.aig, &x, &b)
        }
    }

    /// Reads the value of an identifier.
    pub(crate) fn read_ident(&mut self, scope: ScopeId, e: &Expr) -> EResult<TVal> {
        let ExprKind::Ident(p) = &e.kind else {
            unreachable!("identifier expected")
        };
        match self.lookup_path(scope, p)? {
            Symbol::Signal(s) => Ok(self.read_whole(VarRef::Sig(s))),
            Symbol::Local(l) => Ok(self.read_whole(VarRef::Local(l))),
            Symbol::Const(v) => Ok((*v).clone()),
            _ => {
                self.expr_ty(scope, e)?;
                Err(self.err("S0135", e.span, "unsupported reference"))
            }
        }
    }

    /// Current value of a whole variable.
    pub(crate) fn read_whole(&self, v: VarRef) -> TVal {
        let ty = self.var_ty(v).clone();
        if ty.is_packed() {
            TVal::Packed(self.read_var((v, 0)), ty)
        } else {
            let n = ty.element_count();
            TVal::Unpacked((0..n).map(|k| self.read_var((v, k))).collect(), ty)
        }
    }

    /// Typed evaluation: identifiers and selects keep their declared types.
    pub(crate) fn eval_typed(&mut self, scope: ScopeId, e: &Expr) -> EResult<TVal> {
        match &e.kind {
            ExprKind::Ident(_) => self.read_ident(scope, e),
            ExprKind::Index(b, i) => {
                if let Some(value) = self.index_ident_constant(scope, b, i)? {
                    return Ok(value);
                }
                let base = self.eval_typed(scope, b)?;
                self.index_value(scope, base, i)
            }
            ExprKind::Slice {
                base,
                kind,
                left,
                right,
            } => {
                let b = self.eval_typed(scope, base)?;
                self.slice_value(scope, b, *kind, left, right, e.span)
            }
            ExprKind::Member(b, f) => {
                let base = self.eval_typed(scope, b)?;
                match base {
                    TVal::Packed(v, t) => {
                        let fld = t
                            .field(&f.name)
                            .ok_or_else(|| {
                                self.err("S0170", f.span, format!("no member '{}'", f.name))
                            })?
                            .clone();
                        let lo = fld.offset as usize;
                        Ok(TVal::Packed(
                            v[lo..lo + fld.ty.width() as usize].to_vec(),
                            fld.ty,
                        ))
                    }
                    TVal::Unpacked(..) => {
                        Err(self.err("S0170", f.span, "member select on an unpacked array"))
                    }
                }
            }
            _ => {
                let (v, t) = self.eval_self(scope, e)?;
                Ok(TVal::Packed(v, t))
            }
        }
    }

    /// A constant index into a named array needs only the selected elements. Materializing
    /// the whole array for every unrolled loop iteration makes a static matrix algorithm
    /// needlessly quadratic in the array's size.
    fn index_ident_constant(
        &mut self,
        scope: ScopeId,
        base: &Expr,
        idx: &Expr,
    ) -> EResult<Option<TVal>> {
        // Preserve evaluation order for an index that could have subroutine side effects.
        fn no_calls(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::Number(_) | ExprKind::UnbasedUnsized(_) | ExprKind::Ident(_) => true,
                ExprKind::Unary(_, a) | ExprKind::Cast { expr: a, .. } => no_calls(a),
                ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => no_calls(a) && no_calls(b),
                _ => false,
            }
        }
        let ExprKind::Ident(path) = &base.kind else {
            return Ok(None);
        };
        if !no_calls(idx) {
            return Ok(None);
        }
        let var = match self.lookup_path(scope, path)? {
            Symbol::Signal(s) => VarRef::Sig(s),
            Symbol::Local(l) => VarRef::Local(l),
            _ => return Ok(None),
        };
        let Ty::Unpacked { range, elem } = self.var_ty(var).clone() else {
            return Ok(None);
        };
        let pos = self.index_position(scope, idx, range)?;
        let Pos::Const(pos) = pos else {
            return Ok(None);
        };
        let inner = elem.element_count();
        let width = elem.unpacked_dims().1.width() as usize;
        let values: Vec<_> = (0..inner)
            .map(|j| match pos {
                Some(p) => self.read_var((var, p * inner + j)),
                None => vec![Lit::FALSE; width],
            })
            .collect();
        Ok(Some(if elem.is_packed() {
            TVal::Packed(
                values.into_iter().next().unwrap_or_default(),
                (*elem).clone(),
            )
        } else {
            TVal::Unpacked(values, (*elem).clone())
        }))
    }

    fn index_value(&mut self, scope: ScopeId, base: TVal, idx: &Expr) -> EResult<TVal> {
        match base {
            TVal::Unpacked(elems, ty) => {
                let Ty::Unpacked { elem, range } = &ty else {
                    unreachable!("unpacked type")
                };
                let groups = range.len() as usize;
                let inner = elems.len() / groups.max(1);
                let pos = self.index_position(scope, idx, *range)?;
                let mut out = Vec::with_capacity(inner);
                for j in 0..inner {
                    let chunks: Vec<Bv> =
                        (0..groups).map(|p| elems[p * inner + j].clone()).collect();
                    out.push(self.select_chunk(&chunks, &pos));
                }
                let et = (**elem).clone();
                Ok(if et.is_packed() {
                    TVal::Packed(out.pop().unwrap_or_default(), et)
                } else {
                    TVal::Unpacked(out, et)
                })
            }
            TVal::Packed(v, ty) => {
                let (range, et) = ty
                    .index_dim()
                    .ok_or_else(|| self.err("S0171", idx.span, "value cannot be indexed"))?;
                let ew = et.width() as usize;
                let chunks: Vec<Bv> = v.chunks(ew.max(1)).map(<[Lit]>::to_vec).collect();
                let pos = self.index_offset(scope, idx, range, 0)?;
                Ok(TVal::Packed(self.select_chunk(&chunks, &pos), et))
            }
        }
    }

    /// Lowest selected element offset and element count of a part-select.
    pub(crate) fn slice_bounds(
        &mut self,
        scope: ScopeId,
        range: Range,
        kind: SliceKind,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> EResult<(Pos, u32)> {
        match kind {
            SliceKind::Const => {
                let l = self.eval_const_int(scope, left)?;
                let r = self.eval_const_int(scope, right)?;
                if (l >= r) != (range.left >= range.right) && l != r {
                    return Err(self.err(
                        "S0172",
                        span,
                        "part-select direction does not match the declared range",
                    ));
                }
                let count = (l - r).unsigned_abs() as u32 + 1;
                // Offset of the lower-offset end; out-of-range parts read as zero.
                let lo_idx = if range.left >= range.right {
                    l.min(r)
                } else {
                    l.max(r)
                };
                Ok((self.const_offset_ext(range, lo_idx), count))
            }
            SliceKind::Up | SliceKind::Down => {
                let wd = self.eval_const_int(scope, right)?;
                if wd <= 0 {
                    return Err(self.err(
                        "S0173",
                        right.span,
                        "part-select width must be positive",
                    ));
                }
                // Indices covered: [b, b+wd-1] for +:, [b-wd+1, b] for -:.
                let low_add = if kind == SliceKind::Up { 0 } else { 1 - wd };
                let high_add = low_add + wd - 1;
                // The lower offset end is the lowest index for descending ranges and the highest
                // for ascending ranges.
                let add = if range.left >= range.right {
                    low_add
                } else {
                    high_add
                };
                let (v, t) = self.eval_self(scope, left)?;
                if let Some(b) = bits::const_i64(&v, t.is_signed()) {
                    return Ok((self.const_offset_ext(range, b + add), wd as u32));
                }
                let off = self.dyn_offset(&v, t.is_signed(), range, add, false);
                Ok((Pos::Dyn(off), wd as u32))
            }
        }
    }

    /// Offset of index `i` allowing positions outside the range (encoded as `Const(None)` only
    /// when entirely unrepresentable). Offsets below zero are handled by the caller through
    /// `slice_bits`.
    fn const_offset_ext(&self, range: Range, i: i64) -> Pos {
        let off = if range.left >= range.right {
            i - range.right
        } else {
            range.right - i
        };
        if off < 0 || off > i64::from(u32::MAX / 2) {
            // Encode negative offsets as a dynamic constant vector so that `slice_bits` fills
            // the missing low bits with zeros.
            let v = bits::constant(off as u64, 64);
            return Pos::Dyn(v);
        }
        Pos::Const(Some(off as u32))
    }

    /// Extracts `count` elements of width `ew` starting at element offset `lo`, filling
    /// out-of-range bits with zeros.
    pub(crate) fn slice_bits(&mut self, v: &[Lit], ew: usize, lo: &Pos, count: u32) -> Bv {
        let n = count as usize * ew;
        match lo {
            Pos::Const(Some(o)) => (0..n)
                .map(|j| v.get(*o as usize * ew + j).copied().unwrap_or(Lit::FALSE))
                .collect(),
            Pos::Const(None) => vec![Lit::FALSE; n],
            Pos::Dyn(off) => {
                // Shift right by off*ew with n zero bits padded below, so that negative
                // offsets down to -count elements are representable.
                let mut padded = vec![Lit::FALSE; n];
                padded.extend_from_slice(v);
                let w = off.len() + bits_for(ew as u64) + 1;
                let offw = bits::resize(off, w, true);
                let ewc = bits::constant(ew as u64, w);
                let shift_bits = bits::mul(&mut self.aig, &offw, &ewc);
                let pad = bits::constant(n as u64, w);
                let amount = bits::add(&mut self.aig, &shift_bits, &pad, Lit::FALSE).0;
                // A negative amount means entirely below the vector.
                let neg = *amount.last().unwrap_or(&Lit::FALSE);
                let sh = bits::shift(&mut self.aig, &padded, &amount, false, Lit::FALSE);
                let zero = vec![Lit::FALSE; n];
                let r: Bv = sh[..n].to_vec();
                bits::mux(&mut self.aig, neg, &zero, &r)
            }
        }
    }

    fn slice_value(
        &mut self,
        scope: ScopeId,
        base: TVal,
        kind: SliceKind,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> EResult<TVal> {
        match base {
            TVal::Packed(v, ty) => {
                let (range, et) = ty
                    .index_dim()
                    .ok_or_else(|| self.err("S0171", span, "value cannot be sliced"))?;
                let ew = et.width() as usize;
                let (lo, count) = self.slice_bounds(scope, range, kind, left, right, span)?;
                let bitsv = self.slice_bits(&v, ew, &lo, count);
                let rt = Ty::Vector {
                    elem: std::rc::Rc::new(et),
                    range: Range::down(count),
                    signed: false,
                };
                Ok(TVal::Packed(bitsv, rt))
            }
            TVal::Unpacked(elems, ty) => {
                let Ty::Unpacked { elem, range } = &ty else {
                    unreachable!("unpacked")
                };
                if kind != SliceKind::Const {
                    return Err(self.err(
                        "S0174",
                        span,
                        "indexed part-selects of unpacked arrays are not supported",
                    ));
                }
                let l = self.eval_const_int(scope, left)?;
                let r = self.eval_const_int(scope, right)?;
                let (pl, pr) = match (range.position(l), range.position(r)) {
                    (Some(a), Some(b)) => (a.min(b), a.max(b)),
                    _ => return Err(self.err("S0175", span, "unpacked array slice out of range")),
                };
                let inner = elems.len() / range.len() as usize;
                let sub = elems[pl as usize * inner..(pr as usize + 1) * inner].to_vec();
                let rt = Ty::Unpacked {
                    elem: elem.clone(),
                    range: Range::up(pr - pl + 1),
                };
                Ok(TVal::Unpacked(sub, rt))
            }
        }
    }

    /// Type of a select expression.
    pub(crate) fn select_ty(&mut self, scope: ScopeId, e: &Expr) -> EResult<Ty> {
        match &e.kind {
            ExprKind::Index(b, _) => {
                let bt = self.expr_ty(scope, b)?;
                let (_, et) = bt
                    .index_dim()
                    .ok_or_else(|| self.err("S0171", e.span, "value cannot be indexed"))?;
                Ok(et)
            }
            ExprKind::Slice {
                base,
                kind,
                left,
                right,
            } => {
                let bt = self.expr_ty(scope, base)?;
                let (range, et) = bt
                    .index_dim()
                    .ok_or_else(|| self.err("S0171", e.span, "value cannot be sliced"))?;
                let count = match kind {
                    SliceKind::Const => {
                        let l = self.eval_const_int(scope, left)?;
                        let r = self.eval_const_int(scope, right)?;
                        (l - r).unsigned_abs() as u32 + 1
                    }
                    _ => self.eval_const_int(scope, right)?.max(1) as u32,
                };
                Ok(if bt.is_packed() {
                    Ty::Vector {
                        elem: std::rc::Rc::new(et),
                        range: Range::down(count),
                        signed: false,
                    }
                } else {
                    let _ = range;
                    Ty::Unpacked {
                        elem: std::rc::Rc::new(et),
                        range: Range::up(count),
                    }
                })
            }
            ExprKind::Member(b, f) => {
                let bt = self.expr_ty(scope, b)?;
                bt.field(&f.name)
                    .map(|fl| fl.ty.clone())
                    .ok_or_else(|| self.err("S0170", f.span, format!("no member '{}'", f.name)))
            }
            _ => unreachable!("select expected"),
        }
    }

    // ---------------------------------------------------------------- targets

    /// Resolves an assignment target into pieces, most significant first. Undeclared simple
    /// identifiers are reported by the caller.
    pub(crate) fn lvalue(&mut self, scope: ScopeId, e: &Expr) -> EResult<Vec<LvPiece>> {
        match &e.kind {
            ExprKind::Concat(items) => {
                let mut out = Vec::new();
                for it in items {
                    out.extend(self.lvalue(scope, it)?);
                }
                Ok(out)
            }
            _ => Ok(vec![self.lv_piece(scope, e)?]),
        }
    }

    fn lv_piece(&mut self, scope: ScopeId, e: &Expr) -> EResult<LvPiece> {
        match &e.kind {
            ExprKind::Ident(p) => {
                let var = match self.lookup_path(scope, p)? {
                    Symbol::Signal(s) => VarRef::Sig(s),
                    Symbol::Local(l) => VarRef::Local(l),
                    _ => {
                        return Err(self.err(
                            "S0180",
                            e.span,
                            format!("'{}' cannot be assigned", p.name.name),
                        ));
                    }
                };
                let ty = self.var_ty(var).clone();
                let width = if ty.is_packed() { ty.width() } else { 0 };
                let len = ty.element_count();
                Ok(LvPiece {
                    var,
                    elem: ElemSel::Static { base: 0, len },
                    lo: Pos::Const(Some(0)),
                    width,
                    ty,
                    span: e.span,
                })
            }
            ExprKind::Index(b, idx) => {
                let base = self.lv_piece(scope, b)?;
                if !base.ty.is_packed() {
                    // Element or sub-array of an unpacked variable.
                    let Ty::Unpacked { elem, range } = &base.ty else {
                        unreachable!("unpacked")
                    };
                    let n = range.len();
                    let inner = base.ty.element_count() / n;
                    let et = (**elem).clone();
                    let pos = self.index_position(scope, idx, *range)?;
                    let (pbits, pvalid) = self.pos_bits(&pos, n);
                    let elem_sel = match (base.elem, &pos) {
                        (ElemSel::Static { base: b0, .. }, Pos::Const(Some(p))) => {
                            ElemSel::Static {
                                base: b0 + p * inner,
                                len: inner,
                            }
                        }
                        (ElemSel::Static { base: b0, .. }, _) => ElemSel::Dyn {
                            base: b0,
                            count: n,
                            stride: inner,
                            pos: pbits,
                            valid: pvalid,
                        },
                        (
                            ElemSel::Dyn {
                                base: b0,
                                count,
                                stride: _,
                                pos: p0,
                                valid: v0,
                            },
                            _,
                        ) => {
                            // Combine: new position = p0 * n + pos, with the inner stride.
                            let w = p0.len().max(pbits.len()) + bits_for(u64::from(n)) + 1;
                            let a = bits::resize(&p0, w, false);
                            let nb = bits::constant(u64::from(n), w);
                            let prod = bits::mul(&mut self.aig, &a, &nb);
                            let pb = bits::resize(&pbits, w, false);
                            let total = bits::add(&mut self.aig, &prod, &pb, Lit::FALSE).0;
                            let valid = self.aig.and(v0, pvalid);
                            ElemSel::Dyn {
                                base: b0,
                                count: count * n,
                                stride: inner,
                                pos: total,
                                valid,
                            }
                        }
                    };
                    let width = if et.is_packed() { et.width() } else { 0 };
                    return Ok(LvPiece {
                        var: base.var,
                        elem: elem_sel,
                        lo: Pos::Const(Some(0)),
                        width,
                        ty: et,
                        span: e.span,
                    });
                }
                let (range, et) = base
                    .ty
                    .index_dim()
                    .ok_or_else(|| self.err("S0171", e.span, "value cannot be indexed"))?;
                let ew = et.width();
                let off = self.index_offset(scope, idx, range, 0)?;
                let lo = self.combine_offset(&base.lo, &off, ew);
                Ok(LvPiece {
                    var: base.var,
                    elem: base.elem,
                    lo,
                    width: ew,
                    ty: et,
                    span: e.span,
                })
            }
            ExprKind::Slice {
                base,
                kind,
                left,
                right,
            } => {
                let b = self.lv_piece(scope, base)?;
                if !b.ty.is_packed() {
                    return Err(self.err(
                        "S0182",
                        e.span,
                        "slices of unpacked arrays cannot be assigned",
                    ));
                }
                let (range, et) =
                    b.ty.index_dim()
                        .ok_or_else(|| self.err("S0171", e.span, "value cannot be sliced"))?;
                let ew = et.width();
                let (off, count) = self.slice_bounds(scope, range, *kind, left, right, e.span)?;
                let lo = self.combine_offset(&b.lo, &off, ew);
                let ty = Ty::Vector {
                    elem: std::rc::Rc::new(et),
                    range: Range::down(count),
                    signed: false,
                };
                Ok(LvPiece {
                    var: b.var,
                    elem: b.elem,
                    lo,
                    width: count * ew,
                    ty,
                    span: e.span,
                })
            }
            ExprKind::Member(b, f) => {
                let base = self.lv_piece(scope, b)?;
                let fld = base
                    .ty
                    .field(&f.name)
                    .ok_or_else(|| self.err("S0170", f.span, format!("no member '{}'", f.name)))?
                    .clone();
                let off = Pos::Const(Some(fld.offset));
                let lo = self.combine_offset(&base.lo, &off, 1);
                Ok(LvPiece {
                    var: base.var,
                    elem: base.elem,
                    lo,
                    width: fld.ty.width(),
                    ty: fld.ty,
                    span: e.span,
                })
            }
            _ => Err(self.err("S0183", e.span, "expression cannot be assigned")),
        }
    }

    /// Position as a vector plus validity for `n` positions.
    pub(crate) fn pos_bits(&mut self, pos: &Pos, n: u32) -> (Bv, Lit) {
        match pos {
            Pos::Const(Some(p)) => (
                bits::constant(u64::from(*p), bits_for(u64::from(n)).max(1)),
                Lit::TRUE,
            ),
            Pos::Const(None) => (vec![Lit::FALSE], Lit::FALSE),
            Pos::Dyn(v) => {
                let nb = bits::constant(u64::from(n), v.len().max(bits_for(u64::from(n))));
                let vv = bits::resize(v, nb.len(), false);
                let valid = if v.len() > nb.len() {
                    // High bits of v beyond the comparison width must be zero.
                    let high: Vec<Lit> = v[nb.len()..].to_vec();
                    let any = bits::red_or(&mut self.aig, &high);
                    let lt = bits::ult(&mut self.aig, &vv, &nb);
                    self.aig.and(!any, lt)
                } else {
                    bits::ult(&mut self.aig, &vv, &nb)
                };
                (v.clone(), valid)
            }
        }
    }

    /// `base + off * unit` for bit offsets.
    fn combine_offset(&mut self, base: &Pos, off: &Pos, unit: u32) -> Pos {
        match (base, off) {
            (Pos::Const(Some(b)), Pos::Const(Some(o))) => Pos::Const(Some(b + o * unit)),
            (Pos::Const(None), _) | (_, Pos::Const(None)) => Pos::Const(None),
            _ => {
                let to_dyn = |p: &Pos| match p {
                    Pos::Const(Some(c)) => bits::constant(u64::from(*c), 34),
                    Pos::Dyn(v) => v.clone(),
                    Pos::Const(None) => unreachable!("handled"),
                };
                let b = to_dyn(base);
                let o = to_dyn(off);
                let w = b.len().max(o.len() + bits_for(u64::from(unit))) + 1;
                let bw = bits::resize(&b, w, true);
                let ow = bits::resize(&o, w, true);
                let uw = bits::constant(u64::from(unit), w);
                let prod = bits::mul(&mut self.aig, &ow, &uw);
                Pos::Dyn(bits::add(&mut self.aig, &bw, &prod, Lit::FALSE).0)
            }
        }
    }
}
