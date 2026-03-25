//! Host-side tests for paging constants and utility functions.
//!
//! Tests align_up/align_up_u64 edge cases and verifies paging constant
//! relationships. These functions are used pervasively in kernel memory
//! management; overflow on extreme inputs must be understood.

#[path = "../../kernel/paging.rs"]
mod paging;

use paging::*;

// --- align_up ---

#[test]
fn align_up_already_aligned() {
    assert_eq!(align_up(0x1000, 4096), 0x1000);
    assert_eq!(align_up(0, 4096), 0);
}

#[test]
fn align_up_not_aligned() {
    assert_eq!(align_up(0x1001, 4096), 0x2000);
    assert_eq!(align_up(1, 4096), 4096);
    assert_eq!(align_up(4095, 4096), 4096);
}

#[test]
fn align_up_one_byte_alignment() {
    // align=1 means no alignment needed.
    assert_eq!(align_up(42, 1), 42);
    assert_eq!(align_up(0, 1), 0);
}

#[test]
fn align_up_u64_already_aligned() {
    assert_eq!(align_up_u64(0x1000, 4096), 0x1000);
    assert_eq!(align_up_u64(0, 4096), 0);
}

#[test]
fn align_up_u64_not_aligned() {
    assert_eq!(align_up_u64(0x1001, 4096), 0x2000);
    assert_eq!(align_up_u64(1, 4096), 4096);
}

/// align_up wraps instead of panicking on extreme inputs.
/// The wrapping arithmetic produces a value less than the input,
/// which is incorrect — but callers must ensure inputs are within
/// the valid address space. This test documents the wrapping behavior.
#[test]
fn align_up_overflow_wraps_without_panic() {
    // usize::MAX (0xFFFF_FFFF_FFFF_FFFF) + 4095 wraps to 0x0FFE,
    // then & !(4095) = 0. The result is wildly wrong but no panic.
    let result = align_up(usize::MAX, 4096);
    assert_eq!(result, 0, "wrapping produces 0 for usize::MAX");
    // Key insight: result < input — callers must avoid this range.
    assert!(result < usize::MAX);
}

/// align_up_u64 same wrapping behavior documentation.
#[test]
fn align_up_u64_overflow_wraps_without_panic() {
    let result = align_up_u64(u64::MAX, 4096);
    assert!(
        result < u64::MAX,
        "wrapping produces a value less than input"
    );
}

// --- Constant relationship checks ---

#[test]
fn ram_end_max_equals_start_plus_size_max() {
    assert_eq!(RAM_END_MAX, RAM_START + RAM_SIZE_MAX);
}

#[test]
fn ram_max_region_is_256_mib() {
    assert_eq!(RAM_SIZE_MAX, 256 * 1024 * 1024);
}

#[test]
fn ram_end_defaults_to_max() {
    assert_eq!(ram_end(), RAM_END_MAX);
}

#[test]
fn set_ram_end_updates_runtime_value() {
    let original = ram_end();
    let test_end = RAM_START + 128 * 1024 * 1024;

    set_ram_end(test_end);
    assert_eq!(ram_end(), test_end);

    // Restore original to avoid affecting other tests.
    set_ram_end(original);
}

#[test]
fn page_size_is_16k() {
    assert_eq!(PAGE_SIZE, 16384);
}

#[test]
fn desc_page_is_valid_and_table() {
    // L3 page descriptor has both bit 0 (valid) and bit 1 (table) set.
    assert_eq!(DESC_PAGE, DESC_VALID | DESC_TABLE);
    assert_eq!(DESC_PAGE, 0b11);
}

#[test]
fn pa_mask_preserves_16k_aligned_bits() {
    // PA_MASK should zero out the lower 14 bits and upper control bits.
    assert_eq!(PA_MASK & 0x3FFF, 0, "lower 14 bits must be zero");
    // Bit 47 should be the highest PA bit in the mask.
    assert_eq!(PA_MASK, 0x0000_FFFF_FFFF_C000);
}

#[test]
fn user_va_regions_do_not_overlap() {
    // Verify that the user VA regions are non-overlapping and properly ordered.
    assert!(USER_CODE_BASE < HEAP_BASE, "code before heap");
    assert!(HEAP_END <= DMA_BUFFER_BASE, "heap before DMA");
    assert!(DMA_BUFFER_END <= DEVICE_MMIO_BASE, "DMA before MMIO");
    assert!(
        DEVICE_MMIO_END <= CHANNEL_SHM_BASE,
        "MMIO before channel SHM"
    );
    assert!(CHANNEL_SHM_END <= USER_STACK_VA, "channel SHM before stack");
    assert!(USER_STACK_VA < USER_STACK_TOP, "stack VA before stack top");
    assert!(
        USER_STACK_TOP <= SHARED_MEMORY_BASE,
        "stack before shared memory"
    );
    assert!(
        SHARED_MEMORY_END <= USER_VA_END,
        "shared memory within VA range"
    );
}

#[test]
fn user_stack_size_is_64k() {
    assert_eq!(USER_STACK_PAGES, 4);
    assert_eq!(USER_STACK_VA, USER_STACK_TOP - 4 * PAGE_SIZE);
}

#[test]
fn channel_shm_region_consistent() {
    assert!(CHANNEL_SHM_BASE < CHANNEL_SHM_END);
}
