//! Virtual Memory Object — physical page management, COW snapshots, sealing.
//!
//! The VMO manages physical pages (allocation, COW bookkeeping, seal state,
//! generation counter, pager assignment). Virtual-to-physical mappings are
//! managed by the Address Space module. The syscall layer connects them.

use alloc::{vec, vec::Vec};

use crate::types::{EndpointId, SyscallError, VmoId};

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
    pages: Vec<Option<usize>>,
    size: usize,
    flags: VmoFlags,
    sealed: bool,
    generation: u64,
    pager: Option<EndpointId>,
    cow_parent: Option<VmoId>,
    has_pages: bool,
    refcount: usize,
}

impl Vmo {
    pub fn new(id: VmoId, size: usize, flags: VmoFlags) -> Self {
        let page_count = size.div_ceil(crate::config::PAGE_SIZE);
        Vmo {
            id,
            pages: vec![None; page_count],
            size,
            flags,
            sealed: false,
            generation: 0,
            pager: None,
            cow_parent: None,
            has_pages: false,
            refcount: 1,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn generation(&self) -> u64 {
        self.generation
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
        self.refcount
    }

    pub fn add_ref(&mut self) {
        self.refcount += 1;
    }

    pub fn release_ref(&mut self) -> bool {
        self.refcount -= 1;
        self.refcount == 0
    }

    /// Allocate a physical page at the given offset (page index).
    /// Returns the physical address. Called by the fault handler or eager map.
    pub fn alloc_page_at(
        &mut self,
        page_idx: usize,
        alloc_fn: impl FnOnce() -> Option<usize>,
    ) -> Result<usize, SyscallError> {
        if page_idx >= self.pages.len() {
            return Err(SyscallError::InvalidArgument);
        }
        if let Some(addr) = self.pages[page_idx] {
            return Ok(addr);
        }
        let addr = alloc_fn().ok_or(SyscallError::OutOfMemory)?;
        self.pages[page_idx] = Some(addr);
        self.has_pages = true;
        Ok(addr)
    }

    /// Get the physical page at an offset, if already allocated.
    pub fn page_at(&self, page_idx: usize) -> Option<usize> {
        self.pages.get(page_idx).copied().flatten()
    }

    /// Create a COW snapshot. Returns a new Vmo that shares all page references.
    /// The caller must increment physical page refcounts externally.
    pub fn snapshot(&self, new_id: VmoId) -> Vmo {
        Vmo {
            id: new_id,
            pages: self.pages.clone(),
            size: self.size,
            flags: self.flags,
            sealed: false,
            generation: 0,
            pager: self.pager,
            cow_parent: Some(self.id),
            has_pages: self.has_pages,
            refcount: 1,
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

    /// Resize the VMO. Fails if sealed.
    pub fn resize(&mut self, new_size: usize) -> Result<Vec<usize>, SyscallError> {
        if self.sealed {
            return Err(SyscallError::AlreadySealed);
        }
        let new_page_count = new_size.div_ceil(crate::config::PAGE_SIZE);
        let old_page_count = self.pages.len();
        let mut freed = Vec::new();

        if new_page_count < old_page_count {
            for addr in self.pages.drain(new_page_count..).flatten() {
                freed.push(addr);
            }
        } else {
            self.pages.resize(new_page_count, None);
        }

        self.size = new_size;
        Ok(freed)
    }

    /// Set the userspace pager endpoint. Only valid before any page is allocated.
    pub fn set_pager(&mut self, endpoint: EndpointId) -> Result<(), SyscallError> {
        if self.has_pages {
            return Err(SyscallError::InvalidArgument);
        }
        self.pager = Some(endpoint);
        Ok(())
    }

    /// Increment the generation counter, revoking all existing handles.
    pub fn revoke(&mut self) {
        self.generation += 1;
    }

    /// Replace a page during COW fault resolution.
    pub fn replace_page(&mut self, page_idx: usize, new_addr: usize) {
        if page_idx < self.pages.len() {
            self.pages[page_idx] = Some(new_addr);
        }
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
        assert_eq!(vmo.resize(PAGE_SIZE), Err(SyscallError::AlreadySealed));
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
        let freed = vmo.resize(4 * PAGE_SIZE).unwrap();
        assert!(freed.is_empty());
        assert_eq!(vmo.page_count(), 4);
    }

    #[test]
    fn resize_shrink_frees_pages() {
        let mut vmo = make_vmo(4);
        vmo.alloc_page_at(2, || Some(0xCC)).unwrap();
        vmo.alloc_page_at(3, || Some(0xDD)).unwrap();
        let freed = vmo.resize(2 * PAGE_SIZE).unwrap();
        assert_eq!(freed, vec![0xCC, 0xDD]);
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
    fn generation_revoke() {
        let mut vmo = make_vmo(1);
        assert_eq!(vmo.generation(), 0);
        vmo.revoke();
        assert_eq!(vmo.generation(), 1);
        vmo.revoke();
        assert_eq!(vmo.generation(), 2);
    }

    #[test]
    fn refcount_lifecycle() {
        let mut vmo = make_vmo(1);
        assert_eq!(vmo.refcount(), 1);
        vmo.add_ref();
        assert_eq!(vmo.refcount(), 2);
        assert!(!vmo.release_ref());
        assert_eq!(vmo.refcount(), 1);
        assert!(vmo.release_ref());
    }
}
