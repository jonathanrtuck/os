// AUDIT: 2026-03-11 — 0 unsafe blocks (pure safe Rust). 6-category checklist
// applied. No bugs found. Two-phase wake pattern (own lock → release →
// scheduler lock) correctly prevents lock ordering violations. Destroy path
// properly wakes any blocked waiter. All functions delegate to WaitableRegistry
// which handles missing entries gracefully.

//! Process exit notification.
//!
//! Thin wrapper around `WaitableRegistry<ProcessId>`. Each child process with
//! a handle gets an entry that becomes permanently ready when its last thread
//! exits (level-triggered). Two-phase wake: collect waiter under own lock,
//! wake under scheduler lock.

use super::handle::HandleObject;
use super::process::ProcessId;
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use super::waitable::WaitableRegistry;

static STATE: IrqMutex<WaitableRegistry<ProcessId>> = IrqMutex::new(WaitableRegistry::new());

/// Check if a process has exited (for `sys_wait` readiness check).
pub fn check_exited(process_id: ProcessId) -> bool {
    STATE.lock().check_ready(process_id)
}
/// Create exit notification state for a process (called from `process_create`).
pub fn create(process_id: ProcessId) {
    STATE.lock().create(process_id);
}
/// Destroy exit notification state (called from `handle_close`).
///
/// Wakes any thread blocked waiting for this process's exit.
pub fn destroy(process_id: ProcessId) {
    let waiter = STATE.lock().destroy(process_id);

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Process(process_id);
        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}
/// Notify that a process's last thread has exited. Two-phase wake.
pub fn notify_exit(process_id: ProcessId) {
    let waiter = STATE.lock().notify(process_id);

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Process(process_id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}
/// Register a thread as the waiter for a process exit.
pub fn register_waiter(process_id: ProcessId, waiter: ThreadId) {
    STATE.lock().register_waiter(process_id, waiter);
}
/// Unregister a waiter (cleanup when `wait` returns).
pub fn unregister_waiter(process_id: ProcessId) {
    STATE.lock().unregister_waiter(process_id);
}
