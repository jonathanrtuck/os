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
//!
//! Storage is `Vec<Option<Entry>>` indexed directly by ID. All kernel ID types
//! are sequential integers, so lookup is O(1). Freed slots become `None`.

use super::thread::ThreadId;
use alloc::vec::Vec;
use core::marker::PhantomData;

/// Trait for ID types that can be used as direct indices in `WaitableRegistry`.
///
/// All kernel ID types (ThreadId, ProcessId, TimerId, InterruptId) are
/// sequential integers starting from 0, making them natural array indices.
pub trait WaitableId: Copy + Eq {
    fn index(self) -> usize;
}

struct Entry {
    ready: bool,
    waiter: Option<ThreadId>,
}

/// Tracks readiness and waiter registration for a set of waitable kernel objects.
///
/// Generic over the ID type (ThreadId, ProcessId, TimerId, InterruptId).
/// Uses the ID as a direct Vec index — O(1) for all operations.
pub struct WaitableRegistry<Id: WaitableId> {
    entries: Vec<Option<Entry>>,
    _phantom: PhantomData<Id>,
}

impl<Id: WaitableId> WaitableRegistry<Id> {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Check whether `id` is ready (non-consuming).
    ///
    /// Level-triggered for exit/timer: returns true forever once notified.
    /// Edge-triggered for interrupts: caller uses `clear_ready` to reset.
    pub fn check_ready(&self, id: Id) -> bool {
        self.entries
            .get(id.index())
            .and_then(|slot| slot.as_ref())
            .is_some_and(|e| e.ready)
    }
    /// Reset `id` to not-ready (for edge-triggered semantics like interrupts).
    pub fn clear_ready(&mut self, id: Id) {
        if let Some(Some(entry)) = self.entries.get_mut(id.index()) {
            entry.ready = false;
        }
    }
    /// Add a trackable entry. No-op if `id` already exists.
    pub fn create(&mut self, id: Id) {
        let idx = id.index();

        if idx < self.entries.len() {
            if self.entries[idx].is_some() {
                return;
            }
        } else {
            self.entries.resize_with(idx + 1, || None);
        }

        self.entries[idx] = Some(Entry {
            ready: false,
            waiter: None,
        });
    }
    /// Remove an entry.
    pub fn destroy(&mut self, id: Id) {
        if let Some(slot) = self.entries.get_mut(id.index()) {
            *slot = None;
        }
    }
    /// Mark `id` as ready and return the registered waiter (if any).
    ///
    /// Idempotent: safe to call on already-ready entries (returns None).
    /// The caller is responsible for the two-phase wake — call this under
    /// the module lock, then wake the returned thread under the scheduler lock.
    pub fn notify(&mut self, id: Id) -> Option<ThreadId> {
        let entry = self.entries.get_mut(id.index())?.as_mut()?;

        entry.ready = true;
        entry.waiter.take()
    }
    /// Register a thread as the waiter for `id`.
    pub fn register_waiter(&mut self, id: Id, waiter: ThreadId) {
        if let Some(Some(entry)) = self.entries.get_mut(id.index()) {
            entry.waiter = Some(waiter);
        }
    }
    /// Clear the waiter for `id`.
    pub fn unregister_waiter(&mut self, id: Id) {
        if let Some(Some(entry)) = self.entries.get_mut(id.index()) {
            entry.waiter = None;
        }
    }
}
