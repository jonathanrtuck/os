//! Fixed-capacity ring buffer with zero initialization cost.
//!
//! Uses `MaybeUninit` for slot storage — only occupied slots contain
//! initialized values. Validity is tracked by `head` and `len`, not
//! by `Option` discriminants. This avoids touching N × size_of::<T>()
//! bytes on construction.

use core::mem::MaybeUninit;

pub struct FixedRing<T, const N: usize> {
    slots: [MaybeUninit<T>; N],
    head: u8,
    len: u8,
}

impl<T, const N: usize> Default for FixedRing<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> FixedRing<T, N> {
    pub const fn new() -> Self {
        FixedRing {
            slots: [const { MaybeUninit::uninit() }; N],
            head: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, item: T) -> bool {
        if self.len as usize >= N {
            return false;
        }

        let tail = (self.head as usize + self.len as usize) % N;

        self.slots[tail].write(item);
        self.len += 1;

        true
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }

        // SAFETY: slots at head..head+len (modular) are initialized by push().
        let item = unsafe { self.slots[self.head as usize].assume_init_read() };

        self.head = ((self.head as usize + 1) % N) as u8;
        self.len -= 1;

        Some(item)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn iter(&self) -> FixedRingIter<'_, T, N> {
        FixedRingIter { ring: self, pos: 0 }
    }
}

impl<T, const N: usize> Drop for FixedRing<T, N> {
    fn drop(&mut self) {
        for i in 0..self.len as usize {
            let idx = (self.head as usize + i) % N;

            // SAFETY: slots at head..head+len (modular) are initialized.
            unsafe { self.slots[idx].assume_init_drop() };
        }
    }
}

pub struct FixedRingIter<'a, T, const N: usize> {
    ring: &'a FixedRing<T, N>,
    pos: usize,
}

impl<'a, T, const N: usize> Iterator for FixedRingIter<'a, T, N> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.ring.len as usize {
            return None;
        }

        let idx = (self.ring.head as usize + self.pos) % N;

        self.pos += 1;

        // SAFETY: slots at head..head+len (modular) are initialized.
        Some(unsafe { self.ring.slots[idx].assume_init_ref() })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.ring.len as usize - self.pos;

        (remaining, Some(remaining))
    }
}

impl<T, const N: usize> ExactSizeIterator for FixedRingIter<'_, T, N> {}

impl<'a, T, const N: usize> IntoIterator for &'a FixedRing<T, N> {
    type Item = &'a T;
    type IntoIter = FixedRingIter<'a, T, N>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
