// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Symbol interning (CORE-REQ-0100). Every database owns its interner; there is no global table,
//! so the symbols of one database are a function of the order in which it interned its names.
//!
//! ```
//! use rusteda_core::symbol::SymbolInterner;
//!
//! let mut s = SymbolInterner::new();
//! let a = s.intern("clk");
//! let b = s.intern("rst_n");
//! assert_eq!(s.intern("clk"), a);
//! assert_eq!((a.index(), b.index()), (0, 1));
//! assert_eq!(s.resolve(b), "rst_n");
//! assert_eq!(s.get("missing"), None);
//! ```

use rustc_hash::FxHashMap;

/// An interned string of one [`SymbolInterner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolId(u32);

impl SymbolId {
    /// Position of the symbol in its interner: ids are dense and follow insertion order.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// The strings of one database, numbered densely in insertion order.
#[derive(Debug, Clone, Default)]
pub struct SymbolInterner {
    map: FxHashMap<Box<str>, SymbolId>,
    strings: Vec<Box<str>>,
}

impl SymbolInterner {
    pub fn new() -> SymbolInterner {
        SymbolInterner::default()
    }

    /// The symbol of `s`; a new string gets the next id.
    ///
    /// # Panics
    ///
    /// When the interner already holds `u32::MAX` strings.
    pub fn intern(&mut self, s: &str) -> SymbolId {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = SymbolId(u32::try_from(self.strings.len()).expect("more than u32::MAX symbols"));
        self.strings.push(s.into());
        self.map.insert(s.into(), id);
        id
    }

    /// The symbol of `s` if it was interned; never adds a string.
    pub fn get(&self, s: &str) -> Option<SymbolId> {
        self.map.get(s).copied()
    }

    /// The string of a symbol.
    ///
    /// # Panics
    ///
    /// When `id` was not made by this interner (see [`SymbolInterner::try_resolve`]).
    pub fn resolve(&self, id: SymbolId) -> &str {
        self.try_resolve(id).expect("symbol id of another interner")
    }

    /// The string of a symbol, or `None` for an id this interner did not make.
    pub fn try_resolve(&self, id: SymbolId) -> Option<&str> {
        self.strings.get(id.0 as usize).map(|s| &**s)
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// All symbols in id order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (SymbolId, &str)> + '_ {
        self.strings
            .iter()
            .enumerate()
            .map(|(i, s)| (SymbolId(i as u32), &**s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CORE-REQ-0100: ids depend only on the insertion order, so two databases built alike agree.
    #[test]
    fn dense_ids_in_insertion_order() {
        let names = ["b", "a", "b", "", "u1/q[3]", "a"];
        let build = || {
            let mut s = SymbolInterner::new();
            let ids: Vec<u32> = names.iter().map(|n| s.intern(n).index()).collect();
            (s, ids)
        };
        let (s, ids) = build();
        assert_eq!(ids, [0, 1, 0, 2, 3, 1]);
        assert_eq!(build().1, ids);
        assert_eq!(s.len(), 4);
        assert!(!s.is_empty());
        let all: Vec<(u32, &str)> = s.iter().map(|(id, n)| (id.index(), n)).collect();
        assert_eq!(all, [(0, "b"), (1, "a"), (2, ""), (3, "u1/q[3]")]);
        for (id, n) in s.iter() {
            assert_eq!(s.get(n), Some(id));
            assert_eq!(s.resolve(id), n);
        }
        assert_eq!(s.get("c"), None);
        assert_eq!(s.len(), 4, "get never interns");
    }

    fn foreign() -> (SymbolInterner, SymbolId) {
        let mut big = SymbolInterner::new();
        big.intern("x");
        let y = big.intern("y");
        let mut small = SymbolInterner::new();
        assert!(small.is_empty());
        small.intern("x");
        (small, y)
    }

    // CORE-REQ-0100: an id of another interner resolves to nothing, or panics.
    #[test]
    fn foreign_ids() {
        let (small, y) = foreign();
        assert_eq!(small.try_resolve(y), None);
    }

    #[test]
    #[should_panic(expected = "another interner")]
    fn resolving_a_foreign_id_panics() {
        let (small, y) = foreign();
        small.resolve(y);
    }
}
