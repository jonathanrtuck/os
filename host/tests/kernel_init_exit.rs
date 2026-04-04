//! Host-side tests for the init-exit shutdown mechanism.
//!
//! The kernel tracks the init process PID via a static AtomicU32 (sentinel
//! u32::MAX = "not set"). `set_init_pid` stores the PID, `is_init` checks it.
//! These are used in the scheduler exit path to trigger system shutdown when
//! init dies.
//!
//! We duplicate the three functions here as a self-contained model (same
//! pattern as kernel_process.rs) because process.rs depends on AddressSpace,
//! HandleTable, etc., which cannot be compiled on the host.

use std::sync::atomic::{AtomicU32, Ordering};

// --- Minimal model (mirrors kernel/process.rs) ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessId(u32);

static INIT_PID: AtomicU32 = AtomicU32::new(u32::MAX);

fn set_init_pid(pid: ProcessId) {
    INIT_PID.store(pid.0, Ordering::Release);
}

fn is_init(pid: ProcessId) -> bool {
    INIT_PID.load(Ordering::Acquire) == pid.0
}

/// Reset the global to sentinel so tests don't leak state across runs.
fn reset_init_pid() {
    INIT_PID.store(u32::MAX, Ordering::Release);
}

// ============================================================
// Tests
// ============================================================

#[test]
fn is_init_false_before_set() {
    reset_init_pid();

    // Before set_init_pid is called, the sentinel is u32::MAX.
    // Any reasonable PID (0, 1, 42) should return false.
    assert!(
        !is_init(ProcessId(0)),
        "PID 0 must not be init before set_init_pid"
    );
    assert!(
        !is_init(ProcessId(1)),
        "PID 1 must not be init before set_init_pid"
    );
    assert!(
        !is_init(ProcessId(42)),
        "PID 42 must not be init before set_init_pid"
    );
}

#[test]
fn set_init_pid_stores_correctly() {
    reset_init_pid();

    set_init_pid(ProcessId(7));

    assert!(
        is_init(ProcessId(7)),
        "is_init must return true for the stored PID"
    );
}

#[test]
fn is_init_false_for_other_pids() {
    reset_init_pid();

    set_init_pid(ProcessId(3));

    assert!(is_init(ProcessId(3)), "PID 3 is init");
    assert!(
        !is_init(ProcessId(0)),
        "PID 0 must not be init when init is PID 3"
    );
    assert!(
        !is_init(ProcessId(1)),
        "PID 1 must not be init when init is PID 3"
    );
    assert!(
        !is_init(ProcessId(4)),
        "PID 4 must not be init when init is PID 3"
    );
    assert!(
        !is_init(ProcessId(u32::MAX - 1)),
        "PID u32::MAX-1 must not be init when init is PID 3"
    );
}

#[test]
fn set_init_pid_overwrites_idempotent() {
    reset_init_pid();

    // First call sets PID 5.
    set_init_pid(ProcessId(5));
    assert!(is_init(ProcessId(5)));
    assert!(!is_init(ProcessId(10)));

    // Second call overwrites to PID 10.
    set_init_pid(ProcessId(10));
    assert!(
        !is_init(ProcessId(5)),
        "old init PID must no longer match after overwrite"
    );
    assert!(
        is_init(ProcessId(10)),
        "new init PID must match after overwrite"
    );

    // Third call — same value is idempotent.
    set_init_pid(ProcessId(10));
    assert!(
        is_init(ProcessId(10)),
        "re-setting the same PID must still match"
    );
}

#[test]
fn sentinel_value_is_not_a_valid_init_pid() {
    reset_init_pid();

    // u32::MAX is the sentinel. Before any set, is_init(u32::MAX) would be
    // true — but that's fine because u32::MAX is not a valid process slot index.
    // After setting a real PID, u32::MAX must return false.
    set_init_pid(ProcessId(0));
    assert!(
        !is_init(ProcessId(u32::MAX)),
        "u32::MAX must not be init after a real PID is set"
    );
}

#[test]
fn set_init_pid_zero_is_valid() {
    reset_init_pid();

    // PID 0 is a valid process slot index (the first process).
    set_init_pid(ProcessId(0));
    assert!(is_init(ProcessId(0)), "PID 0 is a valid init PID");
    assert!(!is_init(ProcessId(1)));
}
