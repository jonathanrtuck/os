//! Test for sys_scheduling_context_create resource leak when handle table is full.
//!
//! Bug: sys_scheduling_context_create calls scheduler::create_scheduling_context()
//! which allocates a SchedulingContext with ref_count=1. Then it tries to insert
//! a handle. If the handle table is full, the error path returns without calling
//! release_scheduling_context() — leaking the context (ref_count stays 1 forever,
//! the slot is never freed, the ID is never returned to the free list).
//!
//! Fix: call release_scheduling_context(ctx_id) on the handle-insert error path.

#[path = "../../kernel/handle.rs"]
mod handle;
mod interrupt {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod process {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessId(pub u32);
}
#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}

use handle::*;
use scheduling_context::*;

// --- Minimal scheduling context store (models scheduler's context table) ---

struct SchedulingContextSlot {
    context: SchedulingContext,
    ref_count: u32,
}

struct ContextStore {
    contexts: Vec<Option<SchedulingContextSlot>>,
    free_ids: Vec<u32>,
}

impl ContextStore {
    fn new() -> Self {
        Self {
            contexts: Vec::new(),
            free_ids: Vec::new(),
        }
    }

    /// Model of scheduler::create_scheduling_context — allocates a context with ref_count=1.
    fn create(&mut self, budget: u64, period: u64) -> Option<SchedulingContextId> {
        if !validate_params(budget, period) {
            return None;
        }

        let context = SchedulingContext::new(budget, period, 0);
        let slot = SchedulingContextSlot {
            context,
            ref_count: 1,
        };

        let id = if let Some(free_id) = self.free_ids.pop() {
            self.contexts[free_id as usize] = Some(slot);
            SchedulingContextId(free_id)
        } else {
            let len = self.contexts.len();
            self.contexts.push(Some(slot));
            SchedulingContextId(len as u32)
        };

        Some(id)
    }

    /// Model of scheduler::release_scheduling_context — decrements ref_count, frees if zero.
    fn release(&mut self, ctx_id: SchedulingContextId) {
        if let Some(slot) = self.contexts.get_mut(ctx_id.0 as usize) {
            if let Some(entry) = slot {
                entry.ref_count = entry.ref_count.saturating_sub(1);
                if entry.ref_count == 0 {
                    *slot = None;
                    self.free_ids.push(ctx_id.0);
                }
            }
        }
    }

    /// Check if a context slot is occupied (not freed).
    fn is_alive(&self, ctx_id: SchedulingContextId) -> bool {
        self.contexts
            .get(ctx_id.0 as usize)
            .map_or(false, |s| s.is_some())
    }

    /// Get the ref_count of a context (None if slot is empty).
    fn ref_count(&self, ctx_id: SchedulingContextId) -> Option<u32> {
        self.contexts
            .get(ctx_id.0 as usize)
            .and_then(|s| s.as_ref().map(|e| e.ref_count))
    }
}

// --- Duplicated Error enum from syscall.rs ---

#[repr(i64)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Error {
    InvalidArgument = -4,
}

// --- Model of sys_scheduling_context_create (mirrors kernel after fix) ---

/// Models sys_scheduling_context_create with the fix applied:
/// releases the scheduling context on handle-insert failure.
fn sys_scheduling_context_create_model(
    store: &mut ContextStore,
    handles: &mut HandleTable,
    budget: u64,
    period: u64,
) -> Result<u64, Error> {
    let ctx_id = store.create(budget, period).ok_or(Error::InvalidArgument)?;

    let handle = handles
        .insert(
            HandleObject::SchedulingContext(ctx_id),
            Rights::READ_WRITE,
        )
        .map_err(|_| {
            // FIX: release the scheduling context on handle-insert failure.
            store.release(ctx_id);
            Error::InvalidArgument
        })?;

    Ok(handle.0 as u64)
}

// ==========================================================================
// Helper: fill the handle table to capacity
// ==========================================================================

/// Fill all 256 handle table slots with dummy channel handles.
fn fill_handle_table(handles: &mut HandleTable) {
    for i in 0..256u32 {
        handles
            .insert(HandleObject::Channel(ChannelId(i)), Rights::READ)
            .expect("should be able to fill handle table");
    }
}

// ==========================================================================
// Tests
// ==========================================================================

#[test]
fn test_sched_context_create_leak_context_freed_on_full_table() {
    // Trigger: fill handle table, then attempt scheduling_context_create.
    // The context is allocated (ref_count=1) but the handle insert fails.
    // After the fix, the context must be released (ref_count decremented to 0,
    // slot freed, ID returned to free list).
    let mut store = ContextStore::new();
    let mut handles = HandleTable::new();

    fill_handle_table(&mut handles);

    let result = sys_scheduling_context_create_model(
        &mut store,
        &mut handles,
        1_000_000, // 1ms budget
        5_000_000, // 5ms period
    );

    assert!(result.is_err(), "should fail when handle table is full");

    // After the fix: the context must be freed.
    let ctx_id = SchedulingContextId(0);
    assert!(
        !store.is_alive(ctx_id),
        "context should be freed after handle table full error"
    );
    assert_eq!(
        store.ref_count(ctx_id),
        None,
        "slot should be None (context freed, ref_count decremented to 0)"
    );
}

#[test]
fn test_sched_context_create_leak_id_reusable_after_cleanup() {
    // After a failed create with proper cleanup, the context ID should be
    // on the free list and reusable for a subsequent create.
    let mut store = ContextStore::new();
    let mut handles = HandleTable::new();

    fill_handle_table(&mut handles);

    let result = sys_scheduling_context_create_model(
        &mut store,
        &mut handles,
        1_000_000,
        5_000_000,
    );
    assert!(result.is_err());

    // Free one handle slot so the next create can succeed.
    let _ = handles.close(Handle(0));

    let result2 = sys_scheduling_context_create_model(
        &mut store,
        &mut handles,
        1_000_000,
        5_000_000,
    );
    assert!(result2.is_ok(), "should succeed after freeing a handle slot");

    // The context ID should have been reused from the free list.
    let ctx_id = SchedulingContextId(0);
    assert!(store.is_alive(ctx_id), "reused context should be alive");
    assert_eq!(store.ref_count(ctx_id), Some(1));
}

#[test]
fn test_sched_context_create_success_path_unaffected() {
    // Normal success path: context created, handle inserted, everything fine.
    let mut store = ContextStore::new();
    let mut handles = HandleTable::new();

    let result = sys_scheduling_context_create_model(
        &mut store,
        &mut handles,
        1_000_000,
        5_000_000,
    );

    assert!(result.is_ok(), "should succeed with empty handle table");
    assert_eq!(result.unwrap(), 0, "first handle should be slot 0");

    let ctx_id = SchedulingContextId(0);
    assert!(store.is_alive(ctx_id));
    assert_eq!(store.ref_count(ctx_id), Some(1));

    // Handle should be valid and reference the scheduling context.
    let obj = handles.get(Handle(0), Rights::READ).unwrap();
    assert_eq!(obj, HandleObject::SchedulingContext(ctx_id));
}

#[test]
fn test_sched_context_create_leak_multiple_failures_dont_accumulate() {
    // Multiple failed creates should not accumulate leaked contexts.
    // Each failed create allocates and then releases a context.
    let mut store = ContextStore::new();
    let mut handles = HandleTable::new();

    fill_handle_table(&mut handles);

    // Fail 5 times.
    for _ in 0..5 {
        let result = sys_scheduling_context_create_model(
            &mut store,
            &mut handles,
            1_000_000,
            5_000_000,
        );
        assert!(result.is_err());
    }

    // No contexts should be alive — all were freed on the error path.
    for i in 0..store.contexts.len() {
        assert!(
            store.contexts[i].is_none(),
            "context {} should be freed after error path",
            i
        );
    }

    // The free list should contain the IDs for reuse.
    // (Only 1 ID was ever allocated because it's freed and reused each time.)
    assert!(
        !store.free_ids.is_empty() || store.contexts.is_empty(),
        "free IDs should be available for reuse"
    );
}
