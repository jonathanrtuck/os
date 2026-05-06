//! Address Space — isolation boundary with VA allocation and mapping management.
//!
//! An address space is a page table root + handle table + mapping set. It is
//! the lifecycle boundary: destroying a space kills its threads, closes its
//! handles, and unmaps its VMOs. No separate "process" kernel object exists.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappingRecord {
    pub vmo_id: VmoId,
    pub va_start: usize,
    pub size: usize,
    pub rights: Rights,
}

/// VA allocator over a sorted free list with ASLR (fixed-size, no heap).
///
/// When `rng_state` is non-zero, allocations without a hint land at a
/// random page-aligned offset within the first qualifying free region.
/// When zero, the allocator is deterministic (first-fit from region start).
pub struct VaAllocator {
    regions: [(usize, usize); config::MAX_VA_REGIONS],
    len: usize,
    rng_state: u64,
}

impl VaAllocator {
    pub fn new(base: usize, size: usize) -> Self {
        let mut va = VaAllocator {
            regions: [(0, 0); config::MAX_VA_REGIONS],
            len: 0,
            rng_state: 0,
        };

        if size > 0 {
            va.regions[0] = (base, size);
            va.len = 1;
        }

        va
    }

    pub fn set_aslr_seed(&mut self, seed: u64) {
        self.rng_state = seed;
    }

    fn next_random(&mut self) -> u64 {
        if self.rng_state == 0 {
            return 0;
        }

        // Marsaglia xorshift64 — period 2^64-1, never produces 0 from non-zero state.
        let mut x = self.rng_state;

        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;

        self.rng_state = x;

        x
    }

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
        for i in 0..self.len {
            let (start, len) = self.regions[i];

            if len < size {
                continue;
            }

            let slack_pages = (len - size) / config::PAGE_SIZE;
            let offset = if slack_pages > 0 {
                (self.next_random() as usize % (slack_pages + 1)) * config::PAGE_SIZE
            } else {
                0
            };

            if offset == 0 {
                if len == size {
                    self.remove_at(i);
                } else {
                    self.regions[i] = (start + size, len - size);
                }

                return Ok(start);
            }

            let addr = start + offset;
            let after = len - offset - size;

            // Splitting into 2 fragments is net +1 region. If only 'before'
            // exists (after == 0), it's net 0 — always fits.
            if after > 0 && self.len >= config::MAX_VA_REGIONS {
                if len == size {
                    self.remove_at(i);
                } else {
                    self.regions[i] = (start + size, len - size);
                }

                return Ok(start);
            }

            self.remove_at(i);

            if after > 0 {
                self.insert_sorted(addr + size, after);
            }

            self.insert_sorted(start, offset);

            return Ok(addr);
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

        for i in 0..self.len {
            let (start, len) = self.regions[i];
            let region_end = start + len;

            if addr >= start && end <= region_end {
                let fragments = (addr > start) as usize + (end < region_end) as usize;

                if self.len - 1 + fragments > config::MAX_VA_REGIONS {
                    return Err(SyscallError::OutOfMemory);
                }

                self.remove_at(i);

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

    pub fn free(&mut self, addr: usize, size: usize) {
        debug_assert!(addr.checked_add(size).is_some(), "free: addr+size overflow");

        let list = &self.regions[..self.len];
        let pos = list.partition_point(|&(s, _)| s < addr);

        debug_assert!(
            pos >= self.len || addr + size <= self.regions[pos].0,
            "free: overlaps next free region (double-free?)"
        );
        debug_assert!(
            pos == 0 || {
                let (ps, pl) = self.regions[pos - 1];

                ps + pl <= addr
            },
            "free: overlaps previous free region (double-free?)"
        );

        let merge_prev = pos > 0 && {
            let (ps, pl) = self.regions[pos - 1];

            ps + pl == addr
        };
        let merge_next = pos < self.len && addr + size == self.regions[pos].0;

        match (merge_prev, merge_next) {
            (true, true) => {
                let next_len = self.regions[pos].1;

                self.regions[pos - 1].1 += size + next_len;
                self.remove_at(pos);
            }
            (true, false) => {
                self.regions[pos - 1].1 += size;
            }
            (false, true) => {
                self.regions[pos] = (addr, size + self.regions[pos].1);
            }
            (false, false) => {
                self.insert_at(pos, (addr, size));
            }
        }
    }

    fn insert_sorted(&mut self, start: usize, len: usize) {
        let list = &self.regions[..self.len];
        let pos = list.partition_point(|&(s, _)| s < start);

        self.insert_at(pos, (start, len));
    }

    fn insert_at(&mut self, pos: usize, val: (usize, usize)) {
        assert!(self.len < config::MAX_VA_REGIONS, "VA free list overflow");

        self.regions.copy_within(pos..self.len, pos + 1);
        self.regions[pos] = val;
        self.len += 1;
    }

    fn remove_at(&mut self, pos: usize) {
        self.regions.copy_within(pos + 1..self.len, pos);
        self.len -= 1;
        self.regions[self.len] = (0, 0);
    }

    pub fn free_regions(&self) -> &[(usize, usize)] {
        &self.regions[..self.len]
    }
}

/// Callback for tearing down threads during address space destruction.
/// Breaks the circular dependency with the thread module.
pub trait DestroyCallback {
    fn kill_threads_in_space(&mut self, space_id: AddressSpaceId);
}

/// Fixed-size sorted mapping array.
struct MappingArray {
    entries: [MappingRecord; config::MAX_MAPPINGS],
    len: usize,
}

impl MappingArray {
    const EMPTY: MappingRecord = MappingRecord {
        vmo_id: VmoId(0),
        va_start: 0,
        size: 0,
        rights: Rights::NONE,
    };

    fn new() -> Self {
        MappingArray {
            entries: [const { Self::EMPTY }; config::MAX_MAPPINGS],
            len: 0,
        }
    }

    fn as_slice(&self) -> &[MappingRecord] {
        &self.entries[..self.len]
    }

    fn len(&self) -> usize {
        self.len
    }

    fn insert(&mut self, pos: usize, record: MappingRecord) {
        assert!(self.len < config::MAX_MAPPINGS, "mapping list overflow");

        self.entries.copy_within(pos..self.len, pos + 1);
        self.entries[pos] = record;
        self.len += 1;
    }

    fn remove(&mut self, pos: usize) -> MappingRecord {
        let record = self.entries[pos];

        self.entries.copy_within(pos + 1..self.len, pos);
        self.len -= 1;
        self.entries[self.len] = Self::EMPTY;

        record
    }

    fn partition_point(&self, pred: impl Fn(&MappingRecord) -> bool) -> usize {
        self.as_slice().partition_point(|m| pred(m))
    }
}

/// An address space — isolation boundary and lifecycle owner.
pub struct AddressSpace {
    pub id: AddressSpaceId,
    asid: u8,
    page_table_root: usize,
    handles: HandleTable,
    mappings: MappingArray,
    va_allocator: VaAllocator,
    thread_head: Option<u32>,
}

#[allow(clippy::new_without_default)]
impl AddressSpace {
    pub fn new(id: AddressSpaceId, asid: u8, page_table_root: usize) -> Self {
        AddressSpace {
            id,
            asid,
            page_table_root,
            handles: HandleTable::new(),
            mappings: MappingArray::new(),
            va_allocator: VaAllocator::new(USER_VA_BASE, USER_VA_SIZE),
            thread_head: None,
        }
    }

    pub fn set_aslr_seed(&mut self, seed: u64) {
        self.va_allocator.set_aslr_seed(seed);
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

    pub fn thread_head(&self) -> Option<u32> {
        self.thread_head
    }

    pub fn set_thread_head(&mut self, head: Option<u32>) {
        self.thread_head = head;
    }

    pub fn mappings(&self) -> &[MappingRecord] {
        self.mappings.as_slice()
    }

    pub fn mapping_count(&self) -> usize {
        self.mappings.len()
    }

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
        let record = MappingRecord {
            vmo_id,
            va_start: va,
            size: aligned_size,
            rights,
        };
        let pos = self.mappings.partition_point(|m| m.va_start < va);

        self.mappings.insert(pos, record);

        Ok(va)
    }

    pub fn remove_mappings_for_vmo(&mut self, vmo_id: VmoId) {
        let mut i = 0;

        while i < self.mappings.len() {
            if self.mappings.as_slice()[i].vmo_id == vmo_id {
                let record = self.mappings.remove(i);

                self.va_allocator.free(record.va_start, record.size);
            } else {
                i += 1;
            }
        }
    }

    pub fn unmap(&mut self, addr: usize) -> Result<MappingRecord, SyscallError> {
        let pos = self.mappings.partition_point(|m| m.va_start < addr);
        let slice = self.mappings.as_slice();

        if pos >= slice.len() || slice[pos].va_start != addr {
            return Err(SyscallError::NotFound);
        }

        let record = self.mappings.remove(pos);

        self.va_allocator.free(record.va_start, record.size);

        Ok(record)
    }

    pub fn find_mapping(&self, addr: usize) -> Option<&MappingRecord> {
        let idx = self
            .mappings
            .partition_point(|m| m.va_start + m.size <= addr);

        self.mappings
            .as_slice()
            .get(idx)
            .filter(|m| addr >= m.va_start && addr < m.va_start + m.size)
    }

    pub fn destroy(self, callback: &mut dyn DestroyCallback) -> (DestroyMappings, HandleTable) {
        callback.kill_threads_in_space(self.id);

        (
            DestroyMappings {
                entries: self.mappings.entries,
                len: self.mappings.len,
            },
            self.handles,
        )
    }
}

/// Mappings returned by address space destruction, for VMO cleanup.
pub struct DestroyMappings {
    entries: [MappingRecord; config::MAX_MAPPINGS],
    len: usize,
}

impl DestroyMappings {
    pub fn as_slice(&self) -> &[MappingRecord] {
        &self.entries[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
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
        va.free(b, 0x4_0000);

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
    fn va_region_overflow_returns_error() {
        let page = config::PAGE_SIZE;
        let mut va = VaAllocator::new(USER_VA_BASE, 4096 * page);

        for i in 0..config::MAX_VA_REGIONS - 1 {
            let addr = USER_VA_BASE + (4 * i + 1) * page;
            va.allocate(page, addr).unwrap();
        }

        assert_eq!(va.len, config::MAX_VA_REGIONS);

        let overflow_addr = USER_VA_BASE + (4 * (config::MAX_VA_REGIONS - 1) + 1) * page;

        assert_eq!(
            va.allocate(page, overflow_addr),
            Err(SyscallError::OutOfMemory)
        );
    }

    #[test]
    fn va_free_at_capacity_merges_safely() {
        let page = config::PAGE_SIZE;
        let mut va = VaAllocator::new(USER_VA_BASE, 4096 * page);

        for i in 0..config::MAX_VA_REGIONS - 1 {
            let addr = USER_VA_BASE + (4 * i + 1) * page;

            va.allocate(page, addr).unwrap();
        }

        assert_eq!(va.len, config::MAX_VA_REGIONS);

        let freed_addr = USER_VA_BASE + (4 * 5 + 1) * page;

        va.free(freed_addr, page);

        assert!(va.len < config::MAX_VA_REGIONS);
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

    // -- Mutation-killing tests: VaAllocator boundary precision --

    #[test]
    fn va_allocate_fixed_exact_region_start() {
        let mut va = VaAllocator::new(0x1_0000, 0x8_0000);
        let a = va.allocate(0x4_0000, 0x1_0000).unwrap();

        assert_eq!(a, 0x1_0000);

        let regions = va.free_regions();

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (0x5_0000, 0x4_0000));
    }

    #[test]
    fn va_allocate_fixed_exact_region_end() {
        let mut va = VaAllocator::new(0x1_0000, 0x8_0000);
        let a = va.allocate(0x4_0000, 0x5_0000).unwrap();

        assert_eq!(a, 0x5_0000);

        let regions = va.free_regions();

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (0x1_0000, 0x4_0000));
    }

    #[test]
    fn va_allocate_fixed_consumes_entire_region() {
        let mut va = VaAllocator::new(0x1_0000, 0x4_0000);
        let a = va.allocate(0x4_0000, 0x1_0000).unwrap();

        assert_eq!(a, 0x1_0000);
        assert!(va.free_regions().is_empty());
    }

    #[test]
    fn va_allocate_fixed_splits_region_correctly() {
        let mut va = VaAllocator::new(0x1_0000, 0xC_0000);
        let a = va.allocate(0x4_0000, 0x5_0000).unwrap();

        assert_eq!(a, 0x5_0000);

        let regions = va.free_regions();

        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0], (0x1_0000, 0x4_0000));
        assert_eq!(regions[1], (0x9_0000, 0x4_0000));
    }

    #[test]
    fn va_free_merge_prev_exact_size() {
        let mut va = VaAllocator::new(0x1_0000, 0x8_0000);
        let a = va.allocate(0x4_0000, 0).unwrap();
        let b = va.allocate(0x4_0000, 0).unwrap();

        va.free(a, 0x4_0000);
        va.free(b, 0x4_0000);

        let regions = va.free_regions();

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (0x1_0000, 0x8_0000));
    }

    #[test]
    fn va_free_merge_next_exact_size() {
        let mut va = VaAllocator::new(0x1_0000, 0x8_0000);
        let a = va.allocate(0x4_0000, 0).unwrap();
        let b = va.allocate(0x4_0000, 0).unwrap();

        va.free(b, 0x4_0000);
        va.free(a, 0x4_0000);

        let regions = va.free_regions();

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (0x1_0000, 0x8_0000));
    }

    #[test]
    fn va_free_no_merge_gap_in_between() {
        let mut va = VaAllocator::new(0x1_0000, 0xC_0000);
        let a = va.allocate(0x4_0000, 0).unwrap();
        let _b = va.allocate(0x4_0000, 0).unwrap();
        let c = va.allocate(0x4_0000, 0).unwrap();

        va.free(a, 0x4_0000);
        va.free(c, 0x4_0000);

        let regions = va.free_regions();

        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0], (0x1_0000, 0x4_0000));
        assert_eq!(regions[1], (0x9_0000, 0x4_0000));
    }

    #[test]
    fn va_insert_sorted_preserves_order() {
        let mut va = VaAllocator::new(0x1_0000, 0x14_0000);

        va.allocate(0x4_0000, 0x5_0000).unwrap();
        va.allocate(0x4_0000, 0xD_0000).unwrap();

        let regions = va.free_regions();

        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0], (0x1_0000, 0x4_0000));
        assert_eq!(regions[1], (0x9_0000, 0x4_0000));
        assert_eq!(regions[2], (0x11_0000, 0x4_0000));
        assert!(regions[0].0 < regions[1].0);
        assert!(regions[1].0 < regions[2].0);
    }

    #[test]
    fn va_remove_at_shifts_correctly() {
        let mut va = VaAllocator::new(0x1_0000, 0x14_0000);

        va.allocate(0x4_0000, 0x5_0000).unwrap();
        va.allocate(0x4_0000, 0xD_0000).unwrap();

        assert_eq!(va.free_regions().len(), 3);

        va.allocate(0x4_0000, 0x1_0000).unwrap();

        let regions = va.free_regions();

        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0], (0x9_0000, 0x4_0000));
        assert_eq!(regions[1], (0x11_0000, 0x4_0000));
    }

    // -- Mutation-killing tests: AddressSpace fields --

    #[test]
    fn set_page_table_updates_both_fields() {
        let mut space = make_space(0);

        space.set_page_table(0xBEEF_0000, 42);

        assert_eq!(space.page_table_root(), 0xBEEF_0000);
        assert_eq!(space.asid(), 42);
    }

    #[test]
    fn asid_matches_constructor() {
        let space = AddressSpace::new(AddressSpaceId(0), 99, 0);

        assert_eq!(space.asid(), 99);
    }

    // -- Mutation-killing tests: find_mapping precision --

    #[test]
    fn find_mapping_exact_start_and_end() {
        let mut space = make_space(0);
        let va = space
            .map_vmo(VmoId(1), 2 * config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();

        assert!(space.find_mapping(va).is_some());
        assert!(space.find_mapping(va + 2 * config::PAGE_SIZE - 1).is_some());
        assert!(space.find_mapping(va + 2 * config::PAGE_SIZE).is_none());
        assert!(space.find_mapping(va.wrapping_sub(1)).is_none());
    }

    #[test]
    fn find_mapping_with_multiple_mappings() {
        let mut space = make_space(0);
        let va1 = space
            .map_vmo(VmoId(1), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let va2 = space
            .map_vmo(VmoId(2), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();

        assert_eq!(space.find_mapping(va1).unwrap().vmo_id, VmoId(1));
        assert_eq!(space.find_mapping(va2).unwrap().vmo_id, VmoId(2));
        assert!(space.find_mapping(va2 + config::PAGE_SIZE).is_none());
    }

    #[test]
    fn find_mapping_gap_between_mappings() {
        let mut space = make_space(0);
        let va1 = space
            .map_vmo(VmoId(1), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let gap_hint = va1 + 2 * config::PAGE_SIZE;
        let va2 = space
            .map_vmo(VmoId(2), config::PAGE_SIZE, Rights::READ, gap_hint)
            .unwrap();

        assert_eq!(space.find_mapping(va1).unwrap().vmo_id, VmoId(1));
        assert!(space.find_mapping(va1 + config::PAGE_SIZE).is_none());
        assert_eq!(space.find_mapping(va2).unwrap().vmo_id, VmoId(2));
    }

    // -- Mutation-killing tests: DestroyMappings --

    #[test]
    fn destroy_mappings_is_empty() {
        let space = make_space(0);
        let mut cb = NoopCallback;
        let (mappings, _) = space.destroy(&mut cb);

        assert!(mappings.is_empty());
        assert_eq!(mappings.len(), 0);
    }

    #[test]
    fn destroy_mappings_not_empty() {
        let mut space = make_space(0);

        space
            .map_vmo(VmoId(0), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();

        let mut cb = NoopCallback;
        let (mappings, _) = space.destroy(&mut cb);

        assert!(!mappings.is_empty());
        assert_eq!(mappings.len(), 1);
    }

    // -- Mutation-killing tests: MappingArray shift operations --

    #[test]
    fn mapping_insert_remove_preserves_order() {
        let mut space = make_space(0);
        let va1 = space
            .map_vmo(VmoId(1), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let va2 = space
            .map_vmo(VmoId(2), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();
        let va3 = space
            .map_vmo(VmoId(3), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();

        space.unmap(va2).unwrap();

        assert_eq!(space.mapping_count(), 2);
        assert_eq!(space.mappings()[0].va_start, va1);
        assert_eq!(space.mappings()[0].vmo_id, VmoId(1));
        assert_eq!(space.mappings()[1].va_start, va3);
        assert_eq!(space.mappings()[1].vmo_id, VmoId(3));
    }

    #[test]
    fn mapping_insert_in_middle_shifts_existing() {
        let mut space = make_space(0);
        let high_hint = USER_VA_BASE + 10 * config::PAGE_SIZE;
        let va_high = space
            .map_vmo(VmoId(1), config::PAGE_SIZE, Rights::READ, high_hint)
            .unwrap();
        let va_low = space
            .map_vmo(VmoId(2), config::PAGE_SIZE, Rights::READ, 0)
            .unwrap();

        assert_eq!(space.mapping_count(), 2);
        assert!(va_low < va_high);
        assert_eq!(space.mappings()[0].vmo_id, VmoId(2));
        assert_eq!(space.mappings()[0].va_start, va_low);
        assert_eq!(space.mappings()[1].vmo_id, VmoId(1));
        assert_eq!(space.mappings()[1].va_start, va_high);
    }

    // -- Mutation-killing test: USER_VA_SIZE arithmetic --

    #[test]
    fn va_space_size_is_correct() {
        let va = VaAllocator::new(USER_VA_BASE, USER_VA_SIZE);
        let regions = va.free_regions();

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].0, USER_VA_BASE);
        assert_eq!(regions[0].0 + regions[0].1, USER_VA_END);
    }

    // -- ASLR tests --

    #[test]
    fn aslr_seed_zero_is_deterministic() {
        let mut va = VaAllocator::new(0x1_0000, 0x10_0000);
        let a = va.allocate(0x4000, 0).unwrap();
        let b = va.allocate(0x4000, 0).unwrap();

        assert_eq!(a, 0x1_0000);
        assert_eq!(b, 0x1_4000);
    }

    #[test]
    fn aslr_seeded_allocations_are_page_aligned() {
        let mut va = VaAllocator::new(0x1_0000, 0x10_0000);

        va.set_aslr_seed(0xDEAD_BEEF);

        for _ in 0..8 {
            let addr = va.allocate(config::PAGE_SIZE, 0).unwrap();

            assert!(addr.is_multiple_of(config::PAGE_SIZE));
            assert!(addr >= 0x1_0000);
            assert!(addr + config::PAGE_SIZE <= 0x11_0000);
        }
    }

    #[test]
    fn aslr_seeded_allocations_differ_from_first_fit() {
        let mut va = VaAllocator::new(USER_VA_BASE, USER_VA_SIZE);

        va.set_aslr_seed(0x1234_5678_9ABC_DEF0);

        let addr = va.allocate(config::PAGE_SIZE, 0).unwrap();

        assert_ne!(
            addr, USER_VA_BASE,
            "seeded allocator should not pick first-fit address"
        );
    }

    #[test]
    fn aslr_different_seeds_different_addresses() {
        let mut va1 = VaAllocator::new(USER_VA_BASE, USER_VA_SIZE);

        va1.set_aslr_seed(0xAAAA_BBBB_CCCC_DDDD);

        let addr1 = va1.allocate(config::PAGE_SIZE, 0).unwrap();
        let mut va2 = VaAllocator::new(USER_VA_BASE, USER_VA_SIZE);

        va2.set_aslr_seed(0x1111_2222_3333_4444);

        let addr2 = va2.allocate(config::PAGE_SIZE, 0).unwrap();

        assert_ne!(
            addr1, addr2,
            "different seeds should produce different addresses"
        );
    }

    #[test]
    fn aslr_free_list_integrity_after_split() {
        let mut va = VaAllocator::new(0x1_0000, 0x10_0000);

        va.set_aslr_seed(0xCAFE_BABE);

        let a = va.allocate(config::PAGE_SIZE, 0).unwrap();

        assert!(a >= 0x1_0000 && a + config::PAGE_SIZE <= 0x11_0000);

        let regions = va.free_regions();
        let total_free: usize = regions.iter().map(|(_, len)| len).sum();

        assert_eq!(total_free, 0x10_0000 - config::PAGE_SIZE);

        for w in regions.windows(2) {
            assert!(w[0].0 + w[0].1 <= w[1].0, "free regions must not overlap");
        }
    }

    #[test]
    fn aslr_allocate_then_free_restores_space() {
        let mut va = VaAllocator::new(0x1_0000, 0x10_0000);

        va.set_aslr_seed(0xBEEF_FACE);

        let a = va.allocate(0x4_0000, 0).unwrap();
        let b = va.allocate(0x4_0000, 0).unwrap();

        va.free(a, 0x4_0000);
        va.free(b, 0x4_0000);

        let regions = va.free_regions();
        let total_free: usize = regions.iter().map(|(_, len)| len).sum();

        assert_eq!(total_free, 0x10_0000);
    }

    #[test]
    fn aslr_exact_fit_region_no_split() {
        let mut va = VaAllocator::new(0x1_0000, config::PAGE_SIZE);

        va.set_aslr_seed(0xFFFF);

        let addr = va.allocate(config::PAGE_SIZE, 0).unwrap();

        assert_eq!(addr, 0x1_0000);
        assert!(va.free_regions().is_empty());
    }

    #[test]
    fn aslr_hint_bypasses_randomization() {
        let mut va = VaAllocator::new(0x1_0000, 0x10_0000);

        va.set_aslr_seed(0xDEAD_BEEF);

        let addr = va.allocate(config::PAGE_SIZE, 0x5_0000).unwrap();

        assert_eq!(addr, 0x5_0000);
    }

    #[test]
    fn aslr_entropy_bits() {
        let mut seen = alloc::collections::BTreeSet::new();

        for seed in 1..=256u64 {
            let mut va = VaAllocator::new(USER_VA_BASE, USER_VA_SIZE);

            va.set_aslr_seed(seed);
            seen.insert(va.allocate(config::PAGE_SIZE, 0).unwrap());
        }

        assert!(
            seen.len() > 200,
            "256 seeds should produce >200 distinct addresses, got {}",
            seen.len()
        );
    }
}
