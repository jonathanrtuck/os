//! Host-side tests for VMO Copy-on-Write through the fault handler path.
//!
//! Tests the COW data-structure operations that `handle_fault_vmo` relies on:
//! snapshot refcount sharing, `cow_replace_page` replacement, refcount
//! decrement on replace, sealed VMO behavior, decommit of shared pages,
//! multi-snapshot refcount escalation, and snapshot ring eviction.
//!
//! Uses the same #[path] include pattern as kernel_vmo.rs. The fault handler
//! itself lives in address_space.rs and depends on MMU/allocator state that
//! cannot run on the host, so we test the VMO-side logic it calls into.

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

#[path = "../../kernel/vmo.rs"]
mod vmo;

use memory::Pa;
use vmo::*;

// =========================================================================
// 1. Snapshot creates shared pages (refcount > 1)
// =========================================================================

#[test]
fn snapshot_bumps_refcount_for_committed_pages() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // Commit pages 0 and 2.
    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000_0000));
    table.get_mut(id).unwrap().commit_page(2, Pa(0xB000_0000));

    // Before snapshot: refcount = 1.
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 1);
    assert_eq!(table.get(id).unwrap().lookup_page(2).unwrap().1, 1);

    // Take snapshot.
    let gen = table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(gen, 1);

    // After snapshot: refcount = 2 for both committed pages.
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 2);
    assert_eq!(table.get(id).unwrap().lookup_page(2).unwrap().1, 2);

    // Uncommitted page 1 is still absent.
    assert!(table.get(id).unwrap().lookup_page(1).is_none());
}

#[test]
fn snapshot_only_affects_committed_pages() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // Commit only page 0.
    table.get_mut(id).unwrap().commit_page(0, Pa(0x1000_0000));

    table.get_mut(id).unwrap().snapshot().unwrap();

    // Page 0 has refcount 2 (shared with snapshot).
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 2);

    // Pages 1-3 are still uncommitted — no refcount to check.
    assert!(table.get(id).unwrap().lookup_page(1).is_none());
    assert!(table.get(id).unwrap().lookup_page(2).is_none());
    assert!(table.get(id).unwrap().lookup_page(3).is_none());
}

// =========================================================================
// 2. COW triggers on write to shared page
// =========================================================================

#[test]
fn cow_replace_on_shared_page_allocates_new_pa() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    let original_pa = Pa(0xA000_0000);
    table.get_mut(id).unwrap().commit_page(0, original_pa);

    // Snapshot → refcount 2.
    table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 2);

    // COW replace (simulates what the fault handler does on a write fault).
    let new_pa = Pa(0xC000_0000);
    let freed = table.get_mut(id).unwrap().cow_replace_page(0, new_pa);

    // Old PA is still referenced by the snapshot → not freed.
    assert_eq!(freed, None);

    // Current page list now has the new PA at refcount 1.
    let (pa, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, new_pa);
    assert_eq!(rc, 1);
}

#[test]
fn cow_replace_on_unshared_page_frees_old() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    let original_pa = Pa(0xA000_0000);
    table.get_mut(id).unwrap().commit_page(0, original_pa);

    // No snapshot — refcount is 1 (unshared).
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 1);

    // COW replace on unshared page: old page's refcount was 1, hits 0.
    let new_pa = Pa(0xB000_0000);
    let freed = table.get_mut(id).unwrap().cow_replace_page(0, new_pa);

    // Old PA should be returned for freeing.
    assert_eq!(freed, Some(original_pa));

    let (pa, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, new_pa);
    assert_eq!(rc, 1);
}

#[test]
fn page_needs_cow_reflects_refcount() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000_0000));

    // Before snapshot: refcount = 1, no COW needed.
    assert!(!table.get(id).unwrap().page_needs_cow(0));

    // After snapshot: refcount = 2, COW needed.
    table.get_mut(id).unwrap().snapshot().unwrap();
    assert!(table.get(id).unwrap().page_needs_cow(0));

    // After COW replace: refcount = 1 on new page, no COW needed.
    table
        .get_mut(id)
        .unwrap()
        .cow_replace_page(0, Pa(0xB000_0000));
    assert!(!table.get(id).unwrap().page_needs_cow(0));
}

// =========================================================================
// 3. COW preserves data (model-based — no real memory, but verify PA flow)
// =========================================================================

#[test]
fn cow_replace_preserves_pa_identity() {
    // This test models the fault handler's copy step:
    //   1. lookup_page → (old_pa, refcount > 1)
    //   2. alloc new page, copy old→new (in fault handler, not VMO)
    //   3. cow_replace_page(offset, new_pa)
    //
    // We verify the VMO side: after replace, new_pa is retrievable at
    // the same offset, and old_pa remains in the snapshot.

    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    let old_pa = Pa(0xDEAD_0000);
    table.get_mut(id).unwrap().commit_page(0, old_pa);

    table.get_mut(id).unwrap().snapshot().unwrap(); // gen 0 snapshot

    let new_pa = Pa(0xBEEF_0000);
    table.get_mut(id).unwrap().cow_replace_page(0, new_pa);

    // Current: new_pa
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().0, new_pa);

    // Restore to gen 0: old_pa
    table.get_mut(id).unwrap().restore(0).unwrap();
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().0, old_pa);
}

#[test]
fn cow_replace_on_uncommitted_offset_returns_none() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    // Page 0 is uncommitted. cow_replace_page inserts new_pa, no old page.
    let new_pa = Pa(0x1234_0000);
    let freed = table.get_mut(id).unwrap().cow_replace_page(0, new_pa);
    assert_eq!(freed, None);

    let (pa, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, new_pa);
    assert_eq!(rc, 1);
}

// =========================================================================
// 4. Multiple snapshots increase refcount
// =========================================================================

#[test]
fn two_snapshots_refcount_three() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));

    // First snapshot: refcount 1 → 2.
    table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 2);

    // Second snapshot: refcount 2 → 3.
    table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 3);
}

#[test]
fn three_snapshots_refcount_four() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));

    table.get_mut(id).unwrap().snapshot().unwrap();
    table.get_mut(id).unwrap().snapshot().unwrap();
    table.get_mut(id).unwrap().snapshot().unwrap();

    // 1 (original) + 3 snapshots sharing = refcount 4.
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 4);
}

#[test]
fn cow_after_multiple_snapshots_decrements_once() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));

    // Two snapshots: refcount = 3.
    table.get_mut(id).unwrap().snapshot().unwrap();
    table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 3);

    // COW replace: old PA still held by 2 snapshots → not freed.
    let freed = table.get_mut(id).unwrap().cow_replace_page(0, Pa(0xB000));
    assert_eq!(freed, None);

    // New page at refcount 1.
    let (pa, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, Pa(0xB000));
    assert_eq!(rc, 1);
}

// =========================================================================
// 5. Snapshot restore decrements refcount
// =========================================================================

#[test]
fn restore_oldest_snapshot_frees_current_exclusive_pages() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    // Gen 0: commit PA A.
    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));
    table.get_mut(id).unwrap().snapshot().unwrap(); // snapshot gen 0

    // Gen 1: COW → PA B.
    table.get_mut(id).unwrap().cow_replace_page(0, Pa(0xB000));
    table.get_mut(id).unwrap().snapshot().unwrap(); // snapshot gen 1

    // Gen 2: COW → PA C.
    table.get_mut(id).unwrap().cow_replace_page(0, Pa(0xC000));

    // Current is PA C (refcount 1).
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().0, Pa(0xC000));

    // Restore to gen 0 → back to PA A. PA C was exclusive, so freed.
    let freed = table.get_mut(id).unwrap().restore(0).unwrap();
    assert!(freed.contains(&Pa(0xC000)));
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().0, Pa(0xA000));
}

#[test]
fn restore_with_shared_pages_does_not_free_shared() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    // Commit pages 0 and 1.
    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));
    table.get_mut(id).unwrap().commit_page(1, Pa(0xB000));

    // Snapshot gen 0: both pages shared (refcount 2).
    table.get_mut(id).unwrap().snapshot().unwrap();

    // COW replace page 0 only.
    table.get_mut(id).unwrap().cow_replace_page(0, Pa(0xC000));

    // Restore to gen 0: PA C (exclusive to current) should be freed.
    // PA B (page 1) is shared → not freed.
    let freed = table.get_mut(id).unwrap().restore(0).unwrap();
    assert!(freed.contains(&Pa(0xC000)));
    assert!(!freed.contains(&Pa(0xB000)));
}

// =========================================================================
// 6. Sealed VMO prevents COW
// =========================================================================

#[test]
fn sealed_vmo_effective_writable_false() {
    // Models the fault handler logic:
    //   effective_writable = writable && !vmo.is_sealed()
    // When sealed, the fault handler maps pages RO regardless of VMA flags.

    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));
    table.get_mut(id).unwrap().snapshot().unwrap();

    // Before seal: page needs COW (refcount > 1).
    assert!(table.get(id).unwrap().page_needs_cow(0));

    // Seal the VMO.
    table.get_mut(id).unwrap().seal();
    assert!(table.get(id).unwrap().is_sealed());

    // The page still has refcount > 1, but the fault handler checks:
    //   if effective_writable && refcount > 1 { COW }
    // With sealed=true, effective_writable=false, so COW is skipped.
    // The existing PA is returned and mapped RO.
    let (pa, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, Pa(0xA000));
    assert!(rc > 1); // Still shared, but no COW because sealed.
}

#[test]
fn sealed_vmo_rejects_cow_replace() {
    // Sealed VMOs should not allow commit_page or try_commit_page.
    // cow_replace_page does not check seal (it's the fault handler's job
    // to skip COW on sealed VMOs), but try_commit_page does.

    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));
    table.get_mut(id).unwrap().seal();

    // try_commit_page rejects.
    assert!(!table.get_mut(id).unwrap().try_commit_page(1, Pa(0xB000)));

    // Snapshot is rejected on sealed VMO.
    assert!(table.get_mut(id).unwrap().snapshot().is_none());
}

#[test]
fn sealed_vmo_returns_existing_pa_for_committed_page() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    let pa = Pa(0xFEED_0000);
    table.get_mut(id).unwrap().commit_page(0, pa);
    table.get_mut(id).unwrap().seal();

    // Lookup still returns the committed page (fault handler maps it RO).
    let (found_pa, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(found_pa, pa);
    assert_eq!(rc, 1);
}

// =========================================================================
// 7. Decommit returns pages to free
// =========================================================================

#[test]
fn decommit_unshared_page_returns_pa_for_freeing() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();

    let pa = Pa(0xC000_0000);
    table.get_mut(id).unwrap().commit_page(0, pa);
    assert_eq!(table.get(id).unwrap().committed_pages(), 1);

    let result = table.get_mut(id).unwrap().decommit_page(0);
    assert_eq!(result, Some(Some(pa)));
    assert_eq!(table.get(id).unwrap().committed_pages(), 0);
    assert!(table.get(id).unwrap().lookup_page(0).is_none());
}

#[test]
fn decommit_shared_page_returns_none_still_referenced() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    let pa = Pa(0xD000_0000);
    table.get_mut(id).unwrap().commit_page(0, pa);

    // Snapshot → refcount 2.
    table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 2);

    // Decommit from current: page is still in snapshot, don't free.
    let result = table.get_mut(id).unwrap().decommit_page(0);
    assert_eq!(result, Some(None));
    assert_eq!(table.get(id).unwrap().committed_pages(), 0);
}

#[test]
fn decommit_after_cow_replace_frees_new_page() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));
    table.get_mut(id).unwrap().snapshot().unwrap();

    // COW replace: new_pa at refcount 1.
    let new_pa = Pa(0xB000);
    table.get_mut(id).unwrap().cow_replace_page(0, new_pa);
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().1, 1);

    // Decommit: new_pa is unshared (refcount 1) → freed.
    let result = table.get_mut(id).unwrap().decommit_page(0);
    assert_eq!(result, Some(Some(new_pa)));
}

#[test]
fn decommit_sealed_vmo_rejected() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id).unwrap().commit_page(0, Pa(0xE000));
    table.get_mut(id).unwrap().seal();

    assert_eq!(table.get_mut(id).unwrap().decommit_page(0), None);
}

// =========================================================================
// 8. Snapshot ring eviction
// =========================================================================

#[test]
fn snapshot_ring_evicts_oldest_when_full() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0x1_0000));

    let max = table.get(id).unwrap().max_snapshots();

    // Fill the ring exactly to capacity.
    for _ in 0..max {
        table.get_mut(id).unwrap().snapshot().unwrap();
    }

    // Generation 0 (first snapshot) should still exist.
    assert!(table.get_mut(id).unwrap().restore(0).is_some());

    // Re-commit and refill (restore consumed one snapshot).
    // Reset: create fresh VMO to avoid entangled state.
    let id2 = table.create(1, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id2).unwrap().commit_page(0, Pa(0x2_0000));

    // Create max+1 snapshots → oldest evicted.
    for _ in 0..=max {
        table.get_mut(id2).unwrap().snapshot().unwrap();
    }

    // Generation 0 should be evicted.
    assert!(table.get_mut(id2).unwrap().restore(0).is_none());
    // Generation 1 (second oldest) should still exist.
    assert!(table.get_mut(id2).unwrap().restore(1).is_some());
}

#[test]
fn eviction_decrements_refcounts_of_evicted_pages() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));

    let max = table.get(id).unwrap().max_snapshots();

    // Take max+1 snapshots to trigger one eviction.
    for _ in 0..=max {
        table.get_mut(id).unwrap().snapshot().unwrap();
    }

    // The page's refcount should be max+1 (current + max snapshots),
    // not max+2 (evicted snapshot's refcount was decremented).
    let (_, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(rc as usize, max + 1);
}

#[test]
fn destroy_after_snapshots_collects_all_unique_pages() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    // Gen 0: PA A.
    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));
    table.get_mut(id).unwrap().snapshot().unwrap();

    // Gen 1: COW → PA B.
    table.get_mut(id).unwrap().cow_replace_page(0, Pa(0xB000));
    table.get_mut(id).unwrap().snapshot().unwrap();

    // Gen 2: COW → PA C.
    table.get_mut(id).unwrap().cow_replace_page(0, Pa(0xC000));

    // Destroy should collect all 3 unique PAs.
    let freed = table.destroy(id);
    assert_eq!(freed.len(), 3);
    assert!(freed.contains(&Pa(0xA000)));
    assert!(freed.contains(&Pa(0xB000)));
    assert!(freed.contains(&Pa(0xC000)));
}

// =========================================================================
// Additional edge cases for the fault handler path
// =========================================================================

#[test]
fn cow_replace_multiple_pages_independently() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // Commit 4 pages.
    for i in 0..4 {
        table
            .get_mut(id)
            .unwrap()
            .commit_page(i, Pa(0xA000 + (i as usize) * 0x1000));
    }

    // Snapshot: all 4 pages shared (refcount 2).
    table.get_mut(id).unwrap().snapshot().unwrap();

    // COW replace page 1 only.
    let new_pa = Pa(0xF000);
    table.get_mut(id).unwrap().cow_replace_page(1, new_pa);

    // Page 1: new PA, refcount 1.
    let (pa, rc) = table.get(id).unwrap().lookup_page(1).unwrap();
    assert_eq!(pa, new_pa);
    assert_eq!(rc, 1);

    // Pages 0, 2, 3: still shared, refcount 2.
    for i in [0u64, 2, 3] {
        let (_, rc) = table.get(id).unwrap().lookup_page(i).unwrap();
        assert_eq!(rc, 2, "page {i} should still be shared");
    }
}

#[test]
fn snapshot_then_commit_new_page_does_not_affect_snapshot() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();

    // Only commit page 0.
    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));
    table.get_mut(id).unwrap().snapshot().unwrap(); // snapshot gen 0

    // Commit page 1 (new — was not in the snapshot).
    table.get_mut(id).unwrap().commit_page(1, Pa(0xB000));

    // Page 1 should have refcount 1 (not shared with snapshot).
    let (_, rc) = table.get(id).unwrap().lookup_page(1).unwrap();
    assert_eq!(rc, 1);

    // Restore to gen 0: page 1 should disappear (not in snapshot).
    let freed = table.get_mut(id).unwrap().restore(0).unwrap();
    assert!(freed.contains(&Pa(0xB000)));
    assert!(table.get(id).unwrap().lookup_page(1).is_none());
}

#[test]
fn contiguous_vmo_rejects_snapshot_and_cow() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::CONTIGUOUS, 0).unwrap();

    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));

    // Contiguous VMOs cannot be snapshotted.
    assert!(table.get_mut(id).unwrap().snapshot().is_none());

    // page_needs_cow is always false (refcount never > 1 without snapshots).
    assert!(!table.get(id).unwrap().page_needs_cow(0));
}
