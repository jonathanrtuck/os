//! Physical page frame allocator (free-list, 4 KiB frames).
//!
//! Manages physical memory above the kernel heap. Each free frame stores a
//! pointer to the next free frame (intrusive linked list). Frames are returned
//! as physical addresses; callers convert to kernel VA when needed.

use super::memory;

const PAGE_SIZE: usize = 4096;

struct FreeFrame {
    next: *mut FreeFrame,
}

static mut HEAD: *mut FreeFrame = core::ptr::null_mut();
static mut FREE_COUNT: usize = 0;

/// Allocate one 4 KiB frame. Returns the physical address, or `None`.
pub fn alloc_frame() -> Option<usize> {
    unsafe {
        if HEAD.is_null() {
            return None;
        }

        let frame = HEAD;

        HEAD = (*frame).next;
        FREE_COUNT -= 1;

        // Zero the frame before returning.
        let va = frame as usize;

        core::ptr::write_bytes(va as *mut u8, 0, PAGE_SIZE);

        Some(memory::virt_to_phys(va))
    }
}
/// Number of free frames remaining.
pub fn free_count() -> usize {
    unsafe { FREE_COUNT }
}
/// Return a frame to the free list.
pub fn free_frame(pa: usize) {
    unsafe {
        let va = memory::phys_to_virt(pa);
        let frame = va as *mut FreeFrame;

        (*frame).next = HEAD;
        HEAD = frame;
        FREE_COUNT += 1;
    }
}
/// Initialize the frame allocator with all pages in `[start_pa, end_pa)`.
///
/// `start_pa` must be page-aligned and above the kernel heap.
/// Called once during boot with IRQs masked.
pub fn init(start_pa: usize, end_pa: usize) {
    let start = (start_pa + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let mut count = 0usize;
    let mut addr = start;

    while addr + PAGE_SIZE <= end_pa {
        let va = memory::phys_to_virt(addr);
        let frame = va as *mut FreeFrame;

        unsafe {
            (*frame).next = HEAD;
            HEAD = frame;
        }

        count += 1;
        addr += PAGE_SIZE;
    }

    unsafe { FREE_COUNT = count };
}
