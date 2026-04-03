//! Pager fault dispatch model tests (v0.6).
//!
//! Tests the pager integration with VMOs: fault classification, pending fault
//! deduplication, pager supply, and interactions with sealed VMOs.
//!
//! The pager system: when a VMO has a pager channel and an uncommitted page is
//! accessed, the fault handler sends a message to the pager channel, blocks the
//! faulting thread, and the pager supplies the page later.

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

use handle::ChannelId;
use memory::Pa;
use vmo::*;

// ============================================================
// Fault result classification (model)
// ============================================================

/// Models the fault handler's decision for a page fault on a VMO.
#[derive(Debug, PartialEq, Eq)]
enum FaultResult {
    /// Page already committed — map it directly, no pager involved.
    Handled(Pa),
    /// No pager, uncommitted page — zero-fill and commit.
    ZeroFilled,
    /// Pager exists, uncommitted page — must dispatch to pager.
    NeedsPager {
        channel: ChannelId,
        page_offset: u64,
    },
}

/// Classify a fault on a VMO page. Models the kernel's fault handler logic.
fn classify_fault(vmo: &Vmo, page_offset: u64) -> FaultResult {
    // Already committed — direct map.
    if let Some((pa, _rc)) = vmo.lookup_page(page_offset) {
        return FaultResult::Handled(pa);
    }

    // Uncommitted — check for pager.
    if let Some(channel) = vmo.pager_channel() {
        if page_offset < vmo.size_pages() {
            return FaultResult::NeedsPager {
                channel,
                page_offset,
            };
        }
    }

    // No pager — zero-fill.
    FaultResult::ZeroFilled
}

// ============================================================
// Test 1: VMO with pager → fault returns NeedsPager
// ============================================================

#[test]
fn vmo_with_pager_uncommitted_page_needs_pager() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();

    let ch = ChannelId(42);
    assert!(vmo.set_pager(ch));

    // Page 0 is uncommitted, pager exists → NeedsPager.
    let result = classify_fault(vmo, 0);
    assert_eq!(
        result,
        FaultResult::NeedsPager {
            channel: ch,
            page_offset: 0
        }
    );

    // Page 3 (last valid page) — same result.
    let result = classify_fault(vmo, 3);
    assert_eq!(
        result,
        FaultResult::NeedsPager {
            channel: ch,
            page_offset: 3
        }
    );
}

#[test]
fn needs_pager_for_returns_true_for_uncommitted_with_pager() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    assert!(vmo.needs_pager_for(0));
    assert!(vmo.needs_pager_for(3));
    // Out of bounds — no pager needed.
    assert!(!vmo.needs_pager_for(4));
}

// ============================================================
// Test 2: VMO without pager → fault zero-fills
// ============================================================

#[test]
fn vmo_without_pager_uncommitted_page_zero_fills() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get(id).unwrap();

    // No pager attached — uncommitted page should zero-fill.
    let result = classify_fault(vmo, 0);
    assert_eq!(result, FaultResult::ZeroFilled);
}

#[test]
fn needs_pager_for_returns_false_without_pager() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get(id).unwrap();

    assert!(!vmo.needs_pager_for(0));
    assert!(!vmo.has_pager());
}

// ============================================================
// Test 3: Committed page → no pager involved
// ============================================================

#[test]
fn committed_page_returns_handled() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();

    let ch = ChannelId(10);
    vmo.set_pager(ch);

    // Commit page 0.
    let pa = Pa(0x5000_0000);
    vmo.commit_page(0, pa);

    // Fault on committed page — Handled, pager not consulted.
    let result = classify_fault(vmo, 0);
    assert_eq!(result, FaultResult::Handled(pa));

    // needs_pager_for also returns false for committed pages.
    assert!(!vmo.needs_pager_for(0));
}

#[test]
fn committed_page_without_pager_also_handled() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();

    let pa = Pa(0x6000_0000);
    vmo.commit_page(1, pa);

    let result = classify_fault(vmo, 1);
    assert_eq!(result, FaultResult::Handled(pa));
}

// ============================================================
// Test 4: Pending fault deduplication
// ============================================================

#[test]
fn pending_fault_deduplication() {
    let mut table = VmoTable::new();
    let id = table.create(8, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    // First fault on page 3 — new request, should dispatch to pager.
    assert!(
        vmo.add_pending_fault(3),
        "first fault on a page should return true (new)"
    );

    // Second fault on page 3 — duplicate, should NOT re-dispatch.
    assert!(
        !vmo.add_pending_fault(3),
        "second fault on same page should return false (dedup)"
    );

    // Fault on a different page — new.
    assert!(
        vmo.add_pending_fault(7),
        "fault on different page should return true (new)"
    );

    // Page 7 duplicate.
    assert!(!vmo.add_pending_fault(7));
}

#[test]
fn pending_fault_multiple_pages_independent() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    // Pages 0, 1, 2 all pending — each is independent.
    assert!(vmo.add_pending_fault(0));
    assert!(vmo.add_pending_fault(1));
    assert!(vmo.add_pending_fault(2));

    // All are duplicates now.
    assert!(!vmo.add_pending_fault(0));
    assert!(!vmo.add_pending_fault(1));
    assert!(!vmo.add_pending_fault(2));
}

// ============================================================
// Test 5: Clear pending fault
// ============================================================

#[test]
fn clear_pending_fault_allows_re_request() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    // Add pending fault.
    assert!(vmo.add_pending_fault(2));
    assert!(!vmo.add_pending_fault(2)); // duplicate

    // Pager supplies page → clear pending.
    vmo.clear_pending_fault(2);

    // Now the same page can be requested again.
    assert!(
        vmo.add_pending_fault(2),
        "cleared fault should allow re-request"
    );
}

#[test]
fn clear_pending_fault_does_not_affect_other_pages() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    assert!(vmo.add_pending_fault(0));
    assert!(vmo.add_pending_fault(1));

    // Clear only page 0.
    vmo.clear_pending_fault(0);

    // Page 0 can be re-requested, page 1 is still pending.
    assert!(vmo.add_pending_fault(0));
    assert!(!vmo.add_pending_fault(1));
}

// ============================================================
// Test 6: Pager supply commits page
// ============================================================

#[test]
fn pager_supply_commits_page() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    // Fault arrives, we add pending.
    assert!(vmo.add_pending_fault(2));
    assert!(vmo.needs_pager_for(2));

    // Pager supplies a physical page.
    let pa = Pa(0xBEEF_0000);
    vmo.commit_page(2, pa);
    vmo.clear_pending_fault(2);

    // Page is now committed — subsequent lookups find it.
    let (found_pa, refcount) = vmo.lookup_page(2).unwrap();
    assert_eq!(found_pa, pa);
    assert_eq!(refcount, 1);

    // No longer needs pager for this page.
    assert!(!vmo.needs_pager_for(2));

    // Subsequent fault on this page returns Handled.
    let result = classify_fault(vmo, 2);
    assert_eq!(result, FaultResult::Handled(pa));
}

#[test]
fn pager_supply_increments_committed_count() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    assert_eq!(vmo.committed_pages(), 0);

    vmo.commit_page(0, Pa(0x1000));
    assert_eq!(vmo.committed_pages(), 1);

    vmo.commit_page(1, Pa(0x2000));
    assert_eq!(vmo.committed_pages(), 2);

    // Re-committing same offset replaces — count stays at 2.
    vmo.commit_page(1, Pa(0x3000));
    assert_eq!(vmo.committed_pages(), 2);
}

// ============================================================
// Test 7: Pager on sealed VMO
// ============================================================

#[test]
fn sealed_vmo_with_pager_still_pages_in() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();

    // Attach pager before sealing.
    let ch = ChannelId(99);
    assert!(vmo.set_pager(ch));

    // Pre-commit page 0 (simulates partial backing).
    vmo.commit_page(0, Pa(0xA000));

    // Seal the VMO.
    vmo.seal();
    assert!(vmo.is_sealed());

    // Committed page — Handled as normal.
    let result = classify_fault(vmo, 0);
    assert_eq!(result, FaultResult::Handled(Pa(0xA000)));

    // Uncommitted page 1 — pager still attached, demand paging is not mutation.
    // needs_pager_for checks: pager exists + uncommitted + in bounds.
    assert!(
        vmo.needs_pager_for(1),
        "sealed VMO with pager should still page in uncommitted pages"
    );

    let result = classify_fault(vmo, 1);
    assert_eq!(
        result,
        FaultResult::NeedsPager {
            channel: ch,
            page_offset: 1
        },
        "sealed VMO fault on uncommitted page should dispatch to pager"
    );
}

#[test]
fn sealed_vmo_rejects_new_pager_attachment() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.seal();

    // Cannot attach pager after sealing.
    assert!(
        !vmo.set_pager(ChannelId(1)),
        "sealed VMO should reject pager attachment"
    );
    assert!(!vmo.has_pager());
}

#[test]
fn sealed_vmo_rejects_try_commit_but_allows_commit_page() {
    // Demonstrates the two commit paths:
    // - try_commit_page: respects seal (for userspace writes) — rejected.
    // - commit_page: unconditional (for pager supply) — succeeds.
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));
    vmo.seal();

    // try_commit_page is sealed-aware — rejects.
    assert!(!vmo.try_commit_page(0, Pa(0x1000)));
    assert_eq!(vmo.committed_pages(), 0);

    // commit_page is unconditional (used by fault handler for pager supply).
    vmo.commit_page(0, Pa(0x1000));
    assert_eq!(vmo.committed_pages(), 1);
    assert_eq!(vmo.lookup_page(0).unwrap().0, Pa(0x1000));
}

// ============================================================
// Additional: pager interaction with snapshots
// ============================================================

#[test]
fn pager_supplied_page_participates_in_snapshots() {
    let mut table = VmoTable::new();
    let id = table.create(4, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    // Pager supplies page 0.
    vmo.commit_page(0, Pa(0xAAAA_0000));
    vmo.clear_pending_fault(0);

    // Snapshot captures the pager-supplied page.
    let gen = vmo.snapshot().unwrap();
    assert_eq!(gen, 1);

    // Page 0 refcount should be 2 (current + snapshot).
    let (pa, rc) = vmo.lookup_page(0).unwrap();
    assert_eq!(pa, Pa(0xAAAA_0000));
    assert_eq!(rc, 2);

    // COW-replace simulates a write after snapshot.
    let old = vmo.cow_replace_page(0, Pa(0xBBBB_0000));
    assert_eq!(old, None); // Old page still in snapshot.

    // Current sees new page.
    assert_eq!(vmo.lookup_page(0).unwrap().0, Pa(0xBBBB_0000));

    // Restore to gen 0 — sees original pager-supplied page.
    let freed = vmo.restore(0).unwrap();
    assert_eq!(vmo.lookup_page(0).unwrap().0, Pa(0xAAAA_0000));
    assert!(freed.contains(&Pa(0xBBBB_0000)));
}

#[test]
fn pager_fault_on_out_of_bounds_page() {
    let mut table = VmoTable::new();
    let id = table.create(2, VmoFlags::empty(), 0).unwrap();
    let vmo = table.get_mut(id).unwrap();
    vmo.set_pager(ChannelId(1));

    // In-bounds pages need pager.
    assert!(vmo.needs_pager_for(0));
    assert!(vmo.needs_pager_for(1));

    // Out-of-bounds pages do not need pager.
    assert!(!vmo.needs_pager_for(2));
    assert!(!vmo.needs_pager_for(u64::MAX));
}
