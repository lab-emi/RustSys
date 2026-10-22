// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Seeded random RTL modules with their own evaluator (SYNTH-REQ-0802, suites `random` and
//! `known-answer`). The evaluator implements the expression sizing rules of IEEE 1800-2017
//! clauses 11.6 to 11.8 on 128-bit integers, independently of the synthesis engine and checker.

use std::fmt::Write as _;

mod generate;
pub use generate::{random_comb, random_seq};

/// Deterministic generator (SplitMix64) [@steele2014fast].
#[derive(Debug, Clone)]
pub struct Rng(pub u64);

impl Rng {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    pub fn coin(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bop {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Xnor,
    Shl,
    Shr,
    Ashr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    LAnd,
    LOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Uop {
    Neg,
    Not,
    LNot,
    RAnd,
    ROr,
    RXor,
}

/// Expression tree.
#[derive(Debug, Clone)]
enum E {
    /// Variable (input or earlier-computed local) with width and signedness.
    Var(usize),
    Const(u128, u32, bool),
    Un(Uop, Box<E>),
    Bin(Bop, Box<E>, Box<E>),
    Cond(Box<E>, Box<E>, Box<E>),
    Concat(Vec<E>),
    Repl(u32, Box<E>),
    /// Part-select `[lo +: w]` of a variable.
    Sel(usize, u32, u32),
    /// `$signed(e)` or `$unsigned(e)`.
    Cast(bool, Box<E>),
    /// Call of a module function with arguments.
    Call(usize, Vec<E>),
}

/// A function with conditional returns: `if (c) return r0; case (s) k: return rk; endcase
/// return rd;` over its two parameters.
#[derive(Debug, Clone)]
struct Func {
    name: String,
    params: [Var; 2],
    ret: Var,
    cond: E,
    first: E,
    sel: E,
    items: Vec<(u128, E)>,
    last: E,
}

/// A variable of the random module.
#[derive(Debug, Clone)]
pub struct Var {
    pub name: String,
    pub width: u32,
    pub signed: bool,
}

#[derive(Debug, Clone)]
enum Stmt {
    Assign(usize, E),
    If(E, Vec<Stmt>, Vec<Stmt>),
    Case(E, Vec<(u128, Vec<Stmt>)>, Vec<Stmt>),
}

/// A random combinational module and its evaluator.
#[derive(Debug, Clone)]
pub struct RandomComb {
    pub name: String,
    pub text: String,
    /// Inputs then locals; outputs are a subset of the locals.
    pub vars: Vec<Var>,
    pub inputs: usize,
    pub outputs: Vec<usize>,
    body: Vec<Stmt>,
    funcs: Vec<Func>,
}

fn mask(w: u32) -> u128 {
    if w >= 128 {
        u128::MAX
    } else {
        (1u128 << w) - 1
    }
}

fn sext(v: u128, w: u32, signed: bool, to: u32) -> u128 {
    let v = v & mask(w);
    if signed && w > 0 && (v >> (w - 1)) & 1 == 1 {
        (v | !mask(w)) & mask(to)
    } else {
        v & mask(to)
    }
}

fn as_i128(v: u128, w: u32) -> i128 {
    let v = v & mask(w);
    if w > 0 && (v >> (w - 1)) & 1 == 1 && w < 128 {
        (v as i128) - (1i128 << w)
    } else {
        v as i128
    }
}

struct Gen<'r> {
    r: &'r mut Rng,
    vars: Vec<Var>,
    /// Functions callable from expressions: parameter widths/signs.
    funcs: Vec<[(u32, bool); 2]>,
}

impl Gen<'_> {
    fn expr(&mut self, depth: u32, avail: usize) -> E {
        if depth > 0 && !self.funcs.is_empty() && self.r.coin(10) {
            let f = self.r.below(self.funcs.len() as u64) as usize;
            let a = self.expr(depth - 1, avail);
            let b = self.expr(depth - 1, avail);
            return E::Call(f, vec![a, b]);
        }
        self.expr_plain(depth, avail)
    }

    fn expr_plain(&mut self, depth: u32, avail: usize) -> E {
        if depth == 0 || self.r.coin(25) {
            return match self.r.below(4) {
                0 => {
                    let w = 1 + self.r.below(8) as u32;
                    E::Const(u128::from(self.r.next()) & mask(w), w, self.r.coin(30))
                }
                1 => {
                    let v = self.r.below(avail as u64) as usize;
                    let vw = self.vars[v].width;
                    if vw > 1 {
                        let w = 1 + self.r.below(u64::from(vw)) as u32;
                        let lo = self.r.below(u64::from(vw - w + 1)) as u32;
                        E::Sel(v, lo, w)
                    } else {
                        E::Var(v)
                    }
                }
                _ => E::Var(self.r.below(avail as u64) as usize),
            };
        }
        let d = depth - 1;
        match self.r.below(12) {
            0..=4 => {
                let ops = [
                    Bop::Add,
                    Bop::Sub,
                    Bop::Mul,
                    Bop::Div,
                    Bop::Mod,
                    Bop::And,
                    Bop::Or,
                    Bop::Xor,
                    Bop::Xnor,
                    Bop::Shl,
                    Bop::Shr,
                    Bop::Ashr,
                    Bop::Lt,
                    Bop::Le,
                    Bop::Gt,
                    Bop::Ge,
                    Bop::Eq,
                    Bop::Ne,
                    Bop::LAnd,
                    Bop::LOr,
                ];
                let op = ops[self.r.below(ops.len() as u64) as usize];
                let a = self.expr(d, avail);
                let b = if matches!(op, Bop::Shl | Bop::Shr | Bop::Ashr) && self.r.coin(50) {
                    E::Const(u128::from(self.r.below(9)), 4, false)
                } else {
                    self.expr(d, avail)
                };
                E::Bin(op, Box::new(a), Box::new(b))
            }
            5 | 6 => {
                let ops = [
                    Uop::Neg,
                    Uop::Not,
                    Uop::LNot,
                    Uop::RAnd,
                    Uop::ROr,
                    Uop::RXor,
                ];
                E::Un(ops[self.r.below(6) as usize], Box::new(self.expr(d, avail)))
            }
            7 | 8 => E::Cond(
                Box::new(self.expr(d, avail)),
                Box::new(self.expr(d, avail)),
                Box::new(self.expr(d, avail)),
            ),
            9 => {
                let n = 2 + self.r.below(2) as usize;
                E::Concat((0..n).map(|_| self.expr(d.min(1), avail)).collect())
            }
            10 => E::Repl(
                1 + self.r.below(3) as u32,
                Box::new(self.expr(d.min(1), avail)),
            ),
            _ => E::Cast(self.r.coin(50), Box::new(self.expr(d, avail))),
        }
    }

    fn stmts(&mut self, depth: u32, targets: &[usize], avail: usize, n: usize) -> Vec<Stmt> {
        let mut out = Vec::new();
        for _ in 0..n {
            let s = if depth > 0 && self.r.coin(30) {
                if self.r.coin(60) {
                    let c = self.expr(2, avail);
                    let nt = 1 + self.r.below(2) as usize;
                    let t = self.stmts(depth - 1, targets, avail, nt);
                    let e = if self.r.coin(60) {
                        let ne = 1 + self.r.below(2) as usize;
                        self.stmts(depth - 1, targets, avail, ne)
                    } else {
                        Vec::new()
                    };
                    Stmt::If(c, t, e)
                } else {
                    let sel = self.expr(1, avail);
                    let k = 1 + self.r.below(4) as usize;
                    let mut items = Vec::new();
                    for _ in 0..k {
                        let v = u128::from(self.r.below(8));
                        if !items.iter().any(|(x, _)| *x == v) {
                            items.push((v, self.stmts(depth - 1, targets, avail, 1)));
                        }
                    }
                    let def = if self.r.coin(50) {
                        self.stmts(depth - 1, targets, avail, 1)
                    } else {
                        Vec::new()
                    };
                    Stmt::Case(sel, items, def)
                }
            } else {
                let t = targets[self.r.below(targets.len() as u64) as usize];
                Stmt::Assign(t, self.expr(3, avail))
            };
            out.push(s);
        }
        out
    }
}

fn op_text(op: Bop) -> &'static str {
    match op {
        Bop::Add => "+",
        Bop::Sub => "-",
        Bop::Mul => "*",
        Bop::Div => "/",
        Bop::Mod => "%",
        Bop::And => "&",
        Bop::Or => "|",
        Bop::Xor => "^",
        Bop::Xnor => "~^",
        Bop::Shl => "<<",
        Bop::Shr => ">>",
        Bop::Ashr => ">>>",
        Bop::Lt => "<",
        Bop::Le => "<=",
        Bop::Gt => ">",
        Bop::Ge => ">=",
        Bop::Eq => "==",
        Bop::Ne => "!=",
        Bop::LAnd => "&&",
        Bop::LOr => "||",
    }
}

impl RandomComb {
    fn show(&self, e: &E) -> String {
        match e {
            E::Var(v) => self.vars[*v].name.clone(),
            E::Const(v, w, s) => format!("{w}'{}h{:x}", if *s { "s" } else { "" }, v),
            E::Un(op, a) => {
                let o = match op {
                    Uop::Neg => "-",
                    Uop::Not => "~",
                    Uop::LNot => "!",
                    Uop::RAnd => "&",
                    Uop::ROr => "|",
                    Uop::RXor => "^",
                };
                format!("({o}({}))", self.show(a))
            }
            E::Bin(op, a, b) => format!("({} {} {})", self.show(a), op_text(*op), self.show(b)),
            E::Cond(c, a, b) => format!("({} ? {} : {})", self.show(c), self.show(a), self.show(b)),
            E::Concat(v) => format!(
                "{{{}}}",
                v.iter()
                    .map(|x| self.show(x))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            E::Repl(n, a) => format!("{{{n}{{{}}}}}", self.show(a)),
            E::Sel(v, lo, w) => format!("{}[{}:{}]", self.vars[*v].name, lo + w - 1, lo),
            E::Cast(s, a) => format!(
                "{}({})",
                if *s { "$signed" } else { "$unsigned" },
                self.show(a)
            ),
            E::Call(f, args) => format!(
                "{}({})",
                self.funcs[*f].name,
                args.iter()
                    .map(|x| self.show(x))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Text of an expression over function parameters.
    fn show_in(&self, f: &Func, e: &E) -> String {
        let local = RandomComb {
            name: String::new(),
            text: String::new(),
            vars: f.params.to_vec(),
            inputs: 2,
            outputs: vec![],
            body: vec![],
            funcs: vec![],
        };
        local.show(e)
    }

    fn func_text(&self, f: &Func) -> String {
        let mut t = String::new();
        let decl = |v: &Var| {
            format!(
                "logic{} [{}:0]",
                if v.signed { " signed" } else { "" },
                v.width - 1
            )
        };
        let _ = writeln!(
            t,
            "  function automatic {} {}(input {} {}, input {} {});",
            decl(&f.ret),
            f.name,
            decl(&f.params[0]),
            f.params[0].name,
            decl(&f.params[1]),
            f.params[1].name
        );
        let _ = writeln!(
            t,
            "    if ({}) return {};",
            self.show_in(f, &f.cond),
            self.show_in(f, &f.first)
        );
        let _ = writeln!(t, "    case ({})", self.show_in(f, &f.sel));
        for (k, e) in &f.items {
            let _ = writeln!(t, "      {k}: return {};", self.show_in(f, e));
        }
        let _ = writeln!(t, "      default: ;\n    endcase");
        let _ = writeln!(t, "    return {};\n  endfunction", self.show_in(f, &f.last));
        t
    }

    /// Evaluates a function body.
    fn call_value(&self, fi: usize, args: &[u128]) -> u128 {
        let f = &self.funcs[fi];
        let local = RandomComb {
            name: String::new(),
            text: String::new(),
            vars: f.params.to_vec(),
            inputs: 2,
            outputs: vec![],
            body: vec![],
            funcs: vec![],
        };
        let env = [args[0], args[1]];
        let ret = |e: &E| {
            let (w, s) = local.size(e);
            local.ev(e, &env, w.max(f.ret.width), s) & mask(f.ret.width)
        };
        let (cw, cs) = local.size(&f.cond);
        if local.ev(&f.cond, &env, cw, cs) != 0 {
            return ret(&f.first);
        }
        let (sw, ss) = local.size(&f.sel);
        let x = local.ev(&f.sel, &env, sw.max(32), ss);
        for (k, e) in &f.items {
            if *k == x {
                return ret(e);
            }
        }
        ret(&f.last)
    }

    fn show_stmts(&self, s: &[Stmt], ind: usize, out: &mut String) {
        let pad = " ".repeat(ind);
        for st in s {
            match st {
                Stmt::Assign(t, e) => {
                    let _ = writeln!(out, "{pad}{} = {};", self.vars[*t].name, self.show(e));
                }
                Stmt::If(c, t, e) => {
                    let _ = writeln!(out, "{pad}if ({}) begin", self.show(c));
                    self.show_stmts(t, ind + 2, out);
                    if e.is_empty() {
                        let _ = writeln!(out, "{pad}end");
                    } else {
                        let _ = writeln!(out, "{pad}end else begin");
                        self.show_stmts(e, ind + 2, out);
                        let _ = writeln!(out, "{pad}end");
                    }
                }
                Stmt::Case(sel, items, def) => {
                    let _ = writeln!(out, "{pad}case ({})", self.show(sel));
                    for (v, b) in items {
                        let _ = writeln!(out, "{pad}  {v}: begin");
                        self.show_stmts(b, ind + 4, out);
                        let _ = writeln!(out, "{pad}  end");
                    }
                    let _ = writeln!(out, "{pad}  default: begin");
                    self.show_stmts(def, ind + 4, out);
                    let _ = writeln!(out, "{pad}  end");
                    let _ = writeln!(out, "{pad}endcase");
                }
            }
        }
    }

    // ------------------------------------------------------------- evaluation

    fn size(&self, e: &E) -> (u32, bool) {
        match e {
            E::Var(v) => (self.vars[*v].width, self.vars[*v].signed),
            E::Const(_, w, s) => (*w, *s),
            E::Un(op, a) => match op {
                Uop::Neg | Uop::Not => self.size(a),
                _ => (1, false),
            },
            E::Bin(op, a, b) => {
                let (wa, sa) = self.size(a);
                let (wb, sb) = self.size(b);
                match op {
                    Bop::Shl | Bop::Shr | Bop::Ashr => (wa, sa),
                    Bop::Lt
                    | Bop::Le
                    | Bop::Gt
                    | Bop::Ge
                    | Bop::Eq
                    | Bop::Ne
                    | Bop::LAnd
                    | Bop::LOr => (1, false),
                    _ => (wa.max(wb), sa && sb),
                }
            }
            E::Cond(_, a, b) => {
                let (wa, sa) = self.size(a);
                let (wb, sb) = self.size(b);
                (wa.max(wb), sa && sb)
            }
            E::Concat(v) => (v.iter().map(|x| self.size(x).0).sum(), false),
            E::Repl(n, a) => (n * self.size(a).0, false),
            E::Sel(_, _, w) => (*w, false),
            E::Cast(s, a) => (self.size(a).0, *s),
            E::Call(f, _) => (self.funcs[*f].ret.width, self.funcs[*f].ret.signed),
        }
    }

    /// Value of `e` in a context of width `cw` and signedness `cs`.
    fn ev(&self, e: &E, env: &[u128], cw: u32, cs: bool) -> u128 {
        let m = mask(cw);
        match e {
            E::Var(v) => sext(env[*v], self.vars[*v].width, cs && self.vars[*v].signed, cw),
            E::Const(v, w, s) => sext(*v, *w, cs && *s, cw),
            E::Sel(v, lo, w) => (env[*v] >> lo) & mask(*w) & m,
            E::Cast(s, a) => {
                let (wa, sa) = self.size(a);
                let x = self.ev(a, env, wa, sa);
                sext(x, wa, cs && *s, cw)
            }
            E::Call(fi, args) => {
                let f = &self.funcs[*fi];
                // Arguments are assigned to the parameters.
                let vals: Vec<u128> = args
                    .iter()
                    .zip(&f.params)
                    .map(|(a, p)| {
                        let (w, s) = self.size(a);
                        self.ev(a, env, w.max(p.width), s) & mask(p.width)
                    })
                    .collect();
                let r = self.call_value(*fi, &vals);
                sext(r, f.ret.width, cs && f.ret.signed, cw)
            }
            E::Un(op, a) => match op {
                Uop::Neg => self.ev(a, env, cw, cs).wrapping_neg() & m,
                Uop::Not => !self.ev(a, env, cw, cs) & m,
                _ => {
                    let (wa, sa) = self.size(a);
                    let x = self.ev(a, env, wa, sa);
                    let r = match op {
                        Uop::LNot => x == 0,
                        Uop::RAnd => x == mask(wa),
                        Uop::ROr => x != 0,
                        _ => x.count_ones() % 2 == 1,
                    };
                    u128::from(r)
                }
            },
            E::Bin(op, a, b) => {
                let (wa, sa) = self.size(a);
                let (wb, sb) = self.size(b);
                match op {
                    Bop::Shl | Bop::Shr | Bop::Ashr => {
                        let x = self.ev(a, env, cw, cs);
                        let n = self.ev(b, env, wb, sb) & mask(wb);
                        let n = if n > 200 { 200 } else { n as u32 };
                        match op {
                            Bop::Shl => {
                                if n >= cw {
                                    0
                                } else {
                                    (x << n) & m
                                }
                            }
                            Bop::Shr => {
                                if n >= cw {
                                    0
                                } else {
                                    x >> n
                                }
                            }
                            _ => {
                                if cs {
                                    let v = as_i128(x, cw);
                                    (if n >= cw {
                                        if v < 0 { -1 } else { 0 }
                                    } else {
                                        v >> n
                                    }) as u128
                                        & m
                                } else if n >= cw {
                                    0
                                } else {
                                    x >> n
                                }
                            }
                        }
                    }
                    Bop::Lt | Bop::Le | Bop::Gt | Bop::Ge | Bop::Eq | Bop::Ne => {
                        let w = wa.max(wb);
                        let s = sa && sb;
                        let x = self.ev(a, env, w, s);
                        let y = self.ev(b, env, w, s);
                        let (xi, yi) = if s {
                            (as_i128(x, w), as_i128(y, w))
                        } else {
                            (x as i128, y as i128)
                        };
                        let r = match op {
                            Bop::Lt => xi < yi,
                            Bop::Le => xi <= yi,
                            Bop::Gt => xi > yi,
                            Bop::Ge => xi >= yi,
                            Bop::Eq => x == y,
                            _ => x != y,
                        };
                        u128::from(r)
                    }
                    Bop::LAnd | Bop::LOr => {
                        let x = self.ev(a, env, wa, sa) != 0;
                        let y = self.ev(b, env, wb, sb) != 0;
                        u128::from(if *op == Bop::LAnd { x && y } else { x || y })
                    }
                    _ => {
                        let x = self.ev(a, env, cw, cs);
                        let y = self.ev(b, env, cw, cs);
                        let r = match op {
                            Bop::Add => x.wrapping_add(y),
                            Bop::Sub => x.wrapping_sub(y),
                            Bop::Mul => x.wrapping_mul(y),
                            Bop::Div | Bop::Mod => {
                                if y == 0 {
                                    0
                                } else if cs {
                                    let (xi, yi) = (as_i128(x, cw), as_i128(y, cw));
                                    (if *op == Bop::Div {
                                        xi.wrapping_div(yi)
                                    } else {
                                        xi.wrapping_rem(yi)
                                    }) as u128
                                } else if *op == Bop::Div {
                                    x / y
                                } else {
                                    x % y
                                }
                            }
                            Bop::And => x & y,
                            Bop::Or => x | y,
                            Bop::Xor => x ^ y,
                            _ => !(x ^ y),
                        };
                        r & m
                    }
                }
            }
            E::Cond(c, a, b) => {
                let (wc, sc) = self.size(c);
                if self.ev(c, env, wc, sc) != 0 {
                    self.ev(a, env, cw, cs)
                } else {
                    self.ev(b, env, cw, cs)
                }
            }
            E::Concat(v) => {
                let mut r = 0u128;
                for x in v {
                    let (w, s) = self.size(x);
                    r = (r << w) | (self.ev(x, env, w, s) & mask(w));
                }
                r & m
            }
            E::Repl(n, a) => {
                let (w, s) = self.size(a);
                let x = self.ev(a, env, w, s) & mask(w);
                let mut r = 0u128;
                for _ in 0..*n {
                    r = (r << w) | x;
                }
                r & m
            }
        }
    }

    /// Assignment: the right side is evaluated at the larger of its size and the target's width.
    fn assign_val(&self, t: usize, e: &E, env: &[u128]) -> u128 {
        let (w, s) = self.size(e);
        let tw = self.vars[t].width;
        self.ev(e, env, w.max(tw), s) & mask(tw)
    }

    fn exec(&self, s: &[Stmt], env: &mut [u128]) {
        for st in s {
            match st {
                Stmt::Assign(t, e) => env[*t] = self.assign_val(*t, e, env),
                Stmt::If(c, a, b) => {
                    let (w, sg) = self.size(c);
                    if self.ev(c, env, w, sg) != 0 {
                        self.exec(a, env);
                    } else {
                        self.exec(b, env);
                    }
                }
                Stmt::Case(sel, items, def) => {
                    // Items are plain decimals (signed, 32 bits): the comparison is signed only if
                    // the case expression is signed, at the width of the widest expression.
                    let (w, sg) = self.size(sel);
                    let cw = w.max(32);
                    let x = self.ev(sel, env, cw, sg);
                    match items.iter().find(|(v, _)| *v == x) {
                        Some((_, b)) => self.exec(b, env),
                        None => self.exec(def, env),
                    }
                }
            }
        }
    }

    /// Output values for input values (in input order, masked to their widths).
    pub fn eval(&self, inputs: &[u128]) -> Vec<u128> {
        let mut env = vec![0u128; self.vars.len()];
        for (i, v) in inputs.iter().enumerate() {
            env[i] = v & mask(self.vars[i].width);
        }
        self.exec(&self.body, &mut env);
        self.outputs.iter().map(|o| env[*o]).collect()
    }

    /// Total number of input bits.
    pub fn input_bits(&self) -> u32 {
        self.vars[..self.inputs].iter().map(|v| v.width).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_well_formed() {
        let a = random_comb(42, 16);
        let b = random_comb(42, 16);
        assert_eq!(a.text, b.text);
        assert!(a.input_bits() <= 16 + 6);
        assert!(a.text.contains("always_comb"));
        assert_eq!(a.eval(&[1, 2, 3, 4]).len(), a.outputs.len());
        // Sizing rules: an 8-bit context keeps the carry of 4-bit operands.
        let m = RandomComb {
            name: "t".into(),
            text: String::new(),
            vars: vec![
                Var {
                    name: "a".into(),
                    width: 4,
                    signed: false,
                },
                Var {
                    name: "b".into(),
                    width: 4,
                    signed: false,
                },
                Var {
                    name: "y".into(),
                    width: 8,
                    signed: false,
                },
            ],
            inputs: 2,
            outputs: vec![2],
            body: vec![Stmt::Assign(
                2,
                E::Bin(Bop::Add, Box::new(E::Var(0)), Box::new(E::Var(1))),
            )],
            funcs: vec![],
        };
        assert_eq!(m.eval(&[15, 15]), vec![30]);
        assert!(random_seq(7).contains("module seq_7"));
    }
}
