//! Virtual Memory Object — physical page management, COW snapshots, sealing.
//!
//! The VMO manages physical pages (allocation, COW bookkeeping, seal state,
//! generation counter, pager assignment). Virtual-to-physical mappings are
//! managed by the Address Space module. The syscall layer connects them.
//!
//! Page storage uses inline arrays for VMOs up to MAX_PAGES_INLINE pages
//! (512 KiB at 16 KiB page size), falling back to heap Vec for larger VMOs.
//! This eliminates heap alloc/free from the snapshot hot path — snapshots
//! of inline VMOs are a flat memcpy with zero allocator traffic.

use alloc::{vec, vec::Vec};

use crate::{
    config::MAX_PAGES_INLINE,
    types::{EndpointId, SyscallError, VmoId},
};

const NO_PAGE: usize = 0;

#[allow(clippy::large_enum_variant)]
enum Pages {
    Inline([usize; MAX_PAGES_INLINE]),
    Heap(Vec<usize>),
}

impl Pages {
    fn new(count: usize) -> Self {
        if count <= MAX_PAGES_INLINE {
            Pages::Inline([NO_PAGE; MAX_PAGES_INLINE])
        } else {
            Pages::Heap(vec![NO_PAGE; count])
        }
    }

    fn get(&self, idx: usize) -> Option<usize> {
        let addr = match self {
            Pages::Inline(arr) => *arr.get(idx)?,
            Pages::Heap(v) => *v.get(idx)?,
        };

        if addr == NO_PAGE { None } else { Some(addr) }
    }

    fn set(&mut self, idx: usize, addr: usize) {
        match self {
            Pages::Inline(arr) => arr[idx] = addr,
            Pages::Heap(v) => v[idx] = addr,
        }
    }

    fn clone_for_snapshot(&self, count: usize) -> Self {
        match self {
            Pages::Inline(arr) => Pages::Inline(*arr),
            Pages::Heap(v) => {
                let mut cloned = vec![NO_PAGE; v.len()];
                cloned[..count].copy_from_slice(&v[..count]);
                Pages::Heap(cloned)
            }
        }
    }

    fn grow(&mut self, old_count: usize, new_count: usize) {
        match self {
            Pages::Inline(arr) if new_count > MAX_PAGES_INLINE => {
                let mut v = vec![NO_PAGE; new_count];
                v[..old_count].copy_from_slice(&arr[..old_count]);
                *self = Pages::Heap(v);
            }
            Pages::Inline(_) => {}
            Pages::Heap(v) => v.resize(new_count, NO_PAGE),
        }
    }

    fn shrink_pages(
        &mut self,
        old_count: usize,
        new_count: usize,
        mut free_page: impl FnMut(usize),
    ) {
        match self {
            Pages::Inline(arr) => {
                for slot in &mut arr[new_count..old_count] {
                    if *slot != NO_PAGE {
                        free_page(*slot);
                        *slot = NO_PAGE;
                    }
                }
            }
            Pages::Heap(v) => {
                for addr in v.drain(new_count..).filter(|a| *a != NO_PAGE) {
                    free_page(addr);
                }
            }
        }
    }
}

/// VMO flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmoFlags(pub u32);

impl VmoFlags {
    pub const NONE: VmoFlags = VmoFlags(0);
    pub const HINT_CONTIGUOUS: VmoFlags = VmoFlags(1 << 0);
}

/// A Virtual Memory Object.
pub struct Vmo {
    pub id: VmoId,
    pages: Pages,
    page_count: usize,
    size: usize,
    flags: VmoFlags,
    sealed: bool,
    pager: Option<EndpointId>,
    cow_parent: Option<VmoId>,
    has_pages: bool,
    mapping_count: usize,
    refcount: core::sync::atomic::AtomicUsize,
}

impl Vmo {
    pub fn new(id: VmoId, size: usize, flags: VmoFlags) -> Self {
        let page_count = size.div_ceil(crate::config::PAGE_SIZE);

        Vmo {
            id,
            pages: Pages::new(page_count),
            page_count,
            size,
            flags,
            sealed: false,
            pager: None,
            cow_parent: None,
            has_pages: false,
            mapping_count: 0,
            refcount: core::sync::atomic::AtomicUsize::new(1),
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    pub fn flags(&self) -> VmoFlags {
        self.flags
    }

    pub fn pager(&self) -> Option<EndpointId> {
        self.pager
    }

    pub fn cow_parent(&self) -> Option<VmoId> {
        self.cow_parent
    }

    pub fn refcount(&self) -> usize {
        self.refcount.load(core::sync::atomic::Ordering::Relaxed)
    }

    pub fn add_ref(&self) {
        self.refcount
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }

    pub fn release_ref(&self) -> bool {
        let prev = self
            .refcount
            .fetch_sub(1, core::sync::atomic::Ordering::Release);

        assert!(prev > 0, "VMO refcount underflow");

        if prev == 1 {
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

            return true;
        }

        false
    }

    pub fn mapping_count(&self) -> usize {
        self.mapping_count
    }

    pub fn inc_mapping_count(&mut self) {
        self.mapping_count += 1;
    }

    pub fn dec_mapping_count(&mut self) {
        self.mapping_count = self.mapping_count.saturating_sub(1);
    }

    /// Allocate a physical page at the given offset (page index).
    /// Returns the physical address. Called by the fault handler or eager map.
    pub fn alloc_page_at(
        &mut self,
        page_idx: usize,
        alloc_fn: impl FnOnce() -> Option<usize>,
    ) -> Result<usize, SyscallError> {
        if page_idx >= self.page_count {
            return Err(SyscallError::InvalidArgument);
        }

        if let Some(addr) = self.pages.get(page_idx) {
            return Ok(addr);
        }

        let addr = alloc_fn().ok_or(SyscallError::OutOfMemory)?;

        self.pages.set(page_idx, addr);
        self.has_pages = true;

        Ok(addr)
    }

    /// Get the physical page at an offset, if already allocated.
    pub fn page_at(&self, page_idx: usize) -> Option<usize> {
        self.pages.get(page_idx)
    }

    /// Create a COW snapshot. Returns a new Vmo that shares all page references.
    /// The caller must increment physical page refcounts externally.
    pub fn snapshot(&self, new_id: VmoId) -> Vmo {
        Vmo {
            id: new_id,
            pages: self.pages.clone_for_snapshot(self.page_count),
            page_count: self.page_count,
            size: self.size,
            flags: self.flags,
            sealed: false,
            pager: self.pager,
            cow_parent: Some(self.id),
            has_pages: self.has_pages,
            mapping_count: 0,
            refcount: core::sync::atomic::AtomicUsize::new(1),
        }
    }

    /// Seal the VMO permanently. Irreversible.
    pub fn seal(&mut self) -> Result<(), SyscallError> {
        if self.sealed {
            return Err(SyscallError::AlreadySealed);
        }

        self.sealed = true;

        Ok(())
    }

    /// Resize the VMO. Calls `free_page` for each freed physical page on shrink.
    pub fn resize(
        &mut self,
        new_size: usize,
        free_page: impl FnMut(usize),
    ) -> Result<(), SyscallError> {
        if self.sealed {
            return Err(SyscallError::AlreadySealed);
        }

        let new_page_count = new_size.div_ceil(crate::config::PAGE_SIZE);

        if new_page_count < self.page_count {
            self.pages
                .shrink_pages(self.page_count, new_page_count, free_page);
        } else if new_page_count > self.page_count {
            self.pages.grow(self.page_count, new_page_count);
        }

        self.page_count = new_page_count;
        self.size = new_size;

        Ok(())
    }

    /// Set the userspace pager endpoint. Only valid before any page is allocated.
    pub fn set_pager(&mut self, endpoint: EndpointId) -> Result<(), SyscallError> {
        if self.has_pages {
            return Err(SyscallError::InvalidArgument);
        }

        self.pager = Some(endpoint);

        Ok(())
    }

    /// Record a physical page at the given index (COW resolution or lazy alloc).
    ///
    /// Panics if `page_idx` is out of range — callers must ensure the index
    /// is valid (derived from the mapping and VMO size).
    pub fn replace_page(&mut self, page_idx: usize, new_addr: usize) {
        assert!(
            page_idx < self.page_count,
            "replace_page: index out of range"
        );

        self.pages.set(page_idx, new_addr);
        self.has_pages = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PAGE_SIZE;

    fn make_vmo(pages: usize) -> Vmo {
        Vmo::new(VmoId(0), pages * PAGE_SIZE, VmoFlags::NONE)
    }

    #[test]
    fn new_vmo_has_correct_size() {
        let vmo = make_vmo(4);

        assert_eq!(vmo.size(), 4 * PAGE_SIZE);
        assert_eq!(vmo.page_count(), 4);
    }

    #[test]
    fn new_vmo_has_no_pages_allocated() {
        let vmo = make_vmo(4);

        for i in 0..4 {
            assert!(vmo.page_at(i).is_none());
        }
    }

    #[test]
    fn alloc_page_at_succeeds() {
        let mut vmo = make_vmo(4);
        let addr = vmo.alloc_page_at(0, || Some(0xDEAD_0000)).unwrap();

        assert_eq!(addr, 0xDEAD_0000);
        assert_eq!(vmo.page_at(0), Some(0xDEAD_0000));
    }

    #[test]
    fn alloc_page_at_idempotent() {
        let mut vmo = make_vmo(4);

        vmo.alloc_page_at(0, || Some(0xDEAD_0000)).unwrap();

        let mut called = false;
        let addr = vmo
            .alloc_page_at(0, || {
                called = true;

                Some(0xBEEF_0000)
            })
            .unwrap();

        assert!(!called);
        assert_eq!(addr, 0xDEAD_0000);
    }

    #[test]
    fn alloc_page_out_of_range() {
        let mut vmo = make_vmo(2);

        assert_eq!(
            vmo.alloc_page_at(5, || Some(0x1000)),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn alloc_page_oom() {
        let mut vmo = make_vmo(2);

        assert_eq!(
            vmo.alloc_page_at(0, || None),
            Err(SyscallError::OutOfMemory)
        );
    }

    #[test]
    fn snapshot_shares_pages() {
        let mut vmo = make_vmo(2);

        vmo.alloc_page_at(0, || Some(0xAAAA)).unwrap();
        vmo.alloc_page_at(1, || Some(0xBBBB)).unwrap();

        let snap = vmo.snapshot(VmoId(1));

        assert_eq!(snap.page_at(0), Some(0xAAAA));
        assert_eq!(snap.page_at(1), Some(0xBBBB));
        assert_eq!(snap.cow_parent(), Some(VmoId(0)));
    }

    #[test]
    fn seal_prevents_resize() {
        let mut vmo = make_vmo(2);

        vmo.seal().unwrap();

        assert_eq!(
            vmo.resize(PAGE_SIZE, |_| {}),
            Err(SyscallError::AlreadySealed)
        );
    }

    #[test]
    fn seal_is_idempotent_error() {
        let mut vmo = make_vmo(2);

        vmo.seal().unwrap();

        assert_eq!(vmo.seal(), Err(SyscallError::AlreadySealed));
    }

    #[test]
    fn resize_grow() {
        let mut vmo = make_vmo(2);

        vmo.resize(4 * PAGE_SIZE, |_| {}).unwrap();

        assert_eq!(vmo.page_count(), 4);
    }

    #[test]
    fn resize_shrink_frees_pages() {
        let mut vmo = make_vmo(4);

        vmo.alloc_page_at(2, || Some(0xCC)).unwrap();
        vmo.alloc_page_at(3, || Some(0xDD)).unwrap();

        let mut freed = alloc::vec![];

        vmo.resize(2 * PAGE_SIZE, |pa| freed.push(pa)).unwrap();

        assert_eq!(freed, alloc::vec![0xCC, 0xDD]);
        assert_eq!(vmo.page_count(), 2);
    }

    #[test]
    fn set_pager_before_pages() {
        let mut vmo = make_vmo(2);

        assert!(vmo.set_pager(EndpointId(5)).is_ok());
        assert_eq!(vmo.pager(), Some(EndpointId(5)));
    }

    #[test]
    fn set_pager_after_pages_fails() {
        let mut vmo = make_vmo(2);

        vmo.alloc_page_at(0, || Some(0x1000)).unwrap();

        assert_eq!(
            vmo.set_pager(EndpointId(5)),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn refcount_lifecycle() {
        let vmo = make_vmo(1);

        assert_eq!(vmo.refcount(), 1);

        vmo.add_ref();

        assert_eq!(vmo.refcount(), 2);
        assert!(!vmo.release_ref());
        assert_eq!(vmo.refcount(), 1);
        assert!(vmo.release_ref());
    }

    #[test]
    fn resize_grow_promotes_to_heap() {
        let mut vmo = make_vmo(2);

        vmo.alloc_page_at(0, || Some(0xAA)).unwrap();

        vmo.resize((MAX_PAGES_INLINE + 4) * PAGE_SIZE, |_| {})
            .unwrap();

        assert_eq!(vmo.page_count(), MAX_PAGES_INLINE + 4);
        assert_eq!(vmo.page_at(0), Some(0xAA));
        assert!(vmo.page_at(MAX_PAGES_INLINE + 3).is_none());
    }

    #[test]
    fn heap_vmo_snapshot_and_resize() {
        let big_pages = MAX_PAGES_INLINE + 2;
        let mut vmo = Vmo::new(VmoId(0), big_pages * PAGE_SIZE, VmoFlags::NONE);

        vmo.alloc_page_at(0, || Some(0x1000)).unwrap();
        vmo.alloc_page_at(big_pages - 1, || Some(0x2000)).unwrap();

        let snap = vmo.snapshot(VmoId(1));

        assert_eq!(snap.page_at(0), Some(0x1000));
        assert_eq!(snap.page_at(big_pages - 1), Some(0x2000));
    }
}
