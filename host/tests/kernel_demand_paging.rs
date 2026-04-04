#![feature(allocator_api)]
//! Host-side tests for the demand-paging fix (Bug 3).
//!
//! Bug 3: VMO-mapped pages that were committed via `vmo::write()` were
//! rejected by syscalls because `is_user_page_readable`/`is_user_page_writable`
//! used a hardware AT check that fails when the page hasn't been faulted into
//! the page table yet. The fix services demand-page faults inline when the
//! AT check fails but a VMA exists.
//!
//! These tests verify the data-structure interactions underlying the fix:
//! 1. commit_page + lookup_page finds committed pages (fault handler path)
//! 2. Uncommitted pages get zero-filled by the fault handler
//! 3. VMO pages written via vmo::write (simulated by commit_page) are
//!    discoverable by the fault handler's lookup_page (the exact Bug 3 scenario)
//! 4. VMA lookup correctly identifies VMO-backed regions for fault dispatch

extern crate alloc;

mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}

#[path = "../../kernel/handle.rs"]
mod handle;
#[path = "../../kernel/paging.rs"]
mod paging;
mod interrupt {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod process {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessId(pub u32);
}
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}
mod memory {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    #[repr(transparent)]
    pub struct Pa(pub usize);
}
mod sync {
    // Stub — IrqMutex not needed for tests (behind #[cfg(not(test))]).
}

#[path = "../../kernel/memory_region.rs"]
mod memory_region;
#[path = "../../kernel/vmo.rs"]
mod vmo;

use memory::Pa;
use memory_region::{Backing, Vma, VmaList};
use vmo::*;

// =========================================================================
// 1. commit_page + lookup_page: fault handler finds committed pages
// =========================================================================

#[test]
fn committed_page_found_by_lookup() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let pa = Pa(0x8000_0000);

    // Simulate what vmo::write() does internally: commit a page.
    table.get_mut(id).unwrap().commit_page(0, pa);

    // Simulate what handle_fault_vmo does: look up the page by offset.
    let (found_pa, refcount) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(found_pa, pa);
    assert_eq!(refcount, 1);
}

#[test]
fn committed_page_at_nonzero_offset() {
    let mut table = VmoTable::new();
    let id = table.create(8, VmoFlags::empty(), 0).unwrap();

    // Write to page 5 (not page 0) — offset matters for the mapping calculation.
    let pa = Pa(0x9000_0000);
    table.get_mut(id).unwrap().commit_page(5, pa);

    // Pages 0-4 and 6-7 should be uncommitted.
    assert!(table.get(id).unwrap().lookup_page(0).is_none());
    assert!(table.get(id).unwrap().lookup_page(4).is_none());
    assert!(table.get(id).unwrap().lookup_page(6).is_none());

    // Page 5 should be committed.
    let (found_pa, _) = table.get(id).unwrap().lookup_page(5).unwrap();
    assert_eq!(found_pa, pa);
}

// =========================================================================
// 2. Uncommitted pages: fault handler zero-fills
// =========================================================================

#[test]
fn uncommitted_page_returns_none() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // No pages committed — lookup returns None for every valid offset.
    // In the real kernel, the fault handler allocates a zeroed frame here.
    for offset in 0..4 {
        assert!(
            table.get(id).unwrap().lookup_page(offset).is_none(),
            "page {} should be uncommitted",
            offset
        );
    }
}

#[test]
fn uncommitted_page_gets_committed_by_fault_simulation() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // Before fault: page 2 is uncommitted.
    assert!(table.get(id).unwrap().lookup_page(2).is_none());

    // Simulate fault handler: allocate a frame and commit it.
    let zero_fill_pa = Pa(0xA000_0000); // Would be from alloc_frame() in real kernel
    table.get_mut(id).unwrap().commit_page(2, zero_fill_pa);

    // After fault: page 2 is committed with the zero-filled frame.
    let (pa, rc) = table.get(id).unwrap().lookup_page(2).unwrap();
    assert_eq!(pa, zero_fill_pa);
    assert_eq!(rc, 1);
}

// =========================================================================
// 3. Bug 3 exact scenario: vmo::write commits page, fault handler finds it
// =========================================================================

#[test]
fn vmo_write_then_fault_finds_committed_page() {
    // Bug 3 scenario:
    // 1. Userspace calls sys_vmo_write → kernel's vmo::write() commits pages
    // 2. Userspace maps the VMO into its address space (VMA created)
    // 3. Userspace accesses the mapped address → page fault
    // 4. Fault handler calls handle_fault_vmo → looks up page in VMO
    //
    // Before the fix: is_user_page_readable did AT check, failed (not in
    // page table), and rejected the syscall. After the fix: it checks the
    // VMA, finds the VMO backing, and services the fault inline.
    //
    // This test verifies step 4: the committed page is found by lookup_page.

    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // Step 1: Simulate vmo::write() committing pages.
    // vmo::write() internally calls commit_page for each page it touches.
    let data_pa_0 = Pa(0xC000_0000);
    let data_pa_1 = Pa(0xC000_4000);
    table.get_mut(id).unwrap().commit_page(0, data_pa_0);
    table.get_mut(id).unwrap().commit_page(1, data_pa_1);

    // Step 4: Simulate handle_fault_vmo looking up the page.
    // The fault handler computes: page_offset = (page_va - vma.start) / PAGE_SIZE + mapping_offset
    // Then calls vmo.lookup_page(page_offset).
    let (pa, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, data_pa_0);
    assert_eq!(rc, 1);

    let (pa, rc) = table.get(id).unwrap().lookup_page(1).unwrap();
    assert_eq!(pa, data_pa_1);
    assert_eq!(rc, 1);

    // Pages 2-3 were not written — fault handler would zero-fill these.
    assert!(table.get(id).unwrap().lookup_page(2).is_none());
    assert!(table.get(id).unwrap().lookup_page(3).is_none());
}

#[test]
fn vmo_write_multiple_pages_all_found_by_fault_handler() {
    // Larger write spanning multiple pages — all should be discoverable.
    let mut table = VmoTable::new();
    let id = table.create(16, VmoFlags::empty(), 0).unwrap();

    // Simulate vmo::write() committing pages 3..8 (a 5-page write starting
    // at an offset).
    let base_addr = 0xD000_0000usize;
    for page in 3..8 {
        let pa = Pa(base_addr + (page as usize) * 0x4000);
        table.get_mut(id).unwrap().commit_page(page, pa);
    }

    // All 5 pages should be discoverable by the fault handler.
    for page in 3..8 {
        let expected_pa = Pa(base_addr + (page as usize) * 0x4000);
        let (pa, rc) = table
            .get(id)
            .unwrap()
            .lookup_page(page)
            .unwrap_or_else(|| panic!("page {} should be committed", page));
        assert_eq!(pa, expected_pa, "page {} has wrong PA", page);
        assert_eq!(rc, 1, "page {} should have refcount 1", page);
    }

    // Pages outside the written range should be uncommitted.
    assert!(table.get(id).unwrap().lookup_page(0).is_none());
    assert!(table.get(id).unwrap().lookup_page(2).is_none());
    assert!(table.get(id).unwrap().lookup_page(8).is_none());
    assert!(table.get(id).unwrap().lookup_page(15).is_none());
}

// =========================================================================
// 4. VMA lookup identifies VMO-backed regions for fault dispatch
// =========================================================================

#[test]
fn vma_lookup_finds_vmo_backed_region() {
    let mut vmas = VmaList::new();
    let vmo_id = VmoId(7);

    // Register a VMO-backed VMA (simulates what sys_vmo_map creates).
    vmas.insert(Vma {
        start: 0x4000_0000,
        end: 0x4001_0000, // 4 pages at 16 KiB
        writable: true,
        executable: false,
        backing: Backing::Vmo {
            vmo_id,
            offset: 0,
            writable: true,
        },
    });

    // Look up a faulting address within the VMA.
    let vma = vmas.lookup(0x4000_0000).expect("VMA should be found");
    assert_eq!(vma.start, 0x4000_0000);
    assert!(matches!(
        vma.backing,
        Backing::Vmo {
            vmo_id: VmoId(7),
            ..
        }
    ));
}

#[test]
fn vma_lookup_returns_none_for_unmapped_address() {
    let mut vmas = VmaList::new();

    vmas.insert(Vma {
        start: 0x4000_0000,
        end: 0x4001_0000,
        writable: true,
        executable: false,
        backing: Backing::Vmo {
            vmo_id: VmoId(0),
            offset: 0,
            writable: true,
        },
    });

    // Address before the VMA.
    assert!(vmas.lookup(0x3FFF_FFFF).is_none());
    // Address after the VMA (end is exclusive).
    assert!(vmas.lookup(0x4001_0000).is_none());
}

#[test]
fn vma_lookup_distinguishes_vmo_from_anonymous() {
    let mut vmas = VmaList::new();

    // Anonymous VMA (stack).
    vmas.insert(Vma {
        start: 0x1000_0000,
        end: 0x1001_0000,
        writable: true,
        executable: false,
        backing: Backing::Anonymous,
    });

    // VMO-backed VMA (mapped buffer).
    vmas.insert(Vma {
        start: 0x4000_0000,
        end: 0x4001_0000,
        writable: true,
        executable: false,
        backing: Backing::Vmo {
            vmo_id: VmoId(3),
            offset: 0,
            writable: true,
        },
    });

    // The demand-paging fix only applies to VMO-backed VMAs.
    let anon_vma = vmas.lookup(0x1000_0000).unwrap();
    assert!(matches!(anon_vma.backing, Backing::Anonymous));

    let vmo_vma = vmas.lookup(0x4000_0000).unwrap();
    assert!(matches!(vmo_vma.backing, Backing::Vmo { .. }));
}

// =========================================================================
// 5. Full demand-paging flow: VMA + VMO interaction
// =========================================================================

#[test]
fn full_demand_page_flow_committed_page() {
    // End-to-end simulation of the demand-paging path for a committed page.
    //
    // Real kernel flow:
    // 1. is_user_page_readable() → AT check fails → look up VMA
    // 2. VMA found with Backing::Vmo { vmo_id, offset, .. }
    // 3. Compute page_offset = (fault_va - vma.start) / PAGE_SIZE + mapping_offset
    // 4. vmo.lookup_page(page_offset) → Some((pa, rc))
    // 5. Map pa into page table, return success

    let mut table = VmoTable::new();
    let vmo_id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // Simulate vmo::write() committing page 0.
    let committed_pa = Pa(0xE000_0000);
    table.get_mut(vmo_id).unwrap().commit_page(0, committed_pa);

    // Simulate sys_vmo_map creating a VMA.
    let mut vmas = VmaList::new();
    let mapping_offset = 0u64;
    let vma_start = 0x4000_0000u64;
    let page_size = paging::PAGE_SIZE;
    vmas.insert(Vma {
        start: vma_start,
        end: vma_start + 4 * page_size,
        writable: true,
        executable: false,
        backing: Backing::Vmo {
            vmo_id,
            offset: mapping_offset,
            writable: true,
        },
    });

    // Simulate a page fault at the start of the mapping.
    let fault_va = vma_start;
    let vma = vmas.lookup(fault_va).expect("VMA must exist for fault_va");

    // Extract VMO info from the VMA backing.
    let (backing_vmo_id, backing_offset) = match &vma.backing {
        Backing::Vmo { vmo_id, offset, .. } => (*vmo_id, *offset),
        _ => panic!("expected VMO-backed VMA"),
    };

    // Compute page_offset the same way handle_fault_vmo does.
    let page_offset = (fault_va - vma.start) / page_size + backing_offset;
    assert_eq!(page_offset, 0);

    // Look up the page in the VMO — this is the critical check.
    let vmo = table.get(backing_vmo_id).unwrap();
    let (pa, rc) = vmo
        .lookup_page(page_offset)
        .expect("committed page must be found by fault handler");
    assert_eq!(pa, committed_pa);
    assert_eq!(rc, 1);
}

#[test]
fn full_demand_page_flow_uncommitted_page() {
    // Same flow but for an uncommitted page — fault handler must zero-fill.

    let mut table = VmoTable::new();
    let vmo_id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // No pages committed — this VMO was just created.

    let mut vmas = VmaList::new();
    let vma_start = 0x4000_0000u64;
    let page_size = paging::PAGE_SIZE;
    vmas.insert(Vma {
        start: vma_start,
        end: vma_start + 4 * page_size,
        writable: true,
        executable: false,
        backing: Backing::Vmo {
            vmo_id,
            offset: 0,
            writable: true,
        },
    });

    // Fault at page 2 within the mapping.
    let fault_va = vma_start + 2 * page_size;
    let vma = vmas.lookup(fault_va).unwrap();

    let (backing_vmo_id, backing_offset) = match &vma.backing {
        Backing::Vmo { vmo_id, offset, .. } => (*vmo_id, *offset),
        _ => panic!("expected VMO-backed VMA"),
    };

    let page_offset = (fault_va - vma.start) / page_size + backing_offset;
    assert_eq!(page_offset, 2);

    // Page is uncommitted — lookup returns None.
    // In the real kernel, handle_fault_vmo allocates a zeroed frame here.
    assert!(table
        .get(backing_vmo_id)
        .unwrap()
        .lookup_page(page_offset)
        .is_none());

    // Simulate the fault handler committing a zero-filled page.
    let zero_pa = Pa(0xF000_0000);
    table
        .get_mut(backing_vmo_id)
        .unwrap()
        .commit_page(page_offset, zero_pa);

    // Now the page is discoverable (subsequent faults would find it).
    let (pa, rc) = table
        .get(backing_vmo_id)
        .unwrap()
        .lookup_page(page_offset)
        .unwrap();
    assert_eq!(pa, zero_pa);
    assert_eq!(rc, 1);
}

#[test]
fn full_demand_page_flow_with_mapping_offset() {
    // VMO mapping with a nonzero offset — verifies the page_offset calculation.
    //
    // This catches the case where a VMA maps pages 2..6 of a VMO (offset=2).
    // A fault at the start of the VMA should resolve to VMO page 2, not page 0.

    let mut table = VmoTable::new();
    let vmo_id = table.create(8, VmoFlags::empty(), 0).unwrap();

    // Commit page 3 in the VMO (would be the second page of the mapping).
    let committed_pa = Pa(0xB000_0000);
    table.get_mut(vmo_id).unwrap().commit_page(3, committed_pa);

    let mut vmas = VmaList::new();
    let vma_start = 0x5000_0000u64;
    let page_size = paging::PAGE_SIZE;
    let mapping_offset = 2u64; // VMA maps starting at VMO page 2

    vmas.insert(Vma {
        start: vma_start,
        end: vma_start + 4 * page_size, // 4 pages mapped
        writable: true,
        executable: false,
        backing: Backing::Vmo {
            vmo_id,
            offset: mapping_offset,
            writable: true,
        },
    });

    // Fault at vma_start + 1*PAGE_SIZE (second page of the VMA).
    // This should resolve to VMO page offset = (1*PAGE_SIZE / PAGE_SIZE) + 2 = 3.
    let fault_va = vma_start + page_size;
    let vma = vmas.lookup(fault_va).unwrap();

    let (backing_vmo_id, backing_offset) = match &vma.backing {
        Backing::Vmo { vmo_id, offset, .. } => (*vmo_id, *offset),
        _ => panic!("expected VMO-backed VMA"),
    };

    let page_offset = (fault_va - vma.start) / page_size + backing_offset;
    assert_eq!(page_offset, 3, "should resolve to VMO page 3");

    let (pa, _) = table
        .get(backing_vmo_id)
        .unwrap()
        .lookup_page(page_offset)
        .expect("committed page 3 must be found");
    assert_eq!(pa, committed_pa);
}

#[test]
fn sealed_vmo_page_found_by_fault_handler() {
    // Sealed VMOs are mapped read-only by handle_fault_vmo, but committed
    // pages must still be discoverable.

    let mut table = VmoTable::new();
    let vmo_id = table.create(2, VmoFlags::empty(), 0).unwrap();

    let pa = Pa(0xD000_0000);
    table.get_mut(vmo_id).unwrap().commit_page(0, pa);
    table.get_mut(vmo_id).unwrap().seal();

    assert!(table.get(vmo_id).unwrap().is_sealed());

    // Fault handler still finds the committed page — seal affects permissions,
    // not page visibility.
    let (found_pa, rc) = table.get(vmo_id).unwrap().lookup_page(0).unwrap();
    assert_eq!(found_pa, pa);
    assert_eq!(rc, 1);
}

#[test]
fn cow_page_found_after_snapshot() {
    // After a snapshot, the fault handler must still find the page (with
    // elevated refcount). If it's a write fault, handle_fault_vmo performs COW.

    let mut table = VmoTable::new();
    let vmo_id = table.create(2, VmoFlags::empty(), 0).unwrap();

    let original_pa = Pa(0xA000_0000);
    table.get_mut(vmo_id).unwrap().commit_page(0, original_pa);

    // Snapshot — page refcount becomes 2.
    table.get_mut(vmo_id).unwrap().snapshot().unwrap();

    // Fault handler lookup: page exists but refcount > 1 → needs COW.
    let (pa, rc) = table.get(vmo_id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, original_pa);
    assert_eq!(rc, 2);
    assert!(table.get(vmo_id).unwrap().page_needs_cow(0));

    // Simulate COW: fault handler allocates new page and replaces.
    let cow_pa = Pa(0xA000_4000);
    let freed = table.get_mut(vmo_id).unwrap().cow_replace_page(0, cow_pa);
    assert!(freed.is_none()); // Original still held by snapshot.

    // After COW, the page is found with refcount 1.
    let (pa, rc) = table.get(vmo_id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, cow_pa);
    assert_eq!(rc, 1);
    assert!(!table.get(vmo_id).unwrap().page_needs_cow(0));
}
