#![feature(allocator_api)]
//! Tests for handle duplication (v0.6 Phase 5a).
//!
//! Covers: basic dup, rights attenuation, badge preservation, DUPLICATE right
//! enforcement, independent close semantics, all object variants, table full.

mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}
#[path = "../../paging.rs"]
mod paging;
#[path = "../../handle.rs"]
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
#[path = "../../scheduling_context.rs"]
mod scheduling_context;
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}
mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}

use handle::*;

fn ch(id: u32) -> HandleObject {
    HandleObject::Channel(ChannelId(id))
}

fn all_with_dup() -> Rights {
    Rights::ALL
}

// ---------------------------------------------------------------------------
// Basic dup
// ---------------------------------------------------------------------------

#[test]
fn dup_returns_different_handle_number() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_ne!(h.0, d.0);
}

#[test]
fn duped_handle_refers_to_same_object() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(42), all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    let obj_h = t.get(h, Rights::NONE).unwrap();
    let obj_d = t.get(d, Rights::NONE).unwrap();
    assert_eq!(obj_h, obj_d);
}

// ---------------------------------------------------------------------------
// Rights attenuation
// ---------------------------------------------------------------------------

#[test]
fn dup_attenuates_rights() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), all_with_dup()).unwrap();
    // Attenuate to READ | WRITE only
    let mask = Rights::READ.union(Rights::WRITE);
    let d = t.duplicate(h, mask).unwrap();
    // Dup should have READ and WRITE
    assert!(t.get(d, Rights::READ).is_ok());
    assert!(t.get(d, Rights::WRITE).is_ok());
    // But not SIGNAL (bit 2)
    assert!(matches!(
        t.get(d, Rights::SIGNAL),
        Err(HandleError::InsufficientRights)
    ));
}

#[test]
fn dup_mask_zero_preserves_all_rights() {
    let mut t = HandleTable::new();
    let rights = Rights::READ.union(Rights::WRITE).union(Rights::DUPLICATE);
    let h = t.insert(ch(1), rights).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    // Should have all original rights
    assert!(t.get(d, Rights::READ).is_ok());
    assert!(t.get(d, Rights::WRITE).is_ok());
    assert!(t.get(d, Rights::DUPLICATE).is_ok());
}

#[test]
fn dup_cannot_escalate_rights() {
    let mut t = HandleTable::new();
    let rights = Rights::READ.union(Rights::DUPLICATE);
    let h = t.insert(ch(1), rights).unwrap();
    // Try to dup with ALL rights — attenuation limits to original
    let d = t.duplicate(h, Rights::ALL).unwrap();
    assert!(t.get(d, Rights::READ).is_ok());
    assert!(matches!(
        t.get(d, Rights::WRITE),
        Err(HandleError::InsufficientRights)
    ));
}

// ---------------------------------------------------------------------------
// Badge preservation
// ---------------------------------------------------------------------------

#[test]
fn dup_preserves_badge() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), all_with_dup()).unwrap();
    t.set_badge(h, 0xDEAD_BEEF).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get_badge(d).unwrap(), 0xDEAD_BEEF);
}

// ---------------------------------------------------------------------------
// DUPLICATE right enforcement
// ---------------------------------------------------------------------------

#[test]
fn dup_without_duplicate_right_fails() {
    let mut t = HandleTable::new();
    // Insert with all rights EXCEPT DUPLICATE
    let rights = Rights::READ.union(Rights::WRITE).union(Rights::TRANSFER);
    let h = t.insert(ch(1), rights).unwrap();
    assert!(matches!(
        t.duplicate(h, Rights::NONE),
        Err(HandleError::InsufficientRights)
    ));
}

// ---------------------------------------------------------------------------
// Invalid handle
// ---------------------------------------------------------------------------

#[test]
fn dup_invalid_handle_fails() {
    let mut t = HandleTable::new();
    assert!(matches!(
        t.duplicate(Handle(999), Rights::NONE),
        Err(HandleError::InvalidHandle)
    ));
}

// ---------------------------------------------------------------------------
// Independent close semantics
// ---------------------------------------------------------------------------

#[test]
fn closing_original_does_not_invalidate_dup() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(7), all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    t.close(h).unwrap();
    // Dup is still valid
    assert_eq!(t.get(d, Rights::NONE).unwrap(), ch(7));
}

#[test]
fn closing_dup_does_not_invalidate_original() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(7), all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    t.close(d).unwrap();
    // Original is still valid
    assert_eq!(t.get(h, Rights::NONE).unwrap(), ch(7));
}

// ---------------------------------------------------------------------------
// All HandleObject variants
// ---------------------------------------------------------------------------

#[test]
fn dup_works_for_channel() {
    let mut t = HandleTable::new();
    let obj = HandleObject::Channel(ChannelId(1));
    let h = t.insert(obj, all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get(d, Rights::NONE).unwrap(), obj);
}

#[test]
fn dup_works_for_event() {
    let mut t = HandleTable::new();
    let obj = HandleObject::Event(event::EventId(2));
    let h = t.insert(obj, all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get(d, Rights::NONE).unwrap(), obj);
}

#[test]
fn dup_works_for_interrupt() {
    let mut t = HandleTable::new();
    let obj = HandleObject::Interrupt(interrupt::InterruptId(3));
    let h = t.insert(obj, all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get(d, Rights::NONE).unwrap(), obj);
}

#[test]
fn dup_works_for_process() {
    let mut t = HandleTable::new();
    let obj = HandleObject::Process(process::ProcessId(4));
    let h = t.insert(obj, all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get(d, Rights::NONE).unwrap(), obj);
}

#[test]
fn dup_works_for_scheduling_context() {
    let mut t = HandleTable::new();
    let obj =
        HandleObject::SchedulingContext(scheduling_context::SchedulingContextId(5));
    let h = t.insert(obj, all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get(d, Rights::NONE).unwrap(), obj);
}

#[test]
fn dup_works_for_thread() {
    let mut t = HandleTable::new();
    let obj = HandleObject::Thread(thread::ThreadId(6));
    let h = t.insert(obj, all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get(d, Rights::NONE).unwrap(), obj);
}

#[test]
fn dup_works_for_timer() {
    let mut t = HandleTable::new();
    let obj = HandleObject::Timer(timer::TimerId(7));
    let h = t.insert(obj, all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get(d, Rights::NONE).unwrap(), obj);
}

#[test]
fn dup_works_for_vmo() {
    let mut t = HandleTable::new();
    let obj = HandleObject::Vmo(vmo::VmoId(8));
    let h = t.insert(obj, all_with_dup()).unwrap();
    let d = t.duplicate(h, Rights::NONE).unwrap();
    assert_eq!(t.get(d, Rights::NONE).unwrap(), obj);
}

// ---------------------------------------------------------------------------
// Table full
// ---------------------------------------------------------------------------

#[test]
fn dup_table_full_returns_error() {
    let mut t = HandleTable::new();
    // Fill the table to capacity
    let max = paging::MAX_HANDLES as usize;
    for i in 0..max {
        t.insert(ch(i as u32), all_with_dup()).unwrap();
    }
    // Now try to dup — should fail with TableFull
    assert!(matches!(
        t.duplicate(Handle(0), Rights::NONE),
        Err(HandleError::TableFull)
    ));
}
