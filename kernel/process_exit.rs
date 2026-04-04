// AUDIT: 2026-03-11 — 0 unsafe blocks (pure safe Rust). 6-category checklist
// applied. No bugs found. Two-phase wake pattern (own lock → release →
// scheduler lock) correctly prevents lock ordering violations. Destroy path
// properly wakes any blocked waiter. All functions delegate to WaitableRegistry
// which handles missing entries gracefully.

//! Process exit notification and exit code storage.
//!
//! Thin wrapper around `WaitableRegistry<ProcessId>`. Each child process with
//! a handle gets an entry that becomes permanently ready when its last thread
//! exits (level-triggered). Two-phase wake: collect waiter under own lock,
//! wake under scheduler lock.
//!
//! Exit codes are stored separately from the WaitableRegistry and survive
//! process cleanup. Voluntary exit stores the user-provided code from the
//! `exit` syscall; involuntary termination (kill) leaves the sentinel `i64::MIN`.

use alloc::vec::Vec;

use super::{
    handle::HandleObject, process::ProcessId, scheduler, sync::IrqMutex, thread::ThreadId,
    waitable::WaitableRegistry,
};

static STATE: IrqMutex<ExitState> = IrqMutex::new(ExitState::new());

struct ExitState {
    registry: WaitableRegistry<ProcessId>,
    /// Exit codes indexed by ProcessId. Populated by `notify_exit` when a
    /// process's last thread exits. Cleared by `destroy` when the handle is closed.
    exit_codes: Vec<Option<i64>>,
}

impl ExitState {
    const fn new() -> Self {
        Self {
            registry: WaitableRegistry::new(),
            exit_codes: Vec::new(),
        }
    }
}

/// Check if a process has exited (for `sys_wait` readiness check).
pub fn check_exited(process_id: ProcessId) -> bool {
    STATE.lock().registry.check_ready(process_id)
}
/// Create exit notification state for a process (called from `process_create`).
pub fn create(process_id: ProcessId) {
    let mut s = STATE.lock();

    s.registry.create(process_id);

    let idx = process_id.0 as usize;

    if idx >= s.exit_codes.len() {
        s.exit_codes.resize(idx + 1, None);
    }

    s.exit_codes[idx] = None;
}
/// Destroy exit notification state (called from `handle_close`).
///
/// Wakes any thread blocked waiting for this process's exit.
/// Clears the stored exit code.
pub fn destroy(process_id: ProcessId) {
    let waiter = {
        let mut s = STATE.lock();
        let w = s.registry.destroy(process_id);
        let idx = process_id.0 as usize;

        if idx < s.exit_codes.len() {
            s.exit_codes[idx] = None;
        }

        w
    };

    if let Some(waiter_id) = waiter {
        scheduler::wake_for_handle(waiter_id, HandleObject::Process(process_id));
    }
}
/// Retrieve the exit code of an exited process.
///
/// Returns `Some(code)` if the process has exited and its exit notification
/// state hasn't been destroyed yet. Returns `None` if the process is still
/// running or has already been fully cleaned up.
pub fn get_exit_code(process_id: ProcessId) -> Option<i64> {
    let s = STATE.lock();
    let idx = process_id.0 as usize;

    s.exit_codes.get(idx).copied().flatten()
}
/// Notify that a process's last thread has exited. Two-phase wake.
///
/// `exit_code` is the value from the process's `exit_code` field — set by
/// `sys_exit` (voluntary) or left at `i64::MIN` (killed / unhandled fault).
pub fn notify_exit(process_id: ProcessId, exit_code: i64) {
    let waiter = {
        let mut s = STATE.lock();
        let idx = process_id.0 as usize;

        if idx >= s.exit_codes.len() {
            s.exit_codes.resize(idx + 1, None);
        }

        s.exit_codes[idx] = Some(exit_code);

        s.registry.notify(process_id)
    };

    if let Some(waiter_id) = waiter {
        scheduler::wake_for_handle(waiter_id, HandleObject::Process(process_id));
    }
}
/// Register a thread as the waiter for a process exit.
pub fn register_waiter(process_id: ProcessId, waiter: ThreadId) {
    STATE.lock().registry.register_waiter(process_id, waiter);
}
/// Unregister a waiter (cleanup when `wait` returns).
pub fn unregister_waiter(process_id: ProcessId) {
    STATE.lock().registry.unregister_waiter(process_id);
}
