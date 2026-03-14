//! Test for sys_channel_create PTE leak: when the second map_channel_page
//! fails, the first mapped page's page table entry is not cleaned up.
//!
//! Bug: sys_channel_create calls map_channel_page twice. If the first succeeds
//! but the second fails, the error path closes handles but never calls
//! unmap_channel_page on the VA returned by the first mapping — leaving a
//! leaked page table entry until process destruction.
//!
//! Fix: track va from the first map_channel_page. On second map failure,
//! call unmap_channel_page(va) before closing handles. Same rollback pattern
//! as handle_send (commit d9a32a2).

// Include handle.rs directly — it has zero external dependencies.
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
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}

use std::collections::HashSet;

use handle::*;

// --- Simulated address space with channel SHM bump allocator ---

const PAGE_SIZE: u64 = 4096;
const CHANNEL_SHM_BASE: u64 = 0x4000_0000;
const CHANNEL_SHM_END: u64 = 0x8000_0000;

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

    /// Simulates unmap_channel_page: clears the page table entry at `va`.
    fn unmap_channel_page(&mut self, va: u64) {
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

// --- Model of sys_channel_create (BUGGY version — no PTE rollback) ---

/// Models sys_channel_create as it exists in the kernel BEFORE the PTE fix:
/// handles are properly closed on map failure, but the first page's PTE is
/// NOT unmapped when the second map_channel_page fails.
fn sys_channel_create_buggy(
    handles: &mut HandleTable,
    addr_space: &mut SimAddressSpace,
) -> Result<(Handle, Handle), i64> {
    let ch_a = ChannelId(0);
    let ch_b = ChannelId(1);

    let result: Result<(Handle, Handle), HandleError> = (|| {
        let handle_a = handles.insert(HandleObject::Channel(ch_a), Rights::READ_WRITE)?;

        match handles.insert(HandleObject::Channel(ch_b), Rights::READ_WRITE) {
            Ok(handle_b) => {
                // Both handles inserted — now map shared pages.
                // (Simulating shared_pages lookup always succeeding.)

                if addr_space.map_channel_page(0x1000).is_none() {
                    let _ = handles.close(handle_a);
                    let _ = handles.close(handle_b);
                    return Err(HandleError::TableFull);
                }

                // BUG: no tracking of va_a, no unmap on second failure.
                if addr_space.map_channel_page(0x2000).is_none() {
                    let _ = handles.close(handle_a);
                    let _ = handles.close(handle_b);
                    return Err(HandleError::TableFull);
                }

                Ok((handle_a, handle_b))
            }
            Err(e) => {
                let _ = handles.close(handle_a);
                Err(e)
            }
        }
    })();

    match result {
        Ok((a, b)) => Ok((a, b)),
        Err(e) => Err(e as i64),
    }
}

// --- Model of sys_channel_create (FIXED version — PTE rollback) ---

/// Models sys_channel_create WITH the PTE rollback fix: when the second
/// map_channel_page fails, unmap_channel_page is called on the VA from the
/// first successful mapping.
fn sys_channel_create_fixed(
    handles: &mut HandleTable,
    addr_space: &mut SimAddressSpace,
) -> Result<(Handle, Handle), i64> {
    let ch_a = ChannelId(0);
    let ch_b = ChannelId(1);

    let result: Result<(Handle, Handle), HandleError> = (|| {
        let handle_a = handles.insert(HandleObject::Channel(ch_a), Rights::READ_WRITE)?;

        match handles.insert(HandleObject::Channel(ch_b), Rights::READ_WRITE) {
            Ok(handle_b) => {
                // Both handles inserted — now map shared pages.

                let va_a = match addr_space.map_channel_page(0x1000) {
                    Some(va) => va,
                    None => {
                        let _ = handles.close(handle_a);
                        let _ = handles.close(handle_b);
                        return Err(HandleError::TableFull);
                    }
                };

                if addr_space.map_channel_page(0x2000).is_none() {
                    // FIX: unmap the first page before closing handles.
                    addr_space.unmap_channel_page(va_a);
                    let _ = handles.close(handle_a);
                    let _ = handles.close(handle_b);
                    return Err(HandleError::TableFull);
                }

                Ok((handle_a, handle_b))
            }
            Err(e) => {
                let _ = handles.close(handle_a);
                Err(e)
            }
        }
    })();

    match result {
        Ok((a, b)) => Ok((a, b)),
        Err(e) => Err(e as i64),
    }
}

// ==========================================================================
// Tests
// ==========================================================================

#[test]
fn test_channel_create_pte_leak_second_map_fails_buggy() {
    // Scenario: first map_channel_page succeeds, second fails.
    // Buggy version: the first page's PTE leaks (remains mapped).
    let mut handles = HandleTable::new();
    let mut addr_space = SimAddressSpace::new();
    addr_space.fail_on_map_call = Some(1); // Second map call fails.

    let result = sys_channel_create_buggy(&mut handles, &mut addr_space);

    assert!(
        result.is_err(),
        "channel_create should fail when second map fails"
    );

    // BUG: one page is still mapped — the PTE leaked.
    assert_eq!(
        addr_space.mapped_count(),
        1,
        "buggy: first page's PTE leaked (still mapped in page table)"
    );
    assert!(
        addr_space.is_mapped(CHANNEL_SHM_BASE),
        "buggy: PTE at CHANNEL_SHM_BASE leaked"
    );
}

#[test]
fn test_channel_create_pte_leak_second_map_fails_fixed() {
    // Same scenario with the fix: first page's PTE is cleaned up.
    let mut handles = HandleTable::new();
    let mut addr_space = SimAddressSpace::new();
    addr_space.fail_on_map_call = Some(1); // Second map call fails.

    let result = sys_channel_create_fixed(&mut handles, &mut addr_space);

    assert!(
        result.is_err(),
        "channel_create should fail when second map fails"
    );

    // FIX: no leaked PTEs — first page was unmapped on rollback.
    assert_eq!(
        addr_space.mapped_count(),
        0,
        "fixed: no leaked PTEs after second map failure"
    );
}

#[test]
fn test_channel_create_pte_leak_success_path_no_leak() {
    // Both maps succeed — verify both PTEs exist (no false rollback).
    let mut handles = HandleTable::new();
    let mut addr_space = SimAddressSpace::new();

    let result = sys_channel_create_fixed(&mut handles, &mut addr_space);

    assert!(
        result.is_ok(),
        "channel_create should succeed when both maps succeed"
    );
    assert_eq!(
        addr_space.mapped_count(),
        2,
        "both pages should be mapped on success"
    );
    assert!(addr_space.is_mapped(CHANNEL_SHM_BASE));
    assert!(addr_space.is_mapped(CHANNEL_SHM_BASE + PAGE_SIZE));
}

#[test]
fn test_channel_create_pte_leak_first_map_fails_no_leak() {
    // First map fails — no PTEs should exist (nothing to roll back).
    let mut handles = HandleTable::new();
    let mut addr_space = SimAddressSpace::new();
    addr_space.fail_on_map_call = Some(0); // First map call fails.

    let result = sys_channel_create_fixed(&mut handles, &mut addr_space);

    assert!(
        result.is_err(),
        "channel_create should fail when first map fails"
    );
    assert_eq!(
        addr_space.mapped_count(),
        0,
        "no PTEs should be mapped when first map fails"
    );
}

#[test]
fn test_channel_create_pte_leak_handles_cleaned_up_too() {
    // Verify that the fixed version also cleans up handles (not just PTEs).
    let mut handles = HandleTable::new();
    let mut addr_space = SimAddressSpace::new();
    addr_space.fail_on_map_call = Some(1); // Second map call fails.

    let result = sys_channel_create_fixed(&mut handles, &mut addr_space);

    assert!(result.is_err());

    // Both handle slots should be free after cleanup.
    assert!(
        handles.get(Handle(0), Rights::READ).is_err(),
        "handle_a should be closed after failure"
    );
    assert!(
        handles.get(Handle(1), Rights::READ).is_err(),
        "handle_b should be closed after failure"
    );

    // PTEs should be cleaned up.
    assert_eq!(addr_space.mapped_count(), 0, "no leaked PTEs");
}
