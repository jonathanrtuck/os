//! Host-side tests for VMO (Virtual Memory Object) kernel module.
//!
//! Tests the VMO lifecycle: create, page tracking, snapshots, seal,
//! and integration with the handle table.

mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}

#[path = "../../kernel/handle.rs"]
mod handle;
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

use handle::*;
use memory::Pa;
use vmo::*;

// =========================================================================
// Step 1: VMO type, VmoId, creation, Drop
// =========================================================================

// --- create ---

#[test]
fn create_normal_vmo() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let v = table.get(id).unwrap();
    assert_eq!(v.size_pages(), 4);
    assert!(!v.is_contiguous());
    assert!(!v.is_sealed());
    assert_eq!(v.generation(), 0);
    assert_eq!(v.type_tag(), 0);
    assert_eq!(v.committed_pages(), 0); // lazy — no pages yet
}

#[test]
fn create_with_type_tag() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0xDEAD_BEEF).unwrap();
    let v = table.get(id).unwrap();
    assert_eq!(v.type_tag(), 0xDEAD_BEEF);
}

#[test]
fn create_zero_size_fails() {
    let mut table = VmoTable::new();
    assert!(table.create(0, VmoFlags::empty(), 0).is_none());
}

#[test]
fn create_returns_sequential_ids() {
    let mut table = VmoTable::new();
    let id0 = table.create(1, VmoFlags::empty(), 0).unwrap();
    let id1 = table.create(1, VmoFlags::empty(), 0).unwrap();
    assert_eq!(id0.0, 0);
    assert_eq!(id1.0, 1);
}

// --- destroy ---

#[test]
fn destroy_frees_committed_pages() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    // Simulate committing a page (as the fault handler would).
    let pa = Pa(0x5000_0000);
    table.get_mut(id).unwrap().commit_page(0, pa);
    assert_eq!(table.get(id).unwrap().committed_pages(), 1);

    // Destroy — the page should be marked for freeing.
    let freed = table.destroy(id);
    assert_eq!(freed.len(), 1);
    assert_eq!(freed[0], pa);
}

#[test]
fn destroy_nonexistent_returns_empty() {
    let mut table = VmoTable::new();
    let freed = table.destroy(VmoId(999));
    assert!(freed.is_empty());
}

// --- page tracking ---

#[test]
fn lookup_uncommitted_page_returns_none() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    assert!(table.get(id).unwrap().lookup_page(0).is_none());
    assert!(table.get(id).unwrap().lookup_page(3).is_none());
}

#[test]
fn commit_and_lookup_page() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let pa = Pa(0x6000_0000);
    table.get_mut(id).unwrap().commit_page(1, pa);

    let (found_pa, refcount) = table.get(id).unwrap().lookup_page(1).unwrap();
    assert_eq!(found_pa, pa);
    assert_eq!(refcount, 1);
    assert_eq!(table.get(id).unwrap().committed_pages(), 1);
}

#[test]
fn commit_page_out_of_bounds_ignored() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();
    // Page offset 2 is out of bounds for a 2-page VMO (valid: 0, 1).
    table.get_mut(id).unwrap().commit_page(2, Pa(0x7000_0000));
    assert_eq!(table.get(id).unwrap().committed_pages(), 0);
}

// --- handle integration ---

#[test]
fn vmo_handle_object_variant() {
    let mut handles = HandleTable::new();
    let id = VmoId(42);
    let h = handles.insert(HandleObject::Vmo(id), Rights::ALL).unwrap();
    let obj = handles.get(h, Rights::NONE).unwrap();
    assert!(matches!(obj, HandleObject::Vmo(VmoId(42))));
}

#[test]
fn vmo_rights_attenuation() {
    let mut handles = HandleTable::new();
    let h = handles
        .insert(HandleObject::Vmo(VmoId(0)), Rights::ALL)
        .unwrap();

    // Can get with READ
    assert!(handles.get(h, Rights::READ).is_ok());
    // Can get with MAP
    assert!(handles.get(h, Rights::MAP).is_ok());

    // Close and re-insert with READ only
    handles.close(h).unwrap();
    let h2 = handles
        .insert(HandleObject::Vmo(VmoId(0)), Rights::READ)
        .unwrap();

    // READ succeeds
    assert!(handles.get(h2, Rights::READ).is_ok());
    // WRITE fails
    assert!(matches!(
        handles.get(h2, Rights::WRITE),
        Err(HandleError::InsufficientRights)
    ));
}

// =========================================================================
// Step 2: vmo_get_info
// =========================================================================

#[test]
fn info_reflects_creation_params() {
    let mut table = VmoTable::new();
    let id = table.create(8, VmoFlags::empty(), 0xCAFE).unwrap();
    let info = table.get(id).unwrap().info();
    assert_eq!(info.size_pages, 8);
    assert_eq!(info.type_tag, 0xCAFE);
    assert_eq!(info.generation, 0);
    assert_eq!(info.committed_pages, 0);
    assert_eq!(info.flags, 0);
}

#[test]
fn info_committed_pages_updates() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id).unwrap().commit_page(0, Pa(0x1000_0000));
    table.get_mut(id).unwrap().commit_page(2, Pa(0x2000_0000));
    let info = table.get(id).unwrap().info();
    assert_eq!(info.committed_pages, 2);
}

// =========================================================================
// New rights: APPEND and SEAL
// =========================================================================

#[test]
fn append_right_exists() {
    let r = Rights::APPEND;
    assert!(Rights::ALL.contains(r));
}

#[test]
fn seal_right_exists() {
    let r = Rights::SEAL;
    assert!(Rights::ALL.contains(r));
}

#[test]
fn append_and_seal_attenuate_correctly() {
    let all = Rights::ALL;
    let read_only = all.attenuate(Rights::READ);
    assert!(read_only.contains(Rights::READ));
    assert!(!read_only.contains(Rights::APPEND));
    assert!(!read_only.contains(Rights::SEAL));

    let append_seal = Rights::APPEND.union(Rights::SEAL);
    assert!(append_seal.contains(Rights::APPEND));
    assert!(append_seal.contains(Rights::SEAL));
    assert!(!append_seal.contains(Rights::WRITE));
}

// =========================================================================
// Snapshot / COW (Step 5, but test the data structure now)
// =========================================================================

#[test]
fn snapshot_preserves_pages() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    // Commit page 0
    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000_0000));

    // Snapshot
    let gen = table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(gen, 1);
    assert_eq!(table.get(id).unwrap().generation(), 1);

    // Page 0 should now have refcount 2 (current + snapshot)
    let (_, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(rc, 2);
}

#[test]
fn snapshot_on_sealed_fails() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id).unwrap().seal();
    assert!(table.get_mut(id).unwrap().snapshot().is_none());
}

#[test]
fn restore_reverts_to_snapshot() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    // Commit page 0 with value A
    let pa_a = Pa(0xA000_0000);
    table.get_mut(id).unwrap().commit_page(0, pa_a);

    // Snapshot captures gen 0 (pre-increment), then bumps to gen 1.
    let gen = table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(gen, 1); // current generation is now 1

    // "Overwrite" page 0 with COW: simulates what the fault handler does.
    // After snapshot, page 0 has refcount 2 (shared with snapshot).
    // cow_replace_page inserts new pa at refcount=1, decrements old in snapshot.
    // Old page is NOT freed (still referenced by snapshot) — returns None.
    let pa_b = Pa(0xB000_0000);
    let old = table.get_mut(id).unwrap().cow_replace_page(0, pa_b);
    assert_eq!(old, None); // pa_a still alive in snapshot

    // Current should see pa_b
    let (pa, _) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, pa_b);

    // Restore to gen 0 (the snapshot) — should see pa_a again
    let freed = table.get_mut(id).unwrap().restore(0).unwrap();
    let (pa, _) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(pa, pa_a);

    // pa_b should be in the freed list (refcount hit 0)
    assert!(freed.contains(&pa_b));
}

#[test]
fn restore_invalid_generation_fails() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();
    assert!(table.get_mut(id).unwrap().restore(99).is_none());
}

// =========================================================================
// Seal
// =========================================================================

#[test]
fn seal_prevents_commit() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id).unwrap().seal();
    assert!(table.get(id).unwrap().is_sealed());

    // commit_page should be rejected on sealed VMO
    let committed = table.get_mut(id).unwrap().try_commit_page(0, Pa(0x1000));
    assert!(!committed);
}

#[test]
fn seal_is_reflected_in_info() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id).unwrap().seal();
    let info = table.get(id).unwrap().info();
    assert_ne!(info.flags & VMO_FLAG_SEALED, 0);
}

// =========================================================================
// Snapshot ring bounds
// =========================================================================

#[test]
fn snapshot_ring_evicts_oldest() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    // Commit a page
    table.get_mut(id).unwrap().commit_page(0, Pa(0x1_0000));

    // Create max_snapshots + 1 snapshots
    let max = table.get(id).unwrap().max_snapshots();
    for _ in 0..=max {
        // Each snapshot bumps generation; the page refcount rises
        table.get_mut(id).unwrap().snapshot().unwrap();
    }

    // The oldest snapshot (generation 0) should have been evicted.
    // Snapshots store the pre-increment generation: first snapshot = gen 0,
    // second = gen 1, etc. After max+1 iterations, gen 0 is evicted.
    assert!(table.get_mut(id).unwrap().restore(0).is_none());
    // But generation 1 (second oldest) should still exist.
    assert!(table.get_mut(id).unwrap().restore(1).is_some());
}

// =========================================================================
// Mapping tracking (Cluster B pre-step)
// =========================================================================

#[test]
fn add_mapping_tracks_process_and_va() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.add_mapping(VmoMapping {
        process_id: process::ProcessId(1),
        va_base: 0x4000_0000,
        page_count: 4,
    });
    assert_eq!(vmo.mappings().len(), 1);
    assert_eq!(vmo.mappings()[0].process_id, process::ProcessId(1));
    assert_eq!(vmo.mappings()[0].va_base, 0x4000_0000);
    assert_eq!(vmo.mappings()[0].page_count, 4);
}

#[test]
fn remove_mapping_by_process_and_va() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.add_mapping(VmoMapping {
        process_id: process::ProcessId(1),
        va_base: 0x4000_0000,
        page_count: 4,
    });
    let removed = vmo.remove_mapping(process::ProcessId(1), 0x4000_0000);
    assert!(removed);
    assert!(vmo.mappings().is_empty());
}

#[test]
fn remove_nonexistent_mapping_returns_false() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let removed = table
        .get_mut(id)
        .unwrap()
        .remove_mapping(process::ProcessId(1), 0x4000_0000);
    assert!(!removed);
}

#[test]
fn multiple_mappings_from_different_processes() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.add_mapping(VmoMapping {
        process_id: process::ProcessId(1),
        va_base: 0x4000_0000,
        page_count: 4,
    });
    vmo.add_mapping(VmoMapping {
        process_id: process::ProcessId(2),
        va_base: 0x5000_0000,
        page_count: 4,
    });
    assert_eq!(vmo.mappings().len(), 2);

    // Remove first — second survives
    vmo.remove_mapping(process::ProcessId(1), 0x4000_0000);
    assert_eq!(vmo.mappings().len(), 1);
    assert_eq!(vmo.mappings()[0].process_id, process::ProcessId(2));
}

#[test]
fn destroy_with_mappings_returns_all_pages() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();

    // Commit a page and add a mapping
    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000_0000));
    table.get_mut(id).unwrap().add_mapping(VmoMapping {
        process_id: process::ProcessId(1),
        va_base: 0x4000_0000,
        page_count: 2,
    });

    let freed = table.destroy(id);
    assert_eq!(freed.len(), 1);
    assert_eq!(freed[0], Pa(0xA000_0000));
}

// =========================================================================
// Decommit (Step 7 data structure)
// =========================================================================

#[test]
fn decommit_uncommitted_page_is_noop() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    // Decommit a page that was never committed — should succeed, nothing freed.
    let result = table.get_mut(id).unwrap().decommit_page(0);
    assert_eq!(result, Some(None));
}

#[test]
fn decommit_committed_page_frees_it() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let pa = Pa(0xC000_0000);
    table.get_mut(id).unwrap().commit_page(0, pa);
    assert_eq!(table.get(id).unwrap().committed_pages(), 1);

    let result = table.get_mut(id).unwrap().decommit_page(0);
    assert_eq!(result, Some(Some(pa))); // Should be freed
    assert_eq!(table.get(id).unwrap().committed_pages(), 0);
    assert!(table.get(id).unwrap().lookup_page(0).is_none());
}

#[test]
fn decommit_shared_page_does_not_free() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();
    let pa = Pa(0xD000_0000);
    table.get_mut(id).unwrap().commit_page(0, pa);

    // Snapshot — page refcount becomes 2
    table.get_mut(id).unwrap().snapshot().unwrap();
    let (_, rc) = table.get(id).unwrap().lookup_page(0).unwrap();
    assert_eq!(rc, 2);

    // Decommit from current — page is still in snapshot, don't free
    let result = table.get_mut(id).unwrap().decommit_page(0);
    assert_eq!(result, Some(None)); // Not freed — snapshot still holds it
    assert_eq!(table.get(id).unwrap().committed_pages(), 0);
}

#[test]
fn decommit_on_sealed_vmo_fails() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id).unwrap().commit_page(0, Pa(0xE000_0000));
    table.get_mut(id).unwrap().seal();
    assert_eq!(table.get_mut(id).unwrap().decommit_page(0), None);
}

#[test]
fn decommit_out_of_bounds_fails() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();
    assert_eq!(table.get_mut(id).unwrap().decommit_page(10), None);
}

// =========================================================================
// Snapshot info introspection (novelty — no other microkernel exposes this)
// =========================================================================

#[test]
fn info_includes_snapshot_count() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id).unwrap().commit_page(0, Pa(0x1000));

    assert_eq!(table.get(id).unwrap().info().snapshot_count, 0);

    table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(table.get(id).unwrap().info().snapshot_count, 1);

    table.get_mut(id).unwrap().snapshot().unwrap();
    assert_eq!(table.get(id).unwrap().info().snapshot_count, 2);
}

// =========================================================================
// Multi-generation snapshot/restore (Step 5 thoroughness)
// =========================================================================

#[test]
fn snapshot_three_generations_restore_middle() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::empty(), 0).unwrap();

    // Gen 0: page at 0xA
    table.get_mut(id).unwrap().commit_page(0, Pa(0xA000));
    table.get_mut(id).unwrap().snapshot().unwrap(); // snapshot gen 0, now gen 1

    // Gen 1: COW-replace with 0xB
    table.get_mut(id).unwrap().cow_replace_page(0, Pa(0xB000));
    table.get_mut(id).unwrap().snapshot().unwrap(); // snapshot gen 1, now gen 2

    // Gen 2: COW-replace with 0xC
    table.get_mut(id).unwrap().cow_replace_page(0, Pa(0xC000));

    // Current should be 0xC
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().0, Pa(0xC000));

    // Restore to gen 1 (should see 0xB)
    let freed = table.get_mut(id).unwrap().restore(1).unwrap();
    assert_eq!(table.get(id).unwrap().lookup_page(0).unwrap().0, Pa(0xB000));

    // 0xC should be freed (it was only in the current generation)
    assert!(freed.contains(&Pa(0xC000)));
}

#[test]
fn contiguous_vmo_rejects_snapshot() {
    let mut table = VmoTable::new();
    let id = table.create(1, VmoFlags::CONTIGUOUS, 0).unwrap();
    // Contiguous VMOs cannot be snapshotted (no COW — physically contiguous)
    assert!(table.get_mut(id).unwrap().snapshot().is_none());
}

// =========================================================================
// Seal with mappings (Step 6 — seal returns writable mappings to invalidate)
// =========================================================================

#[test]
fn seal_returns_writable_mappings() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();

    // Add two mappings: one RW (page_count > 0 means writable), one RO
    vmo.add_mapping(VmoMapping {
        process_id: process::ProcessId(1),
        va_base: 0x4000_0000,
        page_count: 4,
    });
    vmo.add_mapping(VmoMapping {
        process_id: process::ProcessId(2),
        va_base: 0x5000_0000,
        page_count: 4,
    });

    // seal_and_get_mappings returns all mappings that need PTE invalidation
    let mappings = vmo.seal_and_get_mappings();
    assert!(vmo.is_sealed());
    // Both mappings returned (all need invalidation — caller decides which are writable)
    assert_eq!(mappings.len(), 2);
}

// =========================================================================
// Pager (Phase 3b)
// =========================================================================

use handle::ChannelId;

#[test]
fn vmo_initially_has_no_pager() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    assert!(!table.get(id).unwrap().has_pager());
}

#[test]
fn set_pager_attaches_channel() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let ch = ChannelId(42);
    table.get_mut(id).unwrap().set_pager(ch);
    assert!(table.get(id).unwrap().has_pager());
    assert_eq!(table.get(id).unwrap().pager_channel(), Some(ch));
}

#[test]
fn set_pager_on_sealed_vmo_fails() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    table.get_mut(id).unwrap().seal();
    // Sealed VMOs reject pager attachment (immutable).
    assert!(!table.get_mut(id).unwrap().set_pager(ChannelId(1)));
}

#[test]
fn pending_faults_tracking() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    // First fault on page 2 — should be new (not already pending).
    assert!(vmo.add_pending_fault(2));
    // Second fault on page 2 — already pending, deduplicated.
    assert!(!vmo.add_pending_fault(2));
    // Different page — new.
    assert!(vmo.add_pending_fault(5));

    // Supply page 2 — clears pending.
    vmo.clear_pending_fault(2);
    // Now page 2 can be re-requested.
    assert!(vmo.add_pending_fault(2));
}

#[test]
fn needs_pager_for_uncommitted_page() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();

    // No pager → uncommitted page should NOT need pager.
    assert!(!vmo.needs_pager_for(0));

    // Attach pager → uncommitted page needs pager.
    vmo.set_pager(ChannelId(1));
    assert!(vmo.needs_pager_for(0));

    // Commit page 0 → no longer needs pager (already committed).
    vmo.commit_page(0, Pa(0x1000));
    assert!(!vmo.needs_pager_for(0));

    // Uncommitted page 1 still needs pager.
    assert!(vmo.needs_pager_for(1));
}
