// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Design units and module items.

use super::expr::is_type_keyword;
use super::types::net_kind;
use super::{PResult, Parser};
use crate::verilog::ast::{
    Always, AlwaysKind, AnsiPort, ContAssign, DataType, Direction, GateKind, Import, Item, Module,
    ModuleItem, NetKind, NonAnsiPort, Package, ParamDecl, ParamKind, PortDecl, PortList,
    SourceUnit, TypeKind, UnitKind,
};
use crate::verilog::token::Tok;

pub(crate) fn direction(t: &Tok) -> Option<Direction> {
    Some(match t {
        Tok::Kw("input") => Direction::Input,
        Tok::Kw("output") => Direction::Output,
        Tok::Kw("inout") => Direction::Inout,
        Tok::Kw("ref") => Direction::Ref,
        _ => return None,
    })
}

pub(crate) fn gate_kind(t: &Tok) -> Option<GateKind> {
    Some(match t {
        Tok::Kw("and") => GateKind::And,
        Tok::Kw("nand") => GateKind::Nand,
        Tok::Kw("or") => GateKind::Or,
        Tok::Kw("nor") => GateKind::Nor,
        Tok::Kw("xor") => GateKind::Xor,
        Tok::Kw("xnor") => GateKind::Xnor,
        Tok::Kw("not") => GateKind::Not,
        Tok::Kw("buf") => GateKind::Buf,
        _ => return None,
    })
}

impl Parser<'_> {
    /// Modport items after the `modport` keyword, up to and including the `;`.
    fn modports(&mut self) -> PResult<Vec<crate::verilog::ast::Modport>> {
        let mut out = Vec::new();
        loop {
            let start = self.span();
            let name = self.ident()?;
            self.expect_op("(")?;
            let mut dir: Option<Direction> = None;
            let mut ports = Vec::new();
            let mut unsupported = None;
            if !self.eat_op(")") {
                loop {
                    if let Some(d) = direction(self.peek()) {
                        self.bump();
                        dir = Some(d);
                    }
                    let simple =
                        self.is_ident() && (self.is_op_at(1, ",") || self.is_op_at(1, ")"));
                    match (dir, simple) {
                        (Some(d), true) => {
                            let id = self.ident()?;
                            ports.push((d, id));
                        }
                        _ => {
                            // Modport expression, clocking or subroutine import/export.
                            unsupported.get_or_insert(self.span());
                            let mut depth = 0usize;
                            while depth > 0 || !(self.is_op(",") || self.is_op(")")) {
                                if self.at_eof() {
                                    return Err(self.unexpected("')'"));
                                }
                                if self.is_op("(") {
                                    depth += 1;
                                } else if self.is_op(")") {
                                    depth -= 1;
                                }
                                self.bump();
                            }
                        }
                    }
                    if self.eat_op(",") {
                        continue;
                    }
                    self.expect_op(")")?;
                    break;
                }
            }
            out.push(crate::verilog::ast::Modport {
                name,
                ports,
                unsupported,
                span: start.to(self.prev_span()),
            });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(";")?;
        Ok(out)
    }

    pub(crate) fn source_unit(&mut self) -> PResult<SourceUnit> {
        let mut items = Vec::new();
        loop {
            match self.peek().clone() {
                Tok::Eof => break,
                Tok::DefaultNettype(v) => {
                    self.bump();
                    self.default_nettype = if v == "wire" { None } else { Some(v) };
                }
                Tok::Op(";") => {
                    self.bump();
                }
                Tok::Kw("module" | "macromodule" | "interface" | "program") => {
                    items.push(Item::Module(Box::new(self.module()?)));
                }
                Tok::Kw("package") => items.push(Item::Package(Box::new(self.package()?))),
                Tok::Kw(k @ ("class" | "checker" | "config" | "primitive")) => {
                    let start = self.span();
                    let end = match k {
                        "class" => "endclass",
                        "checker" => "endchecker",
                        "config" => "endconfig",
                        _ => "endprimitive",
                    };
                    let sp = self.skip_to_kw(end)?;
                    items.push(Item::Unsupported {
                        what: k.to_string(),
                        span: start.to(sp),
                    });
                }
                Tok::Kw(k @ ("bind" | "virtual")) => {
                    let start = self.span();
                    self.skip_to_semicolon()?;
                    items.push(Item::Unsupported {
                        what: k.to_string(),
                        span: start,
                    });
                }
                _ => {
                    if let Some(it) = self.module_item()? {
                        items.push(Item::Decl(it));
                    }
                }
            }
        }
        Ok(SourceUnit { items })
    }

    fn package(&mut self) -> PResult<Package> {
        let start = self.expect_kw("package")?;
        self.eat_kw("automatic");
        self.eat_kw("static");
        let name = self.ident()?;
        self.expect_op(";")?;
        let mut items = Vec::new();
        while !self.is_kw("endpackage") {
            if self.at_eof() {
                return Err(self.unexpected("'endpackage'"));
            }
            if let Some(it) = self.module_item()? {
                items.push(it);
            }
        }
        self.bump();
        self.end_label();
        Ok(Package {
            name,
            items,
            span: start.to(self.prev_span()),
        })
    }

    pub(crate) fn import_stmt(&mut self) -> PResult<Vec<Import>> {
        self.expect_kw("import")?;
        let mut out = Vec::new();
        loop {
            let start = self.span();
            let package = self.ident()?;
            self.expect_op("::")?;
            let name = if self.eat_op("*") {
                None
            } else {
                Some(self.ident()?)
            };
            out.push(Import {
                package,
                name,
                span: start.to(self.prev_span()),
            });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(";")?;
        Ok(out)
    }

    fn module(&mut self) -> PResult<Module> {
        let start = self.span();
        let kind = match self.bump().tok {
            Tok::Kw("interface") => UnitKind::Interface,
            Tok::Kw("program") => UnitKind::Program,
            _ => UnitKind::Module,
        };
        self.eat_kw("automatic");
        self.eat_kw("static");
        let name = self.ident()?;
        let mut imports = Vec::new();
        while self.is_kw("import") {
            imports.extend(self.import_stmt()?);
        }
        let params = if self.eat_op("#") {
            self.param_port_list()?
        } else {
            Vec::new()
        };
        let ports = if self.is_op("(") {
            self.port_list()?
        } else {
            PortList::None
        };
        self.expect_op(";")?;
        let default_nettype = self.default_nettype.clone();
        let end_kw = match kind {
            UnitKind::Module => "endmodule",
            UnitKind::Interface => "endinterface",
            UnitKind::Program => "endprogram",
        };
        let mut items = Vec::new();
        while !self.is_kw(end_kw) {
            if self.at_eof() {
                return Err(self.unexpected(&format!("'{end_kw}'")));
            }
            if let Some(it) = self.module_item()? {
                items.push(it);
            }
        }
        self.bump();
        self.end_label();
        Ok(Module {
            kind,
            name,
            imports,
            params,
            ports,
            items,
            default_nettype,
            span: start.to(self.prev_span()),
        })
    }

    /// `#( ... )` parameter port list (the `#` is consumed).
    fn param_port_list(&mut self) -> PResult<Vec<ParamDecl>> {
        self.expect_op("(")?;
        let mut out: Vec<ParamDecl> = Vec::new();
        if self.eat_op(")") {
            return Ok(out);
        }
        let mut local = false;
        let mut is_type = false;
        let mut cur_ty: Option<DataType> = None;
        loop {
            let start = self.span();
            let mut explicit_kw = false;
            if self.is_kw("parameter") || self.is_kw("localparam") {
                local = self.is_kw("localparam");
                self.bump();
                explicit_kw = true;
                is_type = false;
                cur_ty = None;
            }
            if self.eat_kw("type") {
                is_type = true;
                cur_ty = None;
            } else if explicit_kw
                || self.starts_explicit_type()
                || self.named_type_ahead()
                || self.is_op("[")
                || self.is_kw("signed")
                || self.is_kw("unsigned")
            {
                // A (possibly implicit) data type precedes the name; otherwise the entry
                // inherits kind and type from the previous one.
                is_type = false;
                cur_ty = Some(self.data_type_or_implicit(true)?);
            }
            let name = self.ident()?;
            if is_type {
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
            } else {
                let unpacked = self.dims()?;
                let default = if self.eat_op("=") {
                    Some(self.expr()?)
                } else {
                    None
                };
                let ty = cur_ty
                    .clone()
                    .unwrap_or_else(|| DataType::implicit(name.span));
                out.push(ParamDecl {
                    local,
                    kind: ParamKind::Value {
                        ty,
                        name,
                        unpacked,
                        default,
                    },
                    span: start.to(self.prev_span()),
                });
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(out)
    }

    fn port_list(&mut self) -> PResult<PortList> {
        self.expect_op("(")?;
        if self.eat_op(")") {
            return Ok(PortList::Ansi(Vec::new()));
        }
        let ansi = direction(self.peek()).is_some()
            || net_kind(self.peek()).is_some()
            || self.starts_explicit_type()
            || self.is_kw("var")
            || self.is_kw("interface")
            || self.named_type_ahead()
            || (self.is_ident()
                && self.is_op_at(1, ".")
                && self.is_ident_at(2)
                && self.is_ident_at(3));
        if !ansi {
            let mut ports = Vec::new();
            loop {
                let start = self.span();
                if self.is_op(",") || self.is_op(")") {
                    ports.push(NonAnsiPort {
                        external: None,
                        expr: None,
                        span: start,
                    });
                } else if self.is_op(".") {
                    self.bump();
                    let ext = self.ident()?;
                    self.expect_op("(")?;
                    let e = if self.is_op(")") {
                        None
                    } else {
                        Some(self.expr()?)
                    };
                    self.expect_op(")")?;
                    ports.push(NonAnsiPort {
                        external: Some(ext),
                        expr: e,
                        span: start.to(self.prev_span()),
                    });
                } else {
                    let e = self.expr()?;
                    ports.push(NonAnsiPort {
                        external: None,
                        expr: Some(e),
                        span: start.to(self.prev_span()),
                    });
                }
                if !self.eat_op(",") {
                    break;
                }
            }
            self.expect_op(")")?;
            return Ok(PortList::NonAnsi(ports));
        }
        let mut ports: Vec<AnsiPort> = Vec::new();
        loop {
            let start = self.span();
            let dir = direction(self.peek());
            if dir.is_some() {
                self.bump();
            }
            let mut net = net_kind(self.peek());
            if net.is_some() {
                self.bump();
            }
            let is_var = self.eat_kw("var");
            let mut interface = None;
            if self.eat_kw("interface") {
                let modport = if self.eat_op(".") {
                    Some(self.ident()?)
                } else {
                    None
                };
                interface = Some((
                    crate::verilog::ast::Ident {
                        name: "interface".into(),
                        span: start,
                    },
                    modport,
                ));
            } else if self.is_ident()
                && self.is_op_at(1, ".")
                && self.is_ident_at(2)
                && self.is_ident_at(3)
            {
                let intf = self.ident()?;
                self.bump();
                let mp = self.ident()?;
                interface = Some((intf, Some(mp)));
            }
            let has_type = interface.is_none()
                && (self.starts_explicit_type()
                    || self.named_type_ahead()
                    || self.is_op("[")
                    || self.is_kw("signed")
                    || self.is_kw("unsigned"));
            let ty = if has_type {
                self.data_type_or_implicit(true)?
            } else {
                DataType::implicit(start)
            };
            let name = self.ident()?;
            let unpacked = self.dims()?;
            let default = if self.eat_op("=") {
                Some(self.expr()?)
            } else {
                None
            };
            let prev = ports.last();
            let nothing =
                dir.is_none() && net.is_none() && !is_var && !has_type && interface.is_none();
            let (dir, net, ty) = if let (true, Some(p)) = (nothing, prev) {
                (p.dir, p.net, p.ty.clone())
            } else {
                let dir = dir.or(prev.map(|p| p.dir)).unwrap_or(Direction::Inout);
                if net.is_none() && !is_var {
                    // Implicit types and input/inout ports are nets; outputs with an explicit
                    // data type are variables.
                    let explicit = !ty.is_implicit();
                    if !(explicit && dir == Direction::Output) {
                        net = Some(NetKind::Wire);
                    }
                }
                (dir, net, ty)
            };
            ports.push(AnsiPort {
                dir,
                net,
                ty,
                name,
                unpacked,
                default,
                interface,
                span: start.to(self.prev_span()),
            });
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(PortList::Ansi(ports))
    }

    /// Non-ANSI port declaration or task/function argument declaration.
    pub(crate) fn port_decl(&mut self) -> PResult<PortDecl> {
        let start = self.span();
        let dir = direction(self.peek()).ok_or_else(|| self.unexpected("a port direction"))?;
        self.bump();
        let net = net_kind(self.peek());
        if net.is_some() {
            self.bump();
        }
        let mut is_var = self.eat_kw("var");
        let ty = self.data_type_or_implicit(true)?;
        if matches!(
            ty.kind,
            TypeKind::Reg | TypeKind::Logic | TypeKind::Integer | TypeKind::Int
        ) && net.is_none()
            && dir == Direction::Output
        {
            is_var = true;
        }
        let mut names = Vec::new();
        loop {
            let n = self.ident()?;
            let d = self.dims()?;
            let init = if self.eat_op("=") {
                Some(self.expr()?)
            } else {
                None
            };
            names.push((n, d, init));
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(";")?;
        Ok(PortDecl {
            dir,
            net,
            is_var,
            ty,
            names,
            span: start.to(self.prev_span()),
        })
    }

    /// One module item; `None` for items that produce nothing (empty `;`, forward typedefs).
    #[allow(clippy::too_many_lines)]
    pub(crate) fn module_item(&mut self) -> PResult<Option<ModuleItem>> {
        let start = self.span();
        let item = match self.peek().clone() {
            Tok::DefaultNettype(v) => {
                self.bump();
                self.default_nettype = if v == "wire" { None } else { Some(v) };
                return Ok(None);
            }
            Tok::Op(";") => {
                self.bump();
                return Ok(None);
            }
            Tok::Kw("input" | "output" | "inout" | "ref") => {
                ModuleItem::PortDecl(self.port_decl()?)
            }
            t if net_kind(&t).is_some() => ModuleItem::Data(self.net_decl()?),
            Tok::Kw("parameter" | "localparam") => ModuleItem::Param(self.param_decl_stmt()?),
            Tok::Kw("typedef") => match self.typedef()? {
                Some(t) => ModuleItem::Typedef(t),
                None => return Ok(None),
            },
            Tok::Kw("import") => ModuleItem::Import(self.import_stmt()?),
            Tok::Kw("export") => {
                let sp = self.skip_to_semicolon()?;
                ModuleItem::Ignored {
                    what: "export",
                    span: sp,
                }
            }
            Tok::Kw("assign") => {
                self.bump();
                self.skip_strength_and_delay()?;
                let mut v = Vec::new();
                loop {
                    let s = self.span();
                    let lhs = self.lvalue()?;
                    self.expect_op("=")?;
                    let rhs = self.expr()?;
                    v.push(ContAssign {
                        lhs,
                        rhs,
                        span: s.to(self.prev_span()),
                    });
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op(";")?;
                ModuleItem::Assign(v)
            }
            Tok::Kw(k @ ("always" | "always_comb" | "always_ff" | "always_latch")) => {
                self.bump();
                let kind = match k {
                    "always_comb" => AlwaysKind::Comb,
                    "always_ff" => AlwaysKind::Ff,
                    "always_latch" => AlwaysKind::Latch,
                    _ => AlwaysKind::Always,
                };
                let body = self.stmt()?;
                ModuleItem::Always(Always {
                    kind,
                    body,
                    span: start.to(self.prev_span()),
                })
            }
            Tok::Kw("initial") => {
                self.bump();
                ModuleItem::Initial(self.stmt()?)
            }
            Tok::Kw("final") => {
                self.bump();
                ModuleItem::Final(self.stmt()?)
            }
            Tok::Kw("genvar") => {
                self.bump();
                let mut v = vec![self.ident()?];
                while self.eat_op(",") {
                    v.push(self.ident()?);
                }
                self.expect_op(";")?;
                ModuleItem::Genvar(v)
            }
            Tok::Kw("generate") => {
                self.bump();
                let mut v = Vec::new();
                while !self.is_kw("endgenerate") {
                    if self.at_eof() {
                        return Err(self.unexpected("'endgenerate'"));
                    }
                    if let Some(it) = self.module_item()? {
                        v.push(it);
                    }
                }
                self.bump();
                ModuleItem::GenRegion(v, start.to(self.prev_span()))
            }
            Tok::Kw("for") => ModuleItem::GenFor(Box::new(self.gen_for()?)),
            Tok::Kw("if") => ModuleItem::GenIf(Box::new(self.gen_if()?)),
            Tok::Kw("case") => ModuleItem::GenCase(Box::new(self.gen_case()?)),
            Tok::CaseAttrs(_) => {
                // Case attributes only concern procedural case statements.
                self.bump();
                return self.module_item();
            }
            Tok::Kw("begin") => ModuleItem::GenBlock(self.gen_block()?),
            Tok::Kw("function") => ModuleItem::Function(Box::new(self.function()?)),
            Tok::Kw("task") => ModuleItem::Task(Box::new(self.task()?)),
            t if gate_kind(&t).is_some() => ModuleItem::Gate(self.gates()?),
            Tok::Kw(
                k @ ("bufif0" | "bufif1" | "notif0" | "notif1" | "nmos" | "pmos" | "cmos" | "rnmos"
                | "rpmos" | "rcmos" | "tran" | "tranif0" | "tranif1" | "rtran" | "rtranif0"
                | "rtranif1" | "pullup" | "pulldown"),
            ) => {
                let sp = self.skip_to_semicolon()?;
                ModuleItem::Unsupported {
                    what: format!("gate primitive '{k}'"),
                    span: sp,
                }
            }
            Tok::Kw("assert" | "assume" | "cover" | "restrict") => {
                self.skip_assertion()?;
                ModuleItem::Ignored {
                    what: "assertion",
                    span: start.to(self.prev_span()),
                }
            }
            Tok::Kw("property") => ModuleItem::Ignored {
                what: "property",
                span: self.skip_to_kw("endproperty")?,
            },
            Tok::Kw("sequence") => ModuleItem::Ignored {
                what: "sequence",
                span: self.skip_to_kw("endsequence")?,
            },
            Tok::Kw("covergroup") => ModuleItem::Ignored {
                what: "covergroup",
                span: self.skip_to_kw("endgroup")?,
            },
            Tok::Kw("clocking") => ModuleItem::Ignored {
                what: "clocking block",
                span: self.skip_to_kw("endclocking")?,
            },
            Tok::Kw("specify") => ModuleItem::Ignored {
                what: "specify block",
                span: self.skip_to_kw("endspecify")?,
            },
            Tok::Kw("default") => {
                // default clocking / default disable iff
                if self.is_kw_at(1, "clocking") {
                    let is_block = {
                        // `default clocking name;` versus a clocking block definition.
                        let mut k = 2;
                        while !matches!(self.peek_at(k), Tok::Op(";") | Tok::Eof)
                            && !self.is_op_at(k, "@")
                        {
                            k += 1;
                        }
                        self.is_op_at(k, "@")
                    };
                    let sp = if is_block {
                        self.skip_to_kw("endclocking")?
                    } else {
                        self.skip_to_semicolon()?
                    };
                    ModuleItem::Ignored {
                        what: "default clocking",
                        span: sp,
                    }
                } else {
                    ModuleItem::Ignored {
                        what: "default disable",
                        span: self.skip_to_semicolon()?,
                    }
                }
            }
            Tok::Kw("timeunit" | "timeprecision" | "specparam") => ModuleItem::Ignored {
                what: "time declaration",
                span: self.skip_to_semicolon()?,
            },
            Tok::Kw("defparam") => ModuleItem::Defparam(self.skip_to_semicolon()?),
            Tok::Kw("modport") => {
                self.bump();
                ModuleItem::Modport(self.modports()?)
            }
            Tok::Kw(k @ ("alias" | "let" | "nettype" | "bind")) => ModuleItem::Unsupported {
                what: k.to_string(),
                span: self.skip_to_semicolon()?,
            },
            Tok::Kw(k @ ("class" | "checker" | "program" | "interface" | "module")) => {
                let end = match k {
                    "class" => "endclass",
                    "checker" => "endchecker",
                    "program" => "endprogram",
                    "interface" => "endinterface",
                    _ => "endmodule",
                };
                ModuleItem::Unsupported {
                    what: format!("nested {k}"),
                    span: self.skip_to_kw(end)?,
                }
            }
            Tok::SysIdent(name) => {
                let e = self.expr()?;
                self.expect_op(";")?;
                let args = match e.kind {
                    crate::verilog::ast::ExprKind::SysCall { args, .. } => args,
                    _ => Vec::new(),
                };
                ModuleItem::ElabTask {
                    name,
                    args,
                    span: start.to(self.prev_span()),
                }
            }
            Tok::Kw("var" | "const" | "static" | "automatic") => ModuleItem::Data(self.var_decl()?),
            t if is_type_keyword(&t) || matches!(t, Tok::Kw("signed" | "unsigned")) => {
                ModuleItem::Data(self.var_decl()?)
            }
            Tok::Ident(_) | Tok::EscIdent(_) => {
                // Assertion label, instance, or declaration with a named type.
                if self.is_op_at(1, ":")
                    && matches!(
                        self.peek_at(2),
                        Tok::Kw("assert" | "assume" | "cover" | "restrict")
                    )
                {
                    self.bump();
                    self.bump();
                    self.skip_assertion()?;
                    ModuleItem::Ignored {
                        what: "assertion",
                        span: start.to(self.prev_span()),
                    }
                } else if self.is_instance_ahead() {
                    ModuleItem::Instance(self.instantiation()?)
                } else {
                    ModuleItem::Data(self.var_decl()?)
                }
            }
            _ => return Err(self.unexpected("a module item")),
        };
        Ok(Some(item))
    }
}
