// AUDIT: 2026-03-14 — 2 unsafe blocks + 2 unsafe fn verified, 6-category checklist applied.
// No bugs found. Free list integrity sound: grow() writes FreeNode headers before linking,
// alloc() pops head (O(1)), free() pushes to head (O(1)). All mutations under IrqMutex.
// Alignment correct: obj_size is power-of-two ≥ 64, exceeds size_of::<FreeNode>() (8).
// OOM in grow() returns early, alloc() returns null, heap falls through to linked-list.
// No cache-line or AArch64 issues. Slab pages grow monotonically (never freed) — intentional.

//! Power-of-two slab allocator for small kernel objects.
//!
//! Each `SlabCache` manages objects of a fixed size. Slabs are 4 KiB pages
//! divided into N objects, with an embedded free list (first 8 bytes of each
//! free object = pointer to next free). `alloc()` is O(1) pop, `free()` is
//! O(1) push.
//!
//! Used by the heap allocator for small allocations (≤2048 bytes). Larger
//! allocations fall through to the linked-list allocator.

use super::{memory, page_allocator, paging, sync::IrqMutex};

/// Number of size classes: 64, 128, 256, 512, 1024, 2048 bytes.
const NUM_CLASSES: usize = 6;
const PAGE_SIZE: usize = paging::PAGE_SIZE as usize;
const SIZE_CLASSES: [usize; NUM_CLASSES] = [64, 128, 256, 512, 1024, 2048];

static SLAB: IrqMutex<SlabState> = IrqMutex::new(SlabState {
    caches: [
        SlabCache::new(64),
        SlabCache::new(128),
        SlabCache::new(256),
        SlabCache::new(512),
        SlabCache::new(1024),
        SlabCache::new(2048),
    ],
});

struct FreeNode {
    next: *mut FreeNode,
}
struct SlabCache {
    free_head: *mut FreeNode,
    obj_size: usize,
}
struct SlabState {
    caches: [SlabCache; NUM_CLASSES],
}

impl SlabCache {
    /// Allocate one object from this cache.
    fn alloc(&mut self) -> *mut u8 {
        if self.free_head.is_null() {
            self.grow();
        }

        if self.free_head.is_null() {
            return core::ptr::null_mut();
        }

        let node = self.free_head;

        // SAFETY: node is non-null and was either initialized by grow() or
        // returned by a prior free(). It points to a valid FreeNode.
        unsafe {
            self.free_head = (*node).next;
        }

        node as *mut u8
    }
    /// Return an object to this cache.
    ///
    /// # Safety
    ///
    /// `ptr` must have been returned by a prior `alloc()` on this same
    /// cache (same size class), and must not have been freed already.
    /// The caller is responsible for ensuring no aliases to this memory
    /// remain. Writing a FreeNode header is sound because the object is
    /// at least 64 bytes (minimum size class ≥ `size_of::<FreeNode>()`)
    /// and was originally carved from a page-aligned slab.
    unsafe fn free(&mut self, ptr: *mut u8) {
        // SAFETY: Caller guarantees `ptr` was returned by a prior `alloc()`
        // on this same cache and has not been freed already. The pointer is
        // at least 64-byte aligned (minimum size class) and points to an
        // object within a slab page. Writing a FreeNode header (8 bytes) is
        // within bounds because the minimum object size (64) exceeds
        // size_of::<FreeNode>() (16 on 64-bit, 8 on 32-bit).
        let node = ptr as *mut FreeNode;

        (*node).next = self.free_head;
        self.free_head = node;
    }
    /// Allocate a new 4 KiB slab page and carve it into objects.
    fn grow(&mut self) {
        let pa = match page_allocator::alloc_frame() {
            Some(pa) => pa,
            None => return,
        };
        let va = memory::phys_to_virt(pa);
        let objects_per_slab = PAGE_SIZE / self.obj_size;

        for i in 0..objects_per_slab {
            let obj = (va + i * self.obj_size) as *mut FreeNode;

            // SAFETY: va points to a zeroed 4 KiB page. Each obj is within
            // bounds and properly aligned (obj_size is a power of two ≥ 64,
            // which is ≥ align_of::<FreeNode>()).
            unsafe {
                (*obj).next = self.free_head;
                self.free_head = obj;
            }
        }
    }
    const fn new(obj_size: usize) -> Self {
        Self {
            free_head: core::ptr::null_mut(),
            obj_size,
        }
    }
}

/// Find the size class index for a given size and alignment.
/// Returns `None` if the allocation is too large or has unusual alignment.
pub(crate) fn size_class(size: usize, align: usize) -> Option<usize> {
    // Slab objects are naturally aligned to their size class.
    // If the requested alignment exceeds the size class, we can't serve it.
    for (i, &class_size) in SIZE_CLASSES.iter().enumerate() {
        if size <= class_size && align <= class_size {
            return Some(i);
        }
    }

    None
}
/// Try to allocate from the slab allocator. Returns null if the size/alignment
/// doesn't fit a slab class.
pub fn try_alloc(size: usize, align: usize) -> *mut u8 {
    match size_class(size, align) {
        Some(idx) => {
            let mut s = SLAB.lock();

            s.caches[idx].alloc()
        }
        None => core::ptr::null_mut(),
    }
}
/// Try to free to the slab allocator. Returns false if the size/alignment
/// doesn't match a slab class (caller should use the general allocator).
///
/// SAFETY: `ptr` must have been returned by a prior `try_alloc()` with
/// the same size class.
pub unsafe fn try_free(ptr: *mut u8, size: usize, align: usize) -> bool {
    match size_class(size, align) {
        Some(idx) => {
            let mut s = SLAB.lock();

            s.caches[idx].free(ptr);

            true
        }
        None => false,
    }
}
