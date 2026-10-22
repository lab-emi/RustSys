// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Function and task declarations (IEEE 1800-2017 clause 13).

use super::items::direction;
use super::{PResult, Parser};
use crate::verilog::ast::{
    BlockDecl, DataType, Direction, FunctionDecl, TaskDecl, TfPort, TypeKind,
};

impl Parser<'_> {
    /// Task/function formal arguments in parentheses, with inheritance of direction and type.
    pub(crate) fn tf_ports(&mut self) -> PResult<Vec<TfPort>> {
        self.expect_op("(")?;
        let mut out: Vec<TfPort> = Vec::new();
        if self.eat_op(")") {
            return Ok(out);
        }
        loop {
            let start = self.span();
            let dir = direction(self.peek());
            if dir.is_some() {
                self.bump();
            }
            self.eat_kw("var");
            self.eat_kw("const");
            let has_type = self.starts_explicit_type()
                || self.named_type_ahead()
                || self.is_op("[")
                || self.is_kw("signed")
                || self.is_kw("unsigned");
            let ty = if has_type {
                Some(self.data_type_or_implicit(true)?)
            } else {
                None
            };
            let name = self.ident()?;
            let dims = self.dims()?;
            let default = if self.eat_op("=") {
                Some(self.expr()?)
            } else {
                None
            };
            let prev = out.last();
            let (dir, ty) = match (dir, ty) {
                (None, None) => match prev {
                    Some(p) => (p.dir, p.ty.clone()),
                    None => (
                        Direction::Input,
                        DataType {
                            kind: TypeKind::Logic,
                            signing: None,
                            packed: Vec::new(),
                            span: start,
                        },
                    ),
                },
                (Some(d), None) => (
                    d,
                    DataType {
                        kind: TypeKind::Logic,
                        signing: None,
                        packed: Vec::new(),
                        span: start,
                    },
                ),
                (d, Some(t)) => (d.or(prev.map(|p| p.dir)).unwrap_or(Direction::Input), t),
            };
            out.push(TfPort {
                dir,
                ty,
                name,
                dims,
                default,
            });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(out)
    }

    /// Declarations and statements of a subroutine body up to `end_kw`.
    pub(crate) fn tf_body(
        &mut self,
        end_kw: &str,
    ) -> PResult<(Vec<BlockDecl>, Vec<crate::verilog::ast::Stmt>)> {
        let mut decls = Vec::new();
        let mut body = Vec::new();
        while !self.is_kw(end_kw) {
            if self.at_eof() {
                return Err(self.unexpected(&format!("'{end_kw}'")));
            }
            if direction(self.peek()).is_some() {
                decls.push(BlockDecl::Port(self.port_decl()?));
            } else if self.starts_block_decl() {
                if let Some(d) = self.block_decl()? {
                    decls.push(d);
                }
            } else {
                body.push(self.stmt()?);
            }
        }
        self.bump();
        self.end_label();
        Ok((decls, body))
    }

    pub(crate) fn function(&mut self) -> PResult<FunctionDecl> {
        let start = self.expect_kw("function")?;
        self.eat_kw("automatic");
        self.eat_kw("static");
        let ret = if self.eat_kw("void") {
            None
        } else if self.starts_explicit_type()
            || self.is_op("[")
            || self.is_kw("signed")
            || self.is_kw("unsigned")
        {
            Some(self.data_type_or_implicit(false)?)
        } else if self.named_type_ahead() {
            Some(self.data_type()?)
        } else {
            Some(DataType {
                kind: TypeKind::Logic,
                signing: None,
                packed: Vec::new(),
                span: start,
            })
        };
        // Class-scoped names (`cls::f`) are not supported; plain names only.
        let name = self.ident()?;
        let ports = if self.is_op("(") {
            self.tf_ports()?
        } else {
            Vec::new()
        };
        self.expect_op(";")?;
        let (decls, body) = self.tf_body("endfunction")?;
        Ok(FunctionDecl {
            name,
            ret,
            ports,
            decls,
            body,
            span: start.to(self.prev_span()),
        })
    }

    pub(crate) fn task(&mut self) -> PResult<TaskDecl> {
        let start = self.expect_kw("task")?;
        self.eat_kw("automatic");
        self.eat_kw("static");
        let name = self.ident()?;
        let ports = if self.is_op("(") {
            self.tf_ports()?
        } else {
            Vec::new()
        };
        self.expect_op(";")?;
        let (decls, body) = self.tf_body("endtask")?;
        Ok(TaskDecl {
            name,
            ports,
            decls,
            body,
            span: start.to(self.prev_span()),
        })
    }
}
