//! Ticket-spinlock mutex with IRQ masking.
//!
//! `IrqMutex` masks IRQs and acquires a ticket spinlock on lock. The guard
//! releases the spinlock and restores the previous IRQ state on drop.
//! Correct on both single-core and multi-core.
//!
//! Lock ordering invariant: channel → scheduler (never reversed). No lock
//! may be re-acquired while held (ticket spinlock would deadlock on self).
//!
// AUDIT: 2026-03-11 — 5 unsafe sites verified (2 asm in lock/drop, 2
// UnsafeCell derefs in Deref/DerefMut, 1 unsafe impl Sync). 6-category
// checklist applied. Ticket spinlock uses correct Relaxed/Acquire/Release
// ordering. DAIF asm correctly omits `nomem` (Fix 6 re-verified). No bugs
// found.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU32, Ordering},
};

use super::metrics;

// ---------------------------------------------------------------------------
// IrqMutex — the lock
// ---------------------------------------------------------------------------

pub struct IrqMutex<T> {
    next_ticket: AtomicU32,
    now_serving: AtomicU32,
    data: UnsafeCell<T>,
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
        let saved_daif: u64;

        // Save and mask IRQs before taking a ticket. Prevents timer
        // interrupts from re-entering the locked region on this core,
        // and avoids spinning with IRQs enabled (priority inversion).
        // IMPORTANT: no `nomem` — LLVM must treat these as memory barriers.
        // With `nomem`, LLVM can reorder memory operations past the DAIF
        // masking, allowing lock-protected accesses to execute with interrupts
        // enabled (race condition that manifests at opt-level 3).
        //
        // SAFETY: Reading and writing DAIF is valid at EL1. `nostack` is
        // correct (no stack manipulation). No `nomem` — intentional, see above
        // and Fix 6 analysis.
        unsafe {
            core::arch::asm!("mrs {}, daif", out(reg) saved_daif, options(nostack));
            core::arch::asm!("msr daifset, #2", options(nostack));
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
}

// SAFETY: IrqMutex provides mutual exclusion via ticket spinlock (multi-core
// safe) with IRQ masking (prevents interrupt-time reentry). Only one execution
// context can hold the guard at a time.
unsafe impl<T> Sync for IrqMutex<T> {}

// ---------------------------------------------------------------------------
// IrqGuard — RAII lock guard
// ---------------------------------------------------------------------------

pub struct IrqGuard<'a, T> {
    lock: &'a IrqMutex<T>,
    saved_daif: u64,
}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        // Release the spinlock, then restore IRQ state.
        self.lock.now_serving.fetch_add(1, Ordering::Release);

        // SAFETY: Restoring DAIF to a value previously read from this register
        // is always valid at EL1. `nostack` is correct (no stack manipulation).
        // No `nomem` — the compiler must not reorder memory accesses past this
        // IRQ state restoration (Fix 6).
        unsafe {
            core::arch::asm!("msr daif, {}", in(reg) self.saved_daif, options(nostack));
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
