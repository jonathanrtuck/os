//! Tests for heap ↔ slab dealloc routing.
//!
//! The kernel heap routes small allocations to slab, with linked-list as
//! fallback. The critical invariant: dealloc must route by POINTER ORIGIN
//! (which allocator served it), not by size class. This test verifies that
//! invariant by using a "greedy" slab stub that accepts any free — if the
//! heap's is_in_heap_region() check is removed, this test fails.
//!
//! Background: a cross-allocator contamination bug occurred when slab's
//! try_free() accepted pointers that were allocated by the linked-list
//! (during early boot before the buddy allocator was initialized). This
//! corrupted slab's free list with foreign addresses.

use core::alloc::{GlobalAlloc, Layout};

// --- Stubs ---

mod paging {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));

    pub const fn align_up(addr: usize, align: usize) -> usize {
        (addr + align - 1) & !(align - 1)
    }
}

mod memory {
    #[allow(dead_code)]
    pub const HEAP_SIZE: usize = 4096;

    #[allow(dead_code)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    #[repr(transparent)]
    pub struct Pa(pub usize);
}

/// Greedy slab stub: alloc always fails (simulating no buddy allocator),
/// but free ACCEPTS anything by size class. This is the exact behavior that
/// caused the cross-allocator contamination bug. The heap's address-based
/// routing must prevent these frees from reaching slab.
mod slab {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);

    pub fn try_alloc(_size: usize, _align: usize) -> *mut u8 {
        core::ptr::null_mut()
    }

    /// Greedy: accepts any free and counts it. In the real kernel, slab
    /// would accept based on size class — the heap must not let it.
    pub unsafe fn try_free(_ptr: *mut u8, _size: usize, _align: usize) -> bool {
        FREE_CALLS.fetch_add(1, Ordering::Relaxed);
        true
    }

    pub fn free_call_count() -> usize {
        FREE_CALLS.load(Ordering::Relaxed)
    }

    pub fn reset() {
        FREE_CALLS.store(0, Ordering::Relaxed);
    }
}

mod sync {
    use core::{
        cell::UnsafeCell,
        ops::{Deref, DerefMut},
    };

    pub struct IrqMutex<T> {
        data: UnsafeCell<T>,
    }

    unsafe impl<T: Send> Sync for IrqMutex<T> {}
    unsafe impl<T: Send> Send for IrqMutex<T> {}

    impl<T> IrqMutex<T> {
        pub const fn new(data: T) -> Self {
            Self {
                data: UnsafeCell::new(data),
            }
        }
        pub fn lock(&self) -> IrqGuard<'_, T> {
            IrqGuard {
                data: unsafe { &mut *self.data.get() },
            }
        }
    }

    pub struct IrqGuard<'a, T> {
        data: &'a mut T,
    }

    impl<T> Deref for IrqGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &T {
            self.data
        }
    }
    impl<T> DerefMut for IrqGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            self.data
        }
    }
}

#[path = "../../kernel/heap.rs"]
mod heap;

const PAGE_SIZE: usize = 16384;

fn alloc_region(size: usize) -> (*mut u8, std::alloc::Layout) {
    let layout = std::alloc::Layout::from_size_align(size, PAGE_SIZE).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };

    assert!(!ptr.is_null(), "host allocation failed");

    (ptr, layout)
}

fn init_heap(region: *mut u8, size: usize) {
    unsafe {
        let block_header = region as *mut usize;

        *block_header = size;
        *(block_header.add(1)) = 0;

        heap::ALLOCATOR.head.get().write(region as *mut _);
        heap::ALLOCATOR.region_start.get().write(region as usize);
        heap::ALLOCATOR
            .region_end
            .get()
            .write(region as usize + size);
    }
}

/// Verify that pointers allocated from the linked-list heap are NEVER
/// routed to slab on free — even when slab would accept them by size.
///
/// This is the exact scenario that caused the cross-allocator contamination
/// bug: slab can't grow (no buddy allocator), linked-list serves the alloc,
/// but dealloc routes to slab because the size fits a slab class.
#[test]
fn dealloc_routes_heap_pointers_to_linked_list_not_slab() {
    let (ptr, layout) = alloc_region(PAGE_SIZE);

    init_heap(ptr, PAGE_SIZE);
    slab::reset();

    let alloc = &heap::ALLOCATOR;

    // Allocate sizes that match slab classes (64, 128, 256, 512, 1024).
    // Slab alloc returns null, so linked-list serves all of them.
    let layouts: Vec<Layout> = [64, 128, 256, 512]
        .iter()
        .map(|&s| Layout::from_size_align(s, 8).unwrap())
        .collect();

    let mut ptrs = Vec::new();

    for l in &layouts {
        let p = unsafe { alloc.alloc(*l) };

        if !p.is_null() {
            // Pointer must be in the heap region.
            let addr = p as usize;
            let start = ptr as usize;

            assert!(
                addr >= start && addr < start + PAGE_SIZE,
                "allocated pointer must be in heap region"
            );

            ptrs.push((p, *l));
        }
    }

    assert!(!ptrs.is_empty(), "should allocate at least one block");

    // Free all. The heap must route these to linked-list, NOT slab.
    for (p, l) in &ptrs {
        unsafe { alloc.dealloc(*p, *l) };
    }

    assert_eq!(
        slab::free_call_count(),
        0,
        "slab must NEVER receive frees for linked-list pointers — \
         this would cause cross-allocator contamination"
    );

    unsafe { std::alloc::dealloc(ptr, layout) };
}

/// Verify that the heap can survive a full alloc→free cycle with the greedy
/// slab stub. If routing is wrong, the linked-list free list gets corrupted
/// and subsequent allocations crash or return overlapping pointers.
#[test]
fn no_corruption_after_alloc_free_cycle() {
    let (ptr, layout) = alloc_region(PAGE_SIZE);

    init_heap(ptr, PAGE_SIZE);
    slab::reset();

    let alloc = &heap::ALLOCATOR;
    let l = Layout::from_size_align(64, 8).unwrap();

    // Allocate, free, reallocate. If dealloc routed to the greedy slab
    // instead of linked-list, the linked-list never reclaims the memory
    // and this second allocation will fail or return a corrupt pointer.
    let p1 = unsafe { alloc.alloc(l) };

    assert!(!p1.is_null());

    unsafe { alloc.dealloc(p1, l) };

    let p2 = unsafe { alloc.alloc(l) };

    assert!(
        !p2.is_null(),
        "reallocation must succeed after correct free"
    );

    // Both pointers should be in the heap region.
    let start = ptr as usize;

    assert!(p2 as usize >= start && (p2 as usize) < start + PAGE_SIZE);

    unsafe { alloc.dealloc(p2, l) };

    assert_eq!(
        slab::free_call_count(),
        0,
        "slab must not receive any frees"
    );

    unsafe { std::alloc::dealloc(ptr, layout) };
}
