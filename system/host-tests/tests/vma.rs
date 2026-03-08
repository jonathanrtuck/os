//! Host-side tests for the kernel VMA (Virtual Memory Area) data structure.
//!
//! Tests VmaList insert, lookup (binary search), and page_offset.
//! The vma module depends only on `paging::PAGE_SIZE` and `alloc::vec::Vec`,
//! both trivially available on the host.

extern crate alloc;

// Stub for vma.rs's `use super::paging::PAGE_SIZE`.
mod paging {
    pub const PAGE_SIZE: u64 = 4096;
}

#[path = "../../kernel/src/vma.rs"]
mod vma;

use vma::*;

fn anon_vma(start: u64, end: u64) -> Vma {
    Vma {
        start,
        end,
        readable: true,
        writable: true,
        executable: false,
        backing: Backing::Anonymous,
    }
}

fn code_vma(start: u64, end: u64) -> Vma {
    Vma {
        start,
        end,
        readable: true,
        writable: false,
        executable: true,
        backing: Backing::Elf {
            data: b"hello",
            data_len: 5,
        },
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
fn elf_backing_preserved() {
    let mut list = VmaList::new();
    let data: &'static [u8] = b"ELF data here";

    list.insert(Vma {
        start: 0x400000,
        end: 0x401000,
        readable: true,
        writable: false,
        executable: true,
        backing: Backing::Elf { data, data_len: 13 },
    });

    let vma = list.lookup(0x400000).unwrap();

    match &vma.backing {
        Backing::Elf { data, data_len } => {
            assert_eq!(*data_len, 13);
            assert_eq!(*data, b"ELF data here");
        }
        Backing::Anonymous => panic!("expected Elf backing"),
    }
}

// --- page_offset ---

#[test]
fn page_offset_page_aligned() {
    assert_eq!(VmaList::page_offset(0x1000), 0x1000);
    assert_eq!(VmaList::page_offset(0x2000), 0x2000);
}

#[test]
fn page_offset_rounds_down() {
    assert_eq!(VmaList::page_offset(0x1001), 0x1000);
    assert_eq!(VmaList::page_offset(0x1FFF), 0x1000);
    assert_eq!(VmaList::page_offset(0x2800), 0x2000);
}

#[test]
fn page_offset_zero() {
    assert_eq!(VmaList::page_offset(0), 0);
    assert_eq!(VmaList::page_offset(0xFFF), 0);
}

// --- permissions ---

#[test]
fn permissions_preserved() {
    let mut list = VmaList::new();

    list.insert(Vma {
        start: 0x1000,
        end: 0x2000,
        readable: true,
        writable: false,
        executable: true,
        backing: Backing::Anonymous,
    });

    let vma = list.lookup(0x1500).unwrap();

    assert!(vma.readable);
    assert!(!vma.writable);
    assert!(vma.executable);
}
