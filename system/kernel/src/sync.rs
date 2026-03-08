//! Single-core synchronization primitive.
//!
//! `IrqMutex` masks IRQs on lock to prevent interrupt-time reentry on a
//! single-core system. The guard restores the previous IRQ state on drop,
//! so nested locks work correctly (inner drop restores to "masked", outer
//! drop restores to the original state).
//!
//! Multi-core: replace with a spinlock + IRQ masking combination.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};

pub struct IrqGuard<'a, T> {
    data: &'a UnsafeCell<T>,
    saved_daif: u64,
}
pub struct IrqMutex<T> {
    data: UnsafeCell<T>,
}

impl<T> IrqMutex<T> {
    pub const fn new(val: T) -> Self {
        Self {
            data: UnsafeCell::new(val),
        }
    }

    pub fn lock(&self) -> IrqGuard<'_, T> {
        let saved_daif: u64;

        unsafe {
            core::arch::asm!("mrs {}, daif", out(reg) saved_daif, options(nostack, nomem));
            core::arch::asm!("msr daifset, #2", options(nostack, nomem));
        }

        IrqGuard {
            data: &self.data,
            saved_daif,
        }
    }
}
// SAFETY: Single-core kernel. IrqMutex masks IRQs on lock, preventing
// interrupt-time reentry. Only one execution context can hold the guard.
unsafe impl<T> Sync for IrqMutex<T> {}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        unsafe {
            core::arch::asm!("msr daif, {}", in(reg) self.saved_daif, options(nostack, nomem));
        }
    }
}
impl<T> Deref for IrqGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: Guard existence guarantees exclusive access (IRQs masked,
        // single-core, so no other context can run).
        unsafe { &*self.data.get() }
    }
}
impl<T> DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: Same as Deref — exclusive access guaranteed by IRQ masking.
        unsafe { &mut *self.data.get() }
    }
}
