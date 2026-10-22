// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The checker's own reader of Liberty Boolean functions (LIBERTY-REQ-0020): it builds circuit
//! bits directly from the text.

use crate::circ::{B, Circuit};

struct P<'s, 'c> {
    s: &'s [u8],
    i: usize,
    c: &'c mut Circuit,
    look: &'s dyn Fn(&str) -> Option<B>,
    depth: usize,
}

impl P<'_, '_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r' | b'"')
        {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn starts_operand(&mut self) -> bool {
        matches!(self.peek(), Some(b'(' | b'!' | b'0' | b'1'))
            || self
                .peek()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
    }

    fn or(&mut self) -> Result<B, String> {
        let mut a = self.and()?;
        while matches!(self.peek(), Some(b'|' | b'+')) {
            self.i += 1;
            let b = self.and()?;
            a = self.c.or(a, b);
        }
        Ok(a)
    }

    fn and(&mut self) -> Result<B, String> {
        let mut a = self.xor()?;
        loop {
            if matches!(self.peek(), Some(b'&' | b'*')) {
                self.i += 1;
            } else if !self.starts_operand() {
                break;
            }
            let b = self.xor()?;
            a = self.c.and(a, b);
        }
        Ok(a)
    }

    fn xor(&mut self) -> Result<B, String> {
        let mut a = self.unary()?;
        while self.peek() == Some(b'^') {
            self.i += 1;
            let b = self.unary()?;
            a = self.c.xor(a, b);
        }
        Ok(a)
    }

    fn unary(&mut self) -> Result<B, String> {
        self.depth += 1;
        if self.depth > 256 {
            return Err("function nested too deeply".into());
        }
        let r = if self.peek() == Some(b'!') {
            self.i += 1;
            let x = self.unary()?;
            self.c.not(x)
        } else {
            let mut x = self.primary()?;
            while self.peek() == Some(b'\'') {
                self.i += 1;
                x = self.c.not(x);
            }
            x
        };
        self.depth -= 1;
        Ok(r)
    }

    fn primary(&mut self) -> Result<B, String> {
        match self.peek() {
            Some(b'(') => {
                self.i += 1;
                let x = self.or()?;
                if self.peek() != Some(b')') {
                    return Err("missing ')'".into());
                }
                self.i += 1;
                Ok(x)
            }
            Some(b'0')
                if !self
                    .s
                    .get(self.i + 1)
                    .is_some_and(|b| b.is_ascii_alphanumeric()) =>
            {
                self.i += 1;
                Ok(B::F)
            }
            Some(b'1')
                if !self
                    .s
                    .get(self.i + 1)
                    .is_some_and(|b| b.is_ascii_alphanumeric()) =>
            {
                self.i += 1;
                Ok(B::T)
            }
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                let st = self.i;
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_alphanumeric()
                        || matches!(self.s[self.i], b'_' | b'[' | b']' | b'.'))
                {
                    self.i += 1;
                }
                let name = String::from_utf8_lossy(&self.s[st..self.i]).into_owned();
                (self.look)(&name).ok_or_else(|| format!("unknown name '{name}' in function"))
            }
            _ => Err("operand expected".into()),
        }
    }
}

/// Builds the function `text` over names resolved by `look`.
pub fn build(c: &mut Circuit, text: &str, look: &dyn Fn(&str) -> Option<B>) -> Result<B, String> {
    let mut p = P {
        s: text.as_bytes(),
        i: 0,
        c,
        look,
        depth: 0,
    };
    let r = p.or()?;
    if p.peek().is_some() {
        return Err(format!("unexpected text in function '{text}'"));
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn precedence_matches_the_specification() {
        let mut c = Circuit::new();
        let a = c.free();
        let b = c.free();
        let x = c.free();
        let look = move |n: &str| match n {
            "A" => Some(B::V(a)),
            "B" => Some(B::V(b)),
            "C" => Some(B::V(x)),
            _ => None,
        };
        let cases: [(&str, fn(bool, bool, bool) -> bool); 7] = [
            ("A|B&C", |a, b, c| a | (b & c)),
            ("A&B^C", |a, b, c| a & (b ^ c)),
            ("A B'+C", |a, b, c| (a & !b) | c),
            ("!(A+B)", |a, b, _| !(a | b)),
            ("(A^B)'", |a, b, _| !(a ^ b)),
            ("A*B*C", |a, b, c| a & b & c),
            ("1", |_, _, _| true),
        ];
        for (text, f) in cases {
            let r = build(&mut c, text, &look).unwrap();
            let order = c.order(&[r]).unwrap();
            for m in 0..8u32 {
                let mut vals = HashMap::new();
                let w = |v: u32| {
                    if (v == a && m & 1 != 0) || (v == b && m & 2 != 0) || (v == x && m & 4 != 0) {
                        u64::MAX
                    } else {
                        0
                    }
                };
                c.eval(&order, &w, &mut vals);
                assert_eq!(
                    Circuit::value(r, &vals) & 1 == 1,
                    f(m & 1 != 0, m & 2 != 0, m & 4 != 0),
                    "{text} {m}"
                );
            }
        }
        assert!(build(&mut c, "A &", &look).is_err());
        assert!(build(&mut c, "Z", &look).is_err());
    }
}
