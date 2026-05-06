//! Debug-mode lock ordering validator.
//!
//! Tracks which lock classes are held and panics if a new acquisition
//! violates the global ordering defined in `design/research/smp-concurrency.md`.
//!
//! Only active under `debug_assertions`. Uses a single static on host tests
//! (tests run with `--test-threads=1`). On bare metal, per-CPU tracking
//! would go in the PerCpu struct (not yet wired).

#[cfg(debug_assertions)]
use core::sync::atomic::{AtomicU8, Ordering};

#[cfg(debug_assertions)]
const MAX_HELD: usize = 8;

/// Lock class — determines acquisition ordering.
///
/// Lower values must be acquired before higher values. Acquiring a lock
/// with class < any currently held class is a violation (potential deadlock).
/// Same-class acquisitions are allowed (intra-class ordering uses object
/// IDs, which this module does not check).
#[cfg(debug_assertions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LockClass {
    SpaceSlot = 1,
    EndpointSlot = 2,
    ThreadSlot = 3,
    Scheduler = 4,
    VmoSlot = 5,
    EventSlot = 6,
    IrqTable = 7,
    AllocLock = 8,
}

#[cfg(debug_assertions)]
static HELD_COUNT: AtomicU8 = AtomicU8::new(0);

#[cfg(debug_assertions)]
static mut HELD_STACK: [u8; MAX_HELD] = [0; MAX_HELD];

/// Record a lock acquisition and verify ordering.
///
/// Panics if the new class violates the global lock ordering.
#[cfg(debug_assertions)]
pub fn check_acquire(class: LockClass) {
    let count = HELD_COUNT.load(Ordering::Relaxed) as usize;

    // SAFETY: single-threaded access (tests: --test-threads=1, bare metal:
    // IRQs disabled). addr_of_mut avoids creating a reference to the static mut.
    let stack = unsafe { &mut *core::ptr::addr_of_mut!(HELD_STACK) };

    if count > 0 {
        let highest = stack[count - 1];

        assert!(
            (class as u8) >= highest,
            "lockdep: ordering violation — acquiring {:?} (class {}) while holding class {}",
            class,
            class as u8,
            highest,
        );
    }

    assert!(count < MAX_HELD, "lockdep: nesting depth exceeded");

    stack[count] = class as u8;
    HELD_COUNT.store((count + 1) as u8, Ordering::Relaxed);
}

/// Record a lock release.
#[cfg(debug_assertions)]
pub fn check_release(class: LockClass) {
    let count = HELD_COUNT.load(Ordering::Relaxed) as usize;

    assert!(count > 0, "lockdep: release without matching acquire");

    // SAFETY: same single-threaded guarantee as check_acquire.
    let stack = unsafe { &mut *core::ptr::addr_of_mut!(HELD_STACK) };
    let top = stack[count - 1];

    assert_eq!(
        top, class as u8,
        "lockdep: release class {:?} ({}) doesn't match top of stack ({})",
        class, class as u8, top,
    );

    stack[count - 1] = 0;
    HELD_COUNT.store((count - 1) as u8, Ordering::Relaxed);
}

/// Reset held lock state. Call at the start of each test to prevent
/// cross-test contamination from panicked tests.
#[cfg(debug_assertions)]
pub fn reset() {
    // SAFETY: called between tests (single-threaded).
    unsafe {
        core::ptr::addr_of_mut!(HELD_STACK).write([0; MAX_HELD]);
    }

    HELD_COUNT.store(0, Ordering::Relaxed);
}

/// Number of currently held locks (for assertions in tests).
#[cfg(debug_assertions)]
pub fn held_count() -> usize {
    HELD_COUNT.load(Ordering::Relaxed) as usize
}

#[cfg(all(debug_assertions, test))]
mod tests {
    use super::*;

    fn setup() {
        reset();
    }

    #[test]
    fn single_lock_acquire_release() {
        setup();

        check_acquire(LockClass::EndpointSlot);

        assert_eq!(held_count(), 1);

        check_release(LockClass::EndpointSlot);

        assert_eq!(held_count(), 0);
    }

    #[test]
    fn correct_ordering_succeeds() {
        setup();

        check_acquire(LockClass::SpaceSlot);
        check_acquire(LockClass::EndpointSlot);
        check_acquire(LockClass::ThreadSlot);
        check_acquire(LockClass::Scheduler);

        assert_eq!(held_count(), 4);

        check_release(LockClass::Scheduler);
        check_release(LockClass::ThreadSlot);
        check_release(LockClass::EndpointSlot);
        check_release(LockClass::SpaceSlot);

        assert_eq!(held_count(), 0);
    }

    #[test]
    fn same_class_allowed() {
        setup();

        check_acquire(LockClass::ThreadSlot);
        check_acquire(LockClass::ThreadSlot);

        assert_eq!(held_count(), 2);

        check_release(LockClass::ThreadSlot);
        check_release(LockClass::ThreadSlot);
    }

    #[test]
    #[should_panic(expected = "lockdep: ordering violation")]
    fn reverse_order_panics() {
        setup();

        check_acquire(LockClass::ThreadSlot);
        check_acquire(LockClass::SpaceSlot);
    }

    #[test]
    #[should_panic(expected = "lockdep: ordering violation")]
    fn scheduler_before_endpoint_panics() {
        setup();

        check_acquire(LockClass::Scheduler);
        check_acquire(LockClass::EndpointSlot);
    }

    #[test]
    #[should_panic(expected = "lockdep: release class")]
    fn mismatched_release_panics() {
        setup();

        check_acquire(LockClass::EndpointSlot);
        check_release(LockClass::ThreadSlot);
    }

    #[test]
    fn reset_clears_state() {
        setup();

        check_acquire(LockClass::VmoSlot);

        assert_eq!(held_count(), 1);

        reset();

        assert_eq!(held_count(), 0);
    }

    #[test]
    fn full_ipc_lock_sequence() {
        setup();

        check_acquire(LockClass::SpaceSlot);
        check_release(LockClass::SpaceSlot);

        check_acquire(LockClass::EndpointSlot);
        check_acquire(LockClass::ThreadSlot);
        check_acquire(LockClass::Scheduler);
        check_release(LockClass::Scheduler);
        check_release(LockClass::ThreadSlot);
        check_release(LockClass::EndpointSlot);

        assert_eq!(held_count(), 0);
    }
}
