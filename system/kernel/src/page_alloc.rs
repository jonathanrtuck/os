//! Physical page frame allocator (free-list, 4 KiB frames).
//!
//! Manages physical memory above the kernel heap. Each free frame stores a
//! pointer to the next free frame (intrusive linked list). Frames are returned
//! as physical addresses; callers convert to kernel VA when needed.

use super::memory;
use super::paging;
use super::sync::IrqMutex;

const PAGE_SIZE: usize = paging::PAGE_SIZE as usize;

struct FreeFrame {
    next: *mut FreeFrame,
}
struct State {
    head: *mut FreeFrame,
    free_count: usize,
}

static STATE: IrqMutex<State> = IrqMutex::new(State {
    head: core::ptr::null_mut(),
    free_count: 0,
});

/// Allocate one 4 KiB frame. Returns the physical address, or `None`.
pub fn alloc_frame() -> Option<usize> {
    let mut s = STATE.lock();

    if s.head.is_null() {
        return None;
    }

    let frame = s.head;

    // SAFETY: `head` is non-null and was previously initialized by `init` or
    // `free_frame`, both of which write a valid `FreeFrame` at that address.
    unsafe {
        s.head = (*frame).next;
    }

    s.free_count -= 1;

    // Zero the frame before returning.
    let va = frame as usize;

    // SAFETY: `va` points to a full page of kernel-mapped memory that we own.
    unsafe {
        core::ptr::write_bytes(va as *mut u8, 0, PAGE_SIZE);
    }

    Some(memory::virt_to_phys(va))
}
/// Return a frame to the free list.
pub fn free_frame(pa: usize) {
    let mut s = STATE.lock();
    let va = memory::phys_to_virt(pa);
    let frame = va as *mut FreeFrame;

    // SAFETY: `va` points to a full page of kernel-mapped memory. Writing a
    // FreeFrame header into the first 8 bytes is valid for any owned page.
    unsafe {
        (*frame).next = s.head;
    }

    s.head = frame;
    s.free_count += 1;
}
/// Initialize the frame allocator with all pages in `[start_pa, end_pa)`.
///
/// `start_pa` must be page-aligned and above the kernel heap.
/// Called once during boot with IRQs masked.
pub fn init(start_pa: usize, end_pa: usize) {
    let mut s = STATE.lock();
    let start = (start_pa + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let mut count = 0usize;
    let mut addr = start;

    while addr + PAGE_SIZE <= end_pa {
        let va = memory::phys_to_virt(addr);
        let frame = va as *mut FreeFrame;

        // SAFETY: `va` is a valid kernel VA for physical address `addr`.
        unsafe {
            (*frame).next = s.head;
            s.head = frame;
        }

        count += 1;
        addr += PAGE_SIZE;
    }

    s.free_count = count;
}
