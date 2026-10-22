// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Module and gate instances and generate constructs (IEEE 1800-2017 clauses 23.3, 28 and 27).

use super::items::gate_kind;
use super::{PResult, Parser};
use crate::verilog::ast::{
    GateInst, GenBlock, GenCase, GenFor, GenIf, Instance, Instantiation, ParamAssign, ParamValue,
    PortConn,
};

impl Parser<'_> {
    /// `ID #(` or `ID ID [dims] (`.
    pub(crate) fn is_instance_ahead(&self) -> bool {
        if self.is_op_at(1, "#") {
            return true;
        }
        if !self.is_ident_at(1) {
            return false;
        }
        let mut k = 2;
        while self.is_op_at(k, "[") {
            k = self.skip_brackets_from(k);
        }
        self.is_op_at(k, "(")
    }

    pub(crate) fn instantiation(&mut self) -> PResult<Instantiation> {
        let start = self.span();
        let module = self.ident()?;
        let mut params = Vec::new();
        if self.eat_op("#") {
            self.expect_op("(")?;
            if !self.is_op(")") {
                loop {
                    if self.eat_op(".") {
                        let n = self.ident()?;
                        self.expect_op("(")?;
                        let v = if self.is_op(")") {
                            None
                        } else {
                            Some(self.param_value()?)
                        };
                        self.expect_op(")")?;
                        params.push(ParamAssign::Named(n, v));
                    } else {
                        params.push(ParamAssign::Ordered(self.param_value()?));
                    }
                    if !self.eat_op(",") {
                        break;
                    }
                }
            }
            self.expect_op(")")?;
        }
        let mut instances = Vec::new();
        loop {
            let istart = self.span();
            let name = self.ident()?;
            let dims = self.dims()?;
            self.expect_op("(")?;
            let mut conns = Vec::new();
            if !self.is_op(")") {
                loop {
                    if self.is_op(".*") {
                        conns.push(PortConn::Wildcard(self.bump().span));
                    } else if self.is_op(".") {
                        self.bump();
                        let n = self.ident()?;
                        if self.eat_op("(") {
                            let e = if self.is_op(")") {
                                None
                            } else {
                                Some(self.expr()?)
                            };
                            self.expect_op(")")?;
                            conns.push(PortConn::Named(n, Some(e)));
                        } else {
                            conns.push(PortConn::Named(n, None));
                        }
                    } else if self.is_op(",") || self.is_op(")") {
                        conns.push(PortConn::Ordered(None));
                    } else {
                        conns.push(PortConn::Ordered(Some(self.expr()?)));
                    }
                    if !self.eat_op(",") {
                        break;
                    }
                }
            }
            self.expect_op(")")?;
            instances.push(Instance {
                name,
                dims,
                conns,
                span: istart.to(self.prev_span()),
            });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(";")?;
        Ok(Instantiation {
            module,
            params,
            instances,
            span: start.to(self.prev_span()),
        })
    }

    pub(crate) fn param_value(&mut self) -> PResult<ParamValue> {
        if self.starts_explicit_type() && !self.is_op_at(1, "'") {
            return Ok(ParamValue::Type(self.data_type()?));
        }
        Ok(ParamValue::Expr(self.expr()?))
    }

    pub(crate) fn gates(&mut self) -> PResult<Vec<GateInst>> {
        let kind = gate_kind(self.peek()).ok_or_else(|| self.unexpected("a gate"))?;
        self.bump();
        self.skip_strength_and_delay()?;
        let mut out = Vec::new();
        loop {
            let start = self.span();
            let name = if self.is_ident() {
                Some(self.ident()?)
            } else {
                None
            };
            let _ = self.dims()?;
            self.expect_op("(")?;
            let mut terms = vec![self.expr()?];
            while self.eat_op(",") {
                terms.push(self.expr()?);
            }
            self.expect_op(")")?;
            out.push(GateInst {
                kind,
                name,
                terms,
                span: start.to(self.prev_span()),
            });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(";")?;
        Ok(out)
    }

    /// Generate block: `begin [: l] items end`, `l : begin ... end`, or a single item.
    pub(crate) fn gen_block(&mut self) -> PResult<GenBlock> {
        let start = self.span();
        let mut label = None;
        if self.is_ident() && self.is_op_at(1, ":") && self.is_kw_at(2, "begin") {
            label = Some(self.ident()?);
            self.bump();
        }
        if self.eat_kw("begin") {
            if self.eat_op(":") {
                label = Some(self.ident()?);
            }
            let mut items = Vec::new();
            while !self.is_kw("end") {
                if self.at_eof() {
                    return Err(self.unexpected("'end'"));
                }
                if let Some(it) = self.module_item()? {
                    items.push(it);
                }
            }
            self.bump();
            self.end_label();
            return Ok(GenBlock {
                label,
                items,
                has_begin: true,
                span: start.to(self.prev_span()),
            });
        }
        let items = self.module_item()?.into_iter().collect();
        Ok(GenBlock {
            label,
            items,
            has_begin: false,
            span: start.to(self.prev_span()),
        })
    }

    pub(crate) fn gen_for(&mut self) -> PResult<GenFor> {
        let start = self.expect_kw("for")?;
        self.expect_op("(")?;
        let declares_genvar = self.eat_kw("genvar");
        let genvar = self.ident()?;
        self.expect_op("=")?;
        let init = self.expr()?;
        self.expect_op(";")?;
        let cond = self.expr()?;
        self.expect_op(";")?;
        let step = self.step_stmt()?;
        self.expect_op(")")?;
        let body = self.gen_block()?;
        Ok(GenFor {
            genvar,
            declares_genvar,
            init,
            cond,
            step,
            body,
            span: start.to(self.prev_span()),
        })
    }

    pub(crate) fn gen_if(&mut self) -> PResult<GenIf> {
        let start = self.expect_kw("if")?;
        self.expect_op("(")?;
        let cond = self.expr()?;
        self.expect_op(")")?;
        let then = self.gen_block()?;
        let els = if self.eat_kw("else") {
            Some(self.gen_block()?)
        } else {
            None
        };
        Ok(GenIf {
            cond,
            then,
            els,
            span: start.to(self.prev_span()),
        })
    }

    pub(crate) fn gen_case(&mut self) -> PResult<GenCase> {
        let start = self.expect_kw("case")?;
        self.expect_op("(")?;
        let expr = self.expr()?;
        self.expect_op(")")?;
        let mut items = Vec::new();
        while !self.is_kw("endcase") {
            if self.at_eof() {
                return Err(self.unexpected("'endcase'"));
            }
            let mut labels = Vec::new();
            if self.eat_kw("default") {
                self.eat_op(":");
            } else {
                loop {
                    labels.push(self.expr()?);
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op(":")?;
            }
            items.push((labels, self.gen_block()?));
        }
        self.bump();
        Ok(GenCase {
            expr,
            items,
            span: start.to(self.prev_span()),
        })
    }
}
