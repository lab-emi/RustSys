// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Generational arena: entries addressed by typed ids that detect use after removal
//! (CORE-REQ-0101 to 0103).
//!
//! An [`Id`] holds a slot index and the generation of the slot when the entry was inserted.
//! Removing an entry increments the slot's generation, so ids of removed entries are stale:
//! lookups report them (`None` or [`StaleId`]) instead of reaching another entry. An id carries
//! no mark of its arena: an id of another arena is reported only when it addresses no live entry
//! of this one, so ids must stay with the arena that made them. Freed slots
//! are reused last-freed-first, which makes the ids a function of the operation sequence. A slot
//! whose generation reaches `u32::MAX` is retired and never reused, so generations never wrap.
//!
//! ```
//! use rusteda_core::arena::Arena;
//!
//! let mut a = Arena::new();
//! let x = a.insert("x");
//! assert_eq!(a.remove(x), Ok("x"));
//! let y = a.insert("y");
//! assert_eq!(y.index(), x.index(), "the freed slot is reused");
//! assert!(a.get(x).is_none(), "the old id is stale");
//! assert_eq!(a[y], "y");
//! ```

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

/// The id of an entry of an [`Arena<T>`]: a slot index and a generation. Lookups check both,
/// which detects removed entries; an id of another arena that addresses a live entry of this one
/// is not detected.
pub struct Id<T> {
    index: u32,
    generation: u32,
    _entry: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    /// An id from its parts, for example read back from a handle; lookups check it like any
    /// other id.
    pub fn new(index: u32, generation: u32) -> Id<T> {
        Id {
            index,
            generation,
            _entry: PhantomData,
        }
    }

    pub fn index(self) -> u32 {
        self.index
    }

    pub fn generation(self) -> u32 {
        self.generation
    }
}

// Manual impls: derives would require the same traits of `T`.
impl<T> Clone for Id<T> {
    fn clone(&self) -> Id<T> {
        *self
    }
}

impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Id<T>) -> bool {
        (self.index, self.generation) == (other.index, other.generation)
    }
}

impl<T> Eq for Id<T> {}

impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Id<T>) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Id<T>) -> Ordering {
        (self.index, self.generation).cmp(&(other.index, other.generation))
    }
}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({}#{})", self.index, self.generation)
    }
}

/// An id that addresses no live entry: its entry was removed, its slot does not exist, or it is
/// an id of another arena that addresses no live entry here (see [`Id`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleId {
    pub index: u32,
    pub generation: u32,
}

impl fmt::Display for StaleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stale id (slot {}, generation {})",
            self.index, self.generation
        )
    }
}

impl std::error::Error for StaleId {}

#[derive(Debug, Clone)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// Entries of type `T` in slots addressed by [`Id<T>`].
#[derive(Debug, Clone)]
pub struct Arena<T> {
    slots: Vec<Slot<T>>,
    /// Free slot indices; the last one is reused first.
    free: Vec<u32>,
    len: usize,
}

impl<T> Default for Arena<T> {
    fn default() -> Arena<T> {
        Arena {
            slots: Vec::new(),
            free: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Arena<T> {
    pub fn new() -> Arena<T> {
        Arena::default()
    }

    /// Adds an entry: in the most recently freed slot, otherwise in a new slot.
    ///
    /// # Panics
    ///
    /// When the arena would need more than `u32::MAX` slots.
    pub fn insert(&mut self, value: T) -> Id<T> {
        self.len += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            return Id::new(index, slot.generation);
        }
        let index = u32::try_from(self.slots.len()).expect("more than u32::MAX arena slots");
        self.slots.push(Slot {
            generation: 0,
            value: Some(value),
        });
        Id::new(index, 0)
    }

    /// Removes an entry. Its id and all copies of it become stale.
    pub fn remove(&mut self, id: Id<T>) -> Result<T, StaleId> {
        let Some(slot) = self
            .slots
            .get_mut(id.index as usize)
            .filter(|s| s.generation == id.generation)
        else {
            return Err(stale(id));
        };
        let value = slot.value.take().ok_or_else(|| stale(id))?;
        // Live slots never have generation u32::MAX, so this cannot overflow.
        slot.generation += 1;
        if slot.generation < u32::MAX {
            self.free.push(id.index);
        }
        self.len -= 1;
        Ok(value)
    }

    pub fn get(&self, id: Id<T>) -> Option<&T> {
        self.slots
            .get(id.index as usize)
            .filter(|s| s.generation == id.generation)
            .and_then(|s| s.value.as_ref())
    }

    pub fn get_mut(&mut self, id: Id<T>) -> Option<&mut T> {
        self.slots
            .get_mut(id.index as usize)
            .filter(|s| s.generation == id.generation)
            .and_then(|s| s.value.as_mut())
    }

    /// [`Arena::get`] with the stale id as the error.
    pub fn try_get(&self, id: Id<T>) -> Result<&T, StaleId> {
        self.get(id).ok_or_else(|| stale(id))
    }

    /// [`Arena::get_mut`] with the stale id as the error.
    pub fn try_get_mut(&mut self, id: Id<T>) -> Result<&mut T, StaleId> {
        self.get_mut(id).ok_or_else(|| stale(id))
    }

    pub fn contains(&self, id: Id<T>) -> bool {
        self.get(id).is_some()
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of slots (live, free and retired): every id index is below it, so it sizes side
    /// tables indexed by [`Id::index`].
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Live entries in slot index order.
    pub fn iter(&self) -> impl Iterator<Item = (Id<T>, &T)> + '_ {
        self.slots.iter().enumerate().filter_map(|(i, s)| {
            s.value
                .as_ref()
                .map(|v| (Id::new(i as u32, s.generation), v))
        })
    }

    /// Live entries in slot index order, mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Id<T>, &mut T)> + '_ {
        self.slots.iter_mut().enumerate().filter_map(|(i, s)| {
            let generation = s.generation;
            s.value.as_mut().map(|v| (Id::new(i as u32, generation), v))
        })
    }

    /// Ids of the live entries in slot index order.
    pub fn ids(&self) -> impl Iterator<Item = Id<T>> + '_ {
        self.iter().map(|(id, _)| id)
    }
}

fn stale<T>(id: Id<T>) -> StaleId {
    StaleId {
        index: id.index,
        generation: id.generation,
    }
}

/// Indexing panics when the id is stale.
impl<T> std::ops::Index<Id<T>> for Arena<T> {
    type Output = T;

    fn index(&self, id: Id<T>) -> &T {
        match self.get(id) {
            Some(v) => v,
            None => panic!("{}", stale(id)),
        }
    }
}

/// Indexing panics when the id is stale.
impl<T> std::ops::IndexMut<Id<T>> for Arena<T> {
    fn index_mut(&mut self, id: Id<T>) -> &mut T {
        match self.get_mut(id) {
            Some(v) => v,
            None => panic!("{}", stale(id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CORE-REQ-0101: lookups check slot and generation; removed entries leave stale ids.
    #[test]
    fn insert_get_remove() {
        let mut a: Arena<String> = Arena::new();
        assert!(a.is_empty());
        let x = a.insert("x".into());
        let y = a.insert("y".into());
        assert_eq!((x.index(), x.generation(), y.index()), (0, 0, 1));
        assert_eq!(a.len(), 2);
        a.get_mut(y).unwrap().push('!');
        assert_eq!(a.try_get(y).map(String::as_str), Ok("y!"));
        assert_eq!(a.remove(x).as_deref(), Ok("x"));
        assert_eq!(a.len(), 1);
        assert!(!a.contains(x));
        assert_eq!(a.get(x), None);
        assert_eq!(a.get_mut(x), None);
        let e = a.remove(x).unwrap_err();
        assert_eq!(
            e,
            StaleId {
                index: 0,
                generation: 0
            }
        );
        assert_eq!(e.to_string(), "stale id (slot 0, generation 0)");
        assert_eq!(a.try_get_mut(x), Err(e));
        // Ids beyond the slots are stale too.
        assert!(a.get(Id::new(7, 0)).is_none());
        assert_eq!(a.slot_count(), 2);
    }

    // CORE-REQ-0102: freed slots are reused last-freed-first with the next generation; old ids
    // stay stale.
    #[test]
    fn reuse_is_lifo_and_generational() {
        let mut a = Arena::new();
        let ids: Vec<Id<u32>> = (0..4).map(|v| a.insert(v)).collect();
        a.remove(ids[1]).unwrap();
        a.remove(ids[3]).unwrap();
        let p = a.insert(10);
        let q = a.insert(11);
        assert_eq!((p.index(), p.generation()), (3, 1));
        assert_eq!((q.index(), q.generation()), (1, 1));
        assert!(a.get(ids[3]).is_none() && a.get(ids[1]).is_none());
        assert_eq!(a[p], 10);
        let r = a.insert(12);
        assert_eq!((r.index(), r.generation()), (4, 0));
        // Iteration follows slot indices, not insertion order.
        let order: Vec<u32> = a.iter().map(|(_, v)| *v).collect();
        assert_eq!(order, [0, 11, 2, 10, 12]);
        assert_eq!(
            a.ids().collect::<Vec<_>>(),
            a.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        for (_, v) in a.iter_mut() {
            *v += 1;
        }
        assert_eq!(a[q], 12);
        // The same operation sequence gives the same ids.
        let mut b = Arena::new();
        let ids2: Vec<Id<u32>> = (0..4).map(|v| b.insert(v)).collect();
        b.remove(ids2[1]).unwrap();
        b.remove(ids2[3]).unwrap();
        assert_eq!((b.insert(0), b.insert(0), b.insert(0)), (p, q, r));
    }

    // CORE-REQ-0103: a slot whose generation reaches u32::MAX is never handed out again.
    #[test]
    fn exhausted_slots_retire() {
        let mut a = Arena::new();
        let x = a.insert('x');
        a.slots[0].generation = u32::MAX - 2;
        let x = Id::new(x.index(), u32::MAX - 2);
        assert_eq!(a.remove(x), Ok('x'));
        let y = a.insert('y');
        assert_eq!((y.index(), y.generation()), (0, u32::MAX - 1));
        assert_eq!(a.remove(y), Ok('y'));
        assert!(a.free.is_empty(), "slot retired");
        let z = a.insert('z');
        assert_eq!((z.index(), z.generation()), (1, 0));
        assert!(a.get(Id::new(0, u32::MAX)).is_none());
        assert!(a.get(y).is_none());
        assert_eq!(a.iter().count(), 1);
        assert_eq!(a.slot_count(), 2);
    }

    #[test]
    fn ids_are_plain_values() {
        struct NotClone;
        let mut a = Arena::new();
        let id = a.insert(NotClone);
        let copy = id;
        let mut set = std::collections::HashSet::new();
        set.insert(id);
        assert!(set.contains(&copy));
        assert!(Id::<NotClone>::new(0, 1) > id && Id::<NotClone>::new(1, 0) > id);
        assert_eq!(format!("{id:?}"), "Id(0#0)");
    }

    // CORE-REQ-0101: an id carries no mark of its arena: one of another arena reaches a live
    // entry here, and is reported only when it addresses none.
    #[test]
    fn ids_of_another_arena() {
        let mut a = Arena::new();
        let mut b = Arena::new();
        let x = a.insert('x');
        b.insert('y');
        assert_eq!(b.get(x), Some(&'y'));
        let z = a.insert('z');
        assert_eq!(
            b.try_get(z),
            Err(StaleId {
                index: 1,
                generation: 0
            })
        );
    }

    #[test]
    #[should_panic(expected = "stale id")]
    fn indexing_with_a_stale_id_panics() {
        let mut a = Arena::new();
        let id = a.insert(1);
        a.remove(id).unwrap();
        let _ = a[id];
    }
}
