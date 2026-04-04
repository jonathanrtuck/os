//! Host-side tests for the heap VA allocator (free list with coalescing).
//!
//! Tests the HeapVaAllocator independently from AddressSpace — pure
//! computation, no page tables or arch dependencies.

#[path = "../../heap_va.rs"]
mod heap_va;

use heap_va::HeapVaAllocator;

const PAGE_SIZE: u64 = 16384;

/// Helper: create an allocator with the given base and usable size.
fn make_allocator(base: u64, usable_pages: u64) -> HeapVaAllocator {
    HeapVaAllocator::new(base, base + usable_pages * PAGE_SIZE)
}

// =========================================================================
// Basic allocation (bump)
// =========================================================================

#[test]
fn alloc_returns_base_on_first_call() {
    let mut a = make_allocator(0x100_0000, 100);
    assert_eq!(a.alloc(1), Some(0x100_0000));
}

#[test]
fn alloc_advances_bump_pointer() {
    let mut a = make_allocator(0x100_0000, 100);
    assert_eq!(a.alloc(1), Some(0x100_0000));
    assert_eq!(a.alloc(1), Some(0x100_0000 + PAGE_SIZE));
    assert_eq!(a.alloc(2), Some(0x100_0000 + 2 * PAGE_SIZE));
}

#[test]
fn alloc_fails_when_va_exhausted() {
    let mut a = make_allocator(0x100_0000, 2);
    assert_eq!(a.alloc(2), Some(0x100_0000));
    assert_eq!(a.alloc(1), None); // No more VA
}

#[test]
fn alloc_zero_pages_returns_none() {
    let mut a = make_allocator(0x100_0000, 100);
    assert_eq!(a.alloc(0), None);
}

// =========================================================================
// Free and reuse (free list)
// =========================================================================

#[test]
fn freed_va_is_reused() {
    let mut a = make_allocator(0x100_0000, 4);
    let va1 = a.alloc(1).unwrap();
    let va2 = a.alloc(1).unwrap();
    let _va3 = a.alloc(1).unwrap();
    let _va4 = a.alloc(1).unwrap();

    // VA exhausted.
    assert_eq!(a.alloc(1), None);

    // Free va2 (middle allocation).
    a.free(va2, 1);

    // Should reuse the freed VA.
    let va5 = a.alloc(1).unwrap();
    assert_eq!(va5, va2, "freed VA should be reused");

    // Free va1 (first allocation).
    a.free(va1, 1);
    let va6 = a.alloc(1).unwrap();
    assert_eq!(va6, va1);
}

#[test]
fn alloc_free_cycle_never_exhausts() {
    // The bug this fixes: without reclamation, repeated alloc/free cycles
    // eventually exhaust VA even though logical usage is constant.
    let mut a = make_allocator(0x100_0000, 10);

    for _ in 0..1000 {
        let va = a.alloc(1).expect("alloc should never fail with reclamation");
        a.free(va, 1);
    }
}

#[test]
fn large_alloc_free_cycle() {
    let mut a = make_allocator(0x100_0000, 100);

    for _ in 0..500 {
        let va = a.alloc(50).expect("alloc 50 pages");
        a.free(va, 50);
    }
}

// =========================================================================
// Coalescing
// =========================================================================

#[test]
fn adjacent_frees_coalesce_forward() {
    let mut a = make_allocator(0x100_0000, 10);
    let va1 = a.alloc(1).unwrap();
    let va2 = a.alloc(1).unwrap();
    let _va3 = a.alloc(1).unwrap(); // Keep allocated to block coalesce right

    // Free va1, then va2 (adjacent, va2 = va1 + PAGE_SIZE).
    a.free(va1, 1);
    a.free(va2, 1);

    // Should coalesce into one 2-page range. Allocating 2 pages should succeed
    // from the coalesced range.
    let va = a.alloc(2).unwrap();
    assert_eq!(va, va1, "coalesced range should start at va1");
}

#[test]
fn adjacent_frees_coalesce_backward() {
    let mut a = make_allocator(0x100_0000, 10);
    let va1 = a.alloc(1).unwrap();
    let va2 = a.alloc(1).unwrap();
    let _va3 = a.alloc(1).unwrap();

    // Free va2 first, then va1 (reverse order — va1 should coalesce backward into va2).
    a.free(va2, 1);
    a.free(va1, 1);

    let va = a.alloc(2).unwrap();
    assert_eq!(va, va1, "coalesced range should start at va1");
}

#[test]
fn three_way_coalesce() {
    let mut a = make_allocator(0x100_0000, 10);
    let va1 = a.alloc(1).unwrap();
    let va2 = a.alloc(1).unwrap();
    let va3 = a.alloc(1).unwrap();
    let _va4 = a.alloc(1).unwrap(); // Block coalesce right

    // Free va1 and va3 (gap at va2).
    a.free(va1, 1);
    a.free(va3, 1);

    // Now free va2 — should coalesce all three into a 3-page range.
    a.free(va2, 1);

    let va = a.alloc(3).unwrap();
    assert_eq!(va, va1, "three-way coalesce");
}

#[test]
fn non_adjacent_frees_stay_separate() {
    let mut a = make_allocator(0x100_0000, 10);
    let va1 = a.alloc(1).unwrap();
    let _va2 = a.alloc(1).unwrap(); // Gap (stays allocated)
    let va3 = a.alloc(1).unwrap();

    a.free(va1, 1);
    a.free(va3, 1);

    // Each freed range is 1 page. Allocating 2 pages should NOT use them.
    // It should fail or use bump (depending on bump space).
    // With 10 pages total and 3 allocated, bump has 7 more.
    let va = a.alloc(2).unwrap();
    assert!(va != va1 && va != va3, "non-adjacent ranges should not merge");
}

// =========================================================================
// Best-fit
// =========================================================================

#[test]
fn best_fit_picks_smallest_fitting_range() {
    let mut a = make_allocator(0x100_0000, 20);

    // Allocate and free ranges of different sizes to create free list entries.
    let va_large = a.alloc(4).unwrap();
    let _va_sep1 = a.alloc(1).unwrap(); // Separator
    let va_small = a.alloc(2).unwrap();
    let _va_sep2 = a.alloc(1).unwrap(); // Separator
    let va_medium = a.alloc(3).unwrap();

    a.free(va_large, 4);   // Free 4-page range
    a.free(va_small, 2);   // Free 2-page range
    a.free(va_medium, 3);  // Free 3-page range

    // Allocating 2 pages should pick the 2-page range (exact fit), not the
    // 3-page or 4-page ranges.
    let va = a.alloc(2).unwrap();
    assert_eq!(va, va_small, "best-fit should pick the 2-page range");
}

#[test]
fn best_fit_splits_remainder() {
    let mut a = make_allocator(0x100_0000, 10);
    let va = a.alloc(5).unwrap();
    let _sep = a.alloc(1).unwrap();

    a.free(va, 5); // 5-page free range

    // Allocate 2 pages from the 5-page range.
    let va2 = a.alloc(2).unwrap();
    assert_eq!(va2, va, "split should return start of range");

    // The remaining 3 pages should be available.
    let va3 = a.alloc(3).unwrap();
    assert_eq!(va3, va + 2 * PAGE_SIZE, "remainder should be available");
}

// =========================================================================
// Bump fallback
// =========================================================================

#[test]
fn bump_fallback_when_free_list_has_no_fit() {
    let mut a = make_allocator(0x100_0000, 20);

    // Allocate and free a 1-page range.
    let va1 = a.alloc(1).unwrap();
    let _va2 = a.alloc(1).unwrap();
    a.free(va1, 1);

    // Allocate 2 pages — free list only has 1-page range, so bump should be used.
    let va3 = a.alloc(2).unwrap();
    assert!(
        va3 > _va2,
        "should use bump allocator, not the too-small free range"
    );
}

// =========================================================================
// Edge cases
// =========================================================================

#[test]
fn multi_page_alloc_and_free() {
    let mut a = make_allocator(0x100_0000, 100);

    let va = a.alloc(10).unwrap();
    a.free(va, 10);

    let va2 = a.alloc(10).unwrap();
    assert_eq!(va2, va, "10-page range should be reused");
}

#[test]
fn free_list_sorted_by_va() {
    let mut a = make_allocator(0x100_0000, 10);
    let va1 = a.alloc(1).unwrap();
    let _va2 = a.alloc(1).unwrap();
    let va3 = a.alloc(1).unwrap();
    let _va4 = a.alloc(1).unwrap();
    let va5 = a.alloc(1).unwrap();

    // Free in reverse order.
    a.free(va5, 1);
    a.free(va3, 1);
    a.free(va1, 1);

    // Allocations from free list should come from the best-fit (all 1-page,
    // so first fit by VA). The free list is sorted, so va1 < va3 < va5.
    let r1 = a.alloc(1).unwrap();
    let r2 = a.alloc(1).unwrap();
    let r3 = a.alloc(1).unwrap();
    assert_eq!(r1, va1);
    assert_eq!(r2, va3);
    assert_eq!(r3, va5);
}

#[test]
fn exhaust_then_reclaim_then_reuse() {
    // Simulate the real OOM scenario: bump VA completely exhausted,
    // then free everything, then alloc again.
    let mut a = make_allocator(0x100_0000, 4);

    let v1 = a.alloc(1).unwrap();
    let v2 = a.alloc(1).unwrap();
    let v3 = a.alloc(1).unwrap();
    let v4 = a.alloc(1).unwrap();

    // Bump exhausted.
    assert_eq!(a.alloc(1), None);

    // Free all.
    a.free(v1, 1);
    a.free(v2, 1);
    a.free(v3, 1);
    a.free(v4, 1);

    // Should coalesce into one 4-page range.
    let va = a.alloc(4).unwrap();
    assert_eq!(va, v1, "coalesced range covers full region");
}

#[test]
fn interleaved_alloc_free_pattern() {
    // Simulate real workload: alternating alloc/free with varying sizes.
    let mut a = make_allocator(0x100_0000, 32);
    let mut live: Vec<(u64, u64)> = Vec::new();

    for i in 0..200u64 {
        let pages = (i % 4) + 1; // 1..4 pages
        if let Some(va) = a.alloc(pages) {
            live.push((va, pages));
        }

        // Free every other allocation.
        if i % 2 == 0 && !live.is_empty() {
            let idx = (i as usize) % live.len();
            let (va, pages) = live.swap_remove(idx);
            a.free(va, pages);
        }
    }

    // Clean up remaining.
    for (va, pages) in &live {
        a.free(*va, *pages);
    }

    // After freeing everything, the full region should be available.
    let va = a.alloc(32);
    assert!(va.is_some(), "full region should be available after freeing all");
}

#[test]
fn coalesce_multi_page_ranges() {
    let mut a = make_allocator(0x100_0000, 20);
    let va1 = a.alloc(3).unwrap();
    let va2 = a.alloc(4).unwrap();
    let _sep = a.alloc(1).unwrap();

    // Free va1 (3 pages), then va2 (4 pages) — they're adjacent.
    a.free(va1, 3);
    a.free(va2, 4);

    // Should coalesce into 7-page range.
    let va = a.alloc(7).unwrap();
    assert_eq!(va, va1, "multi-page adjacent ranges coalesce");
}
