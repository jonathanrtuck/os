//! OOM fault injection tests for the kernel page allocator.
//!
//! Uses `set_fail_after(n)` to make alloc_frame/alloc_frames return None
//! after N successful allocations. Verifies that every page allocated before
//! the failure is properly freed (no leaks).
//!
//! Run with: cargo test -- --test-threads=1

// --- Stubs (same as buddy.rs) ---

mod paging {
    pub const PAGE_SIZE: u64 = 16384;
    pub const RAM_SIZE_MAX: u64 = 256 * 1024 * 1024;

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

use memory::Pa;

const PAGE_SIZE: usize = 16384;

fn alloc_region(pages: usize) -> (*mut u8, std::alloc::Layout) {
    let size = pages * PAGE_SIZE;
    let layout = std::alloc::Layout::from_size_align(size, PAGE_SIZE).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };

    assert!(!ptr.is_null());

    (ptr, layout)
}

unsafe fn free_region(ptr: *mut u8, layout: std::alloc::Layout) {
    std::alloc::dealloc(ptr, layout);
}

#[test]
#[cfg_attr(miri, ignore)]
fn oom_fault_injection() {
    let (ptr, layout) = alloc_region(256);
    let start = ptr as usize;
    let end = start + 256 * PAGE_SIZE;

    page_allocator::init(start, end);

    let initial_free = page_allocator::free_count();

    assert_eq!(initial_free, 256);

    // --- Test 1: fail_after(0) — very first alloc fails ---
    page_allocator::set_fail_after(Some(0));

    assert!(
        page_allocator::alloc_frame().is_none(),
        "alloc should fail immediately with fail_after(0)"
    );
    assert_eq!(
        page_allocator::free_count(),
        initial_free,
        "no pages consumed on immediate failure"
    );

    // --- Test 2: fail_after(1) — second alloc fails ---
    page_allocator::set_fail_after(Some(1));

    let pa = page_allocator::alloc_frame().expect("first alloc should succeed");

    assert!(
        page_allocator::alloc_frame().is_none(),
        "second alloc should fail"
    );

    // Free the one we got.
    page_allocator::free_frame(pa);

    assert_eq!(
        page_allocator::free_count(),
        initial_free,
        "free count restored after freeing the one successful alloc"
    );

    // --- Test 3: fail_after disables after trigger ---
    // After hitting the limit, subsequent allocs should also fail.
    page_allocator::set_fail_after(Some(0));

    assert!(page_allocator::alloc_frame().is_none());
    assert!(page_allocator::alloc_frame().is_none());
    assert!(page_allocator::alloc_frame().is_none());
    assert_eq!(page_allocator::free_count(), initial_free);

    // --- Test 4: clear fault injection ---
    page_allocator::set_fail_after(None);

    let pa = page_allocator::alloc_frame().expect("alloc should succeed after clearing");

    page_allocator::free_frame(pa);
    // --- Test 5: multi-page alloc under OOM ---
    page_allocator::set_fail_after(Some(0));

    assert!(
        page_allocator::alloc_frames(2).is_none(),
        "multi-page alloc should fail"
    );
    assert_eq!(page_allocator::free_count(), initial_free);

    // --- Test 6: Simulate channel::create pattern ---
    // Two sequential allocs; if second fails, first must be freed.
    page_allocator::set_fail_after(Some(1)); // first succeeds, second fails

    let page0 = page_allocator::alloc_frame().expect("page0 should succeed");
    let page1 = page_allocator::alloc_frame(); // should fail

    assert!(page1.is_none());

    // Mimic channel::create error path: free page0.
    page_allocator::free_frame(page0);

    assert_eq!(
        page_allocator::free_count(),
        initial_free,
        "channel::create pattern: no leak after second alloc fails"
    );

    // --- Test 7: N allocs then fail — verify all freed ---
    for n in 0..8 {
        page_allocator::set_fail_after(Some(n));

        let mut allocated = Vec::new();

        for _ in 0..n + 1 {
            match page_allocator::alloc_frame() {
                Some(pa) => allocated.push(pa),
                None => break,
            }
        }

        assert_eq!(
            allocated.len(),
            n,
            "should get exactly {n} frames before failure"
        );

        // Free all acquired frames (mimics proper error-path cleanup).
        for pa in allocated {
            page_allocator::free_frame(pa);
        }

        assert_eq!(
            page_allocator::free_count(),
            initial_free,
            "no leak after {n}-then-fail pattern"
        );
    }

    // --- Test 8: fail_after doesn't affect free ---
    page_allocator::set_fail_after(None);

    let pa = page_allocator::alloc_frame().unwrap();

    page_allocator::set_fail_after(Some(0)); // alloc blocked, but free should work
    page_allocator::free_frame(pa); // must not panic or lose the frame

    assert_eq!(page_allocator::free_count(), initial_free);

    // --- Cleanup ---
    page_allocator::set_fail_after(None);

    unsafe { free_region(ptr, layout) };
}
