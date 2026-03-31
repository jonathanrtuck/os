//! Userspace slab allocator — O(1) alloc/free for common sizes.
//!
//! Two-tier design:
//! - **Small** (≤ 2048 bytes): slab with 8 power-of-two size classes.
//!   Free lists carved from dedicated 16 KiB slab pages. O(1) alloc/free.
//! - **Large** (> 2048 bytes): direct page allocation from kernel.
//!   One syscall per alloc/free.
//!
//! Size classes: 16, 32, 64, 128, 256, 512, 1024, 2048 bytes.

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{syscalls::memory_alloc, types::HeapStats, PAGE_SIZE};

// ---------------------------------------------------------------------------
// Size classes
// ---------------------------------------------------------------------------

const NUM_CLASSES: usize = 8;
const CLASS_SIZES: [usize; NUM_CLASSES] = [16, 32, 64, 128, 256, 512, 1024, 2048];

/// Find the size class index for a given effective size.
/// Returns `None` if the size exceeds all classes (large allocation).
#[inline]
fn class_index(size: usize) -> Option<usize> {
    // Binary search on powers of two. The classes are 16..=2048 = 2^4..=2^11.
    if size <= 16 {
        Some(0)
    } else if size <= 2048 {
        // next_power_of_two then log2 to find the class.
        // size=17 → npt=32 → trailing_zeros=5 → index=5-4=1 → CLASS_SIZES[1]=32 ✓
        let npt = size.next_power_of_two();
        Some(npt.trailing_zeros() as usize - 4)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Free slot header — embedded in each free slot
// ---------------------------------------------------------------------------

struct FreeSlot {
    next: *mut FreeSlot,
}

// ---------------------------------------------------------------------------
// Slab allocator
// ---------------------------------------------------------------------------

pub struct UserHeap {
    classes: [UnsafeCell<*mut FreeSlot>; NUM_CLASSES],
    lock: AtomicBool,
    total_allocated: UnsafeCell<usize>,
    total_freed: UnsafeCell<usize>,
    pages_requested: UnsafeCell<usize>,
}

impl UserHeap {
    pub const fn new() -> Self {
        const NULL: UnsafeCell<*mut FreeSlot> = UnsafeCell::new(core::ptr::null_mut());
        Self {
            classes: [NULL; NUM_CLASSES],
            lock: AtomicBool::new(false),
            total_allocated: UnsafeCell::new(0),
            total_freed: UnsafeCell::new(0),
            pages_requested: UnsafeCell::new(0),
        }
    }

    pub(crate) fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    pub(crate) fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }

    /// Carve a freshly allocated slab page into free slots for `class_size`.
    ///
    /// # Safety
    ///
    /// `page_va` must point to a valid, zeroed, `PAGE_SIZE`-byte region.
    /// Caller must hold the lock.
    unsafe fn carve_slab_page(&self, page_va: usize, ci: usize) {
        let class_size = CLASS_SIZES[ci];
        let slots = PAGE_SIZE / class_size;
        let head = &mut *self.classes[ci].get();

        // Build the free list from the end so that the first slot is at the head.
        // This gives ascending-address allocation order.
        for i in (0..slots).rev() {
            let slot = (page_va + i * class_size) as *mut FreeSlot;
            (*slot).next = *head;
            *head = slot;
        }
    }
}

// SAFETY: All free list access is protected by a spinlock (AtomicBool CAS).
unsafe impl Sync for UserHeap {}

unsafe impl GlobalAlloc for UserHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Effective size accounts for alignment: a 16-byte alloc with 64-byte
        // alignment needs a 64-byte slot (every 64-byte slot in a slab page
        // is naturally 64-byte aligned because pages are page-aligned and
        // slots are power-of-two sized).
        let effective = layout.size().max(layout.align());

        match class_index(effective) {
            Some(ci) => {
                // Small allocation — slab path.
                self.acquire();
                let head = &mut *self.classes[ci].get();

                if (*head).is_null() {
                    // Free list empty — allocate a new slab page.
                    let va = match memory_alloc(1) {
                        Ok(va) => va,
                        Err(_) => {
                            self.release();
                            return core::ptr::null_mut();
                        }
                    };
                    *self.pages_requested.get() += 1;
                    self.carve_slab_page(va, ci);
                }

                // Pop from free list.
                let slot = *head;
                *head = (*slot).next;
                *self.total_allocated.get() += CLASS_SIZES[ci];
                self.release();
                slot as *mut u8
            }
            None => {
                // Large allocation — direct page allocation.
                let pages = (layout.size() + PAGE_SIZE - 1) / PAGE_SIZE;
                let va = match memory_alloc(pages as u64) {
                    Ok(va) => va,
                    Err(_) => return core::ptr::null_mut(),
                };
                self.acquire();
                *self.total_allocated.get() += pages * PAGE_SIZE;
                *self.pages_requested.get() += pages;
                self.release();
                va as *mut u8
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let effective = layout.size().max(layout.align());

        match class_index(effective) {
            Some(ci) => {
                // Small deallocation — push to free list.
                self.acquire();
                let head = &mut *self.classes[ci].get();
                let slot = ptr as *mut FreeSlot;
                (*slot).next = *head;
                *head = slot;
                *self.total_freed.get() += CLASS_SIZES[ci];
                self.release();
            }
            None => {
                // Large deallocation — return pages to kernel.
                let pages = (layout.size() + PAGE_SIZE - 1) / PAGE_SIZE;
                let _ = crate::syscalls::memory_free(ptr as usize, pages as u64);
                self.acquire();
                *self.total_freed.get() += pages * PAGE_SIZE;
                self.release();
            }
        }
    }
}

/// Return heap usage statistics.
///
/// Acquires the heap spinlock to read consistent counters.
pub fn heap_stats() -> HeapStats {
    crate::HEAP.acquire();
    // SAFETY: Counters are protected by the same spinlock as the free lists.
    let stats = unsafe {
        HeapStats {
            total_allocated: *crate::HEAP.total_allocated.get(),
            total_freed: *crate::HEAP.total_freed.get(),
            pages_requested: *crate::HEAP.pages_requested.get(),
        }
    };
    crate::HEAP.release();
    stats
}
