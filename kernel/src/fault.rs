//! Page fault handler — COW resolution, lazy allocation, pager dispatch.
//!
//! Handles data aborts from EL0 by inspecting the faulting address against
//! the current address space's mapping table. Determines whether the fault
//! is a COW page, lazy allocation, pager-backed, or an invalid access.
//!
//! VMO page record updates happen unconditionally (not behind cfg gates) so
//! host tests exercise the same state transitions as bare metal. The hardware
//! operations (page allocation, remapping, TLB invalidation) are platform-
//! specific; on host tests, a mock allocator provides unique addresses.

use crate::{frame::state, types::ThreadId};

/// Action the exception handler should take after a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAction {
    Resolved,
    Kill,
}

/// Mock page allocator for host tests — returns unique monotonic addresses.
#[cfg(not(target_os = "none"))]
fn mock_alloc_page() -> Option<usize> {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0x8000_0000);

    Some(NEXT.fetch_add(crate::config::PAGE_SIZE, Ordering::Relaxed))
}

/// Handle a data abort from EL0.
///
/// Looks up the faulting address in the current thread's address space,
/// determines the fault type, and resolves or kills.
pub fn handle_data_abort(current: ThreadId, fault_addr: usize, is_write: bool) -> FaultAction {
    let page_va = fault_addr & !(crate::config::PAGE_SIZE - 1);
    let Some(mut thread) = state::threads().write(current.0) else {
        return FaultAction::Kill;
    };

    if thread.check_repeat_fault(page_va) {
        return FaultAction::Kill;
    }

    drop(thread);

    let Ok(space_id) = crate::syscall::thread_space_id(current) else {
        return FaultAction::Kill;
    };
    let Some(mapping) = state::spaces()
        .read(space_id.0)
        .and_then(|space| space.find_mapping(fault_addr).copied())
    else {
        return FaultAction::Kill;
    };

    if is_write && !mapping.rights.contains(crate::types::Rights::WRITE) {
        return FaultAction::Kill;
    }

    let Some(vmo) = state::vmos().read(mapping.vmo_id.0) else {
        return FaultAction::Kill;
    };
    let page_idx = (fault_addr - mapping.va_start) / crate::config::PAGE_SIZE;

    if vmo.is_sealed() && is_write {
        return FaultAction::Kill;
    }

    let _ = page_va;

    // COW write fault: page exists but is shared with parent.
    if is_write && vmo.page_at(page_idx).is_some() && vmo.cow_parent().is_some() {
        #[cfg(target_os = "none")]
        let new_pa = {
            let space = state::spaces().read(space_id.0).unwrap();
            let root = crate::frame::arch::page_alloc::PhysAddr(space.page_table_root());
            let asid = crate::frame::arch::page_table::Asid(space.asid());
            let page_addr = vmo.page_at(page_idx).unwrap();
            let old_pa = crate::frame::arch::page_alloc::PhysAddr(page_addr);

            crate::frame::fault_resolve::resolve_cow(root, asid, page_va, old_pa)
        };
        #[cfg(not(target_os = "none"))]
        let new_pa = mock_alloc_page();

        // Drop the read guard before taking write access.
        drop(vmo);

        match new_pa {
            Some(pa) => {
                state::vmos()
                    .write(mapping.vmo_id.0)
                    .unwrap()
                    .replace_page(page_idx, pa);

                return FaultAction::Resolved;
            }
            None => return FaultAction::Kill,
        }
    }

    // Existing page not mapped in this space (cross-space mapping, re-fault
    // after TLB eviction, or page committed by another space that shares
    // this VMO), OR permission upgrade (page mapped RX but write fault on
    // a mapping with WRITE rights — e.g. BSS pages on an RWX code VMO).
    if let Some(pa) = vmo.page_at(page_idx) {
        let is_device = vmo.is_device();

        drop(vmo);

        #[cfg(target_os = "none")]
        {
            let space = state::spaces().read(space_id.0).unwrap();
            let root = crate::frame::arch::page_alloc::PhysAddr(space.page_table_root());
            let asid = crate::frame::arch::page_table::Asid(space.asid());
            let has_write = mapping.rights.contains(crate::types::Rights::WRITE);
            let has_exec = mapping.rights.contains(crate::types::Rights::EXECUTE);
            let perms = if is_device {
                crate::frame::arch::page_table::Perms::RW_DEVICE
            } else if is_write && has_write {
                crate::frame::arch::page_table::Perms::RW
            } else if has_exec {
                crate::frame::arch::page_table::Perms::RX
            } else if has_write {
                crate::frame::arch::page_table::Perms::RW
            } else {
                crate::frame::arch::page_table::Perms::RO
            };

            if is_write && has_write && has_exec {
                // Permission upgrade: page was mapped RX (first accessed by
                // read/exec), now needs RW for a write. Use break-before-make
                // to safely transition the valid-to-valid PTE.
                crate::frame::arch::page_table::replace_page(
                    root,
                    asid,
                    crate::frame::arch::page_table::VirtAddr(page_va),
                    crate::frame::arch::page_alloc::PhysAddr(pa),
                    perms,
                );
            } else {
                crate::frame::fault_resolve::resolve_existing(
                    root,
                    page_va,
                    crate::frame::arch::page_alloc::PhysAddr(pa),
                    perms,
                );
            }
        }

        let _ = pa;

        return FaultAction::Resolved;
    }

    // Lazy allocation: page not yet committed, no pager.
    if vmo.page_at(page_idx).is_none() && vmo.pager().is_none() {
        #[cfg(target_os = "none")]
        let new_pa = {
            let space = state::spaces().read(space_id.0).unwrap();
            let root = crate::frame::arch::page_alloc::PhysAddr(space.page_table_root());
            let perms = if mapping.rights.contains(crate::types::Rights::WRITE) {
                crate::frame::arch::page_table::Perms::RW
            } else if mapping.rights.contains(crate::types::Rights::EXECUTE) {
                crate::frame::arch::page_table::Perms::RX
            } else {
                crate::frame::arch::page_table::Perms::RO
            };

            crate::frame::fault_resolve::resolve_lazy(root, page_va, perms)
        };
        #[cfg(not(target_os = "none"))]
        let new_pa = mock_alloc_page();

        // Drop the read guard before taking write access.
        drop(vmo);

        match new_pa {
            Some(pa) => {
                state::vmos()
                    .write(mapping.vmo_id.0)
                    .unwrap()
                    .replace_page(page_idx, pa);

                return FaultAction::Resolved;
            }
            None => {
                return FaultAction::Kill;
            }
        }
    }

    // Pager-backed: page must be fetched from the pager service.
    if vmo.page_at(page_idx).is_none() && vmo.pager().is_some() {
        return FaultAction::Kill;
    }

    FaultAction::Kill
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        address_space::AddressSpace,
        config,
        frame::state,
        thread::Thread,
        types::{AddressSpaceId, Priority, Rights, ThreadId, VmoId},
        vmo::{Vmo, VmoFlags},
    };

    fn setup() {
        crate::frame::arch::page_table::reset_asid_pool();

        state::init(1);

        let space = AddressSpace::new(AddressSpaceId(0), 1, 0);

        state::spaces().alloc_shared(space);

        let thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(0)),
            Priority::Medium,
            0,
            0,
            0,
        );

        state::threads().alloc_shared(thread);
        state::threads()
            .write(0)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Running);
        state::schedulers()
            .core(0)
            .lock()
            .set_current(Some(ThreadId(0)));
    }

    #[test]
    fn unmapped_address_kills() {
        setup();
        let action = handle_data_abort(ThreadId(0), 0xDEAD_0000, false);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn write_to_readonly_kills() {
        setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let action = handle_data_abort(ThreadId(0), va, true);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn lazy_alloc_resolves() {
        setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0)
            .unwrap();
        let action = handle_data_abort(ThreadId(0), va, true);

        assert_eq!(action, FaultAction::Resolved);
    }

    #[test]
    fn lazy_alloc_updates_vmo() {
        setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0)
            .unwrap();

        assert!(state::vmos().read(idx).unwrap().page_at(0).is_none());

        handle_data_abort(ThreadId(0), va, true);

        assert!(state::vmos().read(idx).unwrap().page_at(0).is_some());
    }

    #[test]
    fn cow_resolution_updates_vmo() {
        setup();
        let parent = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (pidx, _) = state::vmos().alloc_shared(parent).unwrap();

        state::vmos()
            .write(pidx)
            .unwrap()
            .alloc_page_at(0, || Some(0x1000))
            .unwrap();

        let snapshot = state::vmos().read(pidx).unwrap().snapshot(VmoId(1));
        let (cidx, _) = state::vmos().alloc_shared(snapshot).unwrap();
        let old_page = state::vmos().read(cidx).unwrap().page_at(0).unwrap();

        assert_eq!(old_page, 0x1000);

        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(cidx), config::PAGE_SIZE, rw, 0)
            .unwrap();

        handle_data_abort(ThreadId(0), va, true);

        let new_page = state::vmos().read(cidx).unwrap().page_at(0).unwrap();

        assert_ne!(new_page, 0x1000);
    }

    #[test]
    fn invalid_thread_kills() {
        setup();
        let action = handle_data_abort(ThreadId(999), 0x1000, false);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn double_fault_resolves_existing_page() {
        setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0)
            .unwrap();

        handle_data_abort(ThreadId(0), va, true);

        let page_after_first = state::vmos().read(idx).unwrap().page_at(0).unwrap();
        // Second fault on same page — page already exists. The "existing page"
        // handler re-maps it without allocating. This happens on TLB eviction
        // or cross-space VMO sharing.
        let action = handle_data_abort(ThreadId(0), va, true);
        let page_after_second = state::vmos().read(idx).unwrap().page_at(0).unwrap();

        assert_eq!(page_after_first, page_after_second);
        assert_eq!(action, FaultAction::Resolved);
    }

    #[test]
    fn sealed_vmo_write_kills() {
        setup();
        let mut vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);

        vmo.seal().unwrap();

        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0)
            .unwrap();
        let action = handle_data_abort(ThreadId(0), va, true);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn lazy_alloc_at_nonzero_page_index() {
        setup();
        let vmo = Vmo::new(VmoId(0), 4 * config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx), 4 * config::PAGE_SIZE, rw, 0)
            .unwrap();
        let action = handle_data_abort(ThreadId(0), va + 2 * config::PAGE_SIZE, true);

        assert_eq!(action, FaultAction::Resolved);
        assert!(state::vmos().read(idx).unwrap().page_at(0).is_none());
        assert!(state::vmos().read(idx).unwrap().page_at(2).is_some());
    }

    #[test]
    fn pager_backed_vmo_returns_kill() {
        setup();
        let mut vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);

        vmo.set_pager(crate::types::EndpointId(0)).unwrap();

        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let action = handle_data_abort(ThreadId(0), va, false);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn repeat_fault_kills_after_threshold() {
        setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE * 2, VmoFlags::NONE);
        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(
                VmoId(idx),
                config::PAGE_SIZE * 2,
                Rights(Rights::READ.0 | Rights::WRITE.0),
                0,
            )
            .unwrap();

        for _ in 0..4 {
            let action = handle_data_abort(ThreadId(0), va, true);

            assert_eq!(action, FaultAction::Resolved);
        }

        let action = handle_data_abort(ThreadId(0), va, true);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn cross_vmo_va_reuse_sees_new_pages() {
        setup();

        let page_count = 64; // > MAX_PAGES_INLINE (32), forces Pages::Heap
        let vmo_size = page_count * config::PAGE_SIZE;
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        // Create VMO A and map it at a fixed VA.
        let vmo_a = Vmo::new(VmoId(0), vmo_size, VmoFlags::NONE);
        let (idx_a, _) = state::vmos().alloc_shared(vmo_a).unwrap();
        let fixed_va = 0x10_0000; // well within user VA range, page-aligned
        let va_a = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx_a), vmo_size, rw, fixed_va)
            .unwrap();

        assert_eq!(va_a, fixed_va);

        // Fault a few pages of VMO A to commit them.
        for page in [0, 1, 2] {
            let fault_addr = va_a + page * config::PAGE_SIZE;
            let action = handle_data_abort(ThreadId(0), fault_addr, true);

            assert_eq!(action, FaultAction::Resolved);
        }

        // Record A's committed page addresses.
        let a_page_0 = state::vmos().read(idx_a).unwrap().page_at(0).unwrap();
        let a_page_1 = state::vmos().read(idx_a).unwrap().page_at(1).unwrap();
        let a_page_2 = state::vmos().read(idx_a).unwrap().page_at(2).unwrap();

        // Unmap VMO A — frees the VA range back to the allocator.
        state::spaces().write(0).unwrap().unmap(va_a).unwrap();

        // Create VMO B (same size) and map it at the SAME VA.
        let vmo_b = Vmo::new(VmoId(1), vmo_size, VmoFlags::NONE);
        let (idx_b, _) = state::vmos().alloc_shared(vmo_b).unwrap();
        let va_b = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(VmoId(idx_b), vmo_size, rw, fixed_va)
            .unwrap();

        assert_eq!(va_b, fixed_va, "VA should be reused after unmap");
        // VMO B should have NO committed pages yet.
        assert!(state::vmos().read(idx_b).unwrap().page_at(0).is_none());
        assert!(state::vmos().read(idx_b).unwrap().page_at(1).is_none());
        assert!(state::vmos().read(idx_b).unwrap().page_at(2).is_none());

        // Fault on VMO B at the same page indices.
        for page in [0, 1, 2] {
            let fault_addr = va_b + page * config::PAGE_SIZE;
            let action = handle_data_abort(ThreadId(0), fault_addr, true);

            assert_eq!(action, FaultAction::Resolved);
        }

        // VMO B should now have its OWN committed pages.
        let b_page_0 = state::vmos().read(idx_b).unwrap().page_at(0).unwrap();
        let b_page_1 = state::vmos().read(idx_b).unwrap().page_at(1).unwrap();
        let b_page_2 = state::vmos().read(idx_b).unwrap().page_at(2).unwrap();

        // B's pages must be DIFFERENT from A's — they are independent VMOs.
        assert_ne!(b_page_0, a_page_0, "page 0: B got A's stale page");
        assert_ne!(b_page_1, a_page_1, "page 1: B got A's stale page");
        assert_ne!(b_page_2, a_page_2, "page 2: B got A's stale page");
        // VMO A's pages should be unaffected.
        assert_eq!(
            state::vmos().read(idx_a).unwrap().page_at(0).unwrap(),
            a_page_0
        );
        assert_eq!(
            state::vmos().read(idx_a).unwrap().page_at(1).unwrap(),
            a_page_1
        );
        assert_eq!(
            state::vmos().read(idx_a).unwrap().page_at(2).unwrap(),
            a_page_2
        );
    }

    #[test]
    fn fault_counter_resets_on_different_page() {
        setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE * 2, VmoFlags::NONE);
        let (idx, _) = state::vmos().alloc_shared(vmo).unwrap();
        let va = state::spaces()
            .write(0)
            .unwrap()
            .map_vmo(
                VmoId(idx),
                config::PAGE_SIZE * 2,
                Rights(Rights::READ.0 | Rights::WRITE.0),
                0,
            )
            .unwrap();

        for _ in 0..3 {
            handle_data_abort(ThreadId(0), va, true);
        }

        let action = handle_data_abort(ThreadId(0), va + config::PAGE_SIZE, true);

        assert_eq!(action, FaultAction::Resolved);
    }
}
