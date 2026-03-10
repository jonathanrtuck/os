//! Generic waitable registry.
//!
//! Provides the common waiter registration, readiness tracking, and two-phase
//! wake collection used by thread exit, process exit, timer, and interrupt
//! modules. Each module embeds or wraps a `WaitableRegistry<Id>` to eliminate
//! duplicated notification boilerplate.
//!
//! The registry is a plain data structure — no lock. Callers provide
//! synchronization, typically by embedding the registry inside an
//! `IrqMutex`-protected struct.

use super::thread::ThreadId;
use alloc::vec::Vec;

struct Entry<Id> {
    id: Id,
    ready: bool,
    waiter: Option<ThreadId>,
}

/// Tracks readiness and waiter registration for a set of waitable kernel objects.
///
/// Generic over the ID type (ThreadId, ProcessId, TimerId, InterruptId).
/// Uses Vec + linear search — adequate for the small counts involved (≤32).
/// Section 10.6 will upgrade to O(1) indexed lookup.
pub struct WaitableRegistry<Id: Copy + Eq> {
    entries: Vec<Entry<Id>>,
}

impl<Id: Copy + Eq> WaitableRegistry<Id> {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Check whether `id` is ready (non-consuming).
    ///
    /// Level-triggered for exit/timer: returns true forever once notified.
    /// Edge-triggered for interrupts: caller uses `clear_ready` to reset.
    pub fn check_ready(&self, id: Id) -> bool {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .is_some_and(|e| e.ready)
    }
    /// Reset `id` to not-ready (for edge-triggered semantics like interrupts).
    pub fn clear_ready(&mut self, id: Id) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.ready = false;
        }
    }
    /// Add a trackable entry. No-op if `id` already exists.
    pub fn create(&mut self, id: Id) {
        if self.entries.iter().any(|e| e.id == id) {
            return;
        }

        self.entries.push(Entry {
            id,
            ready: false,
            waiter: None,
        });
    }
    /// Remove an entry.
    pub fn destroy(&mut self, id: Id) {
        self.entries.retain(|e| e.id != id);
    }
    /// Mark `id` as ready and return the registered waiter (if any).
    ///
    /// Idempotent: safe to call on already-ready entries (returns None).
    /// The caller is responsible for the two-phase wake — call this under
    /// the module lock, then wake the returned thread under the scheduler lock.
    pub fn notify(&mut self, id: Id) -> Option<ThreadId> {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.ready = true;

            entry.waiter.take()
        } else {
            None
        }
    }
    /// Register a thread as the waiter for `id`.
    pub fn register_waiter(&mut self, id: Id, waiter: ThreadId) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.waiter = Some(waiter);
        }
    }
    /// Clear the waiter for `id`.
    pub fn unregister_waiter(&mut self, id: Id) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.waiter = None;
        }
    }
}
