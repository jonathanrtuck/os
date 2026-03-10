//! Ticket-spinlock mutex with IRQ masking.
//!
//! `IrqMutex` masks IRQs and acquires a ticket spinlock on lock. The guard
//! releases the spinlock and restores the previous IRQ state on drop.
//! Correct on both single-core and multi-core.
//!
//! Lock ordering invariant: channel → scheduler (never reversed). No lock
//! may be re-acquired while held (ticket spinlock would deadlock on self).

use super::metrics;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

pub struct IrqGuard<'a, T> {
    lock: &'a IrqMutex<T>,
    saved_daif: u64,
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

        unsafe {
            core::arch::asm!("msr daif, {}", in(reg) self.saved_daif, options(nostack, nomem));
        }
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
    pub fn lock(&self) -> IrqGuard<'_, T> {
        let saved_daif: u64;

        // Save and mask IRQs before taking a ticket. Prevents timer
        // interrupts from re-entering the locked region on this core,
        // and avoids spinning with IRQs enabled (priority inversion).
        unsafe {
            core::arch::asm!("mrs {}, daif", out(reg) saved_daif, options(nostack, nomem));
            core::arch::asm!("msr daifset, #2", options(nostack, nomem));
        }

        let my_ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);

        while self.now_serving.load(Ordering::Acquire) != my_ticket {
            metrics::inc_lock_spins();
            core::hint::spin_loop();
        }

        IrqGuard {
            lock: self,
            saved_daif,
        }
    }
    pub const fn new(val: T) -> Self {
        Self {
            next_ticket: AtomicU32::new(0),
            now_serving: AtomicU32::new(0),
            data: UnsafeCell::new(val),
        }
    }
}
// SAFETY: IrqMutex provides mutual exclusion via ticket spinlock (multi-core
// safe) with IRQ masking (prevents interrupt-time reentry). Only one execution
// context can hold the guard at a time.
unsafe impl<T> Sync for IrqMutex<T> {}
