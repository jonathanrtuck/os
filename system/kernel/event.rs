//! Event objects — lightweight waitable notifications.
//!
//! An event is a waitable kernel object with a boolean signaled state.
//! Signaling wakes all blocked waiters. The signaled state persists until
//! explicitly reset via `event_reset`, enabling both level-triggered (poll
//! without resetting) and edge-triggered (reset after each signal) patterns.
//!
//! Events are the lightest IPC primitive — no shared memory, no message
//! payload, no ring buffers. Use them when you just need "wake me up."
//!
//! # Syscalls
//!
//! - `event_create()` → handle
//! - `event_signal(handle)` → sets signaled, wakes waiters
//! - `event_reset(handle)` → clears signaled state
//! - `wait(handles, count, timeout)` → returns when any event is signaled

use super::{
    handle::HandleObject,
    scheduler,
    sync::IrqMutex,
    thread::ThreadId,
    waitable::{WaitableId, WaitableRegistry},
};

/// Identifies an event in the event table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventId(pub u32);

impl WaitableId for EventId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

struct Table {
    waiters: WaitableRegistry<EventId>,
    next_id: u32,
}

static TABLE: IrqMutex<Table> = IrqMutex::new(Table {
    waiters: WaitableRegistry::new(),
    next_id: 0,
});

/// Create a new event. Returns the EventId.
pub fn create() -> EventId {
    let mut table = TABLE.lock();
    let id = EventId(table.next_id);

    table.next_id += 1;
    table.waiters.create(id);

    id
}

/// Signal an event — marks it as ready and wakes any blocked waiter.
///
/// Level-triggered: the signaled state persists until `reset()`.
/// Two-phase wake: collect waiter under event lock, wake under scheduler lock.
pub fn signal(id: EventId) {
    let waiter = {
        let mut table = TABLE.lock();

        table.waiters.notify(id)
    };

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Event(id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}

/// Reset an event — clears the signaled state.
///
/// After reset, `check_pending` returns false until the next `signal`.
pub fn reset(id: EventId) {
    TABLE.lock().waiters.clear_ready(id);
}

/// Check whether an event is signaled (for `sys_wait` readiness check).
pub fn check_pending(id: EventId) -> bool {
    TABLE.lock().waiters.check_ready(id)
}

/// Register a thread as the waiter for an event.
pub fn register_waiter(id: EventId, waiter: ThreadId) {
    TABLE.lock().waiters.register_waiter(id, waiter);
}

/// Clear the waiter registration for an event.
pub fn unregister_waiter(id: EventId) {
    TABLE.lock().waiters.unregister_waiter(id);
}

/// Destroy an event (called from `handle_close`).
///
/// Wakes any thread blocked on this event (so it doesn't hang forever).
pub fn destroy(id: EventId) {
    let waiter = TABLE.lock().waiters.destroy(id);

    if let Some(waiter_id) = waiter {
        let reason = HandleObject::Event(id);

        if !scheduler::try_wake_for_handle(waiter_id, reason) {
            scheduler::set_wake_pending_for_handle(waiter_id, reason);
        }
    }
}
