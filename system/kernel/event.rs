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

use alloc::vec::Vec;

use super::{
    handle::HandleObject,
    paging, scheduler,
    sync::IrqMutex,
    thread::ThreadId,
    waitable::{WaitableId, WaitableRegistry},
};

static TABLE: IrqMutex<Table> = IrqMutex::new(Table {
    waiters: WaitableRegistry::new(),
    next_id: 0,
    free_ids: Vec::new(),
});

struct Table {
    waiters: WaitableRegistry<EventId>,
    next_id: u32,
    /// Freed event IDs available for reuse.
    free_ids: Vec<u32>,
}

/// Identifies an event in the event table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventId(pub u32);

impl WaitableId for EventId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Check whether an event is signaled (for `sys_wait` readiness check).
pub fn check_pending(id: EventId) -> bool {
    TABLE.lock().waiters.check_ready(id)
}
/// Create a new event. Returns `None` if the system-wide event cap
/// (`MAX_EVENTS`) is reached.
pub fn create() -> Option<EventId> {
    let mut table = TABLE.lock();
    let id = if let Some(free_id) = table.free_ids.pop() {
        EventId(free_id)
    } else {
        if table.next_id >= paging::MAX_EVENTS as u32 {
            return None;
        }

        let id = EventId(table.next_id);

        table.next_id += 1;

        id
    };

    table.waiters.create(id);

    Some(id)
}
/// Destroy an event (called from `handle_close`).
///
/// Wakes any thread blocked on this event (so it doesn't hang forever).
/// Recycles the event ID for reuse by future `create()` calls.
pub fn destroy(id: EventId) {
    let waiter = {
        let mut table = TABLE.lock();
        let waiter = table.waiters.destroy(id);

        table.free_ids.push(id.0);

        waiter
    };

    if let Some(waiter_id) = waiter {
        scheduler::wake_for_handle(waiter_id, HandleObject::Event(id));
    }
}
/// Pre-allocate event data structures to full capacity.
///
/// Called once from `kernel_main` after heap init.
pub fn init() {
    let mut table = TABLE.lock();

    table.free_ids.reserve(paging::MAX_EVENTS as usize);
}
/// Register a thread as the waiter for an event.
pub fn register_waiter(id: EventId, waiter: ThreadId) {
    TABLE.lock().waiters.register_waiter(id, waiter);
}
/// Reset an event — clears the signaled state.
///
/// After reset, `check_pending` returns false until the next `signal`.
pub fn reset(id: EventId) {
    TABLE.lock().waiters.clear_ready(id);
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
        scheduler::wake_for_handle(waiter_id, HandleObject::Event(id));
    }
}
/// Clear the waiter registration for an event.
pub fn unregister_waiter(id: EventId) {
    TABLE.lock().waiters.unregister_waiter(id);
}
