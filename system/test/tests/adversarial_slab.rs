//! Adversarial stress tests for the slab allocator size_class function.
//!
//! Fuzzes all sizes from 0 to 4200+ with various alignment values.
//! Targets the allocator routing correctness from the allocator-mmio
//! audit (milestone 1).
//!
//! Run with: cargo test --test adversarial_slab -- --test-threads=1

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
    unsafe impl<T> Sync for IrqMutex<T> {}
}

#[path = "../../kernel/slab.rs"]
mod slab;

/// Fuzz size_class with all sizes from 0 to 4200+ and many alignments.
#[test]
fn fuzz_size_class_all_sizes() {
    let slab_sizes = [64usize, 128, 256, 512, 1024, 2048];

    for size in 0..=4200 {
        for align in [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048] {
            let result = slab::size_class(size, align);

            if let Some(class) = result {
                assert!(
                    class < slab_sizes.len(),
                    "class {} out of range for size={}, align={}", class, size, align
                );
                assert!(
                    slab_sizes[class] >= size,
                    "class {} ({}B) too small for size={}",
                    class, slab_sizes[class], size
                );
                assert!(
                    slab_sizes[class] >= align,
                    "class {} ({}B) alignment insufficient for align={}",
                    class, slab_sizes[class], align
                );
            }
            // None is valid for sizes > 2048 or alignment > 2048.
        }
    }
}

/// Power-of-two sizes: each should map to its exact class.
#[test]
fn power_of_two_size_boundaries() {
    let expected = [(64, 0), (128, 1), (256, 2), (512, 3), (1024, 4), (2048, 5)];

    for &(size, class) in &expected {
        assert_eq!(
            slab::size_class(size, 8), Some(class),
            "size {} with align 8 should be class {}", size, class
        );
    }
}

/// Just above power-of-two: should go to next class.
#[test]
fn just_above_power_of_two() {
    let transitions = [(65, 1), (129, 2), (257, 3), (513, 4), (1025, 5)];

    for &(size, class) in &transitions {
        assert_eq!(
            slab::size_class(size, 8), Some(class),
            "size {} should round up to class {}", size, class
        );
    }
}

/// Above 2048: should return None (goes to buddy allocator).
#[test]
fn above_max_slab_size() {
    for size in 2049..=8192 {
        assert_eq!(
            slab::size_class(size, 8), None,
            "size {} should return None (too large for slab)", size
        );
    }
}

/// Alignment-driven class selection: large alignment forces larger class.
#[test]
fn alignment_forces_larger_class() {
    // Size fits in class 0 (64B), but alignment=128 requires class 1 (128B).
    assert_eq!(slab::size_class(32, 128), Some(1));

    // Size fits in class 0, but alignment=256 requires class 2.
    assert_eq!(slab::size_class(16, 256), Some(2));
}
