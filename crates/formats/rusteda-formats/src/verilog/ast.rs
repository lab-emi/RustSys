// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Syntax tree of the accepted SystemVerilog subset (VERILOG-REQ-0100, VERILOG-REQ-0103).

use crate::verilog::number::Literal;
use crate::verilog::span::Span;

mod stmt;
pub use stmt::*;

/// An identifier with its location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// A possibly package-scoped name: `name` or `pkg::name`.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub scope: Option<Ident>,
    pub name: Ident,
}

/// A parsed file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceUnit {
    pub items: Vec<Item>,
}

/// A top-level item.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Item {
    Module(Box<Module>),
    Package(Box<Package>),
    /// A declaration in the compilation-unit scope.
    Decl(ModuleItem),
    /// A construct recognized but not modeled (for example `class`); consumers reject it.
    Unsupported {
        what: String,
        span: Span,
    },
}

/// Kind of a module-like design unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Module,
    Interface,
    Program,
}

/// A module, interface or program.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub kind: UnitKind,
    pub name: Ident,
    pub imports: Vec<Import>,
    pub params: Vec<ParamDecl>,
    pub ports: PortList,
    pub items: Vec<ModuleItem>,
    /// Value of `default_nettype in effect at the module header (`None` = `wire`).
    pub default_nettype: Option<String>,
    pub span: Span,
}

/// A package.
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub name: Ident,
    pub items: Vec<ModuleItem>,
    pub span: Span,
}

/// Port list of a module header.
#[derive(Debug, Clone, PartialEq)]
pub enum PortList {
    None,
    Ansi(Vec<AnsiPort>),
    /// Non-ANSI list of port expressions (usually plain names).
    NonAnsi(Vec<NonAnsiPort>),
}

/// Port direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
    Inout,
    Ref,
}

/// Net kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetKind {
    Wire,
    Tri,
    Uwire,
    Wand,
    Wor,
    Triand,
    Trior,
    Tri0,
    Tri1,
    Trireg,
    Supply0,
    Supply1,
    Interconnect,
}

/// An ANSI port declaration after applying the inheritance rules of the standard.
#[derive(Debug, Clone, PartialEq)]
pub struct AnsiPort {
    pub dir: Direction,
    /// `Some` for net ports, `None` for variable ports.
    pub net: Option<NetKind>,
    pub ty: DataType,
    pub name: Ident,
    pub unpacked: Vec<Dim>,
    pub default: Option<Expr>,
    /// Interface port: `intf_name[.modport] name`.
    pub interface: Option<(Ident, Option<Ident>)>,
    pub span: Span,
}

/// Entry of a non-ANSI port list.
#[derive(Debug, Clone, PartialEq)]
pub struct NonAnsiPort {
    /// External name (`.name(expr)` form) if given.
    pub external: Option<Ident>,
    /// Port expression (usually an identifier); `None` for an empty port.
    pub expr: Option<Expr>,
    pub span: Span,
}

/// A data type.
#[derive(Debug, Clone, PartialEq)]
pub struct DataType {
    pub kind: TypeKind,
    /// Explicit `signed` (true) or `unsigned` (false).
    pub signing: Option<bool>,
    pub packed: Vec<Dim>,
    pub span: Span,
}

impl DataType {
    /// The implicit type (no keyword, no dimensions).
    pub fn implicit(span: Span) -> DataType {
        DataType {
            kind: TypeKind::Implicit,
            signing: None,
            packed: Vec::new(),
            span,
        }
    }

    pub fn is_implicit(&self) -> bool {
        matches!(self.kind, TypeKind::Implicit)
    }
}

/// Built-in and user type kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    /// Only signing and packed dimensions were written (or nothing).
    Implicit,
    Logic,
    Reg,
    Bit,
    Integer,
    Int,
    Shortint,
    Longint,
    Byte,
    Time,
    Real,
    Shortreal,
    Realtime,
    String,
    Chandle,
    Event,
    Void,
    Enum {
        base: Option<Box<DataType>>,
        members: Vec<EnumMember>,
    },
    Struct {
        union: bool,
        packed: bool,
        members: Vec<StructMember>,
    },
    /// A named type: typedef, type parameter or package type.
    Named(Path),
    /// `type(expr)` or `type(data_type)`.
    TypeOf(Box<Expr>),
}

/// An enumeration constant.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumMember {
    pub name: Ident,
    /// `name[N]` or `name[N:M]` ranges.
    pub range: Option<(Expr, Option<Expr>)>,
    pub value: Option<Expr>,
}

/// A structure or union member declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct StructMember {
    pub ty: DataType,
    pub names: Vec<(Ident, Vec<Dim>)>,
    pub span: Span,
}

/// An array dimension.
#[derive(Debug, Clone, PartialEq)]
pub enum Dim {
    /// `[msb:lsb]`
    Range(Expr, Expr),
    /// `[size]`
    Size(Expr),
    /// `[]`, `[$]`, `[*]` or `[type]` forms (dynamic, queue, associative); rejected by consumers.
    Unsupported(Span),
}

/// Parameter declaration (value or type).
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDecl {
    pub local: bool,
    pub kind: ParamKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    Value {
        ty: DataType,
        name: Ident,
        unpacked: Vec<Dim>,
        default: Option<Expr>,
    },
    Type {
        name: Ident,
        default: Option<DataType>,
    },
}

/// `import pkg::name;` or `import pkg::*;`
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub package: Ident,
    /// `None` for a wildcard import.
    pub name: Option<Ident>,
    pub span: Span,
}

/// Items allowed in modules, packages, generate blocks and the compilation unit.
#[derive(Debug, Clone, PartialEq)]
pub enum ModuleItem {
    PortDecl(PortDecl),
    Data(DataDecl),
    Param(Vec<ParamDecl>),
    Typedef(Typedef),
    Import(Vec<Import>),
    Assign(Vec<ContAssign>),
    Always(Always),
    Initial(Stmt),
    Final(Stmt),
    Instance(Instantiation),
    Gate(Vec<GateInst>),
    Genvar(Vec<Ident>),
    GenFor(Box<GenFor>),
    GenIf(Box<GenIf>),
    GenCase(Box<GenCase>),
    /// A bare `begin ... end` generate block.
    GenBlock(GenBlock),
    /// `generate ... endgenerate`
    GenRegion(Vec<ModuleItem>, Span),
    Function(Box<FunctionDecl>),
    Task(Box<TaskDecl>),
    /// Elaboration system task (`$error`, `$fatal`, `$warning`, `$info`) at module level.
    ElabTask {
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
    Defparam(Span),
    /// `modport` declarations of an interface.
    Modport(Vec<Modport>),
    /// Construct parsed and ignored (VERILOG-REQ-0104).
    Ignored {
        what: &'static str,
        span: Span,
    },
    /// Construct recognized but not modeled.
    Unsupported {
        what: String,
        span: Span,
    },
}

/// One modport of a `modport` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Modport {
    pub name: Ident,
    /// Simple ports with their direction.
    pub ports: Vec<(Direction, Ident)>,
    /// Span of the first entry that is not a simple port (modport expression, clocking,
    /// `import`/`export`), if any.
    pub unsupported: Option<Span>,
    pub span: Span,
}

/// Non-ANSI port declaration (`input [7:0] a, b;`).
#[derive(Debug, Clone, PartialEq)]
pub struct PortDecl {
    pub dir: Direction,
    pub net: Option<NetKind>,
    /// `true` if declared with `var` or as a variable type (e.g. `output reg`).
    pub is_var: bool,
    pub ty: DataType,
    pub names: Vec<(Ident, Vec<Dim>, Option<Expr>)>,
    pub span: Span,
}

/// Net or variable declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct DataDecl {
    /// `Some` for nets.
    pub net: Option<NetKind>,
    pub is_const: bool,
    pub ty: DataType,
    pub decls: Vec<Declarator>,
    pub span: Span,
}

/// One declared name.
#[derive(Debug, Clone, PartialEq)]
pub struct Declarator {
    pub name: Ident,
    pub dims: Vec<Dim>,
    pub init: Option<Expr>,
}

/// `typedef <type> name <dims>;`
#[derive(Debug, Clone, PartialEq)]
pub struct Typedef {
    pub ty: DataType,
    pub name: Ident,
    pub dims: Vec<Dim>,
    pub span: Span,
}

/// One assignment of an `assign` statement.
#[derive(Debug, Clone, PartialEq)]
pub struct ContAssign {
    pub lhs: Expr,
    pub rhs: Expr,
    pub span: Span,
}

/// Procedure kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlwaysKind {
    Always,
    Comb,
    Ff,
    Latch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Always {
    pub kind: AlwaysKind,
    pub body: Stmt,
    pub span: Span,
}

/// A module instantiation statement (one or more instances).
#[derive(Debug, Clone, PartialEq)]
pub struct Instantiation {
    pub module: Ident,
    pub params: Vec<ParamAssign>,
    pub instances: Vec<Instance>,
    pub span: Span,
}

/// Parameter value in `#( ... )`.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Expr(Expr),
    Type(DataType),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamAssign {
    Ordered(ParamValue),
    Named(Ident, Option<ParamValue>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub name: Ident,
    pub dims: Vec<Dim>,
    pub conns: Vec<PortConn>,
    pub span: Span,
}

/// A port connection.
#[derive(Debug, Clone, PartialEq)]
pub enum PortConn {
    /// Positional; `None` for an empty position.
    Ordered(Option<Expr>),
    /// `.name(expr)`, `.name()` (inner `None`) or `.name` (outer `None`).
    Named(Ident, Option<Option<Expr>>),
    /// `.*`
    Wildcard(Span),
}

/// Gate primitive kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
    Not,
    Buf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateInst {
    pub kind: GateKind,
    pub name: Option<Ident>,
    /// Output terminal(s) first, then inputs.
    pub terms: Vec<Expr>,
    pub span: Span,
}

/// A generate block.
#[derive(Debug, Clone, PartialEq)]
pub struct GenBlock {
    pub label: Option<Ident>,
    pub items: Vec<ModuleItem>,
    /// `true` if written with `begin ... end`.
    pub has_begin: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenFor {
    pub genvar: Ident,
    pub declares_genvar: bool,
    pub init: Expr,
    pub cond: Expr,
    /// Step as an assignment statement (`i++`, `i += 2`, `i = i + 1`).
    pub step: Stmt,
    pub body: GenBlock,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenIf {
    pub cond: Expr,
    pub then: GenBlock,
    pub els: Option<GenBlock>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenCase {
    pub expr: Expr,
    /// Items; an empty label list denotes `default`.
    pub items: Vec<(Vec<Expr>, GenBlock)>,
    pub span: Span,
}

/// Task or function formal argument.
#[derive(Debug, Clone, PartialEq)]
pub struct TfPort {
    pub dir: Direction,
    pub ty: DataType,
    pub name: Ident,
    pub dims: Vec<Dim>,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDecl {
    pub name: Ident,
    /// `None` for `void` functions.
    pub ret: Option<DataType>,
    pub ports: Vec<TfPort>,
    pub decls: Vec<BlockDecl>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskDecl {
    pub name: Ident,
    pub ports: Vec<TfPort>,
    pub decls: Vec<BlockDecl>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// Declarations at the start of a block or subroutine.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockDecl {
    Data(DataDecl),
    Param(Vec<ParamDecl>),
    Typedef(Typedef),
    Import(Vec<Import>),
    /// Old-style task/function argument declaration inside the body.
    Port(PortDecl),
}
