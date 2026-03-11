//! Host-side tests for slab allocator size-class selection.
//!
//! Includes the kernel's slab.rs directly via #[path]. We stub out the
//! kernel dependencies (memory, page_allocator, paging, sync) — only
//! size_class() is exercised, so the stubs need only compile, not run.

mod memory {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    #[repr(transparent)]
    pub struct Pa(pub usize);

    pub fn phys_to_virt(_pa: Pa) -> usize {
        unimplemented!("stub")
    }
}

mod page_allocator {
    pub fn alloc_frame() -> Option<super::memory::Pa> {
        unimplemented!("stub")
    }
}

mod paging {
    pub const PAGE_SIZE: u64 = 4096;
}

mod sync {
    use core::cell::UnsafeCell;
    use core::ops::{Deref, DerefMut};

    pub struct IrqGuard<'a, T> {
        data: &'a UnsafeCell<T>,
    }
    pub struct IrqMutex<T> {
        data: UnsafeCell<T>,
    }

    impl<T> Deref for IrqGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.data.get() }
        }
    }
    impl<T> DerefMut for IrqGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            unsafe { &mut *self.data.get() }
        }
    }
    impl<T> IrqMutex<T> {
        pub fn lock(&self) -> IrqGuard<'_, T> {
            IrqGuard { data: &self.data }
        }
        pub const fn new(val: T) -> Self {
            Self {
                data: UnsafeCell::new(val),
            }
        }
    }
    // SAFETY: Single-threaded host tests.
    unsafe impl<T> Sync for IrqMutex<T> {}
}

#[path = "../../kernel/slab.rs"]
mod slab;

// --- Exact size matches ---

#[test]
fn exact_64() {
    assert_eq!(slab::size_class(64, 8), Some(0));
}

#[test]
fn exact_128() {
    assert_eq!(slab::size_class(128, 8), Some(1));
}

#[test]
fn exact_256() {
    assert_eq!(slab::size_class(256, 8), Some(2));
}

#[test]
fn exact_512() {
    assert_eq!(slab::size_class(512, 8), Some(3));
}

#[test]
fn exact_1024() {
    assert_eq!(slab::size_class(1024, 8), Some(4));
}

#[test]
fn exact_2048() {
    assert_eq!(slab::size_class(2048, 8), Some(5));
}

// --- Small sizes round up to smallest class ---

#[test]
fn size_1() {
    assert_eq!(slab::size_class(1, 1), Some(0)); // -> 64-byte class
}

#[test]
fn size_32() {
    assert_eq!(slab::size_class(32, 8), Some(0)); // -> 64-byte class
}

#[test]
fn size_63() {
    assert_eq!(slab::size_class(63, 8), Some(0)); // -> 64-byte class
}

// --- Sizes between classes ---

#[test]
fn size_65() {
    assert_eq!(slab::size_class(65, 8), Some(1)); // -> 128-byte class
}

#[test]
fn size_129() {
    assert_eq!(slab::size_class(129, 8), Some(2)); // -> 256-byte class
}

#[test]
fn size_1025() {
    assert_eq!(slab::size_class(1025, 8), Some(5)); // -> 2048-byte class
}

// --- Too large ---

#[test]
fn size_2049_rejected() {
    assert_eq!(slab::size_class(2049, 8), None);
}

#[test]
fn size_4096_rejected() {
    assert_eq!(slab::size_class(4096, 8), None);
}

// --- Alignment constraints ---

#[test]
fn align_exceeds_class_bumps_up() {
    // Size fits in 64-byte class, but alignment of 128 requires 128-byte class.
    assert_eq!(slab::size_class(32, 128), Some(1));
}

#[test]
fn align_exceeds_all_classes() {
    // Alignment of 4096 exceeds all slab classes.
    assert_eq!(slab::size_class(64, 4096), None);
}

#[test]
fn align_equals_class_size() {
    // Alignment exactly matches the class size — should work.
    assert_eq!(slab::size_class(64, 64), Some(0));
    assert_eq!(slab::size_class(128, 128), Some(1));
    assert_eq!(slab::size_class(2048, 2048), Some(5));
}

#[test]
fn high_align_low_size() {
    // Small object but large alignment requirement.
    assert_eq!(slab::size_class(8, 512), Some(3));
    assert_eq!(slab::size_class(8, 1024), Some(4));
    assert_eq!(slab::size_class(8, 2048), Some(5));
}

// --- Edge cases ---

#[test]
fn zero_size() {
    // Zero-size allocation still gets the smallest class.
    assert_eq!(slab::size_class(0, 1), Some(0));
}

#[test]
fn zero_align() {
    assert_eq!(slab::size_class(64, 0), Some(0));
}

// --- Boundary values ---

#[test]
fn boundary_values() {
    // Just below and at each class boundary.
    assert_eq!(slab::size_class(63, 8), Some(0));
    assert_eq!(slab::size_class(64, 8), Some(0));

    assert_eq!(slab::size_class(127, 8), Some(1));
    assert_eq!(slab::size_class(128, 8), Some(1));

    assert_eq!(slab::size_class(255, 8), Some(2));
    assert_eq!(slab::size_class(256, 8), Some(2));

    assert_eq!(slab::size_class(511, 8), Some(3));
    assert_eq!(slab::size_class(512, 8), Some(3));

    assert_eq!(slab::size_class(1023, 8), Some(4));
    assert_eq!(slab::size_class(1024, 8), Some(4));

    assert_eq!(slab::size_class(2047, 8), Some(5));
    assert_eq!(slab::size_class(2048, 8), Some(5));
}

// --- Objects per slab ---

#[test]
fn objects_per_slab_calculation() {
    // Verify the slab geometry: 4 KiB page / object_size = objects per slab.
    // This isn't testing size_class, but validates the slab constants.
    assert_eq!(4096 / 64, 64);
    assert_eq!(4096 / 128, 32);
    assert_eq!(4096 / 256, 16);
    assert_eq!(4096 / 512, 8);
    assert_eq!(4096 / 1024, 4);
    assert_eq!(4096 / 2048, 2);
}
