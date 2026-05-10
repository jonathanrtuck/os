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
/// remap as writable using break-before-make.
///
/// Returns the new physical address on success so the caller can update the
/// VMO's page record. Returns `None` on OOM.
#[cfg(target_os = "none")]
pub fn resolve_cow(
    root: page_alloc::PhysAddr,
    asid: page_table::Asid,
    vaddr: usize,
    old_pa: page_alloc::PhysAddr,
) -> Option<usize> {
    let new_pa = page_alloc::alloc_page()?;

    // SAFETY: both old_pa and new_pa are valid physical addresses from the
    // page allocator. phys_to_virt maps to TTBR1 upper-half VAs.
    unsafe {
        let src = super::arch::platform::phys_to_virt(old_pa.0) as *const u8;
        let dst = super::arch::platform::phys_to_virt(new_pa.0) as *mut u8;

        core::ptr::copy_nonoverlapping(src, dst, crate::config::PAGE_SIZE);
    }

    // Break-before-make: unmap old valid PTE, TLBI, then map new PTE.
    page_table::replace_page(
        root,
        asid,
        page_table::VirtAddr(vaddr),
        new_pa,
        page_table::Perms::RW,
    );
    page_alloc::release(old_pa);

    Some(new_pa.0)
}

/// Resolve an existing-page fault: a VMO page is committed but not yet mapped
/// in this address space's page table (cross-space mapping or TLB eviction).
#[cfg(target_os = "none")]
pub fn resolve_existing(
    root: page_alloc::PhysAddr,
    vaddr: usize,
    pa: page_alloc::PhysAddr,
    perms: page_table::Perms,
) {
    page_table::map_page(root, page_table::VirtAddr(vaddr), pa, perms);

    pte_barrier();
}

/// Resolve a lazy allocation fault: allocate a zeroed page and map it.
///
/// Returns the new physical address on success so the caller can update the
/// VMO's page record. Returns `None` on OOM.
#[cfg(target_os = "none")]
pub fn resolve_lazy(
    root: page_alloc::PhysAddr,
    vaddr: usize,
    perms: page_table::Perms,
) -> Option<usize> {
    let pa = page_alloc::alloc_page()?;

    user_mem::zero_phys(pa.0, crate::config::PAGE_SIZE);
    page_table::map_page(root, page_table::VirtAddr(vaddr), pa, perms);

    pte_barrier();

    Some(pa.0)
}

/// Ensure a newly written PTE is visible to the page table walker before
/// the faulting instruction retries. DSB ISH orders the PTE store; ISB
/// flushes the pipeline so the retry fetches through the updated tables.
#[cfg(target_os = "none")]
fn pte_barrier() {
    // SAFETY: DSB ISH + ISB are pure barrier instructions with no side
    // effects beyond ordering. Required by the ARM ARM after writing PTEs.
    unsafe {
        core::arch::asm!("dsb ish", "isb", options(nostack));
    }
}
