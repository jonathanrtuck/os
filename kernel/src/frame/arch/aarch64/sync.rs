//! Synchronization primitives — LSE ticket locks for M4 Pro.
//!
//! Ticket lock: LDADDAL for acquire (~5 cycles), STLR for release (~5 cycles).
//! Each lock is 128-byte aligned to own its M4 Pro cache line.
//! IRQs are disabled while any lock is held (prevents IRQ handler deadlock).
//!
//! Spin wait uses WFE (bare-metal) instead of ISB: puts the core into
//! low-power standby until SEV wakes it on unlock. SEVL primes the event
//! register so the first WFE falls through without stalling.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU32, Ordering},
};

/// DAIF save and disable IRQs.
#[cfg(target_os = "none")]
#[inline(always)]
fn daif_save_and_disable() -> u64 {
    let daif: u64;

    // SAFETY: MRS reads DAIF (no side effects). MSR DAIFSet disables IRQs.
    // Not nomem: MSR writes a system register.
    unsafe {
        core::arch::asm!(
            "mrs {daif}, daif",
            "msr daifset, #2",
            daif = out(reg) daif,
            options(nostack),
        );
    }

    daif
}

/// Restore DAIF state.
#[cfg(target_os = "none")]
#[inline(always)]
fn daif_restore(daif: u64) {
    // SAFETY: MSR writes DAIF. Restoring a previously-read value is safe.
    unsafe {
        core::arch::asm!(
            "msr daif, {daif}",
            daif = in(reg) daif,
            options(nostack),
        );
    }
}

// Host-side stubs for test builds (DAIF is EL1-only).
#[cfg(not(target_os = "none"))]
#[inline(always)]
fn daif_save_and_disable() -> u64 {
    0
}

#[cfg(not(target_os = "none"))]
#[inline(always)]
fn daif_restore(_daif: u64) {}

/// LSE ticket lock. 128-byte aligned (one M4 Pro cache line).
///
/// Acquire: LDADDAL on next_ticket (single LSE instruction, ~5 cycles).
/// Spin: SEVL + WFE loop (bare-metal) or spin_loop hint (host tests).
/// Release: store-release to now_serving + SEV to wake waiting cores.
#[repr(C, align(128))]
pub struct TicketLock {
    now_serving: AtomicU32,
    next_ticket: AtomicU32,
    _pad: [u8; 120],
}

#[allow(clippy::new_without_default)]
impl TicketLock {
    pub const fn new() -> Self {
        Self {
            now_serving: AtomicU32::new(0),
            next_ticket: AtomicU32::new(0),
            _pad: [0; 120],
        }
    }

    pub fn lock(&self) -> u64 {
        let daif = daif_save_and_disable();
        let ticket = self.next_ticket.fetch_add(1, Ordering::AcqRel);

        self.spin_until_serving(ticket);

        daif
    }

    #[cfg(target_os = "none")]
    #[inline(always)]
    fn spin_until_serving(&self, ticket: u32) {
        // SAFETY: SEVL primes the event register so the first WFE falls
        // through. WFE then sleeps until SEV (from unlock) or a store to
        // a monitored cache line wakes the core. LDAPR (RCPC) is a weaker
        // load-acquire sufficient for ticket lock ordering.
        // Not nomem: WFE/SEVL interact with the event register (architectural
        // state), and the load reads memory.
        unsafe {
            core::arch::asm!(
                "sevl",
                "2:",
                "wfe",
                "ldapr {serving:w}, [{addr}]",
                "cmp {serving:w}, {ticket:w}",
                "b.ne 2b",
                addr = in(reg) self.now_serving.as_ptr(),
                ticket = in(reg) ticket,
                serving = out(reg) _,
                options(nostack),
            );
        }
    }

    #[cfg(not(target_os = "none"))]
    #[inline(always)]
    fn spin_until_serving(&self, ticket: u32) {
        while self.now_serving.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }
    }

    pub fn unlock(&self, daif: u64) {
        self.now_serving.fetch_add(1, Ordering::Release);

        // Wake cores waiting in WFE spin loops.
        #[cfg(target_os = "none")]
        {
            // SAFETY: SEV is a hint with no side effects other than setting
            // the event register on all cores. No memory access.
            unsafe {
                core::arch::asm!("sev", options(nostack, nomem));
            }
        }

        daif_restore(daif);
    }
}

/// SpinLock<T> — safe wrapper around TicketLock + UnsafeCell<T>.
pub struct SpinLock<T> {
    lock: TicketLock,
    data: UnsafeCell<T>,
}

// SAFETY: SpinLock serializes all access to T. If T: Send, SpinLock<T>
// can be shared across cores.
unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            lock: TicketLock::new(),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinGuard<'_, T> {
        let daif = self.lock.lock();

        SpinGuard {
            lock: &self.lock,
            data: self.data.get(),
            daif,
        }
    }
}

pub struct SpinGuard<'a, T> {
    lock: &'a TicketLock,
    data: *mut T,
    daif: u64,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: SpinGuard existence proves the lock is held.
        unsafe { &*self.data }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: SpinGuard existence proves exclusive access.
        unsafe { &mut *self.data }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.unlock(self.daif);
    }
}

/// RawTicketLock — implements lock_api::RawMutex for talc integration.
pub struct RawTicketLock {
    inner: TicketLock,
    saved_daif: UnsafeCell<u64>,
}

// SAFETY: RawTicketLock serializes all access through its TicketLock.
unsafe impl Send for RawTicketLock {}
unsafe impl Sync for RawTicketLock {}

#[allow(clippy::new_without_default)]
impl RawTicketLock {
    pub const fn new() -> Self {
        Self {
            inner: TicketLock::new(),
            saved_daif: UnsafeCell::new(0),
        }
    }
}

// lock_api::RawMutex implementation for talc's TalcLock<R>.
unsafe impl lock_api::RawMutex for RawTicketLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardNoSend;

    fn lock(&self) {
        let daif = self.inner.lock();

        // SAFETY: We hold the lock, so no concurrent access to saved_daif.
        unsafe { *self.saved_daif.get() = daif };
    }

    fn try_lock(&self) -> bool {
        false // Ticket locks don't support try_lock.
    }

    unsafe fn unlock(&self) {
        // SAFETY: Caller guarantees the lock is held.
        let daif = unsafe { *self.saved_daif.get() };

        self.inner.unlock(daif);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_lock_alignment() {
        assert_eq!(core::mem::align_of::<TicketLock>(), 128);
    }

    #[test]
    fn ticket_lock_basic() {
        let lock = TicketLock::new();
        let daif = lock.lock();

        lock.unlock(daif);
    }

    #[test]
    fn spinlock_guard_provides_mut() {
        let lock = SpinLock::new(42u64);

        {
            let mut guard = lock.lock();

            assert_eq!(*guard, 42);

            *guard = 99;
        }
        {
            let guard = lock.lock();

            assert_eq!(*guard, 99);
        }
    }

    #[test]
    fn spinlock_drop_releases() {
        let lock = SpinLock::new(0u32);

        {
            let _g = lock.lock();
        }

        // If drop didn't unlock, this would deadlock.
        let _g = lock.lock();
    }
}
