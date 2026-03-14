//! Host-side tests for process lifecycle correctness.
//!
//! Tests cover the page_count and VMA end arithmetic used in ELF loading
//! (process.rs). These functions are now exported as `checked_page_count`
//! and `checked_vma_end` in the kernel. Tests duplicate the pure arithmetic
//! to verify correctness against adversarial ELF segment values.

const PAGE_SIZE: u64 = 4096;

/// Mirrors `checked_page_count` from process.rs.
fn checked_page_count(mem_size: u64) -> Option<u64> {
    mem_size.checked_add(PAGE_SIZE - 1).map(|n| n / PAGE_SIZE)
}

/// Mirrors `checked_vma_end` from process.rs.
fn checked_vma_end(base_va: u64, page_count: u64) -> Option<u64> {
    page_count
        .checked_mul(PAGE_SIZE)
        .and_then(|size| base_va.checked_add(size))
}

// --- Overflow detection tests ---

#[test]
fn page_count_overflow_max_mem_size() {
    // An adversarial ELF segment with mem_size = u64::MAX.
    // The old unchecked calculation would wrap: (u64::MAX + 4095) → 4094,
    // then 4094 / 4096 = 0. The fixed version detects the overflow.
    assert!(
        checked_page_count(u64::MAX).is_none(),
        "must reject mem_size = u64::MAX (addition overflow)"
    );
}

#[test]
fn page_count_overflow_near_max() {
    // Smallest mem_size that causes (mem_size + PAGE_SIZE - 1) to overflow.
    let mem_size = u64::MAX - PAGE_SIZE + 2;

    assert!(
        checked_page_count(mem_size).is_none(),
        "must reject mem_size near u64::MAX"
    );
}

#[test]
fn page_count_no_overflow_max_safe() {
    // Largest mem_size that does NOT overflow: u64::MAX - PAGE_SIZE + 1.
    // (u64::MAX - 4095 + 4095) = u64::MAX, / 4096 = 4503599627370495.
    let mem_size = u64::MAX - PAGE_SIZE + 1;
    let result = checked_page_count(mem_size);

    assert!(result.is_some(), "should accept largest safe mem_size");
    assert_eq!(result.unwrap(), u64::MAX / PAGE_SIZE);
}

#[test]
fn page_count_no_overflow_normal_segment() {
    assert_eq!(checked_page_count(1024 * 1024), Some(256));
}

#[test]
fn page_count_no_overflow_exact_page() {
    assert_eq!(checked_page_count(PAGE_SIZE), Some(1));
}

#[test]
fn page_count_no_overflow_zero() {
    assert_eq!(checked_page_count(0), Some(0));
}

#[test]
fn page_count_round_up_partial_page() {
    // 1 byte past a page boundary rounds up to 2 pages.
    assert_eq!(checked_page_count(PAGE_SIZE + 1), Some(2));
}

#[test]
fn vma_end_overflow_large_page_count() {
    // page_count * PAGE_SIZE overflows u64.
    let base_va = 0x400000u64;
    let page_count = u64::MAX / PAGE_SIZE + 1;

    assert!(
        checked_vma_end(base_va, page_count).is_none(),
        "must reject page_count that overflows multiplication"
    );
}

#[test]
fn vma_end_overflow_addition() {
    // page_count * PAGE_SIZE fits, but base_va + result overflows.
    let base_va = u64::MAX - PAGE_SIZE + 1;
    let page_count = 1;

    // base_va + 4096 overflows
    assert!(
        checked_vma_end(base_va, page_count).is_none(),
        "must reject VMA end that overflows addition"
    );
}

#[test]
fn vma_end_no_overflow_normal() {
    assert_eq!(checked_vma_end(0x400000, 256), Some(0x400000 + 256 * 4096));
}

#[test]
fn vma_end_zero_pages() {
    assert_eq!(checked_vma_end(0x400000, 0), Some(0x400000));
}
