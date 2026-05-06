//! Concurrent wrapper around ObjectTable — per-slot locking and lock-free
//! generation checks for SMP per-object locking.
//!
//! Wraps an ObjectTable with:
//! - `AtomicU64` generation counters (lock-free staleness checks)
//! - Per-slot `TicketLock` (exclusive access to individual objects)
//! - `SpinLock` on allocation state (alloc/dealloc serialization)
//!
//! The wrapped ObjectTable provides storage and the `&mut self` API used
//! during single-threaded boot. The concurrent methods (`alloc_shared`,
//! `dealloc_shared`, `lock_slot`, `get_mut_slot`) enable multi-core access
//! without a global lock.

use alloc::vec::Vec;
use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU64, Ordering},
};

use super::{
    arch::sync::{SpinLock, TicketLock},
    slab::Storage,
};
use crate::table::ObjectTable;

const EMPTY: u32 = u32::MAX;

struct AllocState {
    free_head: u32,
    free_next: Vec<u32>,
    count: usize,
}

/// Concurrent object table — wraps an ObjectTable with per-slot locking and
/// atomic generation counters.
pub struct ConcurrentTable<T, const MAX: usize, S: Storage<T>> {
    storage: UnsafeCell<S>,
    generations: Vec<AtomicU64>,
    alloc_lock: SpinLock<AllocState>,
    slot_locks: Vec<TicketLock>,
    _phantom: core::marker::PhantomData<T>,
}

// SAFETY: ConcurrentTable is safe to share across cores because:
// - `storage` is accessed only under a per-slot TicketLock or during
//   allocation (when no other thread knows about the slot yet).
// - `generations` uses AtomicU64 for lock-free reads.
// - `alloc_lock` serializes allocation/deallocation of slots.
// - `slot_locks` serializes per-object mutation.
unsafe impl<T: Send, const MAX: usize, S: Storage<T>> Send for ConcurrentTable<T, MAX, S> {}
unsafe impl<T: Send, const MAX: usize, S: Storage<T>> Sync for ConcurrentTable<T, MAX, S> {}

/// Guard returned by `lock_slot`. Holds a per-slot TicketLock.
pub struct SlotGuard<'a> {
    lock: &'a TicketLock,
    daif: u64,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.lock.unlock(self.daif);
    }
}

/// Guard providing read-only access to a locked slot. The per-slot lock
/// is held for the lifetime of the guard and released on drop.
pub struct SlotReadGuard<'a, T> {
    _guard: SlotGuard<'a>,
    value: &'a T,
}

impl<T> Deref for SlotReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

/// Guard providing mutable access to a locked slot. The per-slot lock
/// is held for the lifetime of the guard and released on drop.
pub struct SlotWriteGuard<'a, T> {
    _guard: SlotGuard<'a>,
    value: &'a mut T,
}

impl<T> Deref for SlotWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

impl<T> DerefMut for SlotWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value
    }
}

impl<T, const MAX: usize, S: Storage<T>> ConcurrentTable<T, MAX, S> {
    /// Create from an existing ObjectTable. Copies generations into atomics
    /// and takes ownership of the storage. Called once during boot.
    pub fn from_table(table: ObjectTable<T, MAX, S>) -> Self {
        let mut generations = Vec::with_capacity(MAX);

        for g in &table.generations {
            generations.push(AtomicU64::new(*g));
        }

        let mut slot_locks = Vec::with_capacity(MAX);

        for _ in 0..MAX {
            slot_locks.push(TicketLock::new());
        }

        Self {
            storage: UnsafeCell::new(table.storage),
            generations,
            alloc_lock: SpinLock::new(AllocState {
                free_head: table.free_head,
                free_next: table.free_next,
                count: table.count,
            }),
            slot_locks,
            _phantom: core::marker::PhantomData,
        }
    }

    /// Read the generation counter for a slot (lock-free).
    pub fn generation(&self, idx: u32) -> u64 {
        if (idx as usize) < MAX {
            self.generations[idx as usize].load(Ordering::Acquire)
        } else {
            0
        }
    }

    /// Read-only access to an occupied slot.
    ///
    /// # Safety
    ///
    /// The caller must ensure no concurrent mutation of this slot (either
    /// by holding the slot lock or by being the only accessor — e.g.,
    /// immediately after `alloc_shared` before publishing the handle).
    pub unsafe fn get(&self, idx: u32) -> Option<&T> {
        unsafe { (*self.storage.get()).get(idx as usize) }
    }

    /// Acquire the per-slot lock. Returns a guard that releases on drop.
    pub fn lock_slot(&self, idx: u32) -> SlotGuard<'_> {
        let lock = &self.slot_locks[idx as usize];

        SlotGuard {
            lock,
            daif: lock.lock(),
        }
    }

    /// Mutable access under a held slot lock.
    ///
    /// # Safety
    ///
    /// The caller must hold `lock_slot(idx)` for the same index and must
    /// have verified the generation matches the expected value.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get_mut_slot(&self, idx: u32, _guard: &SlotGuard<'_>) -> Option<&mut T> {
        unsafe { (*self.storage.get()).get_mut(idx as usize) }
    }

    /// Allocate a slot (concurrent-safe). Acquires alloc lock internally.
    pub fn alloc_shared(&self, value: T) -> Option<(u32, u64)> {
        let mut alloc = self.alloc_lock.lock();
        let head = alloc.free_head;

        if head == EMPTY {
            return None;
        }

        let i = head as usize;

        alloc.free_head = alloc.free_next[i];
        alloc.free_next[i] = EMPTY;
        alloc.count += 1;

        let generation = self.generations[i].load(Ordering::Relaxed);

        drop(alloc);

        // SAFETY: we just allocated slot `i` — no other thread can access
        // it until we return the (idx, generation) pair.
        unsafe {
            (*self.storage.get()).place(i, value);
        }

        Some((head, generation))
    }

    /// Deallocate a slot (concurrent-safe). Acquires slot lock + alloc lock.
    pub fn dealloc_shared(&self, idx: u32) -> bool {
        let i = idx as usize;

        if i >= MAX {
            return false;
        }

        let _slot_guard = self.lock_slot(idx);
        // SAFETY: we hold the slot lock, ensuring exclusive access.
        let removed = unsafe { (*self.storage.get()).remove(i) };

        if !removed {
            return false;
        }

        self.generations[i].fetch_add(1, Ordering::Release);

        let mut alloc = self.alloc_lock.lock();

        alloc.free_next[i] = alloc.free_head;
        alloc.free_head = idx;
        alloc.count -= 1;

        true
    }

    /// Current number of allocated slots.
    pub fn count(&self) -> usize {
        // Approximate — alloc_lock not held. Safe for diagnostics.
        let guard = self.alloc_lock.lock();

        guard.count
    }

    /// Lock a slot and return a read guard. Returns None if the slot is
    /// empty or out of bounds.
    pub fn read(&self, idx: u32) -> Option<SlotReadGuard<'_, T>> {
        if (idx as usize) >= MAX {
            return None;
        }

        let guard = self.lock_slot(idx);
        // SAFETY: slot lock held, preventing concurrent mutation of this
        // slot. The reference lifetime is tied to the SlotGuard via the
        // returned SlotReadGuard, so it cannot outlive the lock.
        let value = unsafe { (*self.storage.get()).get(idx as usize) }?;

        Some(SlotReadGuard {
            _guard: guard,
            value,
        })
    }

    /// Lock a slot and return a write guard. Returns None if the slot is
    /// empty or out of bounds.
    ///
    /// Multiple `SlotWriteGuard`s for *different* slots may coexist —
    /// per-slot TicketLocks prevent same-slot aliasing, and different
    /// slots are non-overlapping in the storage backend.
    pub fn write(&self, idx: u32) -> Option<SlotWriteGuard<'_, T>> {
        if (idx as usize) >= MAX {
            return None;
        }

        let guard = self.lock_slot(idx);
        // SAFETY: slot lock held — exclusive access to this slot.
        // Different slots have independent TicketLocks and non-overlapping
        // storage, so concurrent SlotWriteGuards for different indices
        // do not alias.
        let value = unsafe { (*self.storage.get()).get_mut(idx as usize) }?;

        Some(SlotWriteGuard {
            _guard: guard,
            value,
        })
    }

    /// Iterate all occupied slots with read access. Each slot is locked
    /// individually during the callback — NOT atomic across slots.
    pub fn for_each<F: FnMut(u32, &T)>(&self, mut f: F) {
        for i in 0..MAX {
            let _guard = self.lock_slot(i as u32);

            // SAFETY: slot lock held for the duration of the callback.
            if let Some(val) = unsafe { (*self.storage.get()).get(i) } {
                f(i as u32, val);
            }
        }
    }

    /// Iterate all occupied slots with write access. Each slot is locked
    /// individually during the callback — NOT atomic across slots.
    pub fn for_each_mut<F: FnMut(u32, &mut T)>(&self, mut f: F) {
        for i in 0..MAX {
            let _guard = self.lock_slot(i as u32);

            // SAFETY: slot lock held — exclusive access to this slot.
            if let Some(val) = unsafe { (*self.storage.get()).get_mut(i) } {
                f(i as u32, val);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::slab::InlineSlab;

    #[test]
    fn alloc_shared_and_lookup() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);
        let (id, generation) = ct.alloc_shared(42).unwrap();

        assert_eq!(unsafe { *ct.get(id).unwrap() }, 42);
        assert_eq!(generation, 0);
        assert_eq!(ct.generation(id), 0);
    }

    #[test]
    fn dealloc_shared_bumps_generation() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);
        let (idx, _) = ct.alloc_shared(1).unwrap();

        assert!(ct.dealloc_shared(idx));
        assert_eq!(ct.generation(idx), 1);
        assert!(unsafe { ct.get(idx) }.is_none());
    }

    #[test]
    fn slot_lock_provides_mut_access() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);
        let (idx, generation) = ct.alloc_shared(10).unwrap();

        {
            let guard = ct.lock_slot(idx);

            assert_eq!(ct.generation(idx), generation);

            let val = unsafe { ct.get_mut_slot(idx, &guard).unwrap() };

            *val = 20;
        }

        assert_eq!(unsafe { *ct.get(idx).unwrap() }, 20);
    }

    #[test]
    fn alloc_shared_reuses_after_dealloc() {
        let table: ObjectTable<u64, 2> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);
        let (a, gen0) = ct.alloc_shared(1).unwrap();

        ct.alloc_shared(2).unwrap();

        assert!(ct.alloc_shared(3).is_none());

        ct.dealloc_shared(a);

        let (c, gen1) = ct.alloc_shared(3).unwrap();

        assert_eq!(c, a);
        assert_eq!(gen0, 0);
        assert_eq!(gen1, 1);
        assert_eq!(unsafe { *ct.get(c).unwrap() }, 3);
    }

    #[test]
    fn dealloc_shared_nonexistent_returns_false() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        assert!(!ct.dealloc_shared(0));
        assert!(!ct.dealloc_shared(99));
    }

    #[test]
    fn generation_out_of_bounds_returns_zero() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        assert_eq!(ct.generation(99), 0);
    }

    #[test]
    fn count_tracks_alloc_dealloc() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        assert_eq!(ct.count(), 0);

        let (a, _) = ct.alloc_shared(1).unwrap();

        assert_eq!(ct.count(), 1);

        ct.alloc_shared(2).unwrap();

        assert_eq!(ct.count(), 2);

        ct.dealloc_shared(a);

        assert_eq!(ct.count(), 1);
    }

    #[test]
    fn slab_backend_concurrent() {
        let table: ObjectTable<u64, 4, InlineSlab<u64>> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);
        let (id, _) = ct.alloc_shared(99).unwrap();

        assert_eq!(unsafe { *ct.get(id).unwrap() }, 99);
        assert!(ct.dealloc_shared(id));
        assert_eq!(ct.generation(id), 1);
    }

    #[test]
    fn from_table_preserves_existing_data() {
        let mut table: ObjectTable<u64, 4> = ObjectTable::new();
        let (a, gen_a) = table.alloc(100).unwrap();
        let (b, gen_b) = table.alloc(200).unwrap();
        let ct = ConcurrentTable::from_table(table);

        assert_eq!(unsafe { *ct.get(a).unwrap() }, 100);
        assert_eq!(unsafe { *ct.get(b).unwrap() }, 200);
        assert_eq!(ct.generation(a), gen_a);
        assert_eq!(ct.generation(b), gen_b);
        assert_eq!(ct.count(), 2);
    }

    // ── Guard API tests ─────────────────────────────────────────

    #[test]
    fn read_guard_provides_access() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);
        let (id, _) = ct.alloc_shared(42).unwrap();
        let guard = ct.read(id).unwrap();

        assert_eq!(*guard, 42);
    }

    #[test]
    fn read_guard_empty_slot_returns_none() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        assert!(ct.read(0).is_none());
    }

    #[test]
    fn read_guard_out_of_bounds_returns_none() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        assert!(ct.read(99).is_none());
    }

    #[test]
    fn write_guard_provides_mut_access() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);
        let (id, _) = ct.alloc_shared(10).unwrap();

        {
            let mut guard = ct.write(id).unwrap();

            *guard = 20;
        }

        let guard = ct.read(id).unwrap();

        assert_eq!(*guard, 20);
    }

    #[test]
    fn write_guard_empty_returns_none() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        assert!(ct.write(0).is_none());
    }

    #[test]
    fn for_each_visits_all_occupied() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        ct.alloc_shared(10).unwrap();
        ct.alloc_shared(20).unwrap();
        ct.alloc_shared(30).unwrap();

        let mut sum = 0u64;

        ct.for_each(|_, val| sum += *val);

        assert_eq!(sum, 60);
    }

    #[test]
    fn for_each_mut_modifies_all() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        ct.alloc_shared(1).unwrap();
        ct.alloc_shared(2).unwrap();
        ct.for_each_mut(|_, val| *val *= 10);

        let mut sum = 0u64;

        ct.for_each(|_, val| sum += *val);

        assert_eq!(sum, 30);
    }

    #[test]
    fn for_each_skips_empty_and_dealloced() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);
        let (a, _) = ct.alloc_shared(10).unwrap();

        ct.alloc_shared(20).unwrap();

        let (c, _) = ct.alloc_shared(30).unwrap();

        ct.dealloc_shared(a);

        let mut items = alloc::vec::Vec::new();

        ct.for_each(|id, val| items.push((id, *val)));

        assert_eq!(items.len(), 2);
        assert!(items.contains(&(1, 20)));
        assert!(items.contains(&(c, 30)));
    }

    #[test]
    fn two_read_guards_different_slots() {
        let table: ObjectTable<u64, 4> = ObjectTable::new();
        let ct = ConcurrentTable::from_table(table);

        ct.alloc_shared(10).unwrap();
        ct.alloc_shared(20).unwrap();

        let g0 = ct.read(0).unwrap();
        let g1 = ct.read(1).unwrap();

        assert_eq!(*g0, 10);
        assert_eq!(*g1, 20);
    }
}
