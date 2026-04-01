//! Adversarial stress tests for the VMA (Virtual Memory Area) data structure.
//!
//! Exercises mass insertion, lookup pressure, reverse-order insertion,
//! and boundary addresses. Targets findings from the memory audit
//! (milestone 1) and memory_region.rs review.
//!
//! Run with: cargo test --test adversarial_vma -- --test-threads=1

extern crate alloc;

mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}

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
/// Lookup at exact boundaries: start, end-1, and end (which should miss).
#[test]
fn lookup_boundary_precision() {
    let mut list = VmaList::new();

    list.insert(anon_vma(0x1000, 0x2000));

    assert!(list.lookup(0x1000).is_some(), "start is inclusive");
    assert!(list.lookup(0x1FFF).is_some(), "end-1 is within range");
    assert!(list.lookup(0x2000).is_none(), "end is exclusive");
    assert!(list.lookup(0x0FFF).is_none(), "before start is outside");
}
/// Insert 10,000 non-overlapping VMAs and verify lookup correctness.
#[test]
fn mass_insert_and_lookup() {
    let mut list = VmaList::new();
    let count = 10_000u64;
    let page_size = 4096u64;

    for i in 0..count {
        let start = i * page_size * 2; // Leave gaps.
        let end = start + page_size;

        list.insert(anon_vma(start, end));
    }

    // Lookup every inserted VMA.
    for i in 0..count {
        let addr = i * page_size * 2;

        assert!(list.lookup(addr).is_some(), "VMA at {} must be found", addr);
    }

    // Lookup in gaps — should return None.
    for i in 0..count {
        let gap_addr = i * page_size * 2 + page_size;

        assert!(
            list.lookup(gap_addr).is_none(),
            "gap at {} must not match",
            gap_addr
        );
    }
}
/// Permissions: verify different permission combinations are preserved.
#[test]
fn permission_combinations() {
    let mut list = VmaList::new();
    let page_size = 4096u64;
    let combos = [
        (false, false), // no permissions (guard page)
        (true, false),  // write-only
        (false, true),  // execute-only
        (true, true),   // write-execute
    ];

    for (i, &(w, x)) in combos.iter().enumerate() {
        let start = i as u64 * page_size;
        let end = start + page_size;

        list.insert(Vma {
            start,
            end,
            writable: w,
            executable: x,
            backing: Backing::Anonymous,
        });
    }

    for (i, &(w, x)) in combos.iter().enumerate() {
        let addr = i as u64 * page_size;
        let vma = list.lookup(addr).expect("must find VMA");

        assert_eq!(vma.writable, w, "writable mismatch at {}", addr);
        assert_eq!(vma.executable, x, "executable mismatch at {}", addr);
    }
}
/// Reverse-order insertion: insert VMAs from high to low address.
/// Binary search correctness shouldn't depend on insertion order.
#[test]
fn reverse_order_insertion() {
    let mut list = VmaList::new();
    let count = 1_000u64;
    let page_size = 4096u64;

    for i in (0..count).rev() {
        let start = i * page_size;
        let end = start + page_size;

        list.insert(anon_vma(start, end));
    }

    for i in 0..count {
        let addr = i * page_size;

        assert!(
            list.lookup(addr).is_some(),
            "VMA at {} must be found after reverse insertion",
            addr
        );
    }
}
/// Random-ish insertion order: insert VMAs in a scrambled order.
#[test]
fn scrambled_insertion_order() {
    let mut list = VmaList::new();
    let count = 1_000u64;
    let page_size = 4096u64;
    // Generate indices in scrambled order using a simple permutation.
    let indices: Vec<u64> = {
        let mut v: Vec<u64> = (0..count).collect();

        for i in 0..v.len() {
            let j = (i * 7 + 13) % v.len();

            v.swap(i, j);
        }

        v
    };

    for &i in &indices {
        let start = i * page_size * 2;
        let end = start + page_size;

        list.insert(anon_vma(start, end));
    }

    // All must be findable.
    for i in 0..count {
        let addr = i * page_size * 2;

        assert!(
            list.lookup(addr).is_some(),
            "VMA at {} must be found after scrambled insertion",
            addr
        );
    }
}
/// Edge: VMA near the top of the address space.
#[test]
fn vma_near_address_space_limit() {
    let mut list = VmaList::new();
    let page_size = 4096u64;
    let start = u64::MAX - page_size;
    let end = u64::MAX;

    list.insert(anon_vma(start, end));

    assert!(list.lookup(start).is_some(), "start of high VMA must match");
    assert!(
        list.lookup(end - 1).is_some(),
        "last byte of high VMA must match"
    );
}
