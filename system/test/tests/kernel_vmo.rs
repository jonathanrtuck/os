//! Host-side tests for VMO (Virtual Memory Object) kernel module.
//!
//! Tests the VMO lifecycle: create, page tracking, snapshots, seal,
//! and integration with the handle table.

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
    let h = handles
        .insert(HandleObject::Vmo(id), Rights::ALL)
        .unwrap();
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
