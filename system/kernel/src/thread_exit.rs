//! Thread exit notification.
//!
//! Each thread gets an exit notification object when another thread holds a
//! `HandleObject::Thread(id)` handle to it. The handle becomes "ready" when
//! the thread exits — same waitable pattern as timers and interrupts.
//!
//! Level-triggered: once exited, the handle is permanently ready (like timers).
//! Two-phase wake: collect waiter under own lock, wake under scheduler lock.

use super::handle::HandleObject;
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use alloc::vec::Vec;

/// Per-thread exit notification state.
struct Notification {
    thread_id: ThreadId,
    exited: bool,
    waiter: Option<ThreadId>,
}
struct State {
    entries: Vec<Notification>,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    entries: Vec::new(),
});

/// Check if a thread has exited (for `sys_wait` readiness check).
///
/// Level-triggered: returns true forever after the thread exits.
pub fn check_exited(thread_id: ThreadId) -> bool {
    let s = STATE.lock();

    s.entries
        .iter()
        .find(|n| n.thread_id == thread_id)
        .is_some_and(|n| n.exited)
}
/// Create exit notification state for a thread. Called when a Thread handle
/// is inserted into a handle table (thread_create syscall).
pub fn create(thread_id: ThreadId) {
    let mut s = STATE.lock();

    // Avoid duplicates.
    if s.entries.iter().any(|n| n.thread_id == thread_id) {
        return;
    }

    s.entries.push(Notification {
        thread_id,
        exited: false,
        waiter: None,
    });
}
/// Destroy exit notification state (called from `handle_close`).
pub fn destroy(thread_id: ThreadId) {
    let mut s = STATE.lock();

    s.entries.retain(|n| n.thread_id != thread_id);
}
/// Notify that a thread has exited. Two-phase wake.
///
/// Called from `exit_current_from_syscall` after marking the thread exited.
/// Collects the waiter under our lock, then wakes via scheduler.
pub fn notify_exit(thread_id: ThreadId) {
    let waiter = {
        let mut s = STATE.lock();

        if let Some(n) = s.entries.iter_mut().find(|n| n.thread_id == thread_id) {
            n.exited = true;
            n.waiter.take()
        } else {
            None
        }
    };

    // Phase 2: wake the waiter (acquires scheduler lock).
    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Thread(thread_id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}
/// Register a thread as the waiter for another thread's exit.
pub fn register_waiter(thread_id: ThreadId, waiter: ThreadId) {
    let mut s = STATE.lock();

    if let Some(n) = s.entries.iter_mut().find(|n| n.thread_id == thread_id) {
        n.waiter = Some(waiter);
    }
}
/// Unregister a waiter (cleanup when `wait` returns).
pub fn unregister_waiter(thread_id: ThreadId) {
    let mut s = STATE.lock();

    if let Some(n) = s.entries.iter_mut().find(|n| n.thread_id == thread_id) {
        n.waiter = None;
    }
}
