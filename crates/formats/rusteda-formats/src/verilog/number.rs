// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Integer literals (IEEE 1800-2017 clause 5.7.1) and their bit patterns.

/// Largest literal width accepted, in bits.
pub const MAX_LITERAL_BITS: u32 = 1 << 20;

/// One four-state bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bit4 {
    Zero,
    One,
    X,
    Z,
}

impl Bit4 {
    /// True for `x` and `z`.
    pub fn is_unknown(self) -> bool {
        matches!(self, Bit4::X | Bit4::Z)
    }
}

/// An integer literal as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    /// Explicit size, if any.
    pub size: Option<u32>,
    /// `true` for plain decimal numbers (`42`), which are signed and unsized.
    pub plain: bool,
    /// `s` flag of a based literal.
    pub signed: bool,
    /// `b`, `o`, `d` or `h`; `d` for plain decimals.
    pub base: char,
    /// Lower-case digits without underscores; `?` is kept.
    pub digits: String,
}

fn decimal_to_bits(digits: &str) -> Vec<Bit4> {
    // Little-endian 32-bit limbs.
    let mut limbs: Vec<u32> = vec![0];
    for c in digits.bytes() {
        let d = u64::from(c.wrapping_sub(b'0'));
        let mut carry = d;
        for l in limbs.iter_mut() {
            let v = u64::from(*l) * 10 + carry;
            *l = v as u32;
            carry = v >> 32;
        }
        if carry != 0 {
            limbs.push(carry as u32);
        }
    }
    let mut bits = Vec::with_capacity(limbs.len() * 32);
    for l in &limbs {
        for k in 0..32 {
            bits.push(if (l >> k) & 1 == 1 {
                Bit4::One
            } else {
                Bit4::Zero
            });
        }
    }
    while bits.len() > 1 && bits.last() == Some(&Bit4::Zero) {
        bits.pop();
    }
    bits
}

impl Literal {
    /// A plain decimal literal.
    pub fn decimal(digits: &str) -> Literal {
        Literal {
            size: None,
            plain: true,
            signed: true,
            base: 'd',
            digits: digits.to_string(),
        }
    }

    /// True if the literal is signed (plain decimals, or based with `s`).
    pub fn is_signed(&self) -> bool {
        self.plain || self.signed
    }

    /// Bits of the digits alone (least significant first), before sizing.
    fn natural_bits(&self) -> Result<Vec<Bit4>, String> {
        let per = match self.base {
            'b' => 1,
            'o' => 3,
            'h' => 4,
            'd' => 0,
            b => return Err(format!("invalid base '{b}'")),
        };
        if per == 0 {
            let d = self.digits.as_str();
            if matches!(d, "x" | "z" | "?") {
                return Ok(vec![if d == "x" { Bit4::X } else { Bit4::Z }]);
            }
            if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("invalid decimal digits '{d}'"));
            }
            if d.len() > 400_000 {
                return Err("decimal literal too long".into());
            }
            return Ok(decimal_to_bits(d));
        }
        let mut bits = Vec::with_capacity(self.digits.len() * per);
        for c in self.digits.bytes().rev() {
            let fill = match c {
                b'x' => Some(Bit4::X),
                b'z' | b'?' => Some(Bit4::Z),
                _ => None,
            };
            if let Some(f) = fill {
                bits.extend(std::iter::repeat_n(f, per));
                continue;
            }
            let v = (c as char)
                .to_digit(16)
                .ok_or_else(|| format!("invalid digit '{}'", c as char))?;
            if v >= (1 << per) {
                return Err(format!(
                    "digit '{}' out of range for base {}",
                    c as char, self.base
                ));
            }
            for k in 0..per {
                bits.push(if (v >> k) & 1 == 1 {
                    Bit4::One
                } else {
                    Bit4::Zero
                });
            }
        }
        if bits.len() > MAX_LITERAL_BITS as usize * 4 {
            return Err("literal too long".into());
        }
        Ok(bits)
    }

    /// Width of the literal: its size, or for unsized literals at least 32 bits.
    pub fn width(&self) -> Result<u32, String> {
        if let Some(s) = self.size {
            if s == 0 || s > MAX_LITERAL_BITS {
                return Err(format!("invalid literal size {s}"));
            }
            return Ok(s);
        }
        let n = self.natural_bits()?.len() as u32;
        Ok(n.max(32))
    }

    /// Four-state bits, least significant first, `width()` long.
    pub fn bits(&self) -> Result<Vec<Bit4>, String> {
        let w = self.width()? as usize;
        let mut bits = self.natural_bits()?;
        let top = bits.last().copied().unwrap_or(Bit4::Zero);
        // A single decimal x/z digit fills the whole width.
        let fill = if top.is_unknown() { top } else { Bit4::Zero };
        if bits.len() > w {
            bits.truncate(w);
        } else {
            bits.resize(w, fill);
        }
        Ok(bits)
    }

    /// True if the literal has more significant digit bits than its size (truncated).
    pub fn is_truncated(&self) -> bool {
        match (self.size, self.natural_bits()) {
            (Some(s), Ok(b)) => {
                let significant = b
                    .iter()
                    .rposition(|x| *x != Bit4::Zero)
                    .map_or(0, |p| p + 1);
                significant > s as usize
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Bit4::{One as I, X, Z, Zero as O};

    fn lit(size: Option<u32>, signed: bool, base: char, digits: &str) -> Literal {
        Literal {
            size,
            plain: false,
            signed,
            base,
            digits: digits.into(),
        }
    }

    // VERILOG-REQ-0001: integer literal forms of clause 5.7.1.
    #[test]
    fn sizes_bases_and_fills() {
        assert_eq!(
            lit(Some(4), false, 'b', "1010").bits().unwrap(),
            vec![O, I, O, I]
        );
        assert_eq!(
            lit(Some(8), false, 'h', "f").bits().unwrap(),
            vec![I, I, I, I, O, O, O, O]
        );
        assert_eq!(
            lit(Some(6), false, 'o', "7").bits().unwrap(),
            vec![I, I, I, O, O, O]
        );
        assert_eq!(
            lit(Some(4), false, 'b', "x1").bits().unwrap(),
            vec![I, X, X, X]
        );
        assert_eq!(lit(Some(4), false, 'b', "z").bits().unwrap(), vec![Z; 4]);
        assert_eq!(lit(Some(4), false, 'd', "x").bits().unwrap(), vec![X; 4]);
        assert_eq!(
            lit(Some(4), false, 'b', "1?0").bits().unwrap(),
            vec![O, Z, I, O]
        );
        // Truncation keeps the least significant bits.
        let t = lit(Some(4), false, 'h', "3f");
        assert_eq!(t.bits().unwrap(), vec![I; 4]);
        assert!(t.is_truncated());
        // Unsized based literal: at least 32 bits.
        assert_eq!(lit(None, false, 'h', "1").width().unwrap(), 32);
        assert_eq!(lit(None, false, 'h', "123456789").width().unwrap(), 36);
        // Plain decimals are signed and 32 bits or more.
        let d = Literal::decimal("4294967296");
        assert!(d.is_signed());
        assert_eq!(d.width().unwrap(), 33);
        let bits = d.bits().unwrap();
        assert_eq!(bits[32], I);
        assert!(bits[..32].iter().all(|b| *b == O));
        assert_eq!(Literal::decimal("5").bits().unwrap()[..3], [I, O, I]);
        assert!(lit(Some(0), false, 'b', "1").bits().is_err());
        assert!(lit(Some(4), false, 'b', "2").bits().is_err());
        assert!(lit(Some(4), false, 'o', "8").bits().is_err());
    }
}
