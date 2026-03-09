//! Process exit notification.
//!
//! Each child process gets an exit notification when a parent holds a
//! `HandleObject::Process(pid)` handle. The handle becomes "ready" when the
//! process's last thread exits — same waitable pattern as thread/timer/interrupt.
//!
//! Level-triggered: once exited, the handle is permanently ready.
//! Two-phase wake: collect waiter under own lock, wake under scheduler lock.

use super::handle::HandleObject;
use super::process::ProcessId;
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;
use alloc::vec::Vec;

/// Per-process exit notification state.
struct Notification {
    process_id: ProcessId,
    exited: bool,
    waiter: Option<ThreadId>,
}
struct State {
    entries: Vec<Notification>,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    entries: Vec::new(),
});

/// Check if a process has exited (for `sys_wait` readiness check).
///
/// Level-triggered: returns true forever after the last thread exits.
pub fn check_exited(process_id: ProcessId) -> bool {
    let s = STATE.lock();

    s.entries
        .iter()
        .find(|n| n.process_id == process_id)
        .is_some_and(|n| n.exited)
}
/// Create exit notification state for a process. Called when a Process handle
/// is inserted into a handle table (process_create syscall).
pub fn create(process_id: ProcessId) {
    let mut s = STATE.lock();

    if s.entries.iter().any(|n| n.process_id == process_id) {
        return;
    }

    s.entries.push(Notification {
        process_id,
        exited: false,
        waiter: None,
    });
}
/// Destroy exit notification state (called from `handle_close`).
pub fn destroy(process_id: ProcessId) {
    let mut s = STATE.lock();

    s.entries.retain(|n| n.process_id != process_id);
}
/// Notify that a process's last thread has exited. Two-phase wake.
///
/// Called from `exit_current_from_syscall` on the last-thread-exit path.
pub fn notify_exit(process_id: ProcessId) {
    let waiter = {
        let mut s = STATE.lock();

        if let Some(n) = s.entries.iter_mut().find(|n| n.process_id == process_id) {
            n.exited = true;
            n.waiter.take()
        } else {
            None
        }
    };

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Process(process_id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}
/// Register a thread as the waiter for a process exit.
pub fn register_waiter(process_id: ProcessId, waiter: ThreadId) {
    let mut s = STATE.lock();

    if let Some(n) = s.entries.iter_mut().find(|n| n.process_id == process_id) {
        n.waiter = Some(waiter);
    }
}
/// Unregister a waiter (cleanup when `wait` returns).
pub fn unregister_waiter(process_id: ProcessId) {
    let mut s = STATE.lock();

    if let Some(n) = s.entries.iter_mut().find(|n| n.process_id == process_id) {
        n.waiter = None;
    }
}
