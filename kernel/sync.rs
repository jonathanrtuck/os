// AUDIT: 2026-04-01 — 3 unsafe sites verified (2 UnsafeCell derefs in
// Deref/DerefMut, 1 unsafe impl Sync). Arch-specific DAIF masking moved to
// arch::interrupts (IrqState). Ticket spinlock uses correct
// Relaxed/Acquire/Release ordering. No bugs found.

//! Ticket-spinlock mutex with IRQ masking.
//!
//! `IrqMutex` masks IRQs and acquires a ticket spinlock on lock. The guard
//! releases the spinlock and restores the previous IRQ state on drop.
//! Correct on both single-core and multi-core.
//!
//! Lock ordering invariant: channel → scheduler (never reversed). No lock
//! may be re-acquired while held (ticket spinlock would deadlock on self).
//!
//! IRQ masking is delegated to `arch::interrupts` — this module contains
//! no architecture-specific code.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU32, Ordering},
};

use super::{arch::interrupts, metrics};

pub struct IrqGuard<'a, T> {
    lock: &'a IrqMutex<T>,
    saved: interrupts::IrqState,
}
pub struct IrqMutex<T> {
    next_ticket: AtomicU32,
    now_serving: AtomicU32,
    data: UnsafeCell<T>,
}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        // Release the spinlock, then restore IRQ state.
        self.lock.now_serving.fetch_add(1, Ordering::Release);

        // Restore saved IRQ state. The arch implementation ensures the
        // compiler cannot reorder memory accesses past this restoration.
        interrupts::restore(self.saved);
    }
}
impl<T> Deref for IrqGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: Guard existence guarantees exclusive access (ticket spinlock
        // + IRQ masking).
        unsafe { &*self.lock.data.get() }
    }
}
impl<T> DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: Same as Deref — exclusive access guaranteed by ticket
        // spinlock + IRQ masking.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> IrqMutex<T> {
    pub const fn new(val: T) -> Self {
        Self {
            next_ticket: AtomicU32::new(0),
            now_serving: AtomicU32::new(0),
            data: UnsafeCell::new(val),
        }
    }

    pub fn lock(&self) -> IrqGuard<'_, T> {
        // Save and mask IRQs before taking a ticket. Prevents timer
        // interrupts from re-entering the locked region on this core,
        // and avoids spinning with IRQs enabled (priority inversion).
        let saved = interrupts::mask_all();
        let my_ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);

        while self.now_serving.load(Ordering::Acquire) != my_ticket {
            metrics::inc_lock_spins();
            core::hint::spin_loop();
        }

        IrqGuard { lock: self, saved }
    }
}
// SAFETY: IrqMutex provides mutual exclusion via ticket spinlock (multi-core
// safe) with IRQ masking (prevents interrupt-time reentry). Only one execution
// context can hold the guard at a time.
unsafe impl<T> Sync for IrqMutex<T> {}
