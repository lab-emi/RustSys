// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Types of the checker's RTL model.

use std::rc::Rc;

/// Packed part of a type.
#[derive(Debug, Clone, PartialEq)]
pub enum Packed {
    /// A single bit.
    Bit,
    /// `elem` repeated over the declared range `[left:right]`.
    Arr {
        elem: Rc<Packed>,
        left: i64,
        right: i64,
    },
    /// Structure members, first declared first (most significant).
    Struct(Rc<Vec<(String, Packed)>>),
    Union(Rc<Vec<(String, Packed)>>),
}

impl Packed {
    pub fn width(&self) -> u64 {
        match self {
            Packed::Bit => 1,
            Packed::Arr { elem, left, right } => elem.width() * ((left - right).unsigned_abs() + 1),
            Packed::Struct(m) => m.iter().map(|(_, t)| t.width()).sum(),
            Packed::Union(m) => m.iter().map(|(_, t)| t.width()).max().unwrap_or(0),
        }
    }

    /// Member offset (from the least significant bit) and type.
    pub fn member(&self, name: &str) -> Option<(u64, Packed)> {
        match self {
            Packed::Struct(m) => {
                let mut off: u64 = m.iter().map(|(_, t)| t.width()).sum();
                for (n, t) in m.iter() {
                    off -= t.width();
                    if n == name {
                        return Some((off, t.clone()));
                    }
                }
                None
            }
            Packed::Union(m) => m
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, t)| (0, t.clone())),
            _ => None,
        }
    }
}

/// A full type: packed part, signedness of the packed value, unpacked dimensions (outer first).
#[derive(Debug, Clone, PartialEq)]
pub struct Ty {
    pub packed: Packed,
    pub signed: bool,
    pub unpacked: Vec<(i64, i64)>,
}

impl Ty {
    pub fn bits(w: u64, signed: bool) -> Ty {
        let packed = if w == 1 {
            Packed::Bit
        } else {
            Packed::Arr {
                elem: Rc::new(Packed::Bit),
                left: w as i64 - 1,
                right: 0,
            }
        };
        Ty {
            packed,
            signed,
            unpacked: Vec::new(),
        }
    }

    pub fn is_array(&self) -> bool {
        !self.unpacked.is_empty()
    }

    /// Width of one element.
    pub fn width(&self) -> u64 {
        self.packed.width()
    }

    /// Number of elements.
    pub fn count(&self) -> u64 {
        self.unpacked
            .iter()
            .map(|(l, r)| (l - r).unsigned_abs() + 1)
            .product::<u64>()
            .max(1)
    }

    /// Type after selecting one index of the outermost dimension.
    pub fn index_type(&self) -> Option<(i64, i64, Ty)> {
        if let Some(((l, r), rest)) = self.unpacked.split_first() {
            return Some((
                *l,
                *r,
                Ty {
                    packed: self.packed.clone(),
                    signed: self.signed,
                    unpacked: rest.to_vec(),
                },
            ));
        }
        match &self.packed {
            Packed::Arr { elem, left, right } => Some((
                *left,
                *right,
                Ty {
                    packed: (**elem).clone(),
                    signed: false,
                    unpacked: Vec::new(),
                },
            )),
            other => {
                let w = other.width() as i64;
                Some((
                    w - 1,
                    0,
                    Ty {
                        packed: Packed::Bit,
                        signed: false,
                        unpacked: Vec::new(),
                    },
                ))
            }
        }
    }
}

/// Offset of index `i` from the right bound, if in range.
pub fn offset(left: i64, right: i64, i: i64) -> Option<u64> {
    let (lo, hi) = (left.min(right), left.max(right));
    if i < lo || i > hi {
        return None;
    }
    Some(if left >= right {
        (i - right) as u64
    } else {
        (right - i) as u64
    })
}

/// Position of index `i` from the left bound, if in range.
pub fn position(left: i64, right: i64, i: i64) -> Option<u64> {
    let n = (left - right).unsigned_abs() + 1;
    offset(left, right, i).map(|o| n - 1 - o)
}

/// Index at position `p` from the left bound.
pub fn index_at(left: i64, right: i64, p: u64) -> i64 {
    if left <= right {
        left + p as i64
    } else {
        left - p as i64
    }
}
