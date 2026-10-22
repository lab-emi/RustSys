// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Bounded deterministic Tcl `[expr]` and `if`-condition parser and lazy evaluator
//! (SDC-REQ-0010, SDC-REQ-0011).

use super::eval::{EvalError, Interpreter, TclVal};

#[derive(Debug, Clone)]
enum Expr {
    Num(f64, bool),
    Str(String),
    Quoted(Vec<QuotedPart>),
    Var(String),
    Cmd(String),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    LogicalAnd(Box<Expr>, Box<Expr>),
    LogicalOr(Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone)]
enum QuotedPart {
    Lit(String),
    Var(String),
    Cmd(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnaryOp {
    Plus,
    Minus,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    StrEq,
    StrNe,
}

#[derive(Debug, Clone)]
enum EvalVal {
    Num(f64, bool),
    Str(String),
}

impl EvalVal {
    fn from_tcl(v: TclVal, loc: (&str, u32, u32)) -> Result<Self, EvalError> {
        match v {
            TclVal::Obj(_) | TclVal::SearchIndex { .. } => Err(EvalError::new(
                "D0006",
                "expression or comparison over an object collection needs the netlist",
                loc,
            )),
            TclVal::Str(s) => Ok(Self::from_str_val(s)),
            TclVal::List(items) => {
                if items.iter().any(|x| matches!(x, TclVal::Obj(_))) {
                    return Err(EvalError::new(
                        "D0006",
                        "expression or comparison over an object collection needs the netlist",
                        loc,
                    ));
                }
                let joined = items
                    .into_iter()
                    .filter_map(|x| x.as_scalar_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                Ok(Self::from_str_val(joined))
            }
        }
    }

    fn from_str_val(s: String) -> Self {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            if let Ok(i) = trimmed.parse::<i64>() {
                return EvalVal::Num(i as f64, true);
            }
            if let Ok(f) = trimmed.parse::<f64>()
                && f.is_finite()
            {
                return EvalVal::Num(f, false);
            }
        }
        EvalVal::Str(s)
    }

    fn to_num(&self, loc: (&str, u32, u32)) -> Result<(f64, bool), EvalError> {
        match self {
            EvalVal::Num(n, is_int) => Ok((*n, *is_int)),
            EvalVal::Str(s) => {
                let t = s.trim();
                if let Ok(i) = t.parse::<i64>() {
                    return Ok((i as f64, true));
                }
                if let Ok(f) = t.parse::<f64>()
                    && f.is_finite()
                {
                    return Ok((f, false));
                }
                Err(EvalError::new(
                    "D0005",
                    format!("expected numeric value in expression, found '{s}'"),
                    loc,
                ))
            }
        }
    }

    fn to_bool(&self, loc: (&str, u32, u32)) -> Result<bool, EvalError> {
        match self {
            EvalVal::Num(n, _) => Ok(*n != 0.0),
            EvalVal::Str(s) => {
                let t = s.trim();
                if let Ok(n) = t.parse::<f64>()
                    && n.is_finite()
                {
                    return Ok(n != 0.0);
                }
                match t.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" => Ok(true),
                    "false" | "no" | "off" => Ok(false),
                    _ => Err(EvalError::new(
                        "D0005",
                        format!("expected boolean or number in expression, found '{s}'"),
                        loc,
                    )),
                }
            }
        }
    }

    fn into_string(self) -> String {
        match self {
            EvalVal::Num(n, is_int) => format_num(n, is_int),
            EvalVal::Str(s) => s,
        }
    }
}

fn format_num(n: f64, is_int: bool) -> String {
    let n = if n == 0.0 { 0.0 } else { n };
    if is_int && n.fract() == 0.0 && n.abs() < (i64::MAX as f64) {
        format!("{}", n as i64)
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        // Keep clean decimal representation when exact integer value is produced by float op
        let s = format!("{n}");
        s
    } else {
        let rounded = (n * 1e12).round() / 1e12;
        if (rounded - n).abs() < 1e-15 {
            format!("{rounded}")
        } else {
            format!("{n}")
        }
    }
}

/// Evaluates an `[expr ...]` or `if` condition string lazily (with short-circuiting `&&`, `||`,
/// and `?:`) using `interp` (SDC-REQ-0010, SDC-REQ-0011).
pub fn eval_expr_str(
    interp: &mut Interpreter,
    src: &str,
    loc: (&str, u32, u32),
) -> Result<String, EvalError> {
    interp.step(loc)?;
    let mut parser = ExprParser::new(src, loc);
    let ast = parser.parse_top()?;
    let val = eval_node(interp, &ast, loc)?;
    Ok(val.into_string())
}

/// Evaluates an `if` / `elseif` condition string to a boolean.
pub fn eval_condition(
    interp: &mut Interpreter,
    src: &str,
    loc: (&str, u32, u32),
) -> Result<bool, EvalError> {
    interp.step(loc)?;
    let mut parser = ExprParser::new(src, loc);
    let ast = parser.parse_top()?;
    let val = eval_node(interp, &ast, loc)?;
    val.to_bool(loc)
}

fn eval_node(
    interp: &mut Interpreter,
    expr: &Expr,
    loc: (&str, u32, u32),
) -> Result<EvalVal, EvalError> {
    interp.step(loc)?;
    match expr {
        Expr::Num(n, is_int) => Ok(EvalVal::Num(*n, *is_int)),
        Expr::Str(s) => Ok(EvalVal::from_str_val(s.clone())),
        Expr::Quoted(parts) => {
            let mut out = String::new();
            for part in parts {
                match part {
                    QuotedPart::Lit(s) => out.push_str(s),
                    QuotedPart::Var(name) => {
                        let v = interp.lookup_var(name, loc)?;
                        let ev = EvalVal::from_tcl(v, loc)?;
                        out.push_str(&ev.into_string());
                    }
                    QuotedPart::Cmd(script) => {
                        let v = interp.eval_nested_script(script, loc)?;
                        let ev = EvalVal::from_tcl(v, loc)?;
                        out.push_str(&ev.into_string());
                    }
                }
            }
            Ok(EvalVal::Str(out))
        }
        Expr::Var(name) => {
            let v = interp.lookup_var(name, loc)?;
            EvalVal::from_tcl(v, loc)
        }
        Expr::Cmd(script) => {
            let v = interp.eval_nested_script(script, loc)?;
            EvalVal::from_tcl(v, loc)
        }
        Expr::Unary(op, inner) => {
            let v = eval_node(interp, inner, loc)?;
            match op {
                UnaryOp::Plus => {
                    let (n, is_int) = v.to_num(loc)?;
                    Ok(EvalVal::Num(n, is_int))
                }
                UnaryOp::Minus => {
                    let (n, is_int) = v.to_num(loc)?;
                    Ok(EvalVal::Num(-n, is_int))
                }
                UnaryOp::Not => {
                    let b = v.to_bool(loc)?;
                    Ok(EvalVal::Num(if b { 0.0 } else { 1.0 }, true))
                }
            }
        }
        Expr::LogicalAnd(lhs, rhs) => {
            let l = eval_node(interp, lhs, loc)?.to_bool(loc)?;
            if !l {
                return Ok(EvalVal::Num(0.0, true));
            }
            let r = eval_node(interp, rhs, loc)?.to_bool(loc)?;
            Ok(EvalVal::Num(if r { 1.0 } else { 0.0 }, true))
        }
        Expr::LogicalOr(lhs, rhs) => {
            let l = eval_node(interp, lhs, loc)?.to_bool(loc)?;
            if l {
                return Ok(EvalVal::Num(1.0, true));
            }
            let r = eval_node(interp, rhs, loc)?.to_bool(loc)?;
            Ok(EvalVal::Num(if r { 1.0 } else { 0.0 }, true))
        }
        Expr::Ternary(cond, then_e, else_e) => {
            if eval_node(interp, cond, loc)?.to_bool(loc)? {
                eval_node(interp, then_e, loc)
            } else {
                eval_node(interp, else_e, loc)
            }
        }
        Expr::Binary(op, lhs, rhs) => {
            let lv = eval_node(interp, lhs, loc)?;
            let rv = eval_node(interp, rhs, loc)?;
            match op {
                BinOp::Add => {
                    let (ln, li) = lv.to_num(loc)?;
                    let (rn, ri) = rv.to_num(loc)?;
                    Ok(EvalVal::Num(ln + rn, li && ri))
                }
                BinOp::Sub => {
                    let (ln, li) = lv.to_num(loc)?;
                    let (rn, ri) = rv.to_num(loc)?;
                    Ok(EvalVal::Num(ln - rn, li && ri))
                }
                BinOp::Mul => {
                    let (ln, li) = lv.to_num(loc)?;
                    let (rn, ri) = rv.to_num(loc)?;
                    Ok(EvalVal::Num(ln * rn, li && ri))
                }
                BinOp::Div => {
                    let (ln, _) = lv.to_num(loc)?;
                    let (rn, _) = rv.to_num(loc)?;
                    if rn == 0.0 {
                        return Err(EvalError::new("D0005", "division by zero in expr", loc));
                    }
                    Ok(EvalVal::Num(ln / rn, false))
                }
                BinOp::Rem => {
                    let (ln, _) = lv.to_num(loc)?;
                    let (rn, _) = rv.to_num(loc)?;
                    let li = ln as i64;
                    let ri = rn as i64;
                    if ri == 0 {
                        return Err(EvalError::new("D0005", "modulo by zero in expr", loc));
                    }
                    Ok(EvalVal::Num((li % ri) as f64, true))
                }
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let cmp = if let (Ok((ln, _)), Ok((rn, _))) = (lv.to_num(loc), rv.to_num(loc)) {
                        match op {
                            BinOp::Lt => ln < rn,
                            BinOp::Le => ln <= rn,
                            BinOp::Gt => ln > rn,
                            BinOp::Ge => ln >= rn,
                            _ => unreachable!(),
                        }
                    } else {
                        let ls = lv.into_string();
                        let rs = rv.into_string();
                        match op {
                            BinOp::Lt => ls < rs,
                            BinOp::Le => ls <= rs,
                            BinOp::Gt => ls > rs,
                            BinOp::Ge => ls >= rs,
                            _ => unreachable!(),
                        }
                    };
                    Ok(EvalVal::Num(if cmp { 1.0 } else { 0.0 }, true))
                }
                BinOp::Eq | BinOp::Ne => {
                    let eq = if let (Ok((ln, _)), Ok((rn, _))) = (lv.to_num(loc), rv.to_num(loc)) {
                        (ln - rn).abs() == 0.0
                    } else {
                        lv.into_string() == rv.into_string()
                    };
                    let res = if *op == BinOp::Eq { eq } else { !eq };
                    Ok(EvalVal::Num(if res { 1.0 } else { 0.0 }, true))
                }
                BinOp::StrEq | BinOp::StrNe => {
                    let eq = lv.into_string() == rv.into_string();
                    let res = if *op == BinOp::StrEq { eq } else { !eq };
                    Ok(EvalVal::Num(if res { 1.0 } else { 0.0 }, true))
                }
            }
        }
        Expr::Call(func, args) => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval_node(interp, a, loc)?);
            }
            match func.as_str() {
                "abs" => {
                    if vals.len() != 1 {
                        return Err(EvalError::new(
                            "D0004",
                            format!("expr function 'abs' expects 1 argument, got {}", vals.len()),
                            loc,
                        ));
                    }
                    let (n, is_int) = vals[0].to_num(loc)?;
                    Ok(EvalVal::Num(n.abs(), is_int))
                }
                "int" => {
                    if vals.len() != 1 {
                        return Err(EvalError::new(
                            "D0004",
                            format!("expr function 'int' expects 1 argument, got {}", vals.len()),
                            loc,
                        ));
                    }
                    let (n, _) = vals[0].to_num(loc)?;
                    Ok(EvalVal::Num((n as i64) as f64, true))
                }
                "double" => {
                    if vals.len() != 1 {
                        return Err(EvalError::new(
                            "D0004",
                            format!(
                                "expr function 'double' expects 1 argument, got {}",
                                vals.len()
                            ),
                            loc,
                        ));
                    }
                    let (n, _) = vals[0].to_num(loc)?;
                    Ok(EvalVal::Num(n, false))
                }
                "round" => {
                    if vals.len() != 1 {
                        return Err(EvalError::new(
                            "D0004",
                            format!(
                                "expr function 'round' expects 1 argument, got {}",
                                vals.len()
                            ),
                            loc,
                        ));
                    }
                    let (n, _) = vals[0].to_num(loc)?;
                    Ok(EvalVal::Num((n.round() as i64) as f64, true))
                }
                "min" => {
                    if vals.len() < 2 {
                        return Err(EvalError::new(
                            "D0004",
                            "expr function 'min' requires at least 2 arguments",
                            loc,
                        ));
                    }
                    let (mut best, mut all_int) = vals[0].to_num(loc)?;
                    for v in &vals[1..] {
                        let (n, is_int) = v.to_num(loc)?;
                        if n < best {
                            best = n;
                        }
                        all_int &= is_int;
                    }
                    Ok(EvalVal::Num(best, all_int))
                }
                "max" => {
                    if vals.len() < 2 {
                        return Err(EvalError::new(
                            "D0004",
                            "expr function 'max' requires at least 2 arguments",
                            loc,
                        ));
                    }
                    let (mut best, mut all_int) = vals[0].to_num(loc)?;
                    for v in &vals[1..] {
                        let (n, is_int) = v.to_num(loc)?;
                        if n > best {
                            best = n;
                        }
                        all_int &= is_int;
                    }
                    Ok(EvalVal::Num(best, all_int))
                }
                other => Err(EvalError::new(
                    "D0006",
                    format!("expr function '{other}' is outside the supported Tcl subset"),
                    loc,
                )),
            }
        }
    }
}

struct ExprParser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    loc: (&'a str, u32, u32),
}

impl<'a> ExprParser<'a> {
    fn new(src: &'a str, loc: (&'a str, u32, u32)) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            loc,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else if b == b'\\'
                && self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos + 1] == b'\n'
            {
                self.pos += 2;
            } else {
                break;
            }
        }
    }

    fn parse_top(&mut self) -> Result<Expr, EvalError> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return Err(EvalError::new("D0002", "empty expression", self.loc));
        }
        let e = self.parse_ternary()?;
        self.skip_ws();
        if self.pos < self.bytes.len() {
            return Err(EvalError::new(
                "D0002",
                format!(
                    "unexpected trailing text '{}' in expression",
                    &self.src[self.pos..]
                ),
                self.loc,
            ));
        }
        Ok(e)
    }

    fn parse_ternary(&mut self) -> Result<Expr, EvalError> {
        let cond = self.parse_or()?;
        self.skip_ws();
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'?' {
            self.pos += 1;
            let then_e = self.parse_ternary()?;
            self.skip_ws();
            if self.pos >= self.bytes.len() || self.bytes[self.pos] != b':' {
                return Err(EvalError::new(
                    "D0002",
                    "expected ':' in ternary expression",
                    self.loc,
                ));
            }
            self.pos += 1;
            let else_e = self.parse_ternary()?;
            return Ok(Expr::Ternary(
                Box::new(cond),
                Box::new(then_e),
                Box::new(else_e),
            ));
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> Result<Expr, EvalError> {
        let mut lhs = self.parse_and()?;
        loop {
            self.skip_ws();
            if self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos] == b'|'
                && self.bytes[self.pos + 1] == b'|'
            {
                self.pos += 2;
                let rhs = self.parse_and()?;
                lhs = Expr::LogicalOr(Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, EvalError> {
        let mut lhs = self.parse_equality()?;
        loop {
            self.skip_ws();
            if self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos] == b'&'
                && self.bytes[self.pos + 1] == b'&'
            {
                self.pos += 2;
                let rhs = self.parse_equality()?;
                lhs = Expr::LogicalAnd(Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Expr, EvalError> {
        let mut lhs = self.parse_relational()?;
        loop {
            self.skip_ws();
            let op = if self.starts_with("==") {
                self.pos += 2;
                Some(BinOp::Eq)
            } else if self.starts_with("!=") {
                self.pos += 2;
                Some(BinOp::Ne)
            } else if self.starts_kw("eq") {
                self.pos += 2;
                Some(BinOp::StrEq)
            } else if self.starts_kw("ne") {
                self.pos += 2;
                Some(BinOp::StrNe)
            } else {
                None
            };
            if let Some(op) = op {
                let rhs = self.parse_relational()?;
                lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn parse_relational(&mut self) -> Result<Expr, EvalError> {
        let mut lhs = self.parse_additive()?;
        loop {
            self.skip_ws();
            let op = if self.starts_with("<=") {
                self.pos += 2;
                Some(BinOp::Le)
            } else if self.starts_with(">=") {
                self.pos += 2;
                Some(BinOp::Ge)
            } else if self.starts_with("<") {
                self.pos += 1;
                Some(BinOp::Lt)
            } else if self.starts_with(">") {
                self.pos += 1;
                Some(BinOp::Gt)
            } else {
                None
            };
            if let Some(op) = op {
                let rhs = self.parse_additive()?;
                lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, EvalError> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'+' {
                self.pos += 1;
                let rhs = self.parse_multiplicative()?;
                lhs = Expr::Binary(BinOp::Add, Box::new(lhs), Box::new(rhs));
            } else if self.pos < self.bytes.len() && self.bytes[self.pos] == b'-' {
                self.pos += 1;
                let rhs = self.parse_multiplicative()?;
                lhs = Expr::Binary(BinOp::Sub, Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, EvalError> {
        let mut lhs = self.parse_unary()?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'*' {
                self.pos += 1;
                let rhs = self.parse_unary()?;
                lhs = Expr::Binary(BinOp::Mul, Box::new(lhs), Box::new(rhs));
            } else if self.pos < self.bytes.len() && self.bytes[self.pos] == b'/' {
                self.pos += 1;
                let rhs = self.parse_unary()?;
                lhs = Expr::Binary(BinOp::Div, Box::new(lhs), Box::new(rhs));
            } else if self.pos < self.bytes.len() && self.bytes[self.pos] == b'%' {
                self.pos += 1;
                let rhs = self.parse_unary()?;
                lhs = Expr::Binary(BinOp::Rem, Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, EvalError> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return Err(EvalError::new(
                "D0002",
                "unexpected end of expression",
                self.loc,
            ));
        }
        match self.bytes[self.pos] {
            b'+' => {
                self.pos += 1;
                Ok(Expr::Unary(UnaryOp::Plus, Box::new(self.parse_unary()?)))
            }
            b'-' => {
                self.pos += 1;
                Ok(Expr::Unary(UnaryOp::Minus, Box::new(self.parse_unary()?)))
            }
            b'!' => {
                self.pos += 1;
                Ok(Expr::Unary(UnaryOp::Not, Box::new(self.parse_unary()?)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, EvalError> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return Err(EvalError::new(
                "D0002",
                "unexpected end of expression",
                self.loc,
            ));
        }
        match self.bytes[self.pos] {
            b'(' => {
                self.pos += 1;
                let inner = self.parse_ternary()?;
                self.skip_ws();
                if self.pos >= self.bytes.len() || self.bytes[self.pos] != b')' {
                    return Err(EvalError::new(
                        "D0001",
                        "unclosed '(' in expression",
                        self.loc,
                    ));
                }
                self.pos += 1;
                Ok(inner)
            }
            b'$' => {
                let name = self.read_var_name()?;
                Ok(Expr::Var(name))
            }
            b'[' => {
                let script = self.read_bracket_script()?;
                Ok(Expr::Cmd(script))
            }
            b'{' => {
                let s = self.read_braced()?;
                Ok(Expr::Str(s))
            }
            b'"' => {
                let parts = self.read_quoted()?;
                Ok(Expr::Quoted(parts))
            }
            b if b.is_ascii_digit()
                || (b == b'.'
                    && self.pos + 1 < self.bytes.len()
                    && self.bytes[self.pos + 1].is_ascii_digit()) =>
            {
                self.read_number()
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = self.pos;
                while self.pos < self.bytes.len()
                    && (self.bytes[self.pos].is_ascii_alphanumeric()
                        || self.bytes[self.pos] == b'_')
                {
                    self.pos += 1;
                }
                let ident = &self.src[start..self.pos];
                self.skip_ws();
                if self.pos < self.bytes.len() && self.bytes[self.pos] == b'(' {
                    self.pos += 1;
                    let mut args = Vec::new();
                    self.skip_ws();
                    if self.pos < self.bytes.len() && self.bytes[self.pos] != b')' {
                        loop {
                            args.push(self.parse_ternary()?);
                            self.skip_ws();
                            if self.pos < self.bytes.len() && self.bytes[self.pos] == b',' {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    if self.pos >= self.bytes.len() || self.bytes[self.pos] != b')' {
                        return Err(EvalError::new(
                            "D0001",
                            format!("unclosed '(' in call to '{ident}'"),
                            self.loc,
                        ));
                    }
                    self.pos += 1;
                    Ok(Expr::Call(ident.to_string(), args))
                } else {
                    match ident.to_ascii_lowercase().as_str() {
                        "true" | "yes" | "on" => Ok(Expr::Num(1.0, true)),
                        "false" | "no" | "off" => Ok(Expr::Num(0.0, true)),
                        _ => Ok(Expr::Str(ident.to_string())),
                    }
                }
            }
            other => Err(EvalError::new(
                "D0002",
                format!("unexpected character '{}' in expression", other as char),
                self.loc,
            )),
        }
    }

    fn starts_with(&self, pat: &str) -> bool {
        self.src[self.pos..].starts_with(pat)
    }

    fn starts_kw(&self, kw: &str) -> bool {
        if !self.src[self.pos..].starts_with(kw) {
            return false;
        }
        let end = self.pos + kw.len();
        end >= self.bytes.len()
            || (!self.bytes[end].is_ascii_alphanumeric() && self.bytes[end] != b'_')
    }

    fn read_number(&mut self) -> Result<Expr, EvalError> {
        let start = self.pos;
        if self.starts_with("0x") || self.starts_with("0X") {
            self.pos += 2;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_hexdigit() {
                self.pos += 1;
            }
            let hex_s = &self.src[start + 2..self.pos];
            let v = i64::from_str_radix(hex_s, 16).map_err(|_| {
                EvalError::new(
                    "D0005",
                    format!("invalid hex literal '{}'", &self.src[start..self.pos]),
                    self.loc,
                )
            })?;
            return Ok(Expr::Num(v as f64, true));
        }
        let mut has_dot_or_exp = false;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'.' {
            has_dot_or_exp = true;
            self.pos += 1;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'e' | b'E') {
            has_dot_or_exp = true;
            self.pos += 1;
            if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'+' | b'-') {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
            if self.pos == exp_start {
                return Err(EvalError::new(
                    "D0005",
                    format!("invalid number literal '{}'", &self.src[start..self.pos]),
                    self.loc,
                ));
            }
        }
        let num_str = &self.src[start..self.pos];
        if !has_dot_or_exp && let Ok(i) = num_str.parse::<i64>() {
            return Ok(Expr::Num(i as f64, true));
        }
        let f = num_str.parse::<f64>().map_err(|_| {
            EvalError::new(
                "D0005",
                format!("invalid number literal '{num_str}'"),
                self.loc,
            )
        })?;
        Ok(Expr::Num(f, false))
    }

    fn read_var_name(&mut self) -> Result<String, EvalError> {
        debug_assert_eq!(self.bytes[self.pos], b'$');
        self.pos += 1;
        if self.pos >= self.bytes.len() {
            return Err(EvalError::new("D0002", "lone '$' in expression", self.loc));
        }
        if self.bytes[self.pos] == b'{' {
            self.pos += 1;
            let start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos] != b'}' {
                self.pos += 1;
            }
            if self.pos >= self.bytes.len() {
                return Err(EvalError::new(
                    "D0001",
                    "unclosed '${' in expression",
                    self.loc,
                ));
            }
            let name = self.src[start..self.pos].to_string();
            self.pos += 1;
            return Ok(name);
        }
        let start = self.pos;
        while self.pos < self.bytes.len()
            && (self.bytes[self.pos].is_ascii_alphanumeric()
                || matches!(self.bytes[self.pos], b'_' | b':'))
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(EvalError::new(
                "D0002",
                "expected variable name after '$'",
                self.loc,
            ));
        }
        let mut name = self.src[start..self.pos].to_string();
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'(' {
            self.pos += 1;
            let key_start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos] != b')' {
                self.pos += 1;
            }
            if self.pos >= self.bytes.len() {
                return Err(EvalError::new(
                    "D0001",
                    "unclosed '(' in array variable reference",
                    self.loc,
                ));
            }
            name.push('(');
            name.push_str(&self.src[key_start..self.pos]);
            name.push(')');
            self.pos += 1;
        }
        Ok(name)
    }

    fn read_bracket_script(&mut self) -> Result<String, EvalError> {
        debug_assert_eq!(self.bytes[self.pos], b'[');
        self.pos += 1;
        let start = self.pos;
        let mut depth = 1usize;
        let mut in_quotes = false;
        let mut brace_depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'\\' && self.pos + 1 < self.bytes.len() {
                self.pos += 2;
                continue;
            }
            if brace_depth > 0 {
                if b == b'{' {
                    brace_depth += 1;
                } else if b == b'}' {
                    brace_depth -= 1;
                }
                self.pos += 1;
                continue;
            }
            if in_quotes {
                if b == b'"' {
                    in_quotes = false;
                }
                self.pos += 1;
                continue;
            }
            match b {
                b'"' => in_quotes = true,
                b'{' => brace_depth = 1,
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        let script = self.src[start..self.pos].to_string();
                        self.pos += 1;
                        return Ok(script);
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        Err(EvalError::new(
            "D0001",
            "unclosed '[' in expression",
            self.loc,
        ))
    }

    fn read_braced(&mut self) -> Result<String, EvalError> {
        debug_assert_eq!(self.bytes[self.pos], b'{');
        self.pos += 1;
        let start = self.pos;
        let mut depth = 1usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'\\' && self.pos + 1 < self.bytes.len() {
                self.pos += 2;
                continue;
            }
            if b == b'{' {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    let s = self.src[start..self.pos].to_string();
                    self.pos += 1;
                    return Ok(s);
                }
            }
            self.pos += 1;
        }
        Err(EvalError::new(
            "D0001",
            "unclosed '{' in expression",
            self.loc,
        ))
    }

    fn read_quoted(&mut self) -> Result<Vec<QuotedPart>, EvalError> {
        debug_assert_eq!(self.bytes[self.pos], b'"');
        self.pos += 1;
        let mut parts = Vec::new();
        let mut lit = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            match b {
                b'"' => {
                    self.pos += 1;
                    if !lit.is_empty() {
                        parts.push(QuotedPart::Lit(lit));
                    }
                    return Ok(parts);
                }
                b'\\' => {
                    self.pos += 1;
                    if self.pos < self.bytes.len() {
                        let esc = self.bytes[self.pos];
                        let ch = match esc {
                            b'n' => '\n',
                            b'r' => '\r',
                            b't' => '\t',
                            _ => esc as char,
                        };
                        lit.push(ch);
                        self.pos += 1;
                    }
                }
                b'$' => {
                    if !lit.is_empty() {
                        parts.push(QuotedPart::Lit(std::mem::take(&mut lit)));
                    }
                    let var = self.read_var_name()?;
                    parts.push(QuotedPart::Var(var));
                }
                b'[' => {
                    if !lit.is_empty() {
                        parts.push(QuotedPart::Lit(std::mem::take(&mut lit)));
                    }
                    let cmd = self.read_bracket_script()?;
                    parts.push(QuotedPart::Cmd(cmd));
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    lit.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Err(EvalError::new(
            "D0001",
            "unclosed '\"' in expression",
            self.loc,
        ))
    }
}
