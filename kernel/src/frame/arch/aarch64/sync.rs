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

// ---------------------------------------------------------------------------
// Reader-writer spinlock — multiple concurrent readers, exclusive writer.
// ---------------------------------------------------------------------------

const WRITER_BIT: u32 = 1 << 31;

/// Reader-writer spinlock. 128-byte aligned (one M4 Pro cache line).
///
/// State encoding: bit 31 = writer flag, bits 0–30 = reader count.
///
/// - Readers: increment reader count if no writer. Concurrent readers proceed.
/// - Writer: set writer bit (blocks new readers), then spin until readers drain.
/// - IRQs disabled while any mode is held.
#[repr(C, align(128))]
pub struct RawRwSpinLock {
    state: AtomicU32,
    _pad: [u8; 124],
}

#[allow(clippy::new_without_default)]
impl RawRwSpinLock {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(0),
            _pad: [0; 124],
        }
    }

    pub fn read_lock(&self) -> u64 {
        let daif = daif_save_and_disable();

        loop {
            let old = self.state.load(Ordering::Relaxed);

            if old & WRITER_BIT != 0 {
                Self::spin_wait();

                continue;
            }

            if self
                .state
                .compare_exchange_weak(old, old + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }

        daif
    }

    pub fn read_unlock(&self, daif: u64) {
        let prev = self.state.fetch_sub(1, Ordering::Release);

        // If we were the last reader and a writer is waiting, wake it.
        if prev == WRITER_BIT | 1 {
            Self::wake_waiters();
        }

        daif_restore(daif);
    }

    pub fn write_lock(&self) -> u64 {
        let daif = daif_save_and_disable();

        // Set the writer bit. This blocks new readers from entering.
        loop {
            let old = self.state.load(Ordering::Relaxed);

            if old & WRITER_BIT != 0 {
                Self::spin_wait();

                continue;
            }

            if self
                .state
                .compare_exchange_weak(old, old | WRITER_BIT, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }

        // Spin until all readers have released.
        while self.state.load(Ordering::Acquire) != WRITER_BIT {
            Self::spin_wait();
        }

        daif
    }

    pub fn write_unlock(&self, daif: u64) {
        self.state.store(0, Ordering::Release);
        Self::wake_waiters();
        daif_restore(daif);
    }

    #[cfg(target_os = "none")]
    #[inline(always)]
    fn spin_wait() {
        // SAFETY: WFE puts the core in low-power standby until an event
        // (SEV from unlock or cache-line update) wakes it.
        unsafe {
            core::arch::asm!("wfe", options(nostack, nomem));
        }
    }

    #[cfg(not(target_os = "none"))]
    #[inline(always)]
    fn spin_wait() {
        core::hint::spin_loop();
    }

    #[cfg(target_os = "none")]
    #[inline(always)]
    fn wake_waiters() {
        // SAFETY: SEV is a hint with no side effects other than setting
        // the event register on all cores.
        unsafe {
            core::arch::asm!("sev", options(nostack, nomem));
        }
    }

    #[cfg(not(target_os = "none"))]
    #[inline(always)]
    fn wake_waiters() {}
}

/// Safe reader-writer spinlock wrapper.
pub struct RwSpinLock<T> {
    lock: RawRwSpinLock,
    data: UnsafeCell<T>,
}

// SAFETY: RwSpinLock serializes all access to T. If T: Send, RwSpinLock<T>
// can be shared across cores.
unsafe impl<T: Send> Send for RwSpinLock<T> {}
unsafe impl<T: Send> Sync for RwSpinLock<T> {}

impl<T> RwSpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            lock: RawRwSpinLock::new(),
            data: UnsafeCell::new(data),
        }
    }

    pub fn read(&self) -> RwReadGuard<'_, T> {
        let daif = self.lock.read_lock();

        RwReadGuard {
            lock: &self.lock,
            data: self.data.get(),
            daif,
        }
    }

    pub fn write(&self) -> RwWriteGuard<'_, T> {
        let daif = self.lock.write_lock();

        RwWriteGuard {
            lock: &self.lock,
            data: self.data.get(),
            daif,
        }
    }
}

pub struct RwReadGuard<'a, T> {
    lock: &'a RawRwSpinLock,
    data: *const T,
    daif: u64,
}

impl<T> Deref for RwReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: RwReadGuard existence proves the read lock is held.
        // Multiple readers may coexist — we only provide &T.
        unsafe { &*self.data }
    }
}

impl<T> Drop for RwReadGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.read_unlock(self.daif);
    }
}

pub struct RwWriteGuard<'a, T> {
    lock: &'a RawRwSpinLock,
    data: *mut T,
    daif: u64,
}

impl<T> Deref for RwWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: RwWriteGuard existence proves exclusive access.
        unsafe { &*self.data }
    }
}

impl<T> DerefMut for RwWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: RwWriteGuard existence proves exclusive access.
        unsafe { &mut *self.data }
    }
}

impl<T> Drop for RwWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.write_unlock(self.daif);
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

    // -- RwSpinLock tests --

    #[test]
    fn rw_spinlock_alignment() {
        assert_eq!(core::mem::align_of::<RawRwSpinLock>(), 128);
        assert_eq!(core::mem::size_of::<RawRwSpinLock>(), 128);
    }

    #[test]
    fn rw_read_provides_shared_access() {
        let lock = RwSpinLock::new(42u64);
        let guard = lock.read();

        assert_eq!(*guard, 42);
    }

    #[test]
    fn rw_write_provides_mut_access() {
        let lock = RwSpinLock::new(42u64);

        {
            let mut guard = lock.write();

            *guard = 99;
        }

        let guard = lock.read();

        assert_eq!(*guard, 99);
    }

    #[test]
    fn rw_multiple_readers() {
        let lock = RwSpinLock::new(42u64);

        // Multiple read guards coexist.
        let r1 = lock.read();
        let r2 = lock.read();
        let r3 = lock.read();

        assert_eq!(*r1, 42);
        assert_eq!(*r2, 42);
        assert_eq!(*r3, 42);
    }

    #[test]
    fn rw_write_after_read_releases() {
        let lock = RwSpinLock::new(0u32);

        {
            let _r = lock.read();
        }

        // If read didn't release, this would deadlock.
        let mut w = lock.write();

        *w = 1;
    }

    #[test]
    fn rw_read_after_write_releases() {
        let lock = RwSpinLock::new(0u32);

        {
            let mut w = lock.write();

            *w = 7;
        }

        // If write didn't release, this would deadlock.
        let r = lock.read();

        assert_eq!(*r, 7);
    }

    #[test]
    fn rw_state_transitions() {
        let raw = RawRwSpinLock::new();

        assert_eq!(raw.state.load(Ordering::Relaxed), 0);

        // Read lock: state should be 1 (one reader).
        let daif1 = raw.read_lock();

        assert_eq!(raw.state.load(Ordering::Relaxed), 1);

        // Second reader: state should be 2.
        let daif2 = raw.read_lock();

        assert_eq!(raw.state.load(Ordering::Relaxed), 2);

        // Release one reader: state back to 1.
        raw.read_unlock(daif2);

        assert_eq!(raw.state.load(Ordering::Relaxed), 1);

        // Release last reader: state back to 0.
        raw.read_unlock(daif1);

        assert_eq!(raw.state.load(Ordering::Relaxed), 0);

        // Write lock: state should be WRITER_BIT.
        let daif3 = raw.write_lock();

        assert_eq!(raw.state.load(Ordering::Relaxed), WRITER_BIT);

        // Release writer: state back to 0.
        raw.write_unlock(daif3);

        assert_eq!(raw.state.load(Ordering::Relaxed), 0);
    }
}
