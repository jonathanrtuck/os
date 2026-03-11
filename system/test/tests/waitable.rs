//! Host-side tests for the generic WaitableRegistry.
//!
//! Includes the kernel's waitable.rs directly — it depends only on
//! thread::ThreadId and alloc::vec::Vec, both available on the host.

extern crate alloc;

mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}

#[path = "../../kernel/waitable.rs"]
mod waitable;

use thread::ThreadId;
use waitable::{WaitableId, WaitableRegistry};

// --- Helpers ---

fn tid(n: u64) -> ThreadId {
    ThreadId(n)
}

/// Trivial ID type for testing (not a real kernel type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TestId(u32);

impl WaitableId for TestId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

// --- Tests ---

#[test]
fn newly_created_entry_is_not_ready() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));

    assert!(!reg.check_ready(TestId(1)));
}

#[test]
fn check_ready_returns_false_for_unknown_id() {
    let reg = WaitableRegistry::<TestId>::new();

    assert!(!reg.check_ready(TestId(42)));
}

#[test]
fn notify_makes_entry_ready() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.notify(TestId(1));

    assert!(reg.check_ready(TestId(1)));
}

#[test]
fn notify_returns_registered_waiter() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.register_waiter(TestId(1), tid(10));

    let waiter = reg.notify(TestId(1));

    assert_eq!(waiter, Some(tid(10)));
}

#[test]
fn notify_returns_none_without_waiter() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));

    assert_eq!(reg.notify(TestId(1)), None);
}

#[test]
fn notify_consumes_waiter() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.register_waiter(TestId(1), tid(10));
    reg.notify(TestId(1));

    // Second notify should return None — waiter was consumed.
    assert_eq!(reg.notify(TestId(1)), None);
}

#[test]
fn notify_is_idempotent_for_readiness() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.notify(TestId(1));
    reg.notify(TestId(1)); // No-op for readiness.

    assert!(reg.check_ready(TestId(1)));
}

#[test]
fn notify_unknown_id_returns_none() {
    let mut reg = WaitableRegistry::<TestId>::new();

    assert_eq!(reg.notify(TestId(99)), None);
}

#[test]
fn destroy_removes_entry() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.notify(TestId(1));
    reg.destroy(TestId(1));

    assert!(!reg.check_ready(TestId(1)));
}

#[test]
fn clear_ready_resets_readiness() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.notify(TestId(1));

    assert!(reg.check_ready(TestId(1)));

    reg.clear_ready(TestId(1));

    assert!(!reg.check_ready(TestId(1)));
}

#[test]
fn unregister_waiter_prevents_notify_return() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.register_waiter(TestId(1), tid(10));
    reg.unregister_waiter(TestId(1));

    assert_eq!(reg.notify(TestId(1)), None);
}

#[test]
#[should_panic(expected = "duplicate waitable ID")]
fn create_duplicate_panics() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.create(TestId(1)); // Should panic — duplicate create is a bug.
}

#[test]
fn multiple_entries_are_independent() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.create(TestId(2));
    reg.create(TestId(3));

    reg.notify(TestId(2));

    assert!(!reg.check_ready(TestId(1)));
    assert!(reg.check_ready(TestId(2)));
    assert!(!reg.check_ready(TestId(3)));
}

#[test]
fn destroy_does_not_affect_other_entries() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.create(TestId(2));
    reg.notify(TestId(1));
    reg.notify(TestId(2));

    reg.destroy(TestId(1));

    assert!(!reg.check_ready(TestId(1)));
    assert!(reg.check_ready(TestId(2)));
}

#[test]
fn reuse_id_after_destroy() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.notify(TestId(1));
    reg.destroy(TestId(1));

    // Re-create with same ID — should start fresh.
    reg.create(TestId(1));

    assert!(!reg.check_ready(TestId(1)));
}

#[test]
fn edge_triggered_cycle() {
    // Simulates the interrupt ack/re-fire cycle.
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));

    // First fire.
    reg.register_waiter(TestId(1), tid(10));
    let w1 = reg.notify(TestId(1));

    assert_eq!(w1, Some(tid(10)));
    assert!(reg.check_ready(TestId(1)));

    // Acknowledge (clear ready).
    reg.clear_ready(TestId(1));

    assert!(!reg.check_ready(TestId(1)));

    // Second fire.
    reg.register_waiter(TestId(1), tid(20));
    let w2 = reg.notify(TestId(1));

    assert_eq!(w2, Some(tid(20)));
    assert!(reg.check_ready(TestId(1)));
}

#[test]
fn register_waiter_on_unknown_id_is_noop() {
    let mut reg = WaitableRegistry::<TestId>::new();

    // Should not panic.
    reg.register_waiter(TestId(99), tid(10));
}

#[test]
fn unregister_waiter_on_unknown_id_is_noop() {
    let mut reg = WaitableRegistry::<TestId>::new();

    // Should not panic.
    reg.unregister_waiter(TestId(99));
}

#[test]
fn clear_ready_on_unknown_id_is_noop() {
    let mut reg = WaitableRegistry::<TestId>::new();

    // Should not panic.
    reg.clear_ready(TestId(99));
}

#[test]
fn destroy_unknown_id_is_noop() {
    let mut reg = WaitableRegistry::<TestId>::new();

    // Should not panic.
    reg.destroy(TestId(99));
}

// ============================================================
// Thread exit notification pattern tests (2026-03-11 audit)
// Models the exact sequences used by thread_exit.rs.
// ============================================================

/// Models the normal thread exit path: create entry, thread exits, notify
/// returns the waiter. The waiter should be woken with the thread's ID.
#[test]
fn thread_exit_notify_returns_waiter() {
    let mut reg = WaitableRegistry::new();

    // sys_thread_create calls thread_exit::create.
    reg.create(TestId(5));

    // sys_wait calls thread_exit::register_waiter.
    reg.register_waiter(TestId(5), tid(10));

    // Thread exits → thread_exit::notify_exit calls reg.notify.
    let waiter = reg.notify(TestId(5));

    assert_eq!(waiter, Some(tid(10)), "waiter must be returned on exit");
    assert!(reg.check_ready(TestId(5)), "entry must be permanently ready after exit");
}

/// Models handle close before thread exit: destroy removes the entry,
/// later notify on the destroyed entry returns None (no crash, no leak).
#[test]
fn handle_close_before_thread_exit() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(5));
    reg.register_waiter(TestId(5), tid(10));

    // handle_close calls thread_exit::destroy — returns waiter to wake.
    let waiter = reg.destroy(TestId(5));
    assert_eq!(waiter, Some(tid(10)), "destroy must wake the waiter");

    // Later, thread exits — notify on destroyed entry.
    let waiter2 = reg.notify(TestId(5));
    assert_eq!(waiter2, None, "notify after destroy must return None");
}

/// Models the race: thread exits while waiter is being registered.
/// The two-phase pattern handles this: notify sees the waiter if registered
/// first, or returns None if not yet registered. The wake_pending flag in
/// the scheduler handles the latter case.
#[test]
fn notify_without_registered_waiter() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(5));

    // Thread exits before anyone registers as waiter.
    let waiter = reg.notify(TestId(5));
    assert_eq!(waiter, None, "no waiter registered yet");

    // Entry is permanently ready — later check_ready returns true.
    // In the real thread_exit flow, sys_wait checks check_ready FIRST.
    // If already ready, it returns immediately without blocking — no
    // register_waiter needed.
    assert!(reg.check_ready(TestId(5)), "entry permanently ready after notify");
}

/// Models multiple threads sequentially waiting on the same thread exit.
/// Only one waiter at a time — the last registered waiter wins.
#[test]
fn sequential_waiters_on_same_thread() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(5));

    // First waiter registers.
    reg.register_waiter(TestId(5), tid(10));

    // First waiter unregisters (e.g., wait returns early).
    reg.unregister_waiter(TestId(5));

    // Second waiter registers.
    reg.register_waiter(TestId(5), tid(20));

    // Thread exits — second waiter should be returned.
    let waiter = reg.notify(TestId(5));
    assert_eq!(waiter, Some(tid(20)));
}

// ============================================================
// Additional tests from sync-primitives audit (2026-03-11)
// ============================================================

/// Destroy returns waiter even if entry is already ready.
#[test]
fn destroy_ready_entry_returns_waiter() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.register_waiter(TestId(1), tid(10));
    reg.notify(TestId(1)); // Marks ready, takes waiter.

    // Re-register after notify consumed it.
    reg.register_waiter(TestId(1), tid(20));

    let waiter = reg.destroy(TestId(1));
    assert_eq!(waiter, Some(tid(20)), "destroy must return waiter even on ready entry");
}

/// Destroy on empty entry returns None.
#[test]
fn destroy_fresh_entry_returns_none() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    let waiter = reg.destroy(TestId(1));

    assert_eq!(waiter, None);
}

/// Large sparse IDs don't panic — registry grows the Vec as needed.
#[test]
fn large_sparse_ids_no_panic() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1000));
    assert!(!reg.check_ready(TestId(1000)));

    reg.notify(TestId(1000));
    assert!(reg.check_ready(TestId(1000)));
}

/// Create at index 0, verify it works (boundary condition).
#[test]
fn create_at_index_zero() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(0));
    reg.register_waiter(TestId(0), tid(1));

    let waiter = reg.notify(TestId(0));
    assert_eq!(waiter, Some(tid(1)));
    assert!(reg.check_ready(TestId(0)));
}

/// Notify after destroy + re-create: fresh entry, not leftover state.
#[test]
fn create_after_destroy_is_fresh() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.notify(TestId(1));
    assert!(reg.check_ready(TestId(1)));

    reg.destroy(TestId(1));
    reg.create(TestId(1));

    // Must be fresh — not ready, no leftover waiter.
    assert!(!reg.check_ready(TestId(1)));
    assert_eq!(reg.notify(TestId(1)), None);
}

/// Destroy with waiter, then create at same ID — waiter is gone.
#[test]
fn destroy_then_create_clears_waiter() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.register_waiter(TestId(1), tid(10));

    let waiter = reg.destroy(TestId(1));
    assert_eq!(waiter, Some(tid(10)));

    reg.create(TestId(1));
    // No waiter after re-create.
    assert_eq!(reg.notify(TestId(1)), None);
}

/// register_waiter replaces old waiter silently (design choice).
#[test]
fn register_waiter_replaces_silently() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.register_waiter(TestId(1), tid(10));
    reg.register_waiter(TestId(1), tid(20)); // Overwrites.

    let waiter = reg.notify(TestId(1));
    assert_eq!(waiter, Some(tid(20)), "last registered waiter wins");
}

/// Interleave notify and clear_ready — edge-triggered stress.
#[test]
fn edge_triggered_rapid_cycle() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));

    for i in 0..10u64 {
        reg.register_waiter(TestId(1), tid(i));
        let w = reg.notify(TestId(1));
        assert_eq!(w, Some(tid(i)));
        assert!(reg.check_ready(TestId(1)));
        reg.clear_ready(TestId(1));
        assert!(!reg.check_ready(TestId(1)));
    }
}
