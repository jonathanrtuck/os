#![feature(allocator_api)]
//! Test for sys_handle_send partial rollback: when channel SHM pages are mapped
//! into the target process but a subsequent step fails, the mapped pages should
//! be unmapped (rolled back).
//!
//! Bug: sys_handle_send maps channel SHM pages into the target address space
//! using map_channel_page (bump allocator). If a subsequent step fails (either
//! the second map_channel_page or handles.insert), the already-mapped pages are
//! NOT unmapped. The page table entries leak until process destruction.
//!
//! Two failure scenarios:
//! 1. First map_channel_page succeeds, second fails → one page leaked in target
//! 2. Both map_channel_page succeed, handles.insert fails → two pages leaked
//!
//! Fix: unmap already-mapped channel pages on the error path within Phase 2.
//! Note: the bump VA is consumed either way (same as DMA/heap bump allocators).
//! The fix addresses the page table entry leak, not the VA consumption.

// Include handle.rs directly — it has zero external dependencies.
mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}
#[path = "../../paging.rs"]
mod paging;
#[path = "../../handle.rs"]
mod handle;
mod interrupt {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod process {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessId(pub u32);
}
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
#[path = "../../scheduling_context.rs"]
mod scheduling_context;
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}
mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}

use std::collections::HashSet;

use handle::*;

// --- Simulated address space with channel SHM bump allocator ---

mod system_config {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));
}

const PAGE_SIZE: u64 = system_config::PAGE_SIZE;
const CHANNEL_SHM_BASE: u64 = system_config::CHANNEL_SHM_BASE;
const CHANNEL_SHM_END: u64 = system_config::USER_STACK_TOP; // SHM ends where stack region begins

/// Models the per-process address space's channel SHM mapping behavior.
/// Tracks which VAs have mapped pages (simulating page table entries).
struct SimAddressSpace {
    next_channel_shm_va: u64,
    /// Set of VAs that have active page table entries (mapped pages).
    mapped_pages: HashSet<u64>,
    /// If set, the Nth call to map_channel_page will fail (0-indexed).
    fail_on_map_call: Option<usize>,
    /// Counter of map_channel_page calls.
    map_call_count: usize,
}

impl SimAddressSpace {
    fn new() -> Self {
        Self {
            next_channel_shm_va: CHANNEL_SHM_BASE,
            mapped_pages: HashSet::new(),
            fail_on_map_call: None,
            map_call_count: 0,
        }
    }

    /// Simulates map_channel_page: bump-allocates VA, maps the page.
    fn map_channel_page(&mut self, _pa: u64) -> Option<u64> {
        let call_nr = self.map_call_count;
        self.map_call_count += 1;

        // Simulate failure on specific call.
        if self.fail_on_map_call == Some(call_nr) {
            return None;
        }

        let va = self.next_channel_shm_va;
        if va + PAGE_SIZE > CHANNEL_SHM_END {
            return None;
        }

        self.mapped_pages.insert(va);
        self.next_channel_shm_va = va + PAGE_SIZE;
        Some(va)
    }

    /// Simulates unmap_page_inner: clears the page table entry at `va`.
    fn unmap_page(&mut self, va: u64) {
        self.mapped_pages.remove(&va);
    }

    /// Returns the number of currently mapped pages.
    fn mapped_count(&self) -> usize {
        self.mapped_pages.len()
    }

    /// Returns whether a specific VA has a mapped page.
    fn is_mapped(&self, va: u64) -> bool {
        self.mapped_pages.contains(&va)
    }
}

// --- Simulated channel shared pages ---

struct ChannelPages {
    pa_a: u64,
    pa_b: u64,
}

// --- Error types (mirrors kernel) ---

#[repr(i64)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Error {
    InvalidArgument = -4,
    OutOfMemory = -7,
}

// --- Model of sys_handle_send Phase 2 (BUGGY version) ---

/// Models Phase 2 of sys_handle_send WITHOUT rollback for mapped pages.
/// This is the buggy behavior: if map fails or handle insert fails, already-
/// mapped pages are not unmapped.
fn handle_send_phase2_buggy(
    target_handles: &mut HandleTable,
    target_addr_space: &mut SimAddressSpace,
    channel_pages: Option<&ChannelPages>,
    source_obj: HandleObject,
    source_rights: Rights,
    target_started: bool,
) -> Result<(), Error> {
    if target_started {
        return Err(Error::InvalidArgument);
    }

    if let Some(pages) = channel_pages {
        target_addr_space
            .map_channel_page(pages.pa_a)
            .ok_or(Error::OutOfMemory)?;
        target_addr_space
            .map_channel_page(pages.pa_b)
            .ok_or(Error::OutOfMemory)?;
    }

    target_handles
        .insert(source_obj, source_rights)
        .map_err(|_| Error::InvalidArgument)?;

    Ok(())
}

/// Models Phase 2 of sys_handle_send WITH rollback for mapped pages.
/// This is the fixed behavior: track mapped VAs, unmap on failure.
fn handle_send_phase2_fixed(
    target_handles: &mut HandleTable,
    target_addr_space: &mut SimAddressSpace,
    channel_pages: Option<&ChannelPages>,
    source_obj: HandleObject,
    source_rights: Rights,
    target_started: bool,
) -> Result<(), Error> {
    if target_started {
        return Err(Error::InvalidArgument);
    }

    if let Some(pages) = channel_pages {
        let va_a = target_addr_space
            .map_channel_page(pages.pa_a)
            .ok_or(Error::OutOfMemory)?;

        match target_addr_space.map_channel_page(pages.pa_b) {
            Some(_va_b) => {
                // Both pages mapped. Try handle insert.
                if let Err(_) = target_handles.insert(source_obj, source_rights) {
                    // Handle insert failed — unmap both pages.
                    target_addr_space.unmap_page(va_a);
                    target_addr_space.unmap_page(_va_b);
                    return Err(Error::InvalidArgument);
                }
            }
            None => {
                // Second map failed — unmap first page.
                target_addr_space.unmap_page(va_a);
                return Err(Error::OutOfMemory);
            }
        }
    } else {
        // Non-channel handle — just insert.
        target_handles
            .insert(source_obj, source_rights)
            .map_err(|_| Error::InvalidArgument)?;
    }

    Ok(())
}

// ==========================================================================
// Tests — exercise the bug
// ==========================================================================

#[test]
fn test_handle_send_rollback_second_map_fails_buggy_leaks() {
    // Scenario: first map_channel_page succeeds, second fails.
    // Buggy version: the first mapped page is NOT unmapped.
    let mut target_handles = HandleTable::new();
    let mut target_addr = SimAddressSpace::new();
    target_addr.fail_on_map_call = Some(1); // Second call fails.

    let pages = ChannelPages {
        pa_a: 0x1000,
        pa_b: 0x2000,
    };

    let result = handle_send_phase2_buggy(
        &mut target_handles,
        &mut target_addr,
        Some(&pages),
        HandleObject::Channel(ChannelId(0)),
        Rights::READ_WRITE,
        false,
    );

    assert!(result.is_err(), "should fail when second map fails");

    // BUG: one page is still mapped — it leaked.
    assert_eq!(
        target_addr.mapped_count(),
        1,
        "buggy: first mapped page leaked (still in page table)"
    );
    assert!(
        target_addr.is_mapped(CHANNEL_SHM_BASE),
        "buggy: page at CHANNEL_SHM_BASE leaked"
    );
}

#[test]
fn test_handle_send_rollback_second_map_fails_fixed_cleans_up() {
    // Same scenario with the fix applied: first mapped page IS unmapped.
    let mut target_handles = HandleTable::new();
    let mut target_addr = SimAddressSpace::new();
    target_addr.fail_on_map_call = Some(1);

    let pages = ChannelPages {
        pa_a: 0x1000,
        pa_b: 0x2000,
    };

    let result = handle_send_phase2_fixed(
        &mut target_handles,
        &mut target_addr,
        Some(&pages),
        HandleObject::Channel(ChannelId(0)),
        Rights::READ_WRITE,
        false,
    );

    assert!(result.is_err(), "should fail when second map fails");

    // Fix: mapped page is cleaned up.
    assert_eq!(
        target_addr.mapped_count(),
        0,
        "fixed: no leaked pages after second map failure"
    );
}

#[test]
fn test_handle_send_rollback_handle_insert_fails_buggy_leaks() {
    // Scenario: both map_channel_page succeed, but handles.insert fails
    // because the target handle table is full.
    let mut target_handles = HandleTable::new();
    // Fill the target handle table.
    for i in 0..handle::MAX_HANDLES as u32 {
        target_handles
            .insert(HandleObject::Channel(ChannelId(i + 100)), Rights::READ)
            .expect("fill target handles");
    }

    let mut target_addr = SimAddressSpace::new();

    let pages = ChannelPages {
        pa_a: 0x1000,
        pa_b: 0x2000,
    };

    let result = handle_send_phase2_buggy(
        &mut target_handles,
        &mut target_addr,
        Some(&pages),
        HandleObject::Channel(ChannelId(0)),
        Rights::READ_WRITE,
        false,
    );

    assert!(result.is_err(), "should fail when handle table is full");

    // BUG: both pages are still mapped — they leaked.
    assert_eq!(
        target_addr.mapped_count(),
        2,
        "buggy: both mapped pages leaked"
    );
    assert!(target_addr.is_mapped(CHANNEL_SHM_BASE));
    assert!(target_addr.is_mapped(CHANNEL_SHM_BASE + PAGE_SIZE));
}

#[test]
fn test_handle_send_rollback_handle_insert_fails_fixed_cleans_up() {
    // Same scenario with the fix: both pages are unmapped on insert failure.
    let mut target_handles = HandleTable::new();
    for i in 0..handle::MAX_HANDLES as u32 {
        target_handles
            .insert(HandleObject::Channel(ChannelId(i + 100)), Rights::READ)
            .expect("fill target handles");
    }

    let mut target_addr = SimAddressSpace::new();

    let pages = ChannelPages {
        pa_a: 0x1000,
        pa_b: 0x2000,
    };

    let result = handle_send_phase2_fixed(
        &mut target_handles,
        &mut target_addr,
        Some(&pages),
        HandleObject::Channel(ChannelId(0)),
        Rights::READ_WRITE,
        false,
    );

    assert!(result.is_err(), "should fail when handle table is full");

    // Fix: both mapped pages are cleaned up.
    assert_eq!(
        target_addr.mapped_count(),
        0,
        "fixed: no leaked pages after handle insert failure"
    );
}

#[test]
fn test_handle_send_rollback_success_path_unaffected() {
    // Normal success: both pages mapped, handle inserted. All good.
    let mut target_handles = HandleTable::new();
    let mut target_addr = SimAddressSpace::new();

    let pages = ChannelPages {
        pa_a: 0x1000,
        pa_b: 0x2000,
    };

    let result = handle_send_phase2_fixed(
        &mut target_handles,
        &mut target_addr,
        Some(&pages),
        HandleObject::Channel(ChannelId(0)),
        Rights::READ_WRITE,
        false,
    );

    assert!(result.is_ok(), "success path should work");
    assert_eq!(target_addr.mapped_count(), 2, "both pages should be mapped");
    assert!(target_handles.get(Handle(0), Rights::READ).is_ok());
}

#[test]
fn test_handle_send_rollback_non_channel_handle_unaffected() {
    // Sending a non-channel handle (Timer, Interrupt, etc.) has no SHM mapping.
    // Verify the fixed version doesn't break this path.
    let mut target_handles = HandleTable::new();
    let mut target_addr = SimAddressSpace::new();

    let result = handle_send_phase2_fixed(
        &mut target_handles,
        &mut target_addr,
        None,
        HandleObject::SchedulingContext(scheduling_context::SchedulingContextId(0)),
        Rights::READ_WRITE,
        false,
    );

    assert!(result.is_ok(), "non-channel send should succeed");
    assert_eq!(
        target_addr.mapped_count(),
        0,
        "no pages mapped for non-channel"
    );
}

#[test]
fn test_handle_send_rollback_target_started_returns_error() {
    // Sending to a started process should fail immediately (no mapping attempted).
    let mut target_handles = HandleTable::new();
    let mut target_addr = SimAddressSpace::new();

    let pages = ChannelPages {
        pa_a: 0x1000,
        pa_b: 0x2000,
    };

    let result = handle_send_phase2_fixed(
        &mut target_handles,
        &mut target_addr,
        Some(&pages),
        HandleObject::Channel(ChannelId(0)),
        Rights::READ_WRITE,
        true, // started
    );

    assert_eq!(result, Err(Error::InvalidArgument));
    assert_eq!(
        target_addr.mapped_count(),
        0,
        "no pages mapped when target is started"
    );
}
