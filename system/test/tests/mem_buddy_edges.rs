//! Edge case tests for the buddy page allocator (page_allocator.rs).
//!
//! Supplements mem_buddy.rs (3 tests) with targeted edge-case coverage:
//! power-of-two allocation alignment, buddy coalescing verification,
//! fragmentation resistance, zero-fill guarantee, OOM fault injection,
//! page alignment, and free-list integrity after mixed alloc/free.
//!
//! Uses the same #[path] include and stub pattern as mem_buddy.rs.
//! Because page_allocator uses a global static STATE, all tests share
//! state. Each test function reinitializes the allocator with a fresh
//! heap region to avoid cross-test interference.

// --- Stubs ---

mod paging {
    #![allow(dead_code)]
    include!(env!("SYSTEM_CONFIG"));

    // RAM_SIZE_MAX is provided by system_config.

    pub fn ram_end() -> u64 {
        // Stub: not used by buddy allocator tests (validation is #[cfg(not(test))]).
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

const PAGE_SIZE: usize = 16384;

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

// All edge-case tests run in a single function to avoid global state
// interference (page_allocator uses a static STATE). Each section
// reinitializes the allocator.
#[test]
#[cfg_attr(miri, ignore)]
fn buddy_edge_cases() {
    // =====================================================================
    // Section 1: Exact power-of-two allocation — alignment
    // =====================================================================
    {
        let (ptr, layout) = alloc_region(256);
        let start = ptr as usize;
        let end = start + 256 * PAGE_SIZE;

        page_allocator::reset();
        page_allocator::init(start, end);

        // Order 0 (1 page): PAGE_SIZE-aligned.
        let pa1 = page_allocator::alloc_frame().expect("order-0 alloc");
        assert_eq!(pa1.0 % PAGE_SIZE, 0, "1-page alloc must be page-aligned");
        assert!(pa1.0 >= start && pa1.0 < end);
        page_allocator::free_frame(pa1);

        // Order 1 (2 pages): 2*PAGE_SIZE-aligned.
        let pa2 = page_allocator::alloc_frames(1).expect("order-1 alloc");
        assert_eq!(
            pa2.0 % (2 * PAGE_SIZE),
            0,
            "2-page alloc must be 2*PAGE_SIZE-aligned"
        );
        page_allocator::free_frames(pa2, 1);

        // Order 2 (4 pages): 4*PAGE_SIZE-aligned.
        let pa4 = page_allocator::alloc_frames(2).expect("order-2 alloc");
        assert_eq!(
            pa4.0 % (4 * PAGE_SIZE),
            0,
            "4-page alloc must be 4*PAGE_SIZE-aligned"
        );
        page_allocator::free_frames(pa4, 2);

        assert_eq!(page_allocator::free_count(), 256, "all pages returned after alloc/free cycle");

        unsafe { free_region(ptr, layout) };
    }

    // =====================================================================
    // Section 2: Buddy coalescing — two adjacent buddies coalesce
    // =====================================================================
    {
        let (ptr, layout) = alloc_region(256);
        let start = ptr as usize;
        let end = start + 256 * PAGE_SIZE;

        page_allocator::reset();
        page_allocator::init(start, end);

        // Allocate two order-0 frames.
        let a = page_allocator::alloc_frame().unwrap();
        let b = page_allocator::alloc_frame().unwrap();

        let before_free = page_allocator::free_count();

        // Free both — they should coalesce.
        page_allocator::free_frame(a);
        page_allocator::free_frame(b);

        assert_eq!(
            page_allocator::free_count(),
            before_free + 2,
            "two freed frames restore count"
        );

        // Proof of coalescing: we can allocate an order-1 block (2 pages).
        let order1 = page_allocator::alloc_frames(1).expect("coalesced order-1");
        assert_eq!(
            order1.0 % (2 * PAGE_SIZE),
            0,
            "coalesced block must be aligned"
        );
        page_allocator::free_frames(order1, 1);

        // Full coalesce: the entire region should be available as max order.
        let full = page_allocator::alloc_frames(8).expect("full coalesce to max order");
        page_allocator::free_frames(full, 8);
        assert_eq!(page_allocator::free_count(), 256, "full coalesce restores baseline");

        unsafe { free_region(ptr, layout) };
    }

    // =====================================================================
    // Section 3: Fragmentation resistance — alloc many, free alternate, alloc large
    // =====================================================================
    {
        let (ptr, layout) = alloc_region(256);
        let start = ptr as usize;
        let end = start + 256 * PAGE_SIZE;

        page_allocator::reset();
        page_allocator::init(start, end);

        // Allocate all 256 frames individually.
        let mut frames = Vec::new();

        for _ in 0..256 {
            frames.push(page_allocator::alloc_frame().unwrap());
        }

        assert_eq!(page_allocator::free_count(), 0);

        // Free every other frame (128 frames freed, interleaved).
        for i in (0..256).step_by(2) {
            page_allocator::free_frame(frames[i]);
        }

        assert_eq!(page_allocator::free_count(), 128);

        // Free the remaining 128 frames.
        for i in (1..256).step_by(2) {
            page_allocator::free_frame(frames[i]);
        }

        assert_eq!(page_allocator::free_count(), 256);

        // After freeing all, coalescing should allow a large allocation.
        let big = page_allocator::alloc_frames(8).expect("full coalesce after fragmented free");
        assert_eq!(big.0, start, "full-region block starts at base");
        page_allocator::free_frames(big, 8);

        unsafe { free_region(ptr, layout) };
    }

    // =====================================================================
    // Section 4: Zero-fill guarantee — alloc_frame returns zeroed memory
    // =====================================================================
    {
        let (ptr, layout) = alloc_region(16);
        let start = ptr as usize;
        let end = start + 16 * PAGE_SIZE;

        page_allocator::reset();
        page_allocator::init(start, end);

        // First allocation: should be zeroed.
        let pa = page_allocator::alloc_frame().unwrap();
        let slice = unsafe { core::slice::from_raw_parts(pa.0 as *const u8, PAGE_SIZE) };

        assert!(
            slice.iter().all(|&b| b == 0),
            "first alloc must be zero-filled"
        );

        // Write non-zero data to the page.
        let write_slice = unsafe { core::slice::from_raw_parts_mut(pa.0 as *mut u8, PAGE_SIZE) };

        write_slice[0] = 0xFF;
        write_slice[PAGE_SIZE - 1] = 0xAB;

        // Free and re-allocate: the buddy allocator zeroes on alloc.
        page_allocator::free_frame(pa);
        let pa2 = page_allocator::alloc_frame().unwrap();

        // The page should be zeroed again (important for VMO demand paging).
        let slice2 = unsafe { core::slice::from_raw_parts(pa2.0 as *const u8, PAGE_SIZE) };

        assert!(
            slice2.iter().all(|&b| b == 0),
            "re-allocated frame must be zero-filled (VMO demand paging depends on this)"
        );

        page_allocator::free_frame(pa2);

        unsafe { free_region(ptr, layout) };
    }

    // =====================================================================
    // Section 5: OOM behavior — set_fail_after
    // =====================================================================
    {
        let (ptr, layout) = alloc_region(16);
        let start = ptr as usize;
        let end = start + 16 * PAGE_SIZE;

        page_allocator::reset();
        page_allocator::init(start, end);

        // set_fail_after(0): next alloc fails immediately.
        page_allocator::set_fail_after(Some(0));

        assert!(
            page_allocator::alloc_frame().is_none(),
            "fail_after(0) must reject next alloc"
        );

        // Disable fault injection: allocations resume.
        page_allocator::set_fail_after(None);

        let pa = page_allocator::alloc_frame().expect("alloc should succeed after clearing fail");
        page_allocator::free_frame(pa);

        // set_fail_after(2): exactly 2 allocations succeed, third fails.
        page_allocator::set_fail_after(Some(2));

        let pa_a = page_allocator::alloc_frame().expect("first of 2 should succeed");
        let pa_b = page_allocator::alloc_frame().expect("second of 2 should succeed");

        assert!(
            page_allocator::alloc_frame().is_none(),
            "third alloc should fail"
        );

        page_allocator::set_fail_after(None);
        page_allocator::free_frame(pa_a);
        page_allocator::free_frame(pa_b);

        unsafe { free_region(ptr, layout) };
    }

    // =====================================================================
    // Section 6: Alignment — all allocated pages are PAGE_SIZE-aligned
    // =====================================================================
    {
        let (ptr, layout) = alloc_region(64);
        let start = ptr as usize;
        let end = start + 64 * PAGE_SIZE;

        page_allocator::reset();
        page_allocator::init(start, end);

        let mut frames = Vec::new();

        // Allocate all frames and verify alignment.
        while let Some(pa) = page_allocator::alloc_frame() {
            assert_eq!(
                pa.0 % PAGE_SIZE,
                0,
                "every frame must be PAGE_SIZE-aligned: got 0x{:x}",
                pa.0
            );
            assert!(
                pa.0 >= start && pa.0 < end,
                "PA must be within managed region"
            );
            frames.push(pa);
        }

        assert_eq!(frames.len(), 64);

        for pa in frames {
            page_allocator::free_frame(pa);
        }

        // Also check multi-page allocations.
        for order in 0..=4 {
            let pa = page_allocator::alloc_frames(order).unwrap();
            let required_align = PAGE_SIZE << order;

            assert_eq!(
                pa.0 % required_align,
                0,
                "order-{order} alloc must be aligned to {}",
                required_align
            );
            page_allocator::free_frames(pa, order);
        }

        unsafe { free_region(ptr, layout) };
    }

    // =====================================================================
    // Section 7: Free list integrity after mixed alloc/free — no duplicates
    // =====================================================================
    {
        let (ptr, layout) = alloc_region(64);
        let start = ptr as usize;
        let end = start + 64 * PAGE_SIZE;

        page_allocator::reset();
        page_allocator::init(start, end);

        // Allocate 10 frames.
        let mut frames: Vec<memory::Pa> = Vec::new();

        for _ in 0..10 {
            frames.push(page_allocator::alloc_frame().unwrap());
        }

        assert_eq!(page_allocator::free_count(), 54);

        // Free 5 non-adjacent frames (indices 0, 2, 4, 6, 8).
        for &i in &[0usize, 2, 4, 6, 8] {
            page_allocator::free_frame(frames[i]);
        }

        assert_eq!(page_allocator::free_count(), 59);

        // Allocate 3 more frames.
        let mut new_frames = Vec::new();

        for _ in 0..3 {
            new_frames.push(page_allocator::alloc_frame().unwrap());
        }

        assert_eq!(page_allocator::free_count(), 56);

        // Verify no duplicates across all held frames.
        let mut held: Vec<usize> = Vec::new();

        // Still-held original frames: indices 1, 3, 5, 7, 9.
        for &i in &[1usize, 3, 5, 7, 9] {
            held.push(frames[i].0);
        }

        for pa in &new_frames {
            held.push(pa.0);
        }

        held.sort();
        let len_before = held.len();

        held.dedup();

        assert_eq!(
            held.len(),
            len_before,
            "all held frame addresses must be unique — no double allocation"
        );

        // Clean up: free everything still held.
        for &i in &[1usize, 3, 5, 7, 9] {
            page_allocator::free_frame(frames[i]);
        }

        for pa in new_frames {
            page_allocator::free_frame(pa);
        }

        // Free all remaining allocations by draining the allocator and freeing.
        // At this point all manually tracked frames are freed, but let's verify
        // the full count is restored by allocating and freeing the entire region.
        let full = page_allocator::alloc_frames(6).expect("full coalesce possible");

        page_allocator::free_frames(full, 6);

        assert_eq!(page_allocator::free_count(), 64);

        unsafe { free_region(ptr, layout) };
    }
}
