// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Continuous assignments and gate primitives: drivers of signal bits (SYNTH-REQ-0120, SYNTH-REQ-0210).

use rusteda_formats::verilog::Span;
use rusteda_formats::verilog::ast::{self, Expr, GateKind};

use super::select::{ElemSel, LvPiece, Pos};
use super::types::Ty;
use super::{EResult, Elaborator, ProcId, ScopeId, TVal, VarRef};
use crate::aig::Lit;
use crate::bits;

impl<'a> Elaborator<'a> {
    /// Continuous assignment to static targets.
    pub(crate) fn cont_assign(
        &mut self,
        scope: ScopeId,
        lhs: &Expr,
        rhs: &Expr,
        proc: ProcId,
        span: Span,
    ) -> EResult<()> {
        let pieces = self.lvalue(scope, lhs)?;
        let target = if pieces.len() == 1 {
            pieces[0].ty.clone()
        } else {
            Ty::vec(pieces.iter().map(|p| p.width).sum(), false)
        };
        let v = self.eval_assign(scope, rhs, &target)?;
        self.drive_pieces(&pieces, v, proc, span)
    }

    /// Records drivers for static target pieces.
    pub(crate) fn drive_pieces(
        &mut self,
        pieces: &[LvPiece],
        v: TVal,
        proc: ProcId,
        span: Span,
    ) -> EResult<()> {
        let mut chunks: Vec<(usize, TVal)> = Vec::new();
        if pieces.len() == 1 {
            chunks.push((0, v));
        } else {
            let TVal::Packed(b, _) = v else {
                return Err(self.err("S0221", span, "unpacked value assigned to a concatenation"));
            };
            let mut hi = b.len();
            for (i, p) in pieces.iter().enumerate() {
                let w = p.width as usize;
                chunks.push((i, TVal::Packed(b[hi - w..hi].to_vec(), p.ty.clone())));
                hi -= w;
            }
        }
        for (i, val) in chunks {
            let p = &pieces[i];
            let VarRef::Sig(sig) = p.var else {
                return Err(self.err("S0183", p.span, "cannot drive a local variable"));
            };
            let ElemSel::Static { base, len } = p.elem else {
                return Err(self.err(
                    "S0184",
                    p.span,
                    "continuous assignment targets need constant indices",
                ));
            };
            let ew = self.signals[sig as usize].elem_width;
            let elems: Vec<Vec<Lit>> = match val {
                TVal::Packed(b, _) => vec![b],
                TVal::Unpacked(es, _) => es,
            };
            if elems.len() == 1 && p.ty.is_packed() {
                let lo = match p.lo {
                    Pos::Const(Some(o)) => o,
                    Pos::Const(None) => continue,
                    Pos::Dyn(_) => {
                        return Err(self.err(
                            "S0184",
                            p.span,
                            "continuous assignment targets need constant indices",
                        ));
                    }
                };
                for (j, l) in elems[0].iter().enumerate() {
                    let bit = lo + j as u32;
                    if bit < ew {
                        self.drive(sig, base * ew + bit, *l, proc, span)?;
                    }
                }
            } else {
                for (k, e) in elems.iter().enumerate().take(len as usize) {
                    for (j, l) in e.iter().enumerate() {
                        self.drive(sig, (base + k as u32) * ew + j as u32, *l, proc, span)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn elab_gate(&mut self, scope: ScopeId, g: &ast::GateInst) -> EResult<()> {
        let n = g.terms.len();
        let (outs, ins) = match g.kind {
            GateKind::Not | GateKind::Buf => (&g.terms[..n - 1], &g.terms[n - 1..]),
            _ => (&g.terms[..1], &g.terms[1..]),
        };
        if ins.is_empty() || outs.is_empty() {
            return Err(self.err(
                "S0120",
                g.span,
                "gate primitive needs an output and at least one input",
            ));
        }
        let mut vals = Vec::new();
        for i in ins {
            let (v, _) = self.eval_self(scope, i)?;
            vals.push(v[0]);
        }
        let aig = &mut self.aig;
        let r = match g.kind {
            GateKind::And | GateKind::Nand => bits::red_and(aig, &vals),
            GateKind::Or | GateKind::Nor => bits::red_or(aig, &vals),
            GateKind::Xor | GateKind::Xnor => bits::red_xor(aig, &vals),
            GateKind::Not | GateKind::Buf => vals[0],
        };
        let r = if matches!(
            g.kind,
            GateKind::Nand | GateKind::Nor | GateKind::Xnor | GateKind::Not
        ) {
            !r
        } else {
            r
        };
        let proc = self.new_proc("gate primitive", g.span);
        for o in outs {
            let pieces = self.lvalue(scope, o)?;
            self.drive_pieces(
                &pieces,
                TVal::Packed(vec![r], Ty::Bit { signed: false }),
                proc,
                g.span,
            )?;
        }
        Ok(())
    }
}
