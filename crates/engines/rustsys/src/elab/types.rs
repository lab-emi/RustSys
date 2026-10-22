// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Elaborated types (SYNTH-REQ-0110, SYNTH-REQ-0207, SYNTH-REQ-0208).

use std::rc::Rc;

/// A declared index range `[left:right]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub left: i64,
    pub right: i64,
}

impl Range {
    /// `[w-1:0]`
    pub fn down(w: u32) -> Range {
        Range {
            left: i64::from(w) - 1,
            right: 0,
        }
    }

    /// `[0:n-1]`, used for unpacked `[n]` dimensions.
    pub fn up(n: u32) -> Range {
        Range {
            left: 0,
            right: i64::from(n) - 1,
        }
    }

    pub fn len(&self) -> u32 {
        (self.left - self.right).unsigned_abs() as u32 + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Offset of index `i` counted from the right bound (packed: from the least significant
    /// element), or `None` if out of range.
    pub fn offset(&self, i: i64) -> Option<u32> {
        let (lo, hi) = (self.left.min(self.right), self.left.max(self.right));
        if i < lo || i > hi {
            return None;
        }
        Some(if self.left >= self.right {
            (i - self.right) as u32
        } else {
            (self.right - i) as u32
        })
    }

    /// Position of index `i` counted from the left bound (unpacked element order).
    pub fn position(&self, i: i64) -> Option<u32> {
        let n = self.len();
        self.offset(i).map(|o| n - 1 - o)
    }

    /// Index at a position counted from the left bound.
    pub fn index_at_position(&self, p: u32) -> i64 {
        if self.left <= self.right {
            self.left + i64::from(p)
        } else {
            self.left - i64::from(p)
        }
    }

    pub fn lo(&self) -> i64 {
        self.left.min(self.right)
    }

    pub fn hi(&self) -> i64 {
        self.left.max(self.right)
    }
}

/// A member of a packed structure or union.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    /// Offset of the member's least significant bit within the structure.
    pub offset: u32,
}

/// Enumeration constants of one enumerated type.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumInfo {
    pub members: Vec<(String, u64)>,
}

/// An elaborated type.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    /// One bit.
    Bit { signed: bool },
    /// Packed array of `elem` over `range`; signedness applies to the whole vector.
    Vector {
        elem: Rc<Ty>,
        range: Range,
        signed: bool,
    },
    /// Packed structure or union.
    Struct {
        fields: Rc<Vec<Field>>,
        signed: bool,
        union: bool,
    },
    /// Enumeration with its base type.
    Enum { base: Rc<Ty>, info: Rc<EnumInfo> },
    /// Unpacked array.
    Unpacked { elem: Rc<Ty>, range: Range },
}

impl Ty {
    /// Unsigned or signed bit vector `[w-1:0]` (a 1-bit vector for w = 1).
    pub fn vec(w: u32, signed: bool) -> Ty {
        Ty::Vector {
            elem: Rc::new(Ty::Bit { signed: false }),
            range: Range::down(w.max(1)),
            signed,
        }
    }

    /// The 32-bit signed `int` type.
    pub fn int() -> Ty {
        Ty::vec(32, true)
    }

    pub fn is_packed(&self) -> bool {
        !matches!(self, Ty::Unpacked { .. })
    }

    /// Packed width in bits (0 for unpacked types).
    pub fn width(&self) -> u32 {
        match self {
            Ty::Bit { .. } => 1,
            Ty::Vector { elem, range, .. } => elem.width().saturating_mul(range.len()),
            Ty::Struct { fields, union, .. } => {
                if *union {
                    fields.iter().map(|f| f.ty.width()).max().unwrap_or(0)
                } else {
                    fields.iter().map(|f| f.ty.width()).sum()
                }
            }
            Ty::Enum { base, .. } => base.width(),
            Ty::Unpacked { .. } => 0,
        }
    }

    pub fn is_signed(&self) -> bool {
        match self {
            Ty::Bit { signed } | Ty::Vector { signed, .. } | Ty::Struct { signed, .. } => *signed,
            Ty::Enum { base, .. } => base.is_signed(),
            Ty::Unpacked { .. } => false,
        }
    }

    /// Same type with the given signedness (packed types only).
    pub fn with_signed(&self, s: bool) -> Ty {
        match self {
            Ty::Bit { .. } => Ty::Bit { signed: s },
            Ty::Vector { elem, range, .. } => Ty::Vector {
                elem: elem.clone(),
                range: *range,
                signed: s,
            },
            Ty::Struct { fields, union, .. } => Ty::Struct {
                fields: fields.clone(),
                signed: s,
                union: *union,
            },
            Ty::Enum { .. } | Ty::Unpacked { .. } => Ty::vec(self.width(), s),
        }
    }

    /// Unpacked dimensions from the outside in, and the packed element type.
    pub fn unpacked_dims(&self) -> (Vec<Range>, &Ty) {
        let mut dims = Vec::new();
        let mut t = self;
        while let Ty::Unpacked { elem, range } = t {
            dims.push(*range);
            t = elem;
        }
        (dims, t)
    }

    /// Number of packed elements of an (unpacked) array type; 1 for packed types.
    pub fn element_count(&self) -> u32 {
        let (dims, _) = self.unpacked_dims();
        dims.iter().map(Range::len).product::<u32>().max(1)
    }

    /// Selectable dimension of this type for an index expression: the range and element type.
    /// Packed types without an explicit dimension (structures, enums, bits) select single bits.
    pub fn index_dim(&self) -> Option<(Range, Ty)> {
        match self {
            Ty::Vector { elem, range, .. } => Some((*range, (**elem).clone())),
            Ty::Unpacked { elem, range } => Some((*range, (**elem).clone())),
            Ty::Struct { .. } | Ty::Enum { .. } | Ty::Bit { .. } => {
                let w = self.width();
                Some((Range::down(w), Ty::Bit { signed: false }))
            }
        }
    }

    /// Structure member lookup.
    pub fn field(&self, name: &str) -> Option<&Field> {
        match self {
            Ty::Struct { fields, .. } => fields.iter().find(|f| f.name == name),
            _ => None,
        }
    }

    /// True if the type is a 1-bit scalar for state-naming purposes (SYNTH-REQ-0603).
    pub fn is_single_bit(&self) -> bool {
        self.is_packed() && self.width() == 1
    }
}

/// Builds structure fields from (name, type) in declaration order; the first member is the most
/// significant (SYNTH-REQ-0208).
pub fn layout_struct(members: Vec<(String, Ty)>, union: bool) -> Vec<Field> {
    let mut fields = Vec::with_capacity(members.len());
    let mut offset = 0u32;
    for (name, ty) in members.into_iter().rev() {
        let w = ty.width();
        fields.push(Field {
            name,
            ty,
            offset: if union { 0 } else { offset },
        });
        if !union {
            offset += w;
        }
    }
    fields.reverse();
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    // SYNTH-REQ-0208
    #[test]
    fn struct_layout_and_ranges() {
        let f = layout_struct(
            vec![
                ("a".into(), Ty::vec(4, false)),
                ("b".into(), Ty::Bit { signed: false }),
            ],
            false,
        );
        assert_eq!(f[0].offset, 1);
        assert_eq!(f[1].offset, 0);
        let r = Range { left: 7, right: 0 };
        assert_eq!(r.offset(3), Some(3));
        assert_eq!(r.offset(8), None);
        let r = Range { left: 0, right: 7 };
        assert_eq!(r.offset(0), Some(7));
        assert_eq!(r.position(0), Some(0));
        assert_eq!(Range { left: 3, right: 0 }.position(3), Some(0));
        assert_eq!(Range::up(4).index_at_position(2), 2);
        let arr = Ty::Unpacked {
            elem: Rc::new(Ty::Unpacked {
                elem: Rc::new(Ty::vec(8, false)),
                range: Range::up(3),
            }),
            range: Range::up(2),
        };
        assert_eq!(arr.element_count(), 6);
        assert_eq!(arr.unpacked_dims().1.width(), 8);
    }
}
