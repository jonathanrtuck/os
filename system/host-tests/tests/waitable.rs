//! Host-side tests for the generic WaitableRegistry.
//!
//! Includes the kernel's waitable.rs directly — it depends only on
//! thread::ThreadId and alloc::vec::Vec, both available on the host.

extern crate alloc;

mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}

#[path = "../../kernel/src/waitable.rs"]
mod waitable;

use thread::ThreadId;
use waitable::WaitableRegistry;

// --- Helpers ---

fn tid(n: u64) -> ThreadId {
    ThreadId(n)
}

/// Trivial ID type for testing (not a real kernel type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TestId(u32);

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
fn create_is_idempotent() {
    let mut reg = WaitableRegistry::new();

    reg.create(TestId(1));
    reg.register_waiter(TestId(1), tid(10));
    reg.create(TestId(1)); // Should not reset the entry.

    // Waiter should still be registered (entry was not overwritten).
    assert_eq!(reg.notify(TestId(1)), Some(tid(10)));
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
