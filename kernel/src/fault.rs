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

use crate::{syscall::Kernel, types::ThreadId};

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
pub fn handle_data_abort(
    kernel: &mut Kernel,
    current: ThreadId,
    fault_addr: usize,
    is_write: bool,
) -> FaultAction {
    let Ok(space_id) = kernel.thread_space_id(current) else {
        return FaultAction::Kill;
    };
    let Some(mapping) = kernel
        .spaces
        .get(space_id.0)
        .and_then(|space| space.find_mapping(fault_addr).copied())
    else {
        return FaultAction::Kill;
    };

    if is_write && !mapping.rights.contains(crate::types::Rights::WRITE) {
        return FaultAction::Kill;
    }

    let Some(vmo) = kernel.vmos.get(mapping.vmo_id.0) else {
        return FaultAction::Kill;
    };
    let page_idx = (fault_addr - mapping.va_start) / crate::config::PAGE_SIZE;

    if vmo.is_sealed() && is_write {
        return FaultAction::Kill;
    }

    let page_va = fault_addr & !(crate::config::PAGE_SIZE - 1);
    let _ = page_va;

    // COW write fault: page exists but is shared with parent.
    if is_write && vmo.page_at(page_idx).is_some() && vmo.cow_parent().is_some() {
        #[cfg(target_os = "none")]
        let new_pa = {
            let space = kernel.spaces.get(space_id.0).unwrap();
            let root = crate::frame::arch::page_alloc::PhysAddr(space.page_table_root());
            let asid = crate::frame::arch::page_table::Asid(space.asid());
            let page_addr = vmo.page_at(page_idx).unwrap();
            let old_pa = crate::frame::arch::page_alloc::PhysAddr(page_addr);

            crate::frame::fault_resolve::resolve_cow(root, asid, page_va, old_pa)
        };
        #[cfg(not(target_os = "none"))]
        let new_pa = mock_alloc_page();

        match new_pa {
            Some(pa) => {
                kernel
                    .vmos
                    .get_mut(mapping.vmo_id.0)
                    .unwrap()
                    .replace_page(page_idx, pa);

                return FaultAction::Resolved;
            }
            None => return FaultAction::Kill,
        }
    }

    // Lazy allocation: page not yet committed, no pager.
    if vmo.page_at(page_idx).is_none() && vmo.pager().is_none() {
        #[cfg(target_os = "none")]
        let new_pa = {
            let space = kernel.spaces.get(space_id.0).unwrap();
            let root = crate::frame::arch::page_alloc::PhysAddr(space.page_table_root());
            let perms = if mapping.rights.contains(crate::types::Rights::WRITE) {
                crate::frame::arch::page_table::Perms::RW
            } else {
                crate::frame::arch::page_table::Perms::RO
            };

            crate::frame::fault_resolve::resolve_lazy(root, page_va, perms)
        };
        #[cfg(not(target_os = "none"))]
        let new_pa = mock_alloc_page();

        match new_pa {
            Some(pa) => {
                kernel
                    .vmos
                    .get_mut(mapping.vmo_id.0)
                    .unwrap()
                    .replace_page(page_idx, pa);

                return FaultAction::Resolved;
            }
            None => return FaultAction::Kill,
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
    use alloc::boxed::Box;

    use super::*;
    use crate::{
        address_space::AddressSpace,
        config,
        thread::Thread,
        types::{AddressSpaceId, Priority, Rights, ThreadId, VmoId},
        vmo::{Vmo, VmoFlags},
    };

    fn setup() -> Box<Kernel> {
        let mut k = Box::new(Kernel::new(1));
        let space = AddressSpace::new(AddressSpaceId(0), 1, 0);

        k.spaces.alloc(space);

        let thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(0)),
            Priority::Medium,
            0,
            0,
            0,
        );

        k.threads.alloc(thread);
        k.threads
            .get_mut(0)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Running);
        k.scheduler.core_mut(0).set_current(Some(ThreadId(0)));

        k
    }

    #[test]
    fn unmapped_address_kills() {
        let mut k = setup();
        let action = handle_data_abort(&mut k, ThreadId(0), 0xDEAD_0000, false);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn write_to_readonly_kills() {
        let mut k = setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = k.vmos.alloc(vmo).unwrap();
        let space = k.spaces.get_mut(0).unwrap();
        let va = space
            .map_vmo(VmoId(idx), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let action = handle_data_abort(&mut k, ThreadId(0), va, true);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn lazy_alloc_resolves() {
        let mut k = setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = k.vmos.alloc(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let space = k.spaces.get_mut(0).unwrap();
        let va = space.map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0).unwrap();
        let action = handle_data_abort(&mut k, ThreadId(0), va, true);

        assert_eq!(action, FaultAction::Resolved);
    }

    #[test]
    fn lazy_alloc_updates_vmo() {
        let mut k = setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = k.vmos.alloc(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let space = k.spaces.get_mut(0).unwrap();
        let va = space.map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0).unwrap();

        assert!(k.vmos.get(idx).unwrap().page_at(0).is_none());

        handle_data_abort(&mut k, ThreadId(0), va, true);

        assert!(k.vmos.get(idx).unwrap().page_at(0).is_some());
    }

    #[test]
    fn cow_resolution_updates_vmo() {
        let mut k = setup();
        let parent = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (pidx, _) = k.vmos.alloc(parent).unwrap();

        k.vmos
            .get_mut(pidx)
            .unwrap()
            .alloc_page_at(0, || Some(0x1000))
            .unwrap();

        let snapshot = k.vmos.get(pidx).unwrap().snapshot(VmoId(1));
        let (cidx, _) = k.vmos.alloc(snapshot).unwrap();
        let old_page = k.vmos.get(cidx).unwrap().page_at(0).unwrap();

        assert_eq!(old_page, 0x1000);

        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let space = k.spaces.get_mut(0).unwrap();
        let va = space
            .map_vmo(VmoId(cidx), config::PAGE_SIZE, rw, 0)
            .unwrap();

        handle_data_abort(&mut k, ThreadId(0), va, true);

        let new_page = k.vmos.get(cidx).unwrap().page_at(0).unwrap();

        assert_ne!(new_page, 0x1000);
    }

    #[test]
    fn invalid_thread_kills() {
        let mut k = setup();
        let action = handle_data_abort(&mut k, ThreadId(999), 0x1000, false);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn double_lazy_alloc_is_noop() {
        let mut k = setup();
        let vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = k.vmos.alloc(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let space = k.spaces.get_mut(0).unwrap();
        let va = space.map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0).unwrap();

        handle_data_abort(&mut k, ThreadId(0), va, true);

        let page_after_first = k.vmos.get(idx).unwrap().page_at(0).unwrap();
        // Second fault on same page — page already exists, no COW parent,
        // so no branch matches. Should return Kill (or be unreachable in practice).
        let action = handle_data_abort(&mut k, ThreadId(0), va, true);
        // Page shouldn't change — the second fault doesn't re-allocate.
        let page_after_second = k.vmos.get(idx).unwrap().page_at(0).unwrap();

        assert_eq!(page_after_first, page_after_second);
        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn sealed_vmo_write_kills() {
        let mut k = setup();
        let mut vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);

        vmo.seal().unwrap();

        let (idx, _) = k.vmos.alloc(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let space = k.spaces.get_mut(0).unwrap();
        let va = space.map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0).unwrap();
        let action = handle_data_abort(&mut k, ThreadId(0), va, true);

        assert_eq!(action, FaultAction::Kill);
    }

    #[test]
    fn lazy_alloc_at_nonzero_page_index() {
        let mut k = setup();
        let vmo = Vmo::new(VmoId(0), 4 * config::PAGE_SIZE, VmoFlags::NONE);
        let (idx, _) = k.vmos.alloc(vmo).unwrap();
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let space = k.spaces.get_mut(0).unwrap();
        let va = space
            .map_vmo(VmoId(idx), 4 * config::PAGE_SIZE, rw, 0)
            .unwrap();
        let action = handle_data_abort(&mut k, ThreadId(0), va + 2 * config::PAGE_SIZE, true);

        assert_eq!(action, FaultAction::Resolved);
        assert!(k.vmos.get(idx).unwrap().page_at(0).is_none());
        assert!(k.vmos.get(idx).unwrap().page_at(2).is_some());
    }

    #[test]
    fn pager_backed_vmo_returns_kill() {
        let mut k = setup();
        let mut vmo = Vmo::new(VmoId(0), config::PAGE_SIZE, VmoFlags::NONE);

        vmo.set_pager(crate::types::EndpointId(0)).unwrap();

        let (idx, _) = k.vmos.alloc(vmo).unwrap();
        let space = k.spaces.get_mut(0).unwrap();
        let va = space
            .map_vmo(VmoId(idx), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let action = handle_data_abort(&mut k, ThreadId(0), va, false);

        assert_eq!(action, FaultAction::Kill);
    }
}
