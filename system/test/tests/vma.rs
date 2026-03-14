//! Host-side tests for the kernel VMA (Virtual Memory Area) data structure.
//!
//! Tests VmaList insert, lookup (binary search), and permissions.
//! The memory_region module depends only on `alloc::vec::Vec`,
//! trivially available on the host.

extern crate alloc;

#[path = "../../kernel/memory_region.rs"]
mod memory_region;

use memory_region::*;

fn anon_vma(start: u64, end: u64) -> Vma {
    Vma {
        start,
        end,
        writable: true,
        executable: false,
        backing: Backing::Anonymous,
    }
}

fn code_vma(start: u64, end: u64) -> Vma {
    Vma {
        start,
        end,
        writable: false,
        executable: true,
        backing: Backing::Anonymous,
    }
}

// --- VmaList::new ---

#[test]
fn new_list_is_empty() {
    let list = VmaList::new();

    assert!(list.lookup(0).is_none());
    assert!(list.lookup(0x1000).is_none());
}

// --- insert ---

#[test]
fn insert_single_vma() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    assert!(list.lookup(0x1000).is_some());
}

#[test]
fn insert_maintains_sorted_order() {
    let mut list = VmaList::new();

    // Insert out of order.
    list.insert(anon_vma(0x3000, 0x4000));
    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x5000, 0x6000));

    // All should be findable (binary search depends on sorted order).
    assert!(list.lookup(0x1000).is_some());
    assert!(list.lookup(0x3000).is_some());
    assert!(list.lookup(0x5000).is_some());
}

// --- lookup ---

#[test]
fn lookup_at_start_of_vma() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    let vma = list.lookup(0x1000).unwrap();

    assert_eq!(vma.start, 0x1000);
}

#[test]
fn lookup_at_last_byte_of_vma() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    // end is exclusive, so 0x1FFF is the last valid address.
    assert!(list.lookup(0x1FFF).is_some());
}

#[test]
fn lookup_at_end_returns_none() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    // end is exclusive.
    assert!(list.lookup(0x2000).is_none());
}

#[test]
fn lookup_before_first_vma_returns_none() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    assert!(list.lookup(0x0FFF).is_none());
}

#[test]
fn lookup_in_gap_returns_none() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x3000, 0x4000));

    // The gap [0x2000, 0x3000) should return None (guard page behavior).
    assert!(list.lookup(0x2000).is_none());
    assert!(list.lookup(0x2FFF).is_none());
}

#[test]
fn lookup_after_last_vma_returns_none() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    assert!(list.lookup(0x2000).is_none());
    assert!(list.lookup(0xFFFF_FFFF).is_none());
}

#[test]
fn lookup_middle_of_vma() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x5000));

    let vma = list.lookup(0x3000).unwrap();

    assert_eq!(vma.start, 0x1000);
    assert_eq!(vma.end, 0x5000);
}

#[test]
fn lookup_correct_vma_among_many() {
    let mut list = VmaList::new();

    list.insert(code_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x4000, 0x5000));
    list.insert(anon_vma(0x8000, 0x9000));

    // Should find the second VMA.
    let vma = list.lookup(0x4500).unwrap();

    assert_eq!(vma.start, 0x4000);
    assert!(vma.writable);
    assert!(!vma.executable);
}

#[test]
fn lookup_first_vma_among_many() {
    let mut list = VmaList::new();

    list.insert(code_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x4000, 0x5000));

    let vma = list.lookup(0x1500).unwrap();

    assert_eq!(vma.start, 0x1000);
    assert!(vma.executable);
}

#[test]
fn lookup_last_vma_among_many() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x4000, 0x5000));
    list.insert(code_vma(0x8000, 0x9000));

    let vma = list.lookup(0x8800).unwrap();

    assert_eq!(vma.start, 0x8000);
    assert!(vma.executable);
}

// --- adjacent VMAs ---

#[test]
fn adjacent_vmas_no_gap() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x2000, 0x3000));

    // Both boundaries should resolve correctly.
    let v1 = list.lookup(0x1FFF).unwrap();
    assert_eq!(v1.start, 0x1000);

    let v2 = list.lookup(0x2000).unwrap();
    assert_eq!(v2.start, 0x2000);
}

// --- backing types ---

#[test]
fn anonymous_backing_preserved() {
    let mut list = VmaList::new();

    list.insert(Vma {
        start: 0x400000,
        end: 0x401000,
        writable: false,
        executable: true,
        backing: Backing::Anonymous,
    });

    let vma = list.lookup(0x400000).unwrap();

    assert!(matches!(vma.backing, Backing::Anonymous));
    assert!(vma.executable);
    assert!(!vma.writable);
}

// --- remove ---

#[test]
fn remove_existing_vma() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    let removed = list.remove(0x1000);

    assert!(removed.is_some());
    assert_eq!(removed.unwrap().start, 0x1000);
    assert!(list.lookup(0x1000).is_none());
}

#[test]
fn remove_nonexistent_returns_none() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    assert!(list.remove(0x3000).is_none());
    // Original VMA still present.
    assert!(list.lookup(0x1000).is_some());
}

#[test]
fn remove_middle_vma_preserves_others() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x3000, 0x4000));
    list.insert(anon_vma(0x5000, 0x6000));

    let removed = list.remove(0x3000).unwrap();

    assert_eq!(removed.start, 0x3000);
    // Others still present.
    assert!(list.lookup(0x1000).is_some());
    assert!(list.lookup(0x5000).is_some());
    // Removed range is now a gap.
    assert!(list.lookup(0x3000).is_none());
}

#[test]
fn remove_from_empty_list() {
    let mut list = VmaList::new();

    assert!(list.remove(0x1000).is_none());
}

#[test]
fn remove_by_start_not_middle() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x3000));

    // Can only remove by exact start address, not mid-range.
    assert!(list.remove(0x2000).is_none());
    assert!(list.lookup(0x1000).is_some());
}

// --- edge cases from audit ---

#[test]
fn zero_length_vma_is_never_matched() {
    let mut list = VmaList::new();

    // A degenerate VMA where start == end has zero size.
    list.insert(anon_vma(0x1000, 0x1000));

    // No address should match a zero-length range.
    assert!(list.lookup(0x1000).is_none());
    assert!(list.lookup(0x0FFF).is_none());
}

#[test]
fn inverted_range_is_never_matched() {
    let mut list = VmaList::new();

    // A degenerate VMA where start > end.
    list.insert(anon_vma(0x2000, 0x1000));

    // No address should match an inverted range.
    assert!(list.lookup(0x1000).is_none());
    assert!(list.lookup(0x1500).is_none());
    assert!(list.lookup(0x2000).is_none());
}

#[test]
fn duplicate_start_insert_preserves_both() {
    let mut list = VmaList::new();

    // Insert two VMAs with the same start. The struct's doc says "no overlaps
    // allowed" but the contract is caller-enforced — insert doesn't reject.
    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(code_vma(0x1000, 0x3000));

    // Lookup should find one of them (whichever binary search hits).
    let vma = list.lookup(0x1000).unwrap();
    assert_eq!(vma.start, 0x1000);
}

#[test]
fn insert_and_remove_then_reinsert() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x3000, 0x4000));

    // Remove middle, reinsert.
    list.remove(0x1000);
    assert!(list.lookup(0x1000).is_none());

    list.insert(anon_vma(0x1000, 0x2000));
    assert!(list.lookup(0x1000).is_some());
    assert!(list.lookup(0x3000).is_some());
}

#[test]
fn lookup_at_zero_address() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0, 0x1000));

    assert!(list.lookup(0).is_some());
    assert_eq!(list.lookup(0).unwrap().start, 0);
}

#[test]
fn remove_first_vma_preserves_rest() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x3000, 0x4000));
    list.insert(anon_vma(0x5000, 0x6000));

    list.remove(0x1000);

    assert!(list.lookup(0x1000).is_none());
    assert!(list.lookup(0x3000).is_some());
    assert!(list.lookup(0x5000).is_some());
}

#[test]
fn remove_last_vma_preserves_rest() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));
    list.insert(anon_vma(0x3000, 0x4000));
    list.insert(anon_vma(0x5000, 0x6000));

    list.remove(0x5000);

    assert!(list.lookup(0x1000).is_some());
    assert!(list.lookup(0x3000).is_some());
    assert!(list.lookup(0x5000).is_none());
}

#[test]
fn single_page_vma() {
    let mut list = VmaList::new();

    // Smallest valid VMA: one byte.
    list.insert(anon_vma(0x1000, 0x1001));

    assert!(list.lookup(0x1000).is_some());
    assert!(list.lookup(0x1001).is_none());
}

// --- permissions ---

#[test]
fn permissions_preserved() {
    let mut list = VmaList::new();

    list.insert(Vma {
        start: 0x1000,
        end: 0x2000,
        writable: false,
        executable: true,
        backing: Backing::Anonymous,
    });

    let vma = list.lookup(0x1500).unwrap();

    assert!(!vma.writable);
    assert!(vma.executable);
}
