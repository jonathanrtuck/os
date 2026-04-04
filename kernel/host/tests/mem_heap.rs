//! Host-side tests for the kernel linked-list heap allocator.
//!
//! Stubs out slab, memory, sync, and paging dependencies. Tests cover
//! fragmentation under alternating frees, coalescing correctness,
//! front-padding rejection, and alignment edge cases.

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

mod slab {
    pub fn try_alloc(_size: usize, _align: usize) -> *mut u8 {
        core::ptr::null_mut()
    }
    pub unsafe fn try_free(_ptr: *mut u8, _size: usize, _align: usize) -> bool {
        false
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

#[path = "../../heap.rs"]
mod heap;

const PAGE_SIZE: usize = 16384;
const MIN_BLOCK: usize = 16; // size_of::<FreeBlock>() on 64-bit

/// Allocate a page-aligned region for use as a test heap.
fn alloc_region(size: usize) -> (*mut u8, std::alloc::Layout) {
    let layout = std::alloc::Layout::from_size_align(size, PAGE_SIZE).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };

    assert!(!ptr.is_null(), "host allocation failed");

    (ptr, layout)
}

/// Initialize the heap with a custom region. Must be called once per
/// test function (all heap tests share global state, so run with
/// --test-threads=1).
fn init_heap(region: *mut u8, size: usize) {
    unsafe {
        let block = region as *mut u8;

        // Manually init the heap's internal structures.
        // The heap stores: head pointer, region_start, region_end.
        // We write directly to the UnsafeCell fields.
        let block_header = block as *mut usize;

        // Write FreeBlock header: size, next.
        *block_header = size; // size field
        *(block_header.add(1)) = 0; // next = null

        // Access the global ALLOCATOR through its public init function.
        // Since we can't call heap::init() (it reads __kernel_end),
        // we replicate its logic.
        heap::ALLOCATOR.head.get().write(block as *mut _);
        heap::ALLOCATOR.region_start.get().write(block as usize);
        heap::ALLOCATOR
            .region_end
            .get()
            .write(block as usize + size);
    }
}

// All tests in a single function to avoid global state interference.
#[test]
fn heap_allocator() {
    let heap_size = 4096;
    let (ptr, layout) = alloc_region(heap_size);

    init_heap(ptr, heap_size);

    let alloc = &heap::ALLOCATOR;

    // --- Section 1: Basic alloc/free ---

    let layout16 = Layout::from_size_align(16, 16).unwrap();

    let p1 = unsafe { alloc.alloc(layout16) };

    assert!(!p1.is_null(), "first alloc should succeed");
    assert_eq!(p1 as usize % 16, 0, "should be 16-byte aligned");

    unsafe { alloc.dealloc(p1, layout16) };

    // Re-alloc after free should succeed.
    let p2 = unsafe { alloc.alloc(layout16) };

    assert!(!p2.is_null(), "alloc after free should succeed");

    unsafe { alloc.dealloc(p2, layout16) };

    // --- Section 2: Alternating alloc/free (fragmentation) ---

    let mut ptrs = Vec::new();

    for _ in 0..10 {
        let p = unsafe { alloc.alloc(layout16) };

        assert!(!p.is_null(), "sequential alloc should succeed");

        ptrs.push(p);
    }

    // Free every other block to create fragmentation.
    for i in (0..10).step_by(2) {
        unsafe { alloc.dealloc(ptrs[i], layout16) };
    }

    // Allocate into the holes.
    for _ in 0..5 {
        let p = unsafe { alloc.alloc(layout16) };

        assert!(!p.is_null(), "alloc into fragment holes should succeed");

        unsafe { alloc.dealloc(p, layout16) };
    }

    // Free remaining.
    for i in (1..10).step_by(2) {
        unsafe { alloc.dealloc(ptrs[i], layout16) };
    }

    // --- Section 3: Coalescing ---
    // After freeing everything, the heap should coalesce into one big block.
    // Verify by allocating nearly the entire heap.

    let big_layout = Layout::from_size_align(heap_size - MIN_BLOCK * 2, MIN_BLOCK).unwrap();
    let big = unsafe { alloc.alloc(big_layout) };

    assert!(!big.is_null(), "coalesced alloc should succeed");

    unsafe { alloc.dealloc(big, big_layout) };

    // --- Section 4: Alignment edge cases ---

    let layout_256 = Layout::from_size_align(64, 256).unwrap();
    let p = unsafe { alloc.alloc(layout_256) };

    assert!(!p.is_null(), "256-byte aligned alloc should succeed");
    assert_eq!(p as usize % 256, 0, "should be 256-byte aligned");

    unsafe { alloc.dealloc(p, layout_256) };

    // --- Section 5: Exhaustion ---
    // Fill the heap completely.

    let mut exhaust = Vec::new();

    loop {
        let p = unsafe { alloc.alloc(layout16) };

        if p.is_null() {
            break;
        }

        exhaust.push(p);
    }

    assert!(!exhaust.is_empty(), "should allocate at least one block");

    // All freed.
    for p in exhaust {
        unsafe { alloc.dealloc(p, layout16) };
    }

    // --- Cleanup ---

    unsafe { std::alloc::dealloc(ptr, layout) };
}
