//! Slab storage backends for ObjectTable.
//!
//! Two implementations of the `Storage` trait:
//!
//! - `BoxStorage<T>`: heap-allocates each object individually via Box.
//!   Low init cost (8 bytes per slot), but every alloc/dealloc hits the
//!   global allocator. Best for large types (AddressSpace ~67 KB).
//!
//! - `InlineSlab<T>`: pre-allocates contiguous storage for all slots at
//!   init. Alloc/dealloc are just MaybeUninit::write / assume_init_drop —
//!   zero allocator traffic. Best for types where MAX × size_of::<T>()
//!   fits comfortably in the heap budget.

use alloc::{boxed::Box, vec, vec::Vec};
use core::mem::MaybeUninit;

pub trait Storage<T> {
    fn new(capacity: usize) -> Self;
    fn place(&mut self, idx: usize, value: T);
    fn remove(&mut self, idx: usize) -> bool;
    fn get(&self, idx: usize) -> Option<&T>;
    fn get_mut(&mut self, idx: usize) -> Option<&mut T>;
    fn get_pair_mut(&mut self, mut_idx: usize, ref_idx: usize, max: usize) -> Option<(&mut T, &T)>;
    fn iter_occupied<'a>(&'a self) -> impl Iterator<Item = (usize, &'a T)>
    where
        T: 'a;
    fn iter_occupied_mut<'a>(&'a mut self) -> impl Iterator<Item = (usize, &'a mut T)>
    where
        T: 'a;
}

// ── BoxStorage ───────────────────────────────────────────────────────

pub struct BoxStorage<T> {
    entries: Vec<Option<Box<T>>>,
}

impl<T> Storage<T> for BoxStorage<T> {
    fn new(capacity: usize) -> Self {
        let mut entries: Vec<Option<Box<T>>> = Vec::with_capacity(capacity);

        entries.resize_with(capacity, || None);

        Self { entries }
    }

    fn place(&mut self, idx: usize, value: T) {
        self.entries[idx] = Some(Box::new(value));
    }

    fn remove(&mut self, idx: usize) -> bool {
        self.entries[idx].take().is_some()
    }

    fn get(&self, idx: usize) -> Option<&T> {
        self.entries.get(idx)?.as_deref()
    }

    fn get_mut(&mut self, idx: usize) -> Option<&mut T> {
        self.entries.get_mut(idx)?.as_deref_mut()
    }

    fn get_pair_mut(&mut self, mut_idx: usize, ref_idx: usize, max: usize) -> Option<(&mut T, &T)> {
        if mut_idx >= max || ref_idx >= max {
            return None;
        }

        if mut_idx < ref_idx {
            let (left, right) = self.entries.split_at_mut(ref_idx);

            Some((left[mut_idx].as_deref_mut()?, right[0].as_deref()?))
        } else {
            let (left, right) = self.entries.split_at_mut(mut_idx);

            Some((right[0].as_deref_mut()?, left[ref_idx].as_deref()?))
        }
    }

    fn iter_occupied<'a>(&'a self) -> impl Iterator<Item = (usize, &'a T)>
    where
        T: 'a,
    {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_deref().map(|v| (i, v)))
    }

    fn iter_occupied_mut<'a>(&'a mut self) -> impl Iterator<Item = (usize, &'a mut T)>
    where
        T: 'a,
    {
        self.entries
            .iter_mut()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_deref_mut().map(|v| (i, v)))
    }
}

// ── InlineSlab ───────────────────────────────────────────────────────

pub struct InlineSlab<T> {
    slots: Vec<MaybeUninit<T>>,
    occupied: Vec<bool>,
}

impl<T> Storage<T> for InlineSlab<T> {
    fn new(capacity: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);

        // SAFETY: MaybeUninit<T> is valid when uninitialized — that's its
        // entire purpose. We set len to capacity so the Vec owns the memory
        // but no T values exist yet. The `occupied` vec tracks which slots
        // contain initialized values.
        unsafe {
            slots.set_len(capacity);
        }

        Self {
            slots,
            occupied: vec![false; capacity],
        }
    }

    fn place(&mut self, idx: usize, value: T) {
        debug_assert!(!self.occupied[idx], "double-place at {idx}");

        self.slots[idx] = MaybeUninit::new(value);
        self.occupied[idx] = true;
    }

    fn remove(&mut self, idx: usize) -> bool {
        if !self.occupied[idx] {
            return false;
        }

        // SAFETY: slot is occupied (checked above), so the value is
        // initialized. After drop, we mark it unoccupied.
        unsafe {
            self.slots[idx].assume_init_drop();
        }

        self.occupied[idx] = false;

        true
    }

    fn get(&self, idx: usize) -> Option<&T> {
        if idx < self.slots.len() && self.occupied[idx] {
            // SAFETY: slot is occupied (checked above), so the value
            // is initialized.
            Some(unsafe { self.slots[idx].assume_init_ref() })
        } else {
            None
        }
    }

    fn get_mut(&mut self, idx: usize) -> Option<&mut T> {
        if idx < self.slots.len() && self.occupied[idx] {
            // SAFETY: slot is occupied (checked above), so the value
            // is initialized.
            Some(unsafe { self.slots[idx].assume_init_mut() })
        } else {
            None
        }
    }

    fn get_pair_mut(&mut self, mut_idx: usize, ref_idx: usize, max: usize) -> Option<(&mut T, &T)> {
        if mut_idx >= max || ref_idx >= max {
            return None;
        }
        if !self.occupied[mut_idx] || !self.occupied[ref_idx] {
            return None;
        }

        // SAFETY: both slots are occupied (checked above), so values
        // are initialized. split_at_mut guarantees no aliasing.
        if mut_idx < ref_idx {
            let (left, right) = self.slots.split_at_mut(ref_idx);

            unsafe { Some((left[mut_idx].assume_init_mut(), right[0].assume_init_ref())) }
        } else {
            let (left, right) = self.slots.split_at_mut(mut_idx);

            unsafe { Some((right[0].assume_init_mut(), left[ref_idx].assume_init_ref())) }
        }
    }

    fn iter_occupied<'a>(&'a self) -> impl Iterator<Item = (usize, &'a T)>
    where
        T: 'a,
    {
        self.slots
            .iter()
            .zip(self.occupied.iter())
            .enumerate()
            .filter_map(|(i, (slot, &occ))| {
                if occ {
                    // SAFETY: slot is occupied, so the value is initialized.
                    Some((i, unsafe { slot.assume_init_ref() }))
                } else {
                    None
                }
            })
    }

    fn iter_occupied_mut<'a>(&'a mut self) -> impl Iterator<Item = (usize, &'a mut T)>
    where
        T: 'a,
    {
        self.slots
            .iter_mut()
            .zip(self.occupied.iter())
            .enumerate()
            .filter_map(|(i, (slot, &occ))| {
                if occ {
                    // SAFETY: slot is occupied, so the value is initialized.
                    Some((i, unsafe { slot.assume_init_mut() }))
                } else {
                    None
                }
            })
    }
}

impl<T> Drop for InlineSlab<T> {
    fn drop(&mut self) {
        for (slot, &occ) in self.slots.iter_mut().zip(self.occupied.iter()) {
            if occ {
                // SAFETY: slot is occupied, so the value is initialized.
                // We must drop all live values before the backing memory
                // is freed by Vec's own Drop.
                unsafe {
                    slot.assume_init_drop();
                }
            }
        }
    }
}
