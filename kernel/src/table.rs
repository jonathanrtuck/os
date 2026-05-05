//! Flat-array object table with lazy allocation.
//!
//! Each kernel object type (VMO, Event, Endpoint, Thread, AddressSpace)
//! is stored in an ObjectTable. Objects are accessed by ID (array index).
//!
//! Entries use `Option<Box<T>>` — only allocated objects consume heap.
//! At init, the entries Vec holds MAX `None` values (8 bytes each).
//! This is critical: an Endpoint is ~7 KB (16 inline PendingCalls with
//! 128-byte message buffers), so pre-allocating 256 of them would cost
//! 1.6 MB. With Box, the entries Vec costs 256 × 8 = 2 KB, and each
//! Endpoint is allocated individually when created via syscall.
//!
//! The free list uses a singly-linked stack. Alloc pops from head,
//! dealloc pushes back. O(1) both directions.

use alloc::{boxed::Box, vec, vec::Vec};

const EMPTY: u32 = u32::MAX;

pub struct ObjectTable<T, const MAX: usize> {
    entries: Vec<Option<Box<T>>>,
    generations: Vec<u64>,
    free_head: u32,
    free_next: Vec<u32>,
    count: usize,
}

#[allow(clippy::new_without_default)]
impl<T, const MAX: usize> ObjectTable<T, MAX> {
    pub fn new() -> Self {
        let mut entries: Vec<Option<Box<T>>> = Vec::with_capacity(MAX);

        entries.resize_with(MAX, || None);

        let generations: Vec<u64> = vec![0; MAX];
        let free_next: Vec<u32> = (0..MAX)
            .map(|i| if i + 1 < MAX { (i + 1) as u32 } else { EMPTY })
            .collect();

        Self {
            entries,
            generations,
            free_head: if MAX > 0 { 0 } else { EMPTY },
            free_next,
            count: 0,
        }
    }

    pub fn alloc(&mut self, value: T) -> Option<(u32, u64)> {
        self.alloc_boxed(Box::new(value))
    }

    pub fn alloc_boxed(&mut self, value: Box<T>) -> Option<(u32, u64)> {
        let head = self.free_head;

        if head == EMPTY {
            return None;
        }

        let next = self.free_next[head as usize];

        self.free_head = next;
        self.free_next[head as usize] = EMPTY;
        self.entries[head as usize] = Some(value);
        self.count += 1;

        let generation = self.generations[head as usize];

        Some((head, generation))
    }

    pub fn dealloc(&mut self, idx: u32) -> Option<T> {
        let i = idx as usize;

        if i >= MAX {
            return None;
        }

        let value = *self.entries[i].take()?;

        self.generations[i] += 1;
        self.free_next[i] = self.free_head;
        self.free_head = idx;
        self.count -= 1;

        Some(value)
    }

    pub fn get(&self, idx: u32) -> Option<&T> {
        self.entries.get(idx as usize)?.as_deref()
    }

    pub fn get_mut(&mut self, idx: u32) -> Option<&mut T> {
        self.entries.get_mut(idx as usize)?.as_deref_mut()
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn generation(&self, idx: u32) -> u64 {
        self.generations.get(idx as usize).copied().unwrap_or(0)
    }

    #[cfg(any(test, fuzzing, debug_assertions))]
    pub fn iter_allocated(&self) -> impl Iterator<Item = (u32, &T)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_deref().map(|v| (i as u32, v)))
    }

    pub fn iter_allocated_mut(&mut self) -> impl Iterator<Item = (u32, &mut T)> {
        self.entries
            .iter_mut()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_deref_mut().map(|v| (i as u32, v)))
    }

    #[cfg(any(test, fuzzing, debug_assertions))]
    pub fn is_allocated(&self, idx: u32) -> bool {
        self.entries.get(idx as usize).is_some_and(|s| s.is_some())
    }

    /// Get a mutable reference and an immutable reference to two different
    /// slots simultaneously. Uses `split_at_mut` — zero unsafe.
    ///
    /// Panics if `mut_idx == ref_idx`.
    pub fn get_pair_mut(&mut self, mut_idx: u32, ref_idx: u32) -> Option<(&mut T, &T)> {
        assert_ne!(mut_idx, ref_idx);

        let mi = mut_idx as usize;
        let ri = ref_idx as usize;

        if mi >= MAX || ri >= MAX {
            return None;
        }

        if mi < ri {
            let (left, right) = self.entries.split_at_mut(ri);

            Some((left[mi].as_deref_mut()?, right[0].as_deref()?))
        } else {
            let (left, right) = self.entries.split_at_mut(mi);

            Some((right[0].as_deref_mut()?, left[ri].as_deref()?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_and_lookup() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();
        let (id, generation) = t.alloc(42).unwrap();

        assert_eq!(*t.get(id).unwrap(), 42);
        assert_eq!(generation, 0);
    }

    #[test]
    fn dealloc_and_reuse() {
        let mut t: ObjectTable<u64, 2> = ObjectTable::new();
        let (a, _) = t.alloc(1).unwrap();
        let (_b, _) = t.alloc(2).unwrap();

        assert!(t.alloc(3).is_none());

        t.dealloc(a);

        let (c, _) = t.alloc(3).unwrap();

        assert_eq!(c, a);
        assert_eq!(*t.get(c).unwrap(), 3);
    }

    #[test]
    fn exhaustion() {
        let mut t: ObjectTable<u64, 2> = ObjectTable::new();

        t.alloc(1).unwrap();
        t.alloc(2).unwrap();

        assert!(t.alloc(3).is_none());
    }

    #[test]
    fn count_tracking() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();

        assert_eq!(t.count(), 0);

        let (a, _) = t.alloc(1).unwrap();

        assert_eq!(t.count(), 1);

        t.alloc(2).unwrap();

        assert_eq!(t.count(), 2);

        t.dealloc(a);

        assert_eq!(t.count(), 1);
    }

    #[test]
    fn get_mut_modifies() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();
        let (id, _) = t.alloc(10).unwrap();

        *t.get_mut(id).unwrap() = 20;

        assert_eq!(*t.get(id).unwrap(), 20);
    }

    #[test]
    fn out_of_bounds_returns_none() {
        let t: ObjectTable<u64, 2> = ObjectTable::new();

        assert!(t.get(5).is_none());
    }

    #[test]
    fn generation_increments_on_dealloc() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();
        let (idx, gen0) = t.alloc(1).unwrap();

        assert_eq!(gen0, 0);
        assert_eq!(t.generation(idx), 0);

        t.dealloc(idx);

        assert_eq!(t.generation(idx), 1);
    }

    #[test]
    fn stale_generation_after_realloc() {
        let mut t: ObjectTable<u64, 2> = ObjectTable::new();
        let (idx, gen_old) = t.alloc(1).unwrap();

        t.dealloc(idx);

        let (idx2, gen_new) = t.alloc(2).unwrap();

        assert_eq!(idx, idx2);
        assert_eq!(gen_old, 0);
        assert_eq!(gen_new, 1);
        assert_ne!(gen_old, gen_new);
    }

    #[test]
    fn fresh_handle_matches_current_generation() {
        let mut t: ObjectTable<u64, 2> = ObjectTable::new();
        let (idx, _) = t.alloc(1).unwrap();

        t.dealloc(idx);

        let (idx2, generation) = t.alloc(2).unwrap();

        assert_eq!(generation, t.generation(idx2));
    }

    #[test]
    fn free_list_alloc_is_o1() {
        let mut t: ObjectTable<u64, 256> = ObjectTable::new();
        let mut ids = [0u32; 256];

        for i in 0..256 {
            let (id, _) = t.alloc(i as u64).unwrap();
            ids[i] = id;
        }

        assert!(t.alloc(999).is_none());

        t.dealloc(ids[100]);
        t.dealloc(ids[200]);
        t.dealloc(ids[50]);

        let (a, _) = t.alloc(1000).unwrap();
        let (b, _) = t.alloc(1001).unwrap();
        let (c, _) = t.alloc(1002).unwrap();

        assert!(t.alloc(1003).is_none());

        assert_eq!(a, ids[50]);
        assert_eq!(b, ids[200]);
        assert_eq!(c, ids[100]);
    }

    #[test]
    fn get_pair_mut_both_directions() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();
        let (a, _) = t.alloc(10).unwrap();
        let (b, _) = t.alloc(20).unwrap();
        let (ma, rb) = t.get_pair_mut(a, b).unwrap();

        assert_eq!(*ma, 10);
        assert_eq!(*rb, 20);

        *ma = 11;

        assert_eq!(*t.get(a).unwrap(), 11);

        let (mb, ra) = t.get_pair_mut(b, a).unwrap();

        assert_eq!(*mb, 20);
        assert_eq!(*ra, 11);
    }

    #[test]
    fn get_pair_mut_out_of_bounds() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();
        let (a, _) = t.alloc(1).unwrap();

        assert!(t.get_pair_mut(a, 99).is_none());
        assert!(t.get_pair_mut(99, a).is_none());
    }

    #[test]
    fn get_pair_mut_one_empty() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();
        let (a, _) = t.alloc(1).unwrap();

        assert!(t.get_pair_mut(a, 1).is_none());
    }

    #[test]
    fn iter_allocated_skips_empty() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();
        let (a, _) = t.alloc(10).unwrap();
        let (b, _) = t.alloc(20).unwrap();
        let (c, _) = t.alloc(30).unwrap();

        t.dealloc(b);

        let items: alloc::vec::Vec<_> = t.iter_allocated().collect();

        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|&(id, v)| id == a && *v == 10));
        assert!(items.iter().any(|&(id, v)| id == c && *v == 30));
    }

    #[test]
    fn dealloc_nonexistent_returns_none() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();

        assert!(t.dealloc(0).is_none());
        assert!(t.dealloc(99).is_none());
    }

    #[test]
    fn get_pair_mut_adjacent_indices() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();
        let (a, _) = t.alloc(100).unwrap();
        let (b, _) = t.alloc(200).unwrap();

        assert_eq!(a, 0);
        assert_eq!(b, 1);

        let (ma, rb) = t.get_pair_mut(a, b).unwrap();

        *ma = 101;

        assert_eq!(*rb, 200);

        let (mb, ra) = t.get_pair_mut(b, a).unwrap();

        *mb = 201;

        assert_eq!(*ra, 101);
        assert_eq!(*t.get(a).unwrap(), 101);
        assert_eq!(*t.get(b).unwrap(), 201);
    }

    #[test]
    fn get_pair_mut_max_index_distance() {
        let mut t: ObjectTable<u64, 8> = ObjectTable::new();

        for i in 0..8 {
            t.alloc(i as u64 * 10).unwrap();
        }

        let (m0, r7) = t.get_pair_mut(0, 7).unwrap();

        assert_eq!(*m0, 0);
        assert_eq!(*r7, 70);

        let (m7, r0) = t.get_pair_mut(7, 0).unwrap();

        assert_eq!(*m7, 70);
        assert_eq!(*r0, 0);
    }

    #[test]
    fn is_allocated_false_for_empty_and_dealloced() {
        let mut t: ObjectTable<u64, 4> = ObjectTable::new();

        assert!(!t.is_allocated(0));
        assert!(!t.is_allocated(3));

        let (a, _) = t.alloc(42).unwrap();

        assert!(t.is_allocated(a));
        assert!(!t.is_allocated(1));

        t.dealloc(a);

        assert!(!t.is_allocated(a));
    }
}
