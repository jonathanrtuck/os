//! Thread exit notification.
//!
//! Thin wrapper around `WaitableRegistry<ThreadId>`. Each thread with a handle
//! gets an entry that becomes permanently ready on exit (level-triggered).
//! Two-phase wake: collect waiter under own lock, wake under scheduler lock.

use super::handle::HandleObject;
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use super::waitable::WaitableRegistry;

static STATE: IrqMutex<WaitableRegistry<ThreadId>> = IrqMutex::new(WaitableRegistry::new());

/// Check if a thread has exited (for `sys_wait` readiness check).
pub fn check_exited(thread_id: ThreadId) -> bool {
    STATE.lock().check_ready(thread_id)
}
/// Create exit notification state for a thread (called from `thread_create`).
pub fn create(thread_id: ThreadId) {
    STATE.lock().create(thread_id);
}
/// Destroy exit notification state (called from `handle_close`).
pub fn destroy(thread_id: ThreadId) {
    STATE.lock().destroy(thread_id);
}
/// Notify that a thread has exited. Two-phase wake.
pub fn notify_exit(thread_id: ThreadId) {
    let waiter = STATE.lock().notify(thread_id);

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Thread(thread_id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}
/// Register a thread as the waiter for another thread's exit.
pub fn register_waiter(thread_id: ThreadId, waiter: ThreadId) {
    STATE.lock().register_waiter(thread_id, waiter);
}
/// Unregister a waiter (cleanup when `wait` returns).
pub fn unregister_waiter(thread_id: ThreadId) {
    STATE.lock().unregister_waiter(thread_id);
}
