//! Page fault handler — COW resolution, lazy allocation, pager dispatch.
//!
//! Handles data aborts from EL0 by inspecting the faulting address against
//! the current address space's mapping table. Determines whether the fault
//! is a COW page, lazy allocation, pager-backed, or an invalid access.

use crate::{syscall::Kernel, types::ThreadId};

/// Action the exception handler should take after a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAction {
    Resolved,
    Kill,
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
    let space_id = match kernel.thread_space_id(current) {
        Ok(id) => id,
        Err(_) => return FaultAction::Kill,
    };

    let mapping = match kernel.spaces.get(space_id.0) {
        Some(space) => space.find_mapping(fault_addr).cloned(),
        None => return FaultAction::Kill,
    };

    let mapping = match mapping {
        Some(m) => m,
        None => return FaultAction::Kill,
    };

    if is_write && !mapping.rights.contains(crate::types::Rights::WRITE) {
        return FaultAction::Kill;
    }

    let vmo = match kernel.vmos.get(mapping.vmo_id.0) {
        Some(v) => v,
        None => return FaultAction::Kill,
    };

    let page_idx = (fault_addr - mapping.va_start) / crate::config::PAGE_SIZE;

    if vmo.is_sealed() && is_write {
        return FaultAction::Kill;
    }

    if is_write && vmo.page_at(page_idx).is_some() && vmo.cow_parent().is_some() {
        // COW fault: page exists but is shared with parent.
        // Real implementation: allocate new page, copy, remap writable.
        // For now, return Resolved as a placeholder.
        return FaultAction::Resolved;
    }

    if vmo.page_at(page_idx).is_none() && vmo.pager().is_none() {
        // Lazy allocation: no page, no pager — allocate and zero-fill.
        // Real implementation: call page_alloc, zero, map in page table.
        return FaultAction::Resolved;
    }

    if vmo.page_at(page_idx).is_none() && vmo.pager().is_some() {
        // Pager-backed: send fault to pager endpoint.
        // Real implementation: enqueue fault message on pager endpoint.
        return FaultAction::Resolved;
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
        let idx = k.vmos.alloc(vmo).unwrap();

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
        let idx = k.vmos.alloc(vmo).unwrap();

        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let space = k.spaces.get_mut(0).unwrap();
        let va = space.map_vmo(VmoId(idx), config::PAGE_SIZE, rw, 0).unwrap();

        let action = handle_data_abort(&mut k, ThreadId(0), va, true);
        assert_eq!(action, FaultAction::Resolved);
    }

    #[test]
    fn invalid_thread_kills() {
        let mut k = setup();
        let action = handle_data_abort(&mut k, ThreadId(999), 0x1000, false);
        assert_eq!(action, FaultAction::Kill);
    }
}
