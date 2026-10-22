// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Liberty Boolean function expressions (LIBERTY-REQ-0020 to LIBERTY-REQ-0022): the parser of
//! function text.

use std::fmt;

use rusteda_core::liberty::Func;

/// Error while parsing a function expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncError {
    pub pos: usize,
    pub msg: String,
}

impl fmt::Display for FuncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at offset {}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for FuncError {}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Name(String),
    Zero,
    One,
    LParen,
    RParen,
    Bang,
    Tick,
    Caret,
    And,
    Or,
}

fn tokenize(s: &str) -> Result<Vec<(usize, Tok)>, FuncError> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        let tok = match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'!' => Tok::Bang,
            b'\'' => Tok::Tick,
            b'^' => Tok::Caret,
            b'&' | b'*' => Tok::And,
            b'|' | b'+' => Tok::Or,
            b'"' => {
                i += 1;
                continue;
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'\\' => {
                let start = i;
                while i < b.len()
                    && (b[i].is_ascii_alphanumeric()
                        || matches!(b[i], b'_' | b'[' | b']' | b'.' | b'$' | b'\\'))
                {
                    i += 1;
                }
                let word = &s[start..i];
                let tok = match word {
                    "0" => Tok::Zero,
                    "1" => Tok::One,
                    w if w.as_bytes()[0].is_ascii_digit() => {
                        return Err(FuncError {
                            pos: start,
                            msg: format!("invalid operand '{w}'"),
                        });
                    }
                    w => Tok::Name(w.to_string()),
                };
                out.push((start, tok));
                continue;
            }
            _ => {
                return Err(FuncError {
                    pos: i,
                    msg: format!("unexpected character '{}'", c as char),
                });
            }
        };
        out.push((i, tok));
        i += 1;
    }
    Ok(out)
}

struct Parser {
    toks: Vec<(usize, Tok)>,
    pos: usize,
    end: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.1)
    }

    fn at(&self) -> usize {
        self.toks.get(self.pos).map_or(self.end, |t| t.0)
    }

    fn err<T>(&self, msg: &str) -> Result<T, FuncError> {
        Err(FuncError {
            pos: self.at(),
            msg: msg.to_string(),
        })
    }

    fn or(&mut self) -> Result<Func, FuncError> {
        let mut lhs = self.and()?;
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            let rhs = self.and()?;
            lhs = Func::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn starts_primary(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::Name(_) | Tok::Zero | Tok::One | Tok::LParen | Tok::Bang)
        )
    }

    fn and(&mut self) -> Result<Func, FuncError> {
        let mut lhs = self.xor()?;
        loop {
            if self.peek() == Some(&Tok::And) {
                self.pos += 1;
            } else if !self.starts_primary() {
                break;
            }
            let rhs = self.xor()?;
            lhs = Func::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn xor(&mut self) -> Result<Func, FuncError> {
        let mut lhs = self.unary()?;
        while self.peek() == Some(&Tok::Caret) {
            self.pos += 1;
            let rhs = self.unary()?;
            lhs = Func::Xor(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Func, FuncError> {
        self.depth += 1;
        if self.depth > 512 {
            return self.err("expression nested too deeply");
        }
        let r = if self.peek() == Some(&Tok::Bang) {
            self.pos += 1;
            Ok(Func::Not(Box::new(self.unary()?)))
        } else {
            let mut p = self.primary()?;
            while self.peek() == Some(&Tok::Tick) {
                self.pos += 1;
                p = Func::Not(Box::new(p));
            }
            Ok(p)
        };
        self.depth -= 1;
        r
    }

    fn primary(&mut self) -> Result<Func, FuncError> {
        match self.peek().cloned() {
            Some(Tok::Zero) => {
                self.pos += 1;
                Ok(Func::Const(false))
            }
            Some(Tok::One) => {
                self.pos += 1;
                Ok(Func::Const(true))
            }
            Some(Tok::Name(n)) => {
                self.pos += 1;
                Ok(Func::Var(n))
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                let e = self.or()?;
                if self.peek() != Some(&Tok::RParen) {
                    return self.err("expected ')'");
                }
                self.pos += 1;
                Ok(e)
            }
            _ => self.err("expected an operand"),
        }
    }
}

/// Parses a Liberty function expression.
pub fn parse_function(text: &str) -> Result<Func, FuncError> {
    let toks = tokenize(text)?;
    if toks.is_empty() {
        return Err(FuncError {
            pos: 0,
            msg: "empty function".into(),
        });
    }
    let mut p = Parser {
        toks,
        pos: 0,
        end: text.len(),
        depth: 0,
    };
    let f = p.or()?;
    if p.pos != p.toks.len() {
        return p.err("unexpected token");
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusteda_core::liberty::var_mask;

    fn tt(s: &str, vars: &[&str]) -> u64 {
        parse_function(s).unwrap().truth_table(vars).unwrap()
    }

    // LIBERTY-REQ-0020: operators, synonyms and precedence.
    #[test]
    fn operators_and_precedence() {
        let a = var_mask(0) & 0xFF;
        let b = var_mask(1) & 0xFF;
        let c = var_mask(2) & 0xFF;
        let v = ["A", "B", "C"];
        assert_eq!(tt("A&B", &v), a & b);
        assert_eq!(tt("A*B", &v), a & b);
        assert_eq!(tt("A B", &v), a & b);
        assert_eq!(tt("A|B", &v), a | b);
        assert_eq!(tt("A+B", &v), a | b);
        assert_eq!(tt("!A", &v), !a & 0xFF);
        assert_eq!(tt("A'", &v), !a & 0xFF);
        assert_eq!(tt("(A+B)'", &v), !(a | b) & 0xFF);
        assert_eq!(tt("!A'", &v), a);
        // AND binds tighter than OR.
        assert_eq!(tt("A|B&C", &v), a | (b & c));
        // XOR binds tighter than AND.
        assert_eq!(tt("A&B^C", &v), a & (b ^ c));
        assert_eq!(tt("A^B|C", &v), (a ^ b) | c);
        // Complement binds tightest.
        assert_eq!(tt("!A&B", &v), !a & b & 0xFF);
        assert_eq!(tt("A B'+C", &v), (a & !b & 0xFF) | c);
        assert_eq!(tt("0", &v), 0);
        assert_eq!(tt("1", &v), 0xFF);
    }

    // LIBERTY-REQ-0022
    #[test]
    fn six_variable_table_and_unknown_names() {
        let v = ["A", "B", "C", "D", "E", "F"];
        assert_eq!(tt("F", &v), 0xFFFF_FFFF_0000_0000);
        assert_eq!(tt("A&B&C&D&E&F", &v), 1u64 << 63);
        assert!(parse_function("A&Z").unwrap().truth_table(&["A"]).is_none());
        assert!(
            parse_function("A")
                .unwrap()
                .truth_table(&["A"; 7])
                .is_none()
        );
    }

    #[test]
    fn parse_errors() {
        for bad in ["", "A &", "(A", "A)", "A # B", "2A"] {
            assert!(parse_function(bad).is_err(), "{bad}");
        }
        let deep = "!".repeat(2000) + "A";
        assert!(parse_function(&deep).is_err());
    }
}
