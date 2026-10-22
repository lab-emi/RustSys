// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Statements and expressions of the syntax tree (VERILOG-REQ-0100).

use super::*;

/// A statement.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub label: Option<Ident>,
    pub span: Span,
}

/// Assignment operators (`=` or a compound operator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Eq,
    Compound(BinOp),
}

/// `unique`, `unique0`, `priority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qualifier {
    Unique,
    Unique0,
    Priority,
}

/// The `full_case` and `parallel_case` attributes of a case statement (IEEE 1364.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaseAttrs {
    pub full: bool,
    pub parallel: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseKind {
    Case,
    Casez,
    Casex,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseItem {
    /// Labels; empty for `default`.
    pub labels: Vec<Expr>,
    pub body: Stmt,
}

/// `for` initializer: optional declared type, variable, value.
#[derive(Debug, Clone, PartialEq)]
pub struct ForInit {
    pub ty: Option<DataType>,
    pub lhs: Expr,
    pub rhs: Expr,
}

/// Edge of an event expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Any,
    Pos,
    Neg,
    Both,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventExpr {
    pub edge: Edge,
    pub expr: Expr,
    pub iff: Option<Expr>,
}

/// Timing control at the head of a statement.
#[derive(Debug, Clone, PartialEq)]
pub enum TimingCtrl {
    /// `@*` or `@(*)`
    Star,
    /// `@(e1 or e2, ...)`
    Events(Vec<EventExpr>),
    /// `#delay` (ignored by consumers).
    Delay,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Null,
    Block {
        name: Option<Ident>,
        decls: Vec<BlockDecl>,
        stmts: Vec<Stmt>,
    },
    Assign {
        lhs: Expr,
        op: AssignOp,
        rhs: Expr,
        nonblocking: bool,
    },
    IncDec {
        target: Expr,
        inc: bool,
    },
    If {
        qual: Option<Qualifier>,
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    Case {
        qual: Option<Qualifier>,
        attrs: CaseAttrs,
        kind: CaseKind,
        inside: bool,
        expr: Expr,
        items: Vec<CaseItem>,
    },
    For {
        init: Vec<ForInit>,
        cond: Option<Expr>,
        step: Vec<Stmt>,
        body: Box<Stmt>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    Repeat {
        count: Expr,
        body: Box<Stmt>,
    },
    Forever(Box<Stmt>),
    Foreach {
        array: Expr,
        vars: Vec<Option<Ident>>,
        body: Box<Stmt>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    Timed {
        ctrl: TimingCtrl,
        body: Box<Stmt>,
    },
    /// Subroutine or system task call used as a statement.
    Call(Expr),
    /// Parsed and ignored (assertions, `wait`, `disable`, ...).
    Ignored(&'static str),
    /// Recognized but not modeled (for example `force`, `fork`).
    Unsupported(String),
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Plus,
    Minus,
    LogNot,
    BitNot,
    RedAnd,
    RedNand,
    RedOr,
    RedNor,
    RedXor,
    RedXnor,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Shl,
    Shr,
    AShl,
    AShr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    CaseEq,
    CaseNe,
    WildEq,
    WildNe,
    BitAnd,
    BitOr,
    BitXor,
    BitXnor,
    LogAnd,
    LogOr,
    Implies,
    Equiv,
}

/// Part-select kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceKind {
    /// `[msb:lsb]`
    Const,
    /// `[base+:width]`
    Up,
    /// `[base-:width]`
    Down,
}

/// Target of a cast.
#[derive(Debug, Clone, PartialEq)]
pub enum CastTarget {
    /// A type written with type syntax (keyword types, `struct`, ...).
    Type(DataType),
    /// An expression before `'`: a size, a parameter, or a named type; resolved by consumers.
    Expr(Box<Expr>),
    Signed,
    Unsigned,
    Const,
}

/// Key of an assignment pattern item.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternKey {
    Default,
    /// A member name or an index expression (resolved by consumers).
    Expr(Expr),
    Type(DataType),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternItem {
    Positional(Expr),
    Keyed(PatternKey, Expr),
}

/// Subroutine call argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Positional(Option<Expr>),
    Named(Ident, Option<Expr>),
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Number(Literal),
    UnbasedUnsized(char),
    Real(String),
    Str(Vec<u8>),
    Ident(Path),
    Index(Box<Expr>, Box<Expr>),
    Slice {
        base: Box<Expr>,
        kind: SliceKind,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Member(Box<Expr>, Ident),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Inside(Box<Expr>, Vec<Expr>),
    /// `[a:b]` in `inside` lists and `case inside` labels.
    Range(Box<Expr>, Box<Expr>),
    Concat(Vec<Expr>),
    Repl(Box<Expr>, Vec<Expr>),
    /// Assignment pattern `'{...}` or `T'{...}`; a replicated pattern `'{n{...}}` is stored with
    /// `repl = Some(n)`.
    Pattern {
        ty: Option<Box<DataType>>,
        repl: Option<Box<Expr>>,
        items: Vec<PatternItem>,
    },
    Call {
        func: Path,
        args: Vec<Arg>,
    },
    SysCall {
        name: String,
        args: Vec<Expr>,
    },
    Cast {
        target: CastTarget,
        expr: Box<Expr>,
    },
    /// A data type used as an argument (`$bits(logic [3:0])`).
    Type(Box<DataType>),
    /// Streaming, `new`, `null`, and other recognized but unsupported expressions.
    Unsupported(String),
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Expr {
        Expr { kind, span }
    }

    /// The simple name if this is an unscoped identifier.
    pub fn as_ident(&self) -> Option<&Ident> {
        match &self.kind {
            ExprKind::Ident(p) if p.scope.is_none() => Some(&p.name),
            _ => None,
        }
    }
}
