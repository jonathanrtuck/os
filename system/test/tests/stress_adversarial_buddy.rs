//! Adversarial stress tests for the buddy allocator (page_allocator.rs).
//!
//! Exercises rapid alloc/free cycles, interleaved patterns, multi-order
//! scrambled frees, and exhaustion recovery. Targets findings from the
//! memory-paging-pagealloc audit (milestone 1).
//!
//! Run with: cargo test --test adversarial_buddy -- --test-threads=1
//!
//! NOTE: page_allocator uses a global static STATE, so all test sections
//! run as one sequential test (same pattern as buddy.rs).

// --- Stubs (same as buddy.rs) ---

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
    pub fn phys_to_virt(pa: Pa) -> usize {
        pa.0
    }
    pub fn virt_to_phys(va: usize) -> Pa {
        Pa(va)
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

#[path = "../../kernel/page_allocator.rs"]
mod page_allocator;

use std::alloc::{alloc, dealloc, Layout};

const PAGE_SIZE: usize = 16384;
const REGION_PAGES: usize = 256;
const REGION_SIZE: usize = REGION_PAGES * PAGE_SIZE;

unsafe fn alloc_region() -> (*mut u8, Layout) {
    let layout = Layout::from_size_align(REGION_SIZE, REGION_SIZE).unwrap();
    let ptr = alloc(layout);
    assert!(!ptr.is_null(), "failed to allocate test region");
    (ptr, layout)
}

unsafe fn free_region(ptr: *mut u8, layout: Layout) {
    dealloc(ptr, layout);
}

/// Comprehensive adversarial buddy allocator stress test.
/// Runs as a single test because page_allocator uses global state.
#[test]
#[cfg_attr(miri, ignore)]
fn adversarial_buddy_stress() {
    unsafe {
        let (ptr, layout) = alloc_region();
        let start = ptr as usize;
        let end = start + REGION_SIZE;
        page_allocator::init(start, end);

        // --- Section 1: Rapid alloc-free cycles (10 rounds) ---
        for cycle in 0..10 {
            let mut frames = Vec::new();
            while let Some(pa) = page_allocator::alloc_frame() {
                frames.push(pa);
            }
            assert_eq!(
                frames.len(),
                REGION_PAGES,
                "cycle {}: should exhaust all {} frames",
                cycle,
                REGION_PAGES
            );
            assert!(page_allocator::alloc_frame().is_none());

            // Free in reverse order.
            for pa in frames.into_iter().rev() {
                page_allocator::free_frame(pa);
            }
            assert_eq!(
                page_allocator::free_count(),
                REGION_PAGES,
                "cycle {}: free count must restore",
                cycle
            );
        }

        // --- Section 2: Interleaved alloc/free (50 rounds) ---
        {
            let mut held = Vec::new();
            for round in 0..50 {
                let batch_size = (round % 7) + 1;
                for _ in 0..batch_size {
                    if let Some(pa) = page_allocator::alloc_frame() {
                        held.push(pa);
                    }
                }
                let free_count = held.len() / 2;
                for _ in 0..free_count {
                    if !held.is_empty() {
                        let idx = round % held.len();
                        let pa = held.swap_remove(idx);
                        page_allocator::free_frame(pa);
                    }
                }
            }
            for pa in held {
                page_allocator::free_frame(pa);
            }
            assert_eq!(
                page_allocator::free_count(),
                REGION_PAGES,
                "interleaved: all frames must be returned"
            );
        }

        // --- Section 3: Multi-order scrambled free (5 cycles) ---
        for _cycle in 0..5 {
            let mut allocs: Vec<(memory::Pa, usize)> = Vec::new();
            let orders: [usize; 8] = [0, 1, 2, 0, 0, 1, 3, 0];
            let mut order_idx = 0;
            loop {
                let order = orders[order_idx % orders.len()];
                if let Some(pa) = page_allocator::alloc_frames(order) {
                    allocs.push((pa, order));
                } else if order > 0 {
                    if let Some(pa) = page_allocator::alloc_frame() {
                        allocs.push((pa, 0));
                    } else {
                        break;
                    }
                } else {
                    break;
                }
                order_idx += 1;
            }

            // Deterministic scramble.
            let len = allocs.len();
            for i in 0..len {
                let j = (i * 7 + 3) % len;
                allocs.swap(i, j);
            }

            for &(pa, order) in &allocs {
                page_allocator::free_frames(pa, order);
            }
            assert_eq!(
                page_allocator::free_count(),
                REGION_PAGES,
                "scrambled free: full coalescing must restore all pages"
            );
        }

        // --- Section 4: Alternating free pattern ---
        {
            let mut frames = Vec::new();
            while let Some(pa) = page_allocator::alloc_frame() {
                frames.push(pa);
            }
            assert_eq!(frames.len(), REGION_PAGES);

            // Free even-indexed frames.
            let mut kept = Vec::new();
            for (i, pa) in frames.into_iter().enumerate() {
                if i % 2 == 0 {
                    page_allocator::free_frame(pa);
                } else {
                    kept.push(pa);
                }
            }
            assert_eq!(page_allocator::free_count(), REGION_PAGES / 2);

            // Free odd-indexed frames.
            for pa in kept {
                page_allocator::free_frame(pa);
            }
            assert_eq!(
                page_allocator::free_count(),
                REGION_PAGES,
                "alternating: all frames restored"
            );
        }

        // --- Section 5: Address uniqueness under pressure ---
        {
            let mut all_addrs = std::collections::HashSet::new();
            let mut frames = Vec::new();
            while let Some(pa) = page_allocator::alloc_frame() {
                assert!(
                    all_addrs.insert(pa.0),
                    "duplicate address returned: 0x{:x}",
                    pa.0
                );
                frames.push(pa);
            }
            assert_eq!(frames.len(), REGION_PAGES);

            for pa in frames {
                page_allocator::free_frame(pa);
            }
            assert_eq!(page_allocator::free_count(), REGION_PAGES);
        }

        free_region(ptr, layout);
    }
}
