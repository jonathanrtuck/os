//! Address Space — isolation boundary with VA allocation and mapping management.
//!
//! An address space is a page table root + handle table + mapping set. It is
//! the lifecycle boundary: destroying a space kills its threads, closes its
//! handles, and unmaps its VMOs. No separate "process" kernel object exists.

use alloc::vec::Vec;

use crate::{
    config,
    handle::HandleTable,
    types::{AddressSpaceId, Rights, SyscallError, VmoId},
};

/// First user virtual address (skip null guard page).
const USER_VA_BASE: usize = config::PAGE_SIZE;

/// End of user VA range: 2^36 = 64 GiB (T0SZ=28, 16 KiB granule).
const USER_VA_END: usize = 1 << 36;

/// Total usable user VA space.
const USER_VA_SIZE: usize = USER_VA_END - USER_VA_BASE;

/// A record of a VMO mapped into an address space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingRecord {
    pub vmo_id: VmoId,
    pub va_start: usize,
    pub size: usize,
    pub rights: Rights,
}

/// First-fit VA allocator over a sorted free list.
///
/// Free regions are `(start, length)` pairs kept sorted by start address.
/// Allocation is first-fit; deallocation coalesces adjacent regions.
pub struct VaAllocator {
    free_list: Vec<(usize, usize)>,
}

impl VaAllocator {
    pub fn new(base: usize, size: usize) -> Self {
        let mut free_list = Vec::new();

        if size > 0 {
            free_list.push((base, size));
        }

        VaAllocator { free_list }
    }

    /// Allocate a VA range. `hint=0`: first-fit. `hint!=0`: reserve exact address.
    /// Size is page-aligned internally.
    pub fn allocate(&mut self, size: usize, hint: usize) -> Result<usize, SyscallError> {
        let aligned = size.next_multiple_of(config::PAGE_SIZE);

        if aligned == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        if hint != 0 {
            return self.allocate_fixed(hint, aligned);
        }

        self.allocate_first_fit(aligned)
    }

    fn allocate_first_fit(&mut self, size: usize) -> Result<usize, SyscallError> {
        for i in 0..self.free_list.len() {
            let (start, len) = self.free_list[i];

            if len >= size {
                if len == size {
                    self.free_list.remove(i);
                } else {
                    self.free_list[i] = (start + size, len - size);
                }

                return Ok(start);
            }
        }

        Err(SyscallError::OutOfMemory)
    }

    fn allocate_fixed(&mut self, addr: usize, size: usize) -> Result<usize, SyscallError> {
        if !addr.is_multiple_of(config::PAGE_SIZE) {
            return Err(SyscallError::InvalidArgument);
        }
        let end = addr
            .checked_add(size)
            .ok_or(SyscallError::InvalidArgument)?;

        for i in 0..self.free_list.len() {
            let (start, len) = self.free_list[i];
            let region_end = start + len;

            if addr >= start && end <= region_end {
                self.free_list.remove(i);

                if end < region_end {
                    self.insert_sorted(end, region_end - end);
                }
                if addr > start {
                    self.insert_sorted(start, addr - start);
                }

                return Ok(addr);
            }
        }

        Err(SyscallError::InvalidArgument)
    }

    /// Free a VA range, coalescing with adjacent free regions.
    pub fn free(&mut self, addr: usize, size: usize) {
        let pos = self.free_list.partition_point(|&(s, _)| s < addr);

        self.free_list.insert(pos, (addr, size));

        if pos + 1 < self.free_list.len() {
            let (cur_start, cur_len) = self.free_list[pos];
            let (next_start, next_len) = self.free_list[pos + 1];

            if cur_start + cur_len == next_start {
                self.free_list[pos] = (cur_start, cur_len + next_len);
                self.free_list.remove(pos + 1);
            }
        }
        if pos > 0 {
            let (prev_start, prev_len) = self.free_list[pos - 1];
            let (cur_start, cur_len) = self.free_list[pos];

            if prev_start + prev_len == cur_start {
                self.free_list[pos - 1] = (prev_start, prev_len + cur_len);
                self.free_list.remove(pos);
            }
        }
    }

    fn insert_sorted(&mut self, start: usize, len: usize) {
        let pos = self.free_list.partition_point(|&(s, _)| s < start);

        self.free_list.insert(pos, (start, len));
    }

    pub fn free_regions(&self) -> &[(usize, usize)] {
        &self.free_list
    }
}

/// Callback for tearing down threads during address space destruction.
/// Breaks the circular dependency with the thread module.
pub trait DestroyCallback {
    fn kill_threads_in_space(&mut self, space_id: AddressSpaceId);
}

/// An address space — isolation boundary and lifecycle owner.
///
/// Contains a page table root (physical address), ASID for TLB tagging,
/// a per-space handle table, VA allocator, and mapping records.
pub struct AddressSpace {
    pub id: AddressSpaceId,
    asid: u8,
    page_table_root: usize,
    handles: HandleTable,
    mappings: Vec<MappingRecord>,
    va_allocator: VaAllocator,
}

#[allow(clippy::new_without_default)]
impl AddressSpace {
    pub fn new(id: AddressSpaceId, asid: u8, page_table_root: usize) -> Self {
        AddressSpace {
            id,
            asid,
            page_table_root,
            handles: HandleTable::new(),
            mappings: Vec::new(),
            va_allocator: VaAllocator::new(USER_VA_BASE, USER_VA_SIZE),
        }
    }

    pub fn asid(&self) -> u8 {
        self.asid
    }

    pub fn page_table_root(&self) -> usize {
        self.page_table_root
    }

    pub fn set_page_table(&mut self, root: usize, asid: u8) {
        self.page_table_root = root;
        self.asid = asid;
    }

    pub fn handles(&self) -> &HandleTable {
        &self.handles
    }

    pub fn handles_mut(&mut self) -> &mut HandleTable {
        &mut self.handles
    }

    pub fn mappings(&self) -> &[MappingRecord] {
        &self.mappings
    }

    pub fn mapping_count(&self) -> usize {
        self.mappings.len()
    }

    /// Map a VMO region into this address space.
    /// `addr_hint=0`: kernel picks the address (first-fit).
    /// `addr_hint!=0`: map at that exact address (must be free and page-aligned).
    pub fn map_vmo(
        &mut self,
        vmo_id: VmoId,
        size: usize,
        rights: Rights,
        addr_hint: usize,
    ) -> Result<usize, SyscallError> {
        if self.mappings.len() >= config::MAX_MAPPINGS {
            return Err(SyscallError::OutOfMemory);
        }

        let aligned_size = size.next_multiple_of(config::PAGE_SIZE);

        if aligned_size == 0 {
            return Err(SyscallError::InvalidArgument);
        }

        let va = self.va_allocator.allocate(aligned_size, addr_hint)?;

        self.mappings.push(MappingRecord {
            vmo_id,
            va_start: va,
            size: aligned_size,
            rights,
        });

        Ok(va)
    }

    /// Unmap a region by its start virtual address.
    pub fn unmap(&mut self, addr: usize) -> Result<MappingRecord, SyscallError> {
        let pos = self
            .mappings
            .iter()
            .position(|m| m.va_start == addr)
            .ok_or(SyscallError::NotFound)?;
        let record = self.mappings.swap_remove(pos);

        self.va_allocator.free(record.va_start, record.size);

        Ok(record)
    }

    /// Find the mapping containing `addr` (for page fault handling).
    pub fn find_mapping(&self, addr: usize) -> Option<&MappingRecord> {
        self.mappings
            .iter()
            .find(|m| addr >= m.va_start && addr < m.va_start + m.size)
    }

    /// Destroy the address space. Kills threads via callback, returns
    /// mappings and handle table for the caller to clean up VMO refcounts
    /// and trigger peer-closed events.
    pub fn destroy(self, callback: &mut dyn DestroyCallback) -> (Vec<MappingRecord>, HandleTable) {
        callback.kill_threads_in_space(self.id);

        (self.mappings, self.handles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ObjectType;

    fn make_space(id: u32) -> AddressSpace {
        AddressSpace::new(AddressSpaceId(id), (id + 1) as u8, 0xDEAD_0000)
    }

    struct NoopCallback;
    impl DestroyCallback for NoopCallback {
        fn kill_threads_in_space(&mut self, _: AddressSpaceId) {}
    }

    // -- AddressSpace lifecycle --

    #[test]
    fn create_and_inspect() {
        let space = make_space(0);

        assert_eq!(space.id, AddressSpaceId(0));
        assert_eq!(space.asid(), 1);
        assert_eq!(space.page_table_root(), 0xDEAD_0000);
        assert_eq!(space.mapping_count(), 0);
    }

    #[test]
    fn map_vmo_auto_allocate() {
        let mut space = make_space(0);
        let va = space
            .map_vmo(VmoId(0), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();

        assert_eq!(va, USER_VA_BASE);
        assert_eq!(space.mapping_count(), 1);

        let m = &space.mappings()[0];

        assert_eq!(m.vmo_id, VmoId(0));
        assert_eq!(m.rights, Rights::READ);
    }

    #[test]
    fn map_vmo_with_hint() {
        let mut space = make_space(0);
        let hint = USER_VA_BASE + 4 * config::PAGE_SIZE;
        let va = space
            .map_vmo(VmoId(0), config::PAGE_SIZE, Rights::READ, hint)
            .unwrap();

        assert_eq!(va, hint);
    }

    #[test]
    fn map_vmo_hint_unaligned() {
        let mut space = make_space(0);

        assert_eq!(
            space.map_vmo(VmoId(0), config::PAGE_SIZE, Rights::READ, 0x1234),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn map_vmo_zero_size() {
        let mut space = make_space(0);

        assert_eq!(
            space.map_vmo(VmoId(0), 0, Rights::READ, 0),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn unmap_frees_va_for_reuse() {
        let mut space = make_space(0);
        let va = space
            .map_vmo(VmoId(0), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let record = space.unmap(va).unwrap();

        assert_eq!(record.vmo_id, VmoId(0));
        assert_eq!(space.mapping_count(), 0);

        let va2 = space
            .map_vmo(VmoId(1), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();

        assert_eq!(va2, va);
    }

    #[test]
    fn unmap_nonexistent() {
        let mut space = make_space(0);

        assert_eq!(space.unmap(0x1_0000), Err(SyscallError::NotFound));
    }

    #[test]
    fn find_mapping_within_range() {
        let mut space = make_space(0);
        let va = space
            .map_vmo(VmoId(5), 2 * config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();

        assert_eq!(space.find_mapping(va).unwrap().vmo_id, VmoId(5));
        assert_eq!(
            space.find_mapping(va + config::PAGE_SIZE).unwrap().vmo_id,
            VmoId(5)
        );
        assert!(space.find_mapping(va + 3 * config::PAGE_SIZE).is_none());
    }

    #[test]
    fn handle_table_integration() {
        let mut space = make_space(0);
        let hid = space
            .handles_mut()
            .allocate(ObjectType::Vmo, 42, Rights::READ, 0)
            .unwrap();
        let h = space.handles().lookup(hid).unwrap();

        assert_eq!(h.object_id, 42);
    }

    #[test]
    fn destroy_returns_mappings_and_handles() {
        let mut space = make_space(0);

        space
            .map_vmo(VmoId(0), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        space
            .map_vmo(VmoId(1), config::PAGE_SIZE, Rights::WRITE, 0)
            .unwrap();
        space
            .handles_mut()
            .allocate(ObjectType::Event, 99, Rights::SIGNAL, 0)
            .unwrap();

        let mut cb = NoopCallback;
        let (mappings, handles) = space.destroy(&mut cb);

        assert_eq!(mappings.len(), 2);
        assert_eq!(handles.count(), 1);
    }

    #[test]
    fn destroy_invokes_callback() {
        let space = make_space(7);

        struct Recorder(Option<AddressSpaceId>);

        impl DestroyCallback for Recorder {
            fn kill_threads_in_space(&mut self, id: AddressSpaceId) {
                self.0 = Some(id);
            }
        }

        let mut cb = Recorder(None);

        space.destroy(&mut cb);

        assert_eq!(cb.0, Some(AddressSpaceId(7)));
    }

    #[test]
    fn mapping_limit_exhaustion() {
        let mut space = make_space(0);

        for i in 0..config::MAX_MAPPINGS {
            space
                .map_vmo(VmoId(i as u32), config::PAGE_SIZE, Rights::READ, 0)
                .unwrap();
        }

        assert_eq!(
            space.map_vmo(VmoId(999), config::PAGE_SIZE, Rights::READ, 0),
            Err(SyscallError::OutOfMemory)
        );

        // Free one, remap
        let va = space.mappings()[0].va_start;

        space.unmap(va).unwrap();

        assert!(
            space
                .map_vmo(VmoId(999), config::PAGE_SIZE, Rights::READ, 0)
                .is_ok()
        );
    }

    // -- VA allocator unit tests --

    #[test]
    fn va_first_fit_sequential() {
        let mut va = VaAllocator::new(0x1_0000, 0x10_0000);
        let a = va.allocate(0x4000, 0).unwrap();
        let b = va.allocate(0x4000, 0).unwrap();

        assert_eq!(a, 0x1_0000);
        assert_eq!(b, 0x1_4000);
    }

    #[test]
    fn va_hint_reserves_exact() {
        let mut va = VaAllocator::new(0x1_0000, 0x10_0000);
        let a = va.allocate(0x4000, 0x5_0000).unwrap();

        assert_eq!(a, 0x5_0000);

        // Region before and after the hint should still be free
        let b = va.allocate(0x4000, 0).unwrap();

        assert_eq!(b, 0x1_0000);
    }

    #[test]
    fn va_coalescing_on_free() {
        let mut va = VaAllocator::new(0x1_0000, 0xC_0000);
        let a = va.allocate(0x4_0000, 0).unwrap();
        let b = va.allocate(0x4_0000, 0).unwrap();
        let _c = va.allocate(0x4_0000, 0).unwrap();

        va.free(b, 0x4_0000);
        va.free(a, 0x4_0000);

        let regions = va.free_regions();

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (0x1_0000, 0x8_0000));
    }

    #[test]
    fn va_full_coalesce_restores_original() {
        let mut va = VaAllocator::new(0x1_0000, 0xC_0000);
        let a = va.allocate(0x4_0000, 0).unwrap();
        let b = va.allocate(0x4_0000, 0).unwrap();
        let c = va.allocate(0x4_0000, 0).unwrap();

        assert!(va.allocate(0x4_0000, 0).is_err());

        va.free(a, 0x4_0000);
        va.free(c, 0x4_0000);
        va.free(b, 0x4_0000); // bridges a and c

        let regions = va.free_regions();

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (0x1_0000, 0xC_0000));
    }

    #[test]
    fn va_exhaustion() {
        let mut va = VaAllocator::new(0x1_0000, 0x4_0000);

        va.allocate(0x4_0000, 0).unwrap();

        assert_eq!(va.allocate(0x4_0000, 0), Err(SyscallError::OutOfMemory));
    }

    #[test]
    fn va_hint_out_of_range() {
        let mut va = VaAllocator::new(0x1_0000, 0x4_0000);

        assert_eq!(
            va.allocate(0x4_0000, 0x10_0000),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn va_hint_overlaps_allocated() {
        let mut va = VaAllocator::new(0x1_0000, 0x10_0000);

        va.allocate(0x4_0000, 0x1_0000).unwrap();

        assert_eq!(
            va.allocate(0x4_0000, 0x1_0000),
            Err(SyscallError::InvalidArgument)
        );
    }
}
