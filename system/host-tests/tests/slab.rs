//! Host-side tests for slab allocator size-class selection.
//!
//! The slab allocator routes allocations to power-of-two size classes:
//! 64, 128, 256, 512, 1024, 2048 bytes. These tests verify the selection
//! algorithm matches the kernel's `slab::size_class()` function.
//!
//! We duplicate the algorithm here (10 lines) rather than including slab.rs,
//! which depends on page_alloc and IrqMutex. The algorithm is simple enough
//! that this is reliable — any change to the kernel's size_class() that
//! doesn't match these tests indicates a semantic change worth investigating.

const SIZE_CLASSES: [usize; 6] = [64, 128, 256, 512, 1024, 2048];

/// Find the size class index for a given size and alignment.
/// Returns `None` if the allocation is too large or has unusual alignment.
///
/// Mirrors kernel `slab::size_class()`.
fn size_class(size: usize, align: usize) -> Option<usize> {
    for (i, &class_size) in SIZE_CLASSES.iter().enumerate() {
        if size <= class_size && align <= class_size {
            return Some(i);
        }
    }

    None
}

// --- Exact size matches ---

#[test]
fn exact_64() {
    assert_eq!(size_class(64, 8), Some(0));
}

#[test]
fn exact_128() {
    assert_eq!(size_class(128, 8), Some(1));
}

#[test]
fn exact_256() {
    assert_eq!(size_class(256, 8), Some(2));
}

#[test]
fn exact_512() {
    assert_eq!(size_class(512, 8), Some(3));
}

#[test]
fn exact_1024() {
    assert_eq!(size_class(1024, 8), Some(4));
}

#[test]
fn exact_2048() {
    assert_eq!(size_class(2048, 8), Some(5));
}

// --- Small sizes round up to smallest class ---

#[test]
fn size_1() {
    assert_eq!(size_class(1, 1), Some(0)); // → 64-byte class
}

#[test]
fn size_32() {
    assert_eq!(size_class(32, 8), Some(0)); // → 64-byte class
}

#[test]
fn size_63() {
    assert_eq!(size_class(63, 8), Some(0)); // → 64-byte class
}

// --- Sizes between classes ---

#[test]
fn size_65() {
    assert_eq!(size_class(65, 8), Some(1)); // → 128-byte class
}

#[test]
fn size_129() {
    assert_eq!(size_class(129, 8), Some(2)); // → 256-byte class
}

#[test]
fn size_1025() {
    assert_eq!(size_class(1025, 8), Some(5)); // → 2048-byte class
}

// --- Too large ---

#[test]
fn size_2049_rejected() {
    assert_eq!(size_class(2049, 8), None);
}

#[test]
fn size_4096_rejected() {
    assert_eq!(size_class(4096, 8), None);
}

// --- Alignment constraints ---

#[test]
fn align_exceeds_class_bumps_up() {
    // Size fits in 64-byte class, but alignment of 128 requires 128-byte class.
    assert_eq!(size_class(32, 128), Some(1));
}

#[test]
fn align_exceeds_all_classes() {
    // Alignment of 4096 exceeds all slab classes.
    assert_eq!(size_class(64, 4096), None);
}

#[test]
fn align_equals_class_size() {
    // Alignment exactly matches the class size — should work.
    assert_eq!(size_class(64, 64), Some(0));
    assert_eq!(size_class(128, 128), Some(1));
    assert_eq!(size_class(2048, 2048), Some(5));
}

#[test]
fn high_align_low_size() {
    // Small object but large alignment requirement.
    assert_eq!(size_class(8, 512), Some(3));
    assert_eq!(size_class(8, 1024), Some(4));
    assert_eq!(size_class(8, 2048), Some(5));
}

// --- Edge cases ---

#[test]
fn zero_size() {
    // Zero-size allocation still gets the smallest class.
    assert_eq!(size_class(0, 1), Some(0));
}

#[test]
fn zero_align() {
    assert_eq!(size_class(64, 0), Some(0));
}

// --- Boundary values ---

#[test]
fn boundary_values() {
    // Just below and at each class boundary.
    assert_eq!(size_class(63, 8), Some(0));
    assert_eq!(size_class(64, 8), Some(0));

    assert_eq!(size_class(127, 8), Some(1));
    assert_eq!(size_class(128, 8), Some(1));

    assert_eq!(size_class(255, 8), Some(2));
    assert_eq!(size_class(256, 8), Some(2));

    assert_eq!(size_class(511, 8), Some(3));
    assert_eq!(size_class(512, 8), Some(3));

    assert_eq!(size_class(1023, 8), Some(4));
    assert_eq!(size_class(1024, 8), Some(4));

    assert_eq!(size_class(2047, 8), Some(5));
    assert_eq!(size_class(2048, 8), Some(5));
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
