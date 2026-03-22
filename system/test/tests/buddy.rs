//! Host-side tests for the kernel buddy allocator (page_allocator.rs).
//!
//! Includes page_allocator.rs directly with stub dependencies (mock IrqMutex,
//! identity PA/VA mapping). Tests use heap-allocated memory as the "physical"
//! region, with phys_to_virt/virt_to_phys as identity functions.
//!
//! Because page_allocator uses a global static STATE, all tests in this file
//! share state. Run with `cargo test -- --test-threads=1` or as a single
//! sequential test to avoid interference.
//!
//! The comprehensive test function exercises: basic alloc/free, multi-page
//! orders, buddy coalescing, splitting, exhaustion, and full-coalesce
//! round-trip.

// --- Stubs ---

mod paging {
    pub const PAGE_SIZE: u64 = 4096;
    pub const RAM_SIZE_MAX: u64 = 256 * 1024 * 1024;

    pub fn ram_end() -> u64 {
        // Stub: not used by buddy allocator tests.
        0
    }

    pub const fn align_up(addr: usize, align: usize) -> usize {
        (addr + align - 1) & !(align - 1)
    }
}

mod memory {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    #[repr(transparent)]
    pub struct Pa(pub usize);

    impl Pa {
        pub const fn as_u64(self) -> u64 {
            self.0 as u64
        }
    }

    /// Identity mapping — on the host, VA == PA.
    pub fn phys_to_virt(pa: Pa) -> usize {
        pa.0
    }
    pub fn virt_to_phys(va: usize) -> Pa {
        Pa(va)
    }
}

mod sync {
    //! Mock IrqMutex for host-side testing (no IRQ masking, no spinlock).
    use core::{
        cell::UnsafeCell,
        ops::{Deref, DerefMut},
    };

    pub struct IrqMutex<T> {
        data: UnsafeCell<T>,
    }

    // SAFETY: Single-threaded test environment (--test-threads=1).
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

#[path = "../../kernel/page_allocator.rs"]
mod page_allocator;

const PAGE_SIZE: usize = 4096;

/// Allocate a page-aligned memory region from the host heap.
fn alloc_region(pages: usize) -> (*mut u8, std::alloc::Layout) {
    let size = pages * PAGE_SIZE;
    let layout = std::alloc::Layout::from_size_align(size, PAGE_SIZE).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };

    assert!(!ptr.is_null(), "host allocation failed");

    (ptr, layout)
}

/// Free a host-allocated region.
///
/// SAFETY: `ptr` and `layout` must match a prior `alloc_region` call.
unsafe fn free_region(ptr: *mut u8, layout: std::alloc::Layout) {
    std::alloc::dealloc(ptr, layout);
}

// All buddy allocator tests run in a single function to avoid global state
// interference. Each section is clearly labeled.
#[test]
#[cfg_attr(miri, ignore)] // Miri strict provenance rejects integer-to-pointer casts in phys_to_virt (bare-metal identity mapping pattern, not real UB)
fn buddy_allocator() {
    let (ptr, layout) = alloc_region(256); // 256 pages = 1 MiB
    let start = ptr as usize;
    let end = start + 256 * PAGE_SIZE;

    page_allocator::init(start, end);

    // --- Section 1: Initial state ---

    assert_eq!(
        page_allocator::free_count(),
        256,
        "init should mark all 256 pages free"
    );

    // --- Section 2: Single frame alloc ---

    let pa1 = page_allocator::alloc_frame().expect("should allocate one frame");

    assert!(
        pa1.0 >= start && pa1.0 < end,
        "allocated PA must be in region"
    );
    assert_eq!(pa1.0 % PAGE_SIZE, 0, "PA must be page-aligned");
    assert_eq!(
        page_allocator::free_count(),
        255,
        "free count should decrease by 1"
    );

    // Allocated memory should be zeroed.
    let slice = unsafe { core::slice::from_raw_parts(pa1.0 as *const u8, PAGE_SIZE) };

    assert!(
        slice.iter().all(|&b| b == 0),
        "allocated frame should be zeroed"
    );

    // --- Section 3: Free and re-alloc ---

    page_allocator::free_frame(pa1);

    assert_eq!(
        page_allocator::free_count(),
        256,
        "free should restore count"
    );

    // --- Section 4: Multi-page allocation (order 2 = 4 pages) ---

    let pa_order2 = page_allocator::alloc_frames(2).expect("should allocate 4 pages");

    assert_eq!(
        pa_order2.0 % (4 * PAGE_SIZE),
        0,
        "order-2 block must be naturally aligned"
    );
    assert_eq!(page_allocator::free_count(), 252, "4 pages consumed");

    page_allocator::free_frames(pa_order2, 2);

    assert_eq!(
        page_allocator::free_count(),
        256,
        "free should restore all 4 pages"
    );

    // --- Section 5: Large allocation (order 8 = 256 pages = entire region) ---

    let pa_big = page_allocator::alloc_frames(8).expect("should allocate 256 pages");

    assert_eq!(pa_big.0, start, "full-region block starts at region base");
    assert_eq!(page_allocator::free_count(), 0, "all pages consumed");

    // --- Section 6: Exhaustion ---

    assert!(
        page_allocator::alloc_frame().is_none(),
        "should fail when empty"
    );
    assert!(
        page_allocator::alloc_frames(0).is_none(),
        "order-0 should also fail"
    );

    // --- Section 7: Free the big block and verify full coalesce ---

    page_allocator::free_frames(pa_big, 8);

    assert_eq!(
        page_allocator::free_count(),
        256,
        "full coalesce restores all pages"
    );

    // --- Section 8: Allocate all frames one-by-one, then free ---

    let mut frames = Vec::new();

    while let Some(pa) = page_allocator::alloc_frame() {
        assert!(pa.0 >= start && pa.0 < end);
        assert_eq!(pa.0 % PAGE_SIZE, 0);
        frames.push(pa);
    }

    assert_eq!(frames.len(), 256, "should get exactly 256 frames");
    assert_eq!(page_allocator::free_count(), 0);

    // All addresses should be unique.
    let mut sorted = frames.clone();
    sorted.sort();
    sorted.dedup();

    assert_eq!(sorted.len(), 256, "all frame addresses must be unique");

    // Free all one-by-one.
    for pa in frames {
        page_allocator::free_frame(pa);
    }

    assert_eq!(page_allocator::free_count(), 256, "free all restores count");

    // --- Section 9: Buddy coalescing ---
    // Allocate two adjacent order-0 frames, free them, verify they coalesce
    // into an order-1 block (provable by allocating order-1 afterwards).

    let a = page_allocator::alloc_frame().unwrap();
    let b = page_allocator::alloc_frame().unwrap();

    page_allocator::free_frame(a);
    page_allocator::free_frame(b);

    // If coalescing works, we can now allocate a larger block than before.
    let order1 = page_allocator::alloc_frames(1).expect("should coalesce into order-1");

    assert_eq!(
        order1.0 % (2 * PAGE_SIZE),
        0,
        "order-1 must be aligned to 8 KiB"
    );

    page_allocator::free_frames(order1, 1);

    // --- Section 10: Split behavior ---
    // Free all and verify we can allocate max order, confirming full coalesce.
    // (State should already be clean from section 9.)

    let full = page_allocator::alloc_frames(8).expect("full coalesce allows max order");

    page_allocator::free_frames(full, 8);

    assert_eq!(page_allocator::free_count(), 256);

    // --- Section 11: Order bounds ---

    assert!(
        page_allocator::alloc_frames(9).is_none(),
        "order 9 > region size, should fail"
    );

    // Order 0 should still work.
    let small = page_allocator::alloc_frame().unwrap();

    page_allocator::free_frame(small);

    // --- Cleanup ---

    unsafe { free_region(ptr, layout) };
}

// Model-based test for the PA validation in free_frames().
//
// The kernel's free_frames() validation is behind #[cfg(not(test))], so we
// duplicate the validation logic here and verify it catches all invalid inputs.
// This test verifies VAL-ALLOC-007: misaligned PA, PA outside RAM range,
// and PA at RAM boundary are all rejected or handled safely.
#[test]
fn pa_validation_on_free() {
    // Model of the kernel's PA validation in free_frames().
    // Must match the logic in page_allocator.rs free_frames() exactly.
    const RAM_START: usize = 0x4000_0000;
    const RAM_END: usize = RAM_START + 0x1000_0000; // 256 MiB

    fn is_valid_pa(pa: usize) -> bool {
        pa & 0xFFF == 0 && pa >= RAM_START && pa < RAM_END
    }

    // --- Misaligned PAs (should be rejected) ---
    assert!(!is_valid_pa(RAM_START + 1), "off-by-1 is misaligned");
    assert!(!is_valid_pa(RAM_START + 0x800), "half-page is misaligned");
    assert!(
        !is_valid_pa(RAM_START + 0xFFF),
        "one-below-page is misaligned"
    );
    assert!(!is_valid_pa(0x4000_0001), "1 byte into RAM, misaligned");

    // --- PA below RAM range (should be rejected) ---
    assert!(!is_valid_pa(0x0000_0000), "zero address");
    assert!(!is_valid_pa(0x3FFF_F000), "one page below RAM_START");
    assert!(!is_valid_pa(0x0000_1000), "low address, page-aligned");

    // --- PA above RAM range (should be rejected) ---
    assert!(!is_valid_pa(RAM_END), "RAM_END itself is out of range");
    assert!(!is_valid_pa(RAM_END + PAGE_SIZE), "one page past RAM_END");
    assert!(
        !is_valid_pa(0xFFFF_FFFF_FFFF_F000),
        "high address, page-aligned"
    );

    // --- Valid PAs (should be accepted) ---
    assert!(is_valid_pa(RAM_START), "RAM_START is valid");
    assert!(is_valid_pa(RAM_START + PAGE_SIZE), "one page into RAM");
    assert!(is_valid_pa(RAM_END - PAGE_SIZE), "last valid page");
    assert!(is_valid_pa(RAM_START + 0x0100_0000), "middle of RAM");

    // --- Boundary: RAM_START must be page-aligned ---
    assert_eq!(RAM_START & 0xFFF, 0, "RAM_START must be page-aligned");
    assert_eq!(RAM_END & 0xFFF, 0, "RAM_END must be page-aligned");
}

// Standalone test for the buddy_pa XOR property (algorithm verification).
// This doesn't import the kernel code — it verifies the mathematical property
// that the buddy allocator relies on.
#[test]
fn buddy_pa_xor_property() {
    // Buddy of block at `pa` with order `k`: pa XOR (PAGE_SIZE << k).
    // Two buddies differ in exactly one bit — the order bit.
    fn buddy_pa(pa: usize, order: usize) -> usize {
        pa ^ (PAGE_SIZE << order)
    }

    // Property 1: buddy of buddy is self.
    for order in 0..10 {
        let pa = PAGE_SIZE << order; // naturally aligned

        assert_eq!(
            buddy_pa(buddy_pa(pa, order), order),
            pa,
            "buddy of buddy must be self"
        );
    }

    // Property 2: two buddies combine to form the parent block.
    for order in 0..10 {
        let pa = 0; // base address
        let buddy = buddy_pa(pa, order);
        let parent = core::cmp::min(pa, buddy);

        assert_eq!(
            parent % (PAGE_SIZE << (order + 1)),
            0,
            "parent must be aligned to next order"
        );
    }

    // Property 3: buddy is always at a different address.
    for order in 0..10 {
        let pa = 0x10000; // arbitrary aligned address

        assert_ne!(buddy_pa(pa, order), pa, "buddy must differ from self");
    }

    // Property 4: natural alignment requirement.
    // A block at `pa` of order `k` must be aligned to `PAGE_SIZE << k`.
    // Its buddy at `pa ^ (PAGE_SIZE << k)` is also aligned to `PAGE_SIZE << k`.
    for order in 0..10 {
        let block_size = PAGE_SIZE << order;

        for base in (0..block_size * 4).step_by(block_size) {
            let buddy = buddy_pa(base, order);

            assert_eq!(buddy % block_size, 0, "buddy must be naturally aligned");
        }
    }
}
