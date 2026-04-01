//! Tests for capability rights attenuation (v0.6 Phase 2a).
//!
//! Covers: 8 named rights, bitwise AND attenuation, per-right enforcement,
//! ALL constant, NONE constant, and backward-compat semantics.

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

use handle::*;

fn ch(id: u32) -> HandleObject {
    HandleObject::Channel(ChannelId(id))
}

fn pr(id: u32) -> HandleObject {
    HandleObject::Process(process::ProcessId(id))
}

// ---------------------------------------------------------------------------
// Named rights exist and are distinct
// ---------------------------------------------------------------------------

#[test]
fn all_eight_rights_are_distinct() {
    let rights = [
        Rights::READ,
        Rights::WRITE,
        Rights::SIGNAL,
        Rights::WAIT,
        Rights::MAP,
        Rights::TRANSFER,
        Rights::CREATE,
        Rights::KILL,
    ];

    // Each right is a single bit — no overlaps.
    for (i, a) in rights.iter().enumerate() {
        for (j, b) in rights.iter().enumerate() {
            if i != j {
                // a does not contain b.
                assert!(
                    !a.contains(*b),
                    "Right {} should not contain right {}",
                    i,
                    j
                );
            }
        }
    }
}

#[test]
fn each_right_contains_itself() {
    let rights = [
        Rights::READ,
        Rights::WRITE,
        Rights::SIGNAL,
        Rights::WAIT,
        Rights::MAP,
        Rights::TRANSFER,
        Rights::CREATE,
        Rights::KILL,
    ];

    for r in &rights {
        assert!(r.contains(*r));
    }
}

// ---------------------------------------------------------------------------
// ALL and NONE constants
// ---------------------------------------------------------------------------

#[test]
fn all_contains_every_right() {
    assert!(Rights::ALL.contains(Rights::READ));
    assert!(Rights::ALL.contains(Rights::WRITE));
    assert!(Rights::ALL.contains(Rights::SIGNAL));
    assert!(Rights::ALL.contains(Rights::WAIT));
    assert!(Rights::ALL.contains(Rights::MAP));
    assert!(Rights::ALL.contains(Rights::TRANSFER));
    assert!(Rights::ALL.contains(Rights::CREATE));
    assert!(Rights::ALL.contains(Rights::KILL));
}

#[test]
fn none_contains_nothing() {
    assert!(!Rights::NONE.contains(Rights::READ));
    assert!(!Rights::NONE.contains(Rights::WRITE));
    assert!(!Rights::NONE.contains(Rights::SIGNAL));
    assert!(!Rights::NONE.contains(Rights::WAIT));
    assert!(!Rights::NONE.contains(Rights::MAP));
    assert!(!Rights::NONE.contains(Rights::TRANSFER));
    assert!(!Rights::NONE.contains(Rights::CREATE));
    assert!(!Rights::NONE.contains(Rights::KILL));
}

// ---------------------------------------------------------------------------
// Bitwise AND attenuation
// ---------------------------------------------------------------------------

#[test]
fn attenuate_preserves_common_rights() {
    let original = Rights::READ.union(Rights::WRITE).union(Rights::SIGNAL);
    let mask = Rights::READ.union(Rights::SIGNAL);
    let result = original.attenuate(mask);

    assert!(result.contains(Rights::READ));
    assert!(result.contains(Rights::SIGNAL));
    assert!(!result.contains(Rights::WRITE));
}

#[test]
fn attenuate_cannot_add_rights() {
    let original = Rights::READ;
    let mask = Rights::ALL;
    let result = original.attenuate(mask);

    // Even with ALL mask, only READ survives.
    assert!(result.contains(Rights::READ));
    assert!(!result.contains(Rights::WRITE));
    assert!(!result.contains(Rights::KILL));
}

#[test]
fn attenuate_with_none_yields_none() {
    let original = Rights::ALL;
    let result = original.attenuate(Rights::NONE);

    assert!(!result.contains(Rights::READ));
    assert!(!result.contains(Rights::WRITE));
}

#[test]
fn attenuate_with_all_preserves_original() {
    let original = Rights::READ.union(Rights::WRITE);
    let result = original.attenuate(Rights::ALL);

    assert!(result.contains(Rights::READ));
    assert!(result.contains(Rights::WRITE));
    assert!(!result.contains(Rights::SIGNAL));
}

#[test]
fn attenuate_is_monotonic() {
    // Attenuating twice can only reduce rights, never increase.
    let original = Rights::ALL;
    let step1 = original.attenuate(Rights::READ.union(Rights::WRITE).union(Rights::SIGNAL));
    let step2 = step1.attenuate(Rights::READ.union(Rights::SIGNAL));

    assert!(step2.contains(Rights::READ));
    assert!(step2.contains(Rights::SIGNAL));
    assert!(!step2.contains(Rights::WRITE));
}

// ---------------------------------------------------------------------------
// union
// ---------------------------------------------------------------------------

#[test]
fn union_combines_rights() {
    let rw = Rights::READ.union(Rights::WRITE);

    assert!(rw.contains(Rights::READ));
    assert!(rw.contains(Rights::WRITE));
    assert!(!rw.contains(Rights::SIGNAL));
}

// ---------------------------------------------------------------------------
// READ_WRITE backward compat
// ---------------------------------------------------------------------------

#[test]
fn read_write_contains_read_and_write() {
    // READ_WRITE must still work as before.
    assert!(Rights::READ_WRITE.contains(Rights::READ));
    assert!(Rights::READ_WRITE.contains(Rights::WRITE));
}

// ---------------------------------------------------------------------------
// Handle table with new rights
// ---------------------------------------------------------------------------

#[test]
fn insert_with_granular_rights() {
    let mut t = HandleTable::new();
    let rights = Rights::READ.union(Rights::SIGNAL).union(Rights::WAIT);
    let h = t.insert(ch(1), rights).unwrap();

    // Can get with READ.
    assert!(t.get(h, Rights::READ).is_ok());
    // Can get with SIGNAL.
    assert!(t.get(h, Rights::SIGNAL).is_ok());
    // Cannot get with WRITE — not granted.
    assert!(matches!(
        t.get(h, Rights::WRITE).unwrap_err(),
        HandleError::InsufficientRights
    ));
    // Cannot get with KILL — not granted.
    assert!(matches!(
        t.get(h, Rights::KILL).unwrap_err(),
        HandleError::InsufficientRights
    ));
}

#[test]
fn get_entry_returns_exact_rights() {
    let mut t = HandleTable::new();
    let rights = Rights::SIGNAL.union(Rights::WAIT);
    let h = t.insert(ch(1), rights).unwrap();
    let (_, returned) = t.get_entry(h, Rights::SIGNAL).unwrap();

    assert!(returned.contains(Rights::SIGNAL));
    assert!(returned.contains(Rights::WAIT));
    assert!(!returned.contains(Rights::READ));
}

#[test]
fn close_returns_original_rights() {
    let mut t = HandleTable::new();
    let rights = Rights::MAP.union(Rights::TRANSFER);
    let h = t.insert(ch(1), rights).unwrap();
    let (_, returned, _) = t.close(h).unwrap();

    assert!(returned.contains(Rights::MAP));
    assert!(returned.contains(Rights::TRANSFER));
    assert!(!returned.contains(Rights::WRITE));
}

// ---------------------------------------------------------------------------
// from_raw
// ---------------------------------------------------------------------------

#[test]
fn from_raw_masks_to_defined_bits() {
    // Bits 8-31 are silently dropped.
    let r = Rights::from_raw(0xFFFF_FFFF);

    assert!(r.contains(Rights::ALL));
    // ALL is 0xFF, so from_raw(0xFFFF_FFFF) == ALL.
    assert_eq!(r.attenuate(Rights::ALL), Rights::ALL);
}

#[test]
fn from_raw_zero_is_none() {
    let r = Rights::from_raw(0);

    assert!(!r.contains(Rights::READ));
}

#[test]
fn from_raw_specific_bits() {
    // READ=1, SIGNAL=4 → 5
    let r = Rights::from_raw(5);

    assert!(r.contains(Rights::READ));
    assert!(r.contains(Rights::SIGNAL));
    assert!(!r.contains(Rights::WRITE));
}

// ---------------------------------------------------------------------------
// Simulated attenuated handle_send (table-level)
// ---------------------------------------------------------------------------

#[test]
fn simulated_attenuated_send() {
    // Simulate: source has ALL rights. Send with mask = READ | SIGNAL | WAIT.
    // Target should only have those three.
    let mut source = HandleTable::new();
    let mut target = HandleTable::new();

    let h = source.insert(ch(42), Rights::ALL).unwrap();

    // Take from source (move semantics).
    let (obj, original_rights, _) = source.close(h).unwrap();

    // Attenuate.
    let mask = Rights::READ.union(Rights::SIGNAL).union(Rights::WAIT);
    let attenuated = original_rights.attenuate(mask);

    // Insert into target with attenuated rights.
    let th = target.insert(obj, attenuated).unwrap();

    // Target can READ.
    assert!(target.get(th, Rights::READ).is_ok());
    // Target can SIGNAL.
    assert!(target.get(th, Rights::SIGNAL).is_ok());
    // Target cannot WRITE.
    assert!(matches!(
        target.get(th, Rights::WRITE).unwrap_err(),
        HandleError::InsufficientRights
    ));
    // Target cannot TRANSFER.
    assert!(matches!(
        target.get(th, Rights::TRANSFER).unwrap_err(),
        HandleError::InsufficientRights
    ));
    // Target cannot KILL.
    assert!(matches!(
        target.get(th, Rights::KILL).unwrap_err(),
        HandleError::InsufficientRights
    ));
}

#[test]
fn attenuated_send_with_zero_mask_preserves_all() {
    // Backward compat: mask of ALL (or 0 treated as ALL) preserves original rights.
    let mut source = HandleTable::new();
    let mut target = HandleTable::new();

    let original = Rights::READ.union(Rights::WRITE).union(Rights::SIGNAL);
    let h = source.insert(ch(1), original).unwrap();
    let (obj, rights, _) = source.close(h).unwrap();

    // Using ALL as mask = no attenuation.
    let attenuated = rights.attenuate(Rights::ALL);
    let th = target.insert(obj, attenuated).unwrap();

    assert!(target.get(th, Rights::READ).is_ok());
    assert!(target.get(th, Rights::WRITE).is_ok());
    assert!(target.get(th, Rights::SIGNAL).is_ok());
}

// ---------------------------------------------------------------------------
// Multi-hop attenuation (capability chain)
// ---------------------------------------------------------------------------

#[test]
fn multi_hop_attenuation_only_reduces() {
    // Process A → B → C. Each hop attenuates further.
    let mut table_a = HandleTable::new();
    let mut table_b = HandleTable::new();
    let mut table_c = HandleTable::new();

    // A has full rights.
    let ha = table_a.insert(ch(1), Rights::ALL).unwrap();

    // A → B: attenuate to READ | WRITE | SIGNAL | WAIT | TRANSFER.
    let (obj, rights_a, _) = table_a.close(ha).unwrap();
    let mask_ab = Rights::READ
        .union(Rights::WRITE)
        .union(Rights::SIGNAL)
        .union(Rights::WAIT)
        .union(Rights::TRANSFER);
    let rights_b = rights_a.attenuate(mask_ab);
    let hb = table_b.insert(obj, rights_b).unwrap();

    // B has TRANSFER so it can forward. But no KILL, no MAP, no CREATE.
    assert!(table_b.get(hb, Rights::TRANSFER).is_ok());
    assert!(matches!(
        table_b.get(hb, Rights::KILL).unwrap_err(),
        HandleError::InsufficientRights
    ));

    // B → C: attenuate to READ | SIGNAL (drop WRITE, WAIT, TRANSFER).
    let (obj, rights_b, _) = table_b.close(hb).unwrap();
    let mask_bc = Rights::READ.union(Rights::SIGNAL);
    let rights_c = rights_b.attenuate(mask_bc);
    let hc = table_c.insert(obj, rights_c).unwrap();

    // C can READ and SIGNAL.
    assert!(table_c.get(hc, Rights::READ).is_ok());
    assert!(table_c.get(hc, Rights::SIGNAL).is_ok());
    // C cannot WRITE (dropped at B→C).
    assert!(matches!(
        table_c.get(hc, Rights::WRITE).unwrap_err(),
        HandleError::InsufficientRights
    ));
    // C cannot TRANSFER (dropped at B→C).
    assert!(matches!(
        table_c.get(hc, Rights::TRANSFER).unwrap_err(),
        HandleError::InsufficientRights
    ));
    // C cannot KILL (dropped at A→B, still gone).
    assert!(matches!(
        table_c.get(hc, Rights::KILL).unwrap_err(),
        HandleError::InsufficientRights
    ));
}

// ---------------------------------------------------------------------------
// TRANSFER enforcement (simulated handle_send gate)
// ---------------------------------------------------------------------------

/// Simulates the kernel's handle_send TRANSFER check: verify the source handle
/// has TRANSFER right before allowing the move.
fn simulated_handle_send_with_transfer_check(
    source: &mut HandleTable,
    target: &mut HandleTable,
    source_handle: Handle,
    mask: Rights,
) -> Result<Handle, HandleError> {
    // Step 1: Verify TRANSFER right on source (matches kernel's get_entry check).
    let (obj, rights) = source.get_entry(source_handle, Rights::TRANSFER)?;

    // Step 2: Close (move out).
    let _ = source.close(source_handle);

    // Step 3: Attenuate and insert into target.
    let attenuated = rights.attenuate(mask);
    target.insert(obj, attenuated)
}

#[test]
fn transfer_right_allows_send() {
    let mut source = HandleTable::new();
    let mut target = HandleTable::new();

    // Handle with TRANSFER right — send should succeed.
    let rights = Rights::READ.union(Rights::WRITE).union(Rights::TRANSFER);
    let h = source.insert(ch(1), rights).unwrap();

    assert!(simulated_handle_send_with_transfer_check(
        &mut source,
        &mut target,
        h,
        Rights::ALL,
    )
    .is_ok());
}

#[test]
fn missing_transfer_right_blocks_send() {
    let mut source = HandleTable::new();
    let mut target = HandleTable::new();

    // Handle WITHOUT TRANSFER right — send must fail.
    let rights = Rights::READ.union(Rights::WRITE);
    let h = source.insert(ch(1), rights).unwrap();

    let err =
        simulated_handle_send_with_transfer_check(&mut source, &mut target, h, Rights::ALL)
            .unwrap_err();

    assert!(matches!(err, HandleError::InsufficientRights));

    // Source handle should still be intact (get_entry failed, close never ran).
    assert!(source.get(h, Rights::READ).is_ok());
}

#[test]
fn attenuated_away_transfer_blocks_re_delegation() {
    let mut table_a = HandleTable::new();
    let mut table_b = HandleTable::new();
    let mut table_c = HandleTable::new();

    // A has ALL rights including TRANSFER.
    let ha = table_a.insert(ch(1), Rights::ALL).unwrap();

    // A → B: attenuate away TRANSFER (give READ | WRITE only).
    let hb = simulated_handle_send_with_transfer_check(
        &mut table_a,
        &mut table_b,
        ha,
        Rights::READ.union(Rights::WRITE),
    )
    .unwrap();

    // B tries to send to C — should fail (no TRANSFER right).
    let err =
        simulated_handle_send_with_transfer_check(&mut table_b, &mut table_c, hb, Rights::ALL)
            .unwrap_err();

    assert!(matches!(err, HandleError::InsufficientRights));
}
