// AUDIT: 2026-03-14 — 6 unsafe blocks + 1 unsafe impl (Send) verified, 6-category checklist applied.
// No bugs found. PA validation on free verified (page-aligned, within RAM range). Buddy coalescing
// XOR trick verified (involution, alignment, parent formation). Free count accounting verified
// correct through split and coalesce paths. All SAFETY comments accurate.
//
//! Buddy allocator for physical page frames.
//!
//! Manages physical memory above the kernel heap. Supports single-page
//! allocation (order 0, 16 KiB) and multi-page contiguous allocation (up to
//! order MAX_ORDER). Buddy coalescing on free keeps fragmentation low.
//!
//! Existing single-page API (`alloc_frame`/`free_frame`) is preserved —
//! callers are unaffected.

#[cfg(not(test))]
use super::serial;
use super::{
    memory::{self, Pa},
    paging,
    sync::IrqMutex,
};

/// Maximum order: log2(RAM pages) so the allocator can coalesce up to the
/// full physical memory range. With 256 MiB RAM: 65536 pages = 2^16 → order 16.
/// Uses RAM_SIZE_MAX (compile-time upper bound) for array sizing. The actual
/// RAM size from the DTB may be smaller; init() sets region_end accordingly.
const RAM_PAGES: usize = (paging::RAM_SIZE_MAX / paging::PAGE_SIZE) as usize;
const MAX_ORDER: usize = RAM_PAGES.ilog2() as usize;
const PAGE_SIZE: usize = paging::PAGE_SIZE as usize;

static STATE: IrqMutex<State> = IrqMutex::new(State {
    free_lists: [core::ptr::null_mut(); MAX_ORDER + 1],
    free_count: 0,
    region_start: 0,
    region_end: 0,
    fail_after: None,
});

/// Intrusive free-list node stored at the start of each free block.
struct FreeBlock {
    next: *mut FreeBlock,
}
struct State {
    /// Per-order free lists. `free_lists[k]` chains free blocks of 2^k pages.
    free_lists: [*mut FreeBlock; MAX_ORDER + 1],
    /// Total number of free pages (across all orders).
    free_count: usize,
    /// Physical address range managed by this allocator.
    region_start: usize,
    region_end: usize,
    /// OOM fault injection: if Some(n), alloc returns None after n successes.
    fail_after: Option<usize>,
}
// SAFETY: FreeBlock pointers are only accessed under STATE lock.
unsafe impl Send for State {}

/// Compute the buddy's physical address for a block at `pa` of order `order`.
///
/// Buddy PA = pa XOR (block_size). Two buddies always differ in exactly one
/// bit position (the order bit), so XOR toggles between them.
fn buddy_pa(pa: usize, order: usize) -> usize {
    pa ^ (PAGE_SIZE << order)
}
/// Remove a specific block from a free list (by physical address).
/// Returns true if found and removed.
fn remove_from_list(head: &mut *mut FreeBlock, pa: usize) -> bool {
    let target_va = memory::phys_to_virt(Pa(pa)) as *mut FreeBlock;
    let mut prev: *mut *mut FreeBlock = head;

    // SAFETY: All pointers in the free list were placed there by init()
    // or free_frames() and point to valid kernel-mapped memory.
    unsafe {
        loop {
            let current = *prev;

            if current.is_null() {
                return false;
            }
            if current == target_va {
                *prev = (*current).next;

                return true;
            }

            prev = &mut (*current).next;
        }
    }
}

/// Allocate one 4 KiB frame. Returns the physical address, or `None`.
pub fn alloc_frame() -> Option<Pa> {
    alloc_frames(0)
}
/// Allocate 2^order contiguous pages. Returns the physical address of the
/// first page, or `None` if no contiguous block is available.
pub fn alloc_frames(order: usize) -> Option<Pa> {
    assert!(order <= MAX_ORDER, "order exceeds MAX_ORDER");

    let mut s = STATE.lock();

    if let Some(ref mut remaining) = s.fail_after {
        if *remaining == 0 {
            return None;
        }

        *remaining -= 1;
    }

    // Find the smallest order >= requested that has a free block.
    let mut found_order = order;

    while found_order <= MAX_ORDER {
        if !s.free_lists[found_order].is_null() {
            break;
        }

        found_order += 1;
    }

    if found_order > MAX_ORDER {
        return None;
    }

    // Pop a block from the found order.
    let block = s.free_lists[found_order];

    // SAFETY: block is non-null (checked above) and was placed in the list
    // by init() or free_frames(), pointing to valid kernel-mapped memory.
    unsafe {
        s.free_lists[found_order] = (*block).next;
    }

    let pages = 1usize << found_order;

    s.free_count -= pages;

    let va = block as usize;
    let pa = memory::virt_to_phys(va).0;
    // Split down to the requested order.
    let mut current_order = found_order;

    while current_order > order {
        current_order -= 1;

        let buddy = pa + (PAGE_SIZE << current_order);
        let buddy_va = memory::phys_to_virt(Pa(buddy)) as *mut FreeBlock;

        // SAFETY: buddy_va points to kernel-mapped memory within our
        // managed region. Writing a FreeBlock header is valid.
        unsafe {
            (*buddy_va).next = s.free_lists[current_order];
            s.free_lists[current_order] = buddy_va;
        }

        s.free_count += 1 << current_order;
    }

    // Zero the allocated block.
    let block_size = PAGE_SIZE << order;

    // SAFETY: va points to `block_size` bytes of kernel-mapped memory.
    unsafe {
        core::ptr::write_bytes(va as *mut u8, 0, block_size);
    }

    Some(Pa(pa))
}
/// Total number of free pages (across all orders).
pub fn free_count() -> usize {
    STATE.lock().free_count
}
/// Return a single 4 KiB frame to the allocator.
pub fn free_frame(pa: Pa) {
    free_frames(pa, 0)
}
/// Free 2^order contiguous pages starting at physical address `pa`.
///
/// Coalesces with buddy blocks up to MAX_ORDER.
pub fn free_frames(pa: Pa, order: usize) {
    assert!(order <= MAX_ORDER, "order exceeds MAX_ORDER");

    // Validate PA before writing to it — a corrupted PA would cause a data abort.
    // Uses runtime ram_end() (from DTB) instead of the compile-time upper bound.
    // Gated on not(test) because host tests use heap addresses, not real RAM.
    #[cfg(not(test))]
    if pa.0 & (PAGE_SIZE - 1) != 0
        || pa.0 < paging::RAM_START as usize
        || pa.0 >= paging::ram_end() as usize
    {
        serial::panic_puts("free_frames: bad PA 0x");
        serial::panic_put_hex(pa.0 as u64);
        serial::panic_puts("\n");

        panic!("free_frames: PA outside RAM or unaligned");
    }

    let mut s = STATE.lock();
    let mut current_pa = pa.0;
    let mut current_order = order;

    // Try to coalesce with buddy at each level.
    while current_order < MAX_ORDER {
        let buddy = buddy_pa(current_pa, current_order);

        // Buddy must be within our managed region.
        if buddy < s.region_start || buddy >= s.region_end {
            break;
        }
        // Try to remove buddy from the free list at this order.
        if !remove_from_list(&mut s.free_lists[current_order], buddy) {
            break; // Buddy is allocated — can't coalesce.
        }

        // Coalesce: the combined block starts at the lower address.
        s.free_count -= 1 << current_order;
        current_pa = core::cmp::min(current_pa, buddy);
        current_order += 1;
    }

    // Insert the (possibly coalesced) block into the appropriate list.
    let va = memory::phys_to_virt(Pa(current_pa)) as *mut FreeBlock;

    // SAFETY: va points to kernel-mapped memory within our managed region.
    unsafe {
        (*va).next = s.free_lists[current_order];
        s.free_lists[current_order] = va;
    }

    s.free_count += 1 << current_order;
}
/// Initialize the frame allocator with all pages in `[start_pa, end_pa)`.
///
/// Pages are inserted at the highest possible order for efficient coalescing.
/// Called once during boot with IRQs masked.
pub fn init(start_pa: usize, end_pa: usize) {
    let mut s = STATE.lock();
    let start = paging::align_up(start_pa, PAGE_SIZE);

    s.region_start = start;
    s.region_end = end_pa;

    let mut addr = start;

    while addr + PAGE_SIZE <= end_pa {
        // Find the highest order this block can be inserted at.
        // Constraints: must be naturally aligned AND must fit within region.
        let mut order = 0;

        while order < MAX_ORDER {
            let next_order = order + 1;
            let block_size = PAGE_SIZE << next_order;

            // Natural alignment check: addr must be aligned to block_size.
            if addr & (block_size - 1) != 0 {
                break;
            }
            // Fit check: the block must not extend beyond end_pa.
            if addr + block_size > end_pa {
                break;
            }

            order = next_order;
        }

        let va = memory::phys_to_virt(Pa(addr)) as *mut FreeBlock;

        // SAFETY: va is a valid kernel VA for physical address addr.
        unsafe {
            (*va).next = s.free_lists[order];
            s.free_lists[order] = va;
        }

        let pages = 1usize << order;

        s.free_count += pages;
        addr += PAGE_SIZE << order;
    }
}
/// Set OOM fault injection: alloc returns None after `n` successful allocations.
/// Pass `None` to disable (used by test crate).
#[allow(dead_code)]
pub fn set_fail_after(n: Option<usize>) {
    STATE.lock().fail_after = n;
}
