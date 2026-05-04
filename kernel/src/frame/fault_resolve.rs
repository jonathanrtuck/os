//! Hardware fault resolution — COW copy and lazy page allocation.
//!
//! These functions perform the actual physical page operations that resolve
//! page faults. They live in `frame/` because they require unsafe page
//! table and physical memory manipulation.

#[cfg(target_os = "none")]
use super::arch::{page_alloc, page_table};
#[cfg(target_os = "none")]
use super::user_mem;

/// Resolve a COW fault: allocate a new page, copy from the original, and
/// remap as writable.
#[cfg(target_os = "none")]
pub fn resolve_cow(
    root: page_alloc::PhysAddr,
    asid: page_table::Asid,
    vaddr: usize,
    old_pa: page_alloc::PhysAddr,
) -> bool {
    let new_pa = match page_alloc::alloc_page() {
        Some(pa) => pa,
        None => return false,
    };
    // SAFETY: both old_pa and new_pa are valid physical addresses from the
    // page allocator. Identity-mapped, so PA == VA for kernel access.
    unsafe {
        core::ptr::copy_nonoverlapping(
            old_pa.0 as *const u8,
            new_pa.0 as *mut u8,
            crate::config::PAGE_SIZE,
        );
    }
    page_table::map_page(
        root,
        page_table::VirtAddr(vaddr),
        new_pa,
        page_table::Perms::RW,
    );
    page_table::invalidate_page(asid, page_table::VirtAddr(vaddr));
    page_alloc::release(old_pa);
    true
}

/// Resolve a lazy allocation fault: allocate a zeroed page and map it.
#[cfg(target_os = "none")]
pub fn resolve_lazy(root: page_alloc::PhysAddr, vaddr: usize, perms: page_table::Perms) -> bool {
    let pa = match page_alloc::alloc_page() {
        Some(pa) => pa,
        None => return false,
    };
    user_mem::zero_phys(pa.0, crate::config::PAGE_SIZE);
    page_table::map_page(root, page_table::VirtAddr(vaddr), pa, perms);
    true
}
