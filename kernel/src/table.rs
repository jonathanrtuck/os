//! Flat-array object table — O(1) alloc/dealloc for kernel objects.
//!
//! Each kernel object type (VMO, Event, Endpoint, Thread, AddressSpace)
//! is stored in an ObjectTable. Objects are accessed by ID (array index).
//! Storage is heap-backed (Vec) so composite tables don't overflow the stack.

use alloc::{vec, vec::Vec};

pub struct ObjectTable<T, const MAX: usize> {
    entries: Vec<Option<T>>,
    generations: Vec<u64>,
    free_hint: usize,
    count: usize,
}

#[allow(clippy::new_without_default)]
impl<T, const MAX: usize> ObjectTable<T, MAX> {
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(MAX);
        entries.resize_with(MAX, || None);

        let generations = vec![0u64; MAX];

        Self {
            entries,
            generations,
            free_hint: 0,
            count: 0,
        }
    }

    pub fn alloc(&mut self, value: T) -> Option<(u32, u64)> {
        for offset in 0..MAX {
            let idx = (self.free_hint + offset) % MAX;

            if self.entries[idx].is_none() {
                self.entries[idx] = Some(value);
                self.free_hint = (idx + 1) % MAX;
                self.count += 1;
                let generation = self.generations[idx];

                return Some((idx as u32, generation));
            }
        }

        None
    }

    pub fn dealloc(&mut self, idx: u32) -> Option<T> {
        let i = idx as usize;

        if i >= MAX {
            return None;
        }

        let value = self.entries[i].take()?;
        self.generations[i] += 1;
        self.count -= 1;

        if i < self.free_hint {
            self.free_hint = i;
        }

        Some(value)
    }

    /// Lookup by index. O(1).
    pub fn get(&self, idx: u32) -> Option<&T> {
        self.entries.get(idx as usize)?.as_ref()
    }

    /// Mutable lookup by index. O(1).
    pub fn get_mut(&mut self, idx: u32) -> Option<&mut T> {
        self.entries.get_mut(idx as usize)?.as_mut()
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn generation(&self, idx: u32) -> u64 {
        self.generations
            .get(idx as usize)
            .copied()
            .unwrap_or(0)
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

            Some((left[mi].as_mut()?, right[0].as_ref()?))
        } else {
            let (left, right) = self.entries.split_at_mut(mi);

            Some((right[0].as_mut()?, left[ri].as_ref()?))
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
}
