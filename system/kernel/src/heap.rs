//! Bump allocator.
//!
//! Advances a pointer for each allocation, never frees. Simple and fast,
//! suitable while the kernel has no deallocation scenarios. Upgrade to a
//! linked-list or buddy allocator when threads/teardown need real freeing.

use super::memory;
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

struct BumpAllocator {
    next: AtomicUsize,
    end: AtomicUsize,
}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            next: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + layout.align() - 1) & !(layout.align() - 1);
            let new_next = match aligned.checked_add(layout.size()) {
                Some(n) => n,
                None => return core::ptr::null_mut(),
            };

            if new_next > self.end.load(Ordering::Relaxed) {
                return core::ptr::null_mut();
            }

            if self
                .next
                .compare_exchange_weak(current, new_next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator: individual frees are a no-op.
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

/// Initialize the heap. Call after `memory::init()` (page tables must be live).
pub fn init() {
    extern "C" {
        static __kernel_end: u8;
    }

    let start = unsafe { &__kernel_end as *const u8 as usize };

    ALLOCATOR.next.store(start, Ordering::Relaxed);
    ALLOCATOR
        .end
        .store(start + memory::HEAP_SIZE, Ordering::Relaxed);
}
