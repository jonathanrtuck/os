// AUDIT: 2026-03-11 — 0 unsafe blocks (pure safe Rust). 6-category checklist
// applied. No bugs found.
//
// Exit notification completeness verified:
//   - User threads: notify_exit called from exit_current_from_syscall (scheduler.rs)
//     and from kill_process path (syscall.rs). Both paths correctly notify for
//     every killed thread.
//   - Kernel threads: exit via exit_current() without notification — correct
//     because kernel threads never have thread handles (thread_exit::create is
//     only called from sys_thread_create for user threads).
//
// Resource cleanup sequencing verified:
//   - destroy() removes the entry and wakes any blocked waiter — called from
//     handle_close when a thread handle is closed. Correct even if the target
//     thread is still alive (waiter is woken, later notify_exit finds no entry).
//   - notify_exit() marks ready and wakes waiter — called after the scheduler
//     lock is released (Phase 2 of exit). Two-phase wake maintains lock
//     ordering: thread_exit lock → scheduler lock.
//
// Races between thread exit and scheduler operations:
//   - No race: notify_exit is called between scheduler lock releases (Phase 1
//     drops lock, Phase 2 calls notify_exit, Phase 5 re-acquires lock). The
//     exiting thread is still `current` on its core, so no other scheduler
//     operation can move or drop it.
//   - kill_process concurrent with exit: safe because Phase 1 either takes
//     the process (last thread) or decrements thread_count (non-last).
//     kill_process acquires the scheduler lock, so it serializes with Phase 1
//     and Phase 5. mark_exited is idempotent.

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
///
/// Wakes any thread blocked waiting for this thread's exit.
pub fn destroy(thread_id: ThreadId) {
    let waiter = STATE.lock().destroy(thread_id);

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Thread(thread_id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
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
