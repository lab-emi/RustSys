// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Data types, dimensions and declarations.

use super::expr::is_type_keyword;
use super::{PResult, Parser};
use crate::verilog::ast::{
    DataDecl, DataType, Declarator, Dim, EnumMember, NetKind, ParamDecl, ParamKind, StructMember,
    TypeKind, Typedef,
};
use crate::verilog::token::Tok;

pub(crate) fn net_kind(t: &Tok) -> Option<NetKind> {
    Some(match t {
        Tok::Kw("wire") => NetKind::Wire,
        Tok::Kw("tri") => NetKind::Tri,
        Tok::Kw("uwire") => NetKind::Uwire,
        Tok::Kw("wand") => NetKind::Wand,
        Tok::Kw("wor") => NetKind::Wor,
        Tok::Kw("triand") => NetKind::Triand,
        Tok::Kw("trior") => NetKind::Trior,
        Tok::Kw("tri0") => NetKind::Tri0,
        Tok::Kw("tri1") => NetKind::Tri1,
        Tok::Kw("trireg") => NetKind::Trireg,
        Tok::Kw("supply0") => NetKind::Supply0,
        Tok::Kw("supply1") => NetKind::Supply1,
        Tok::Kw("interconnect") => NetKind::Interconnect,
        _ => return None,
    })
}

impl Parser<'_> {
    /// Packed dimensions `[a:b]` / `[n]` (n unusual in packed position but accepted).
    pub(crate) fn dims(&mut self) -> PResult<Vec<Dim>> {
        let mut dims = Vec::new();
        while self.is_op("[") {
            let start = self.bump().span;
            if self.is_op("]") || self.is_op("$") || self.is_op("*") {
                // Dynamic array, queue or wildcard associative array.
                while !self.is_op("]") {
                    if self.at_eof() {
                        return Err(self.unexpected("']'"));
                    }
                    self.bump();
                }
                let end = self.bump().span;
                dims.push(Dim::Unsupported(start.to(end)));
                continue;
            }
            if is_type_keyword(self.peek()) {
                // Associative array with a type index.
                while !self.is_op("]") && !self.at_eof() {
                    self.bump();
                }
                let end = self.expect_op("]")?;
                dims.push(Dim::Unsupported(start.to(end)));
                continue;
            }
            let a = self.expr()?;
            if self.eat_op(":") {
                let b = self.expr()?;
                self.expect_op("]")?;
                dims.push(Dim::Range(a, b));
            } else {
                self.expect_op("]")?;
                dims.push(Dim::Size(a));
            }
        }
        Ok(dims)
    }

    fn signing(&mut self) -> Option<bool> {
        if self.eat_kw("signed") {
            Some(true)
        } else if self.eat_kw("unsigned") {
            Some(false)
        } else {
            None
        }
    }

    /// True if the cursor starts an explicit data type (not implicit).
    pub(crate) fn starts_explicit_type(&self) -> bool {
        is_type_keyword(self.peek()) || self.is_kw("type")
    }

    /// An explicit data type (keyword, enum, struct, `type(...)`, or a named type).
    pub(crate) fn data_type(&mut self) -> PResult<DataType> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Kw(k) => match k {
                "logic" | "reg" | "bit" => {
                    self.bump();
                    match k {
                        "logic" => TypeKind::Logic,
                        "reg" => TypeKind::Reg,
                        _ => TypeKind::Bit,
                    }
                }
                "byte" | "shortint" | "int" | "longint" | "integer" | "time" => {
                    self.bump();
                    match k {
                        "byte" => TypeKind::Byte,
                        "shortint" => TypeKind::Shortint,
                        "int" => TypeKind::Int,
                        "longint" => TypeKind::Longint,
                        "integer" => TypeKind::Integer,
                        _ => TypeKind::Time,
                    }
                }
                "real" | "shortreal" | "realtime" | "string" | "chandle" | "event" | "void" => {
                    self.bump();
                    match k {
                        "real" => TypeKind::Real,
                        "shortreal" => TypeKind::Shortreal,
                        "realtime" => TypeKind::Realtime,
                        "string" => TypeKind::String,
                        "chandle" => TypeKind::Chandle,
                        "event" => TypeKind::Event,
                        _ => TypeKind::Void,
                    }
                }
                "enum" => return self.enum_type(),
                "struct" | "union" => return self.struct_type(),
                "type" => {
                    self.bump();
                    self.expect_op("(")?;
                    let e = if self.starts_explicit_type() {
                        let t = self.data_type()?;
                        let sp = t.span;
                        crate::verilog::ast::Expr::new(
                            crate::verilog::ast::ExprKind::Type(Box::new(t)),
                            sp,
                        )
                    } else {
                        self.expr()?
                    };
                    self.expect_op(")")?;
                    TypeKind::TypeOf(Box::new(e))
                }
                "signed" | "unsigned" => {
                    let signing = self.signing();
                    let packed = self.dims()?;
                    return Ok(DataType {
                        kind: TypeKind::Implicit,
                        signing,
                        packed,
                        span: start.to(self.prev_span()),
                    });
                }
                _ => return Err(self.unexpected("a data type")),
            },
            Tok::Ident(_) | Tok::EscIdent(_) => TypeKind::Named(self.path()?),
            Tok::Op("[") => {
                let packed = self.dims()?;
                return Ok(DataType {
                    kind: TypeKind::Implicit,
                    signing: None,
                    packed,
                    span: start.to(self.prev_span()),
                });
            }
            _ => return Err(self.unexpected("a data type")),
        };
        let signing = self.signing();
        let packed = self.dims()?;
        Ok(DataType {
            kind,
            signing,
            packed,
            span: start.to(self.prev_span()),
        })
    }

    /// Data type or implicit type (`[signing] [dims]`). Named types are only recognized when
    /// `named_ok` is set and the lookahead shows a declared name after the type.
    pub(crate) fn data_type_or_implicit(&mut self, named_ok: bool) -> PResult<DataType> {
        if self.starts_explicit_type() || self.is_kw("signed") || self.is_kw("unsigned") {
            return self.data_type();
        }
        if named_ok && self.named_type_ahead() {
            return self.data_type();
        }
        let start = self.span();
        let signing = self.signing();
        let packed = self.dims()?;
        Ok(DataType {
            kind: TypeKind::Implicit,
            signing,
            packed,
            span: start.to(self.prev_span()),
        })
    }

    /// True if the cursor is at `ID [::ID] {[..]} ID`, i.e. a named type followed by a name.
    pub(crate) fn named_type_ahead(&self) -> bool {
        if !self.is_ident() {
            return false;
        }
        let mut k = 1;
        if self.is_op_at(1, "::") && self.is_ident_at(2) {
            k = 3;
        }
        while self.is_op_at(k, "[") {
            k = self.skip_brackets_from(k);
        }
        self.is_ident_at(k)
    }

    fn enum_type(&mut self) -> PResult<DataType> {
        let start = self.expect_kw("enum")?;
        let base = if self.is_op("{") {
            None
        } else {
            Some(Box::new(self.data_type()?))
        };
        self.expect_op("{")?;
        let mut members = Vec::new();
        loop {
            let name = self.ident()?;
            let mut range = None;
            if self.eat_op("[") {
                let a = self.expr()?;
                let b = if self.eat_op(":") {
                    Some(self.expr()?)
                } else {
                    None
                };
                self.expect_op("]")?;
                range = Some((a, b));
            }
            let value = if self.eat_op("=") {
                Some(self.expr()?)
            } else {
                None
            };
            members.push(EnumMember { name, range, value });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op("}")?;
        let packed = self.dims()?;
        Ok(DataType {
            kind: TypeKind::Enum { base, members },
            signing: None,
            packed,
            span: start.to(self.prev_span()),
        })
    }

    fn struct_type(&mut self) -> PResult<DataType> {
        let start = self.span();
        let union = self.is_kw("union");
        self.bump();
        self.eat_kw("tagged");
        let packed = self.eat_kw("packed");
        let signing = self.signing();
        self.expect_op("{")?;
        let mut members = Vec::new();
        while !self.is_op("}") {
            if self.at_eof() {
                return Err(self.unexpected("'}'"));
            }
            let mstart = self.span();
            self.eat_kw("rand");
            self.eat_kw("randc");
            let ty = self.data_type()?;
            let mut names = Vec::new();
            loop {
                let n = self.ident()?;
                let d = self.dims()?;
                if self.eat_op("=") {
                    // Default member values are only meaningful for unpacked structures.
                    let _ = self.expr()?;
                }
                names.push((n, d));
                if !self.eat_op(",") {
                    break;
                }
            }
            self.expect_op(";")?;
            members.push(StructMember {
                ty,
                names,
                span: mstart.to(self.prev_span()),
            });
        }
        self.expect_op("}")?;
        let dims = self.dims()?;
        Ok(DataType {
            kind: TypeKind::Struct {
                union,
                packed,
                members,
            },
            signing,
            packed: dims,
            span: start.to(self.prev_span()),
        })
    }

    /// Declarators after a type: `a [dims] [= init], b ...` up to (not including) `;`.
    pub(crate) fn declarators(&mut self) -> PResult<Vec<Declarator>> {
        let mut out = Vec::new();
        loop {
            let name = self.ident()?;
            let dims = self.dims()?;
            let init = if self.eat_op("=") {
                Some(self.expr()?)
            } else {
                None
            };
            out.push(Declarator { name, dims, init });
            if !self.eat_op(",") {
                break;
            }
        }
        Ok(out)
    }

    /// Skips drive strength `( strong0, weak1 )` and delays `#...` in net declarations.
    pub(crate) fn skip_strength_and_delay(&mut self) -> PResult<()> {
        if self.is_op("(")
            && matches!(
                self.peek_at(1),
                Tok::Kw(
                    "strong0"
                        | "strong1"
                        | "weak0"
                        | "weak1"
                        | "pull0"
                        | "pull1"
                        | "highz0"
                        | "highz1"
                        | "supply0"
                        | "supply1"
                        | "small"
                        | "medium"
                        | "large"
                )
            )
        {
            self.skip_balanced()?;
        }
        self.skip_delay()
    }

    /// Skips a delay `#n`, `#name`, `#(expr)`.
    pub(crate) fn skip_delay(&mut self) -> PResult<()> {
        if self.is_op("#") && !self.is_op_at(1, "(") {
            self.bump();
            self.bump();
        } else if self.is_op("#") {
            self.bump();
            self.skip_balanced()?;
        }
        Ok(())
    }

    /// Variable declaration after optional `var`/`const`/lifetime: `type declarators ;`
    pub(crate) fn var_decl(&mut self) -> PResult<DataDecl> {
        let start = self.span();
        let mut is_const = false;
        loop {
            if self.eat_kw("const") {
                is_const = true;
            } else if self.eat_kw("var") || self.eat_kw("static") || self.eat_kw("automatic") {
            } else {
                break;
            }
        }
        let ty = self.data_type_or_implicit(true)?;
        let decls = self.declarators()?;
        self.expect_op(";")?;
        Ok(DataDecl {
            net: None,
            is_const,
            ty,
            decls,
            span: start.to(self.prev_span()),
        })
    }

    /// Net declaration at a net-kind keyword.
    pub(crate) fn net_decl(&mut self) -> PResult<DataDecl> {
        let start = self.span();
        let net = net_kind(self.peek());
        self.bump();
        self.eat_kw("vectored");
        self.eat_kw("scalared");
        self.skip_strength_and_delay()?;
        let ty = self.data_type_or_implicit(true)?;
        self.skip_delay()?;
        let decls = self.declarators()?;
        self.expect_op(";")?;
        Ok(DataDecl {
            net,
            is_const: false,
            ty,
            decls,
            span: start.to(self.prev_span()),
        })
    }

    /// `typedef ...;`
    pub(crate) fn typedef(&mut self) -> PResult<Option<Typedef>> {
        let start = self.expect_kw("typedef")?;
        // Forward typedefs: `typedef name;`, `typedef enum name;`, `typedef struct name;`.
        if (self.is_ident() && self.is_op_at(1, ";"))
            || (matches!(self.peek(), Tok::Kw("enum" | "struct" | "union" | "class"))
                && self.is_ident_at(1)
                && self.is_op_at(2, ";"))
        {
            self.skip_to_semicolon()?;
            return Ok(None);
        }
        let ty = self.data_type()?;
        let name = self.ident()?;
        let dims = self.dims()?;
        self.expect_op(";")?;
        Ok(Some(Typedef {
            ty,
            name,
            dims,
            span: start.to(self.prev_span()),
        }))
    }

    /// `parameter ...;` or `localparam ...;` (body form).
    pub(crate) fn param_decl_stmt(&mut self) -> PResult<Vec<ParamDecl>> {
        let local = self.is_kw("localparam");
        self.bump();
        let mut out = Vec::new();
        if self.eat_kw("type") {
            loop {
                let start = self.span();
                let name = self.ident()?;
                let default = if self.eat_op("=") {
                    Some(self.data_type()?)
                } else {
                    None
                };
                out.push(ParamDecl {
                    local,
                    kind: ParamKind::Type { name, default },
                    span: start.to(self.prev_span()),
                });
                if !self.eat_op(",") {
                    break;
                }
            }
        } else {
            let ty = self.data_type_or_implicit(true)?;
            loop {
                let start = self.span();
                let name = self.ident()?;
                let unpacked = self.dims()?;
                let default = if self.eat_op("=") {
                    Some(self.expr()?)
                } else {
                    None
                };
                out.push(ParamDecl {
                    local,
                    kind: ParamKind::Value {
                        ty: ty.clone(),
                        name,
                        unpacked,
                        default,
                    },
                    span: start.to(self.prev_span()),
                });
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op(";")?;
        Ok(out)
    }
}
