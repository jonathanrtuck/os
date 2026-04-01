//! Tests for handle badges (v0.6 Phase 2c).
//!
//! Covers: set/get roundtrip, default badge, badge preserved through transfer,
//! badge survives rights attenuation, badge on different handle types.

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

// ---------------------------------------------------------------------------
// Default badge
// ---------------------------------------------------------------------------

#[test]
fn default_badge_is_zero() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::ALL).unwrap();

    assert_eq!(t.get_badge(h).unwrap(), 0);
}

// ---------------------------------------------------------------------------
// Set / get roundtrip
// ---------------------------------------------------------------------------

#[test]
fn set_and_get_badge() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::ALL).unwrap();

    t.set_badge(h, 42).unwrap();

    assert_eq!(t.get_badge(h).unwrap(), 42);
}

#[test]
fn set_badge_large_value() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::ALL).unwrap();

    t.set_badge(h, u64::MAX).unwrap();

    assert_eq!(t.get_badge(h).unwrap(), u64::MAX);
}

#[test]
fn set_badge_overwrite() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::ALL).unwrap();

    t.set_badge(h, 10).unwrap();
    t.set_badge(h, 20).unwrap();

    assert_eq!(t.get_badge(h).unwrap(), 20);
}

// ---------------------------------------------------------------------------
// Badge on invalid/closed handles
// ---------------------------------------------------------------------------

#[test]
fn get_badge_invalid_handle() {
    let t = HandleTable::new();

    assert!(matches!(
        t.get_badge(Handle(0)).unwrap_err(),
        HandleError::InvalidHandle
    ));
}

#[test]
fn set_badge_invalid_handle() {
    let mut t = HandleTable::new();

    assert!(matches!(
        t.set_badge(Handle(0), 42).unwrap_err(),
        HandleError::InvalidHandle
    ));
}

#[test]
fn get_badge_after_close() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::ALL).unwrap();

    t.set_badge(h, 42).unwrap();
    t.close(h).unwrap();

    assert!(matches!(
        t.get_badge(h).unwrap_err(),
        HandleError::InvalidHandle
    ));
}

// ---------------------------------------------------------------------------
// Badge preserved through simulated handle_send (move)
// ---------------------------------------------------------------------------

#[test]
fn badge_preserved_through_transfer() {
    let mut source = HandleTable::new();
    let mut target = HandleTable::new();

    let h = source.insert(ch(1), Rights::ALL).unwrap();

    source.set_badge(h, 99).unwrap();

    // Move: close from source, insert into target.
    let (obj, rights, badge) = source.close(h).unwrap();
    let th = target.insert_with_badge(obj, rights, badge).unwrap();

    assert_eq!(target.get_badge(th).unwrap(), 99);
}

#[test]
fn badge_preserved_through_attenuated_transfer() {
    let mut source = HandleTable::new();
    let mut target = HandleTable::new();

    let h = source.insert(ch(1), Rights::ALL).unwrap();

    source.set_badge(h, 777).unwrap();

    let (obj, rights, badge) = source.close(h).unwrap();
    let attenuated = rights.attenuate(Rights::READ.union(Rights::SIGNAL));
    let th = target.insert_with_badge(obj, attenuated, badge).unwrap();

    // Badge survives even though rights were reduced.
    assert_eq!(target.get_badge(th).unwrap(), 777);
    assert!(target.get(th, Rights::READ).is_ok());
    assert!(matches!(
        target.get(th, Rights::WRITE).unwrap_err(),
        HandleError::InsufficientRights
    ));
}

// ---------------------------------------------------------------------------
// Badge on different handle types
// ---------------------------------------------------------------------------

#[test]
fn badge_on_process_handle() {
    let mut t = HandleTable::new();
    let h = t
        .insert(
            HandleObject::Process(process::ProcessId(5)),
            Rights::ALL,
        )
        .unwrap();

    t.set_badge(h, 123).unwrap();

    assert_eq!(t.get_badge(h).unwrap(), 123);
}

#[test]
fn badge_on_timer_handle() {
    let mut t = HandleTable::new();
    let h = t
        .insert(HandleObject::Timer(timer::TimerId(3)), Rights::ALL)
        .unwrap();

    t.set_badge(h, 456).unwrap();

    assert_eq!(t.get_badge(h).unwrap(), 456);
}

// ---------------------------------------------------------------------------
// Multiple handles with different badges
// ---------------------------------------------------------------------------

#[test]
fn different_badges_per_handle() {
    let mut t = HandleTable::new();
    let h1 = t.insert(ch(1), Rights::ALL).unwrap();
    let h2 = t.insert(ch(2), Rights::ALL).unwrap();
    let h3 = t.insert(ch(3), Rights::ALL).unwrap();

    t.set_badge(h1, 100).unwrap();
    t.set_badge(h2, 200).unwrap();
    t.set_badge(h3, 300).unwrap();

    assert_eq!(t.get_badge(h1).unwrap(), 100);
    assert_eq!(t.get_badge(h2).unwrap(), 200);
    assert_eq!(t.get_badge(h3).unwrap(), 300);
}

// ---------------------------------------------------------------------------
// Badge in overflow region
// ---------------------------------------------------------------------------

#[test]
fn badge_on_overflow_handle() {
    let mut t = HandleTable::new();

    // Fill base.
    for i in 0..256u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    // Overflow handle.
    let h = t.insert(ch(999), Rights::ALL).unwrap();

    assert_eq!(h.0, 256);

    t.set_badge(h, 42).unwrap();

    assert_eq!(t.get_badge(h).unwrap(), 42);
}

// ---------------------------------------------------------------------------
// Drain includes badges
// ---------------------------------------------------------------------------

#[test]
fn drain_returns_badges() {
    let mut t = HandleTable::new();

    let h1 = t.insert(ch(1), Rights::ALL).unwrap();
    let h2 = t.insert(ch(2), Rights::ALL).unwrap();

    t.set_badge(h1, 10).unwrap();
    t.set_badge(h2, 20).unwrap();

    let items: Vec<_> = t.drain().collect();

    assert_eq!(items.len(), 2);
    assert_eq!(items[0].2, 10); // badge
    assert_eq!(items[1].2, 20);
}
