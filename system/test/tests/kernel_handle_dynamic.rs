//! Tests for dynamic (two-level) handle table (v0.6 Phase 2b).
//!
//! Covers: Handle(u16), overflow beyond 256, two-level lookup, insert/close/drain
//! across base+overflow, capacity limits, insert_at with large indices.

mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}
#[path = "../../kernel/paging.rs"]
mod paging;
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
mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}

use handle::*;

fn ch(id: u32) -> HandleObject {
    HandleObject::Channel(ChannelId(id))
}

// ---------------------------------------------------------------------------
// Handle type is u16
// ---------------------------------------------------------------------------

#[test]
fn handle_is_u16() {
    // Handle must hold values > 255.
    let h = Handle(300);

    assert_eq!(h.0, 300u16);
}

// ---------------------------------------------------------------------------
// Base level (0..255) works as before
// ---------------------------------------------------------------------------

#[test]
fn base_insert_and_get() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(42), Rights::ALL).unwrap();

    assert_eq!(h.0, 0);
    assert!(matches!(
        t.get(h, Rights::READ).unwrap(),
        HandleObject::Channel(ChannelId(42))
    ));
}

#[test]
fn base_fills_256_slots() {
    let mut t = HandleTable::new();

    for i in 0..256u32 {
        let h = t.insert(ch(i), Rights::ALL).unwrap();

        assert_eq!(h.0, i as u16);
    }
}

// ---------------------------------------------------------------------------
// Overflow (>256 handles)
// ---------------------------------------------------------------------------

#[test]
fn overflow_beyond_256() {
    let mut t = HandleTable::new();

    // Fill base (256 slots).
    for i in 0..256u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    // Insert into overflow — should succeed and return index 256.
    let h = t.insert(ch(1000), Rights::ALL).unwrap();

    assert_eq!(h.0, 256);

    // Verify the overflow handle works.
    assert!(matches!(
        t.get(h, Rights::READ).unwrap(),
        HandleObject::Channel(ChannelId(1000))
    ));
}

#[test]
fn overflow_insert_512_handles() {
    let mut t = HandleTable::new();

    for i in 0..512u32 {
        let h = t.insert(ch(i), Rights::ALL).unwrap();

        assert_eq!(h.0, i as u16);
    }

    // Verify first, last-in-base, first-overflow, and last.
    assert!(matches!(
        t.get(Handle(0), Rights::READ).unwrap(),
        HandleObject::Channel(ChannelId(0))
    ));
    assert!(matches!(
        t.get(Handle(255), Rights::READ).unwrap(),
        HandleObject::Channel(ChannelId(255))
    ));
    assert!(matches!(
        t.get(Handle(256), Rights::READ).unwrap(),
        HandleObject::Channel(ChannelId(256))
    ));
    assert!(matches!(
        t.get(Handle(511), Rights::READ).unwrap(),
        HandleObject::Channel(ChannelId(511))
    ));
}

// ---------------------------------------------------------------------------
// Close in overflow
// ---------------------------------------------------------------------------

#[test]
fn close_overflow_handle() {
    let mut t = HandleTable::new();

    for i in 0..260u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    // Close an overflow handle.
    let (obj, _, _) = t.close(Handle(258)).unwrap();

    assert!(matches!(obj, HandleObject::Channel(ChannelId(258))));

    // Use-after-close.
    assert!(matches!(
        t.close(Handle(258)).unwrap_err(),
        HandleError::InvalidHandle
    ));
}

#[test]
fn close_and_reuse_overflow_slot() {
    let mut t = HandleTable::new();

    for i in 0..260u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    // Close slot 258 (overflow).
    t.close(Handle(258)).unwrap();

    // Next insert should reuse slot 258 (first free).
    let h = t.insert(ch(9999), Rights::ALL).unwrap();

    assert_eq!(h.0, 258);
    assert!(matches!(
        t.get(h, Rights::READ).unwrap(),
        HandleObject::Channel(ChannelId(9999))
    ));
}

// ---------------------------------------------------------------------------
// insert_at with overflow indices
// ---------------------------------------------------------------------------

#[test]
fn insert_at_overflow_index() {
    let mut t = HandleTable::new();

    // Fill base.
    for i in 0..256u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    // Insert 4 overflow entries.
    for i in 256..260u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    // Close overflow slot 257.
    t.close(Handle(257)).unwrap();

    // insert_at should work for overflow slot.
    t.insert_at(Handle(257), ch(7777), Rights::READ, 0).unwrap();

    let (obj, rights) = t.get_entry(Handle(257), Rights::READ).unwrap();

    assert!(matches!(obj, HandleObject::Channel(ChannelId(7777))));
    assert!(rights.contains(Rights::READ));
    assert!(!rights.contains(Rights::WRITE));
}

#[test]
fn insert_at_occupied_overflow_fails() {
    let mut t = HandleTable::new();

    for i in 0..260u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    let err = t.insert_at(Handle(258), ch(0), Rights::ALL, 0).unwrap_err();

    assert!(matches!(err, HandleError::SlotOccupied));
}

// ---------------------------------------------------------------------------
// Drain across base + overflow
// ---------------------------------------------------------------------------

#[test]
fn drain_includes_overflow() {
    let mut t = HandleTable::new();

    for i in 0..260u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    // Close a few from each level.
    t.close(Handle(100)).unwrap();
    t.close(Handle(257)).unwrap();

    let items: Vec<_> = t.drain().collect();

    assert_eq!(items.len(), 258); // 260 - 2 closed

    // Table is now empty.
    assert!(t.get(Handle(0), Rights::READ).is_err());
    assert!(t.get(Handle(256), Rights::READ).is_err());
}

#[test]
fn drain_empty_overflow() {
    let mut t = HandleTable::new();

    // Only base entries.
    t.insert(ch(1), Rights::ALL).unwrap();
    t.insert(ch(2), Rights::ALL).unwrap();

    let items: Vec<_> = t.drain().collect();

    assert_eq!(items.len(), 2);
}

// ---------------------------------------------------------------------------
// get_entry across levels
// ---------------------------------------------------------------------------

#[test]
fn get_entry_overflow() {
    let mut t = HandleTable::new();

    for i in 0..260u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    let (obj, rights) = t.get_entry(Handle(259), Rights::SIGNAL).unwrap();

    assert!(matches!(obj, HandleObject::Channel(ChannelId(259))));
    assert!(rights.contains(Rights::ALL));
}

// ---------------------------------------------------------------------------
// Out-of-bounds handle
// ---------------------------------------------------------------------------

#[test]
fn get_beyond_allocated_returns_invalid() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::ALL).unwrap();

    // Handle 500 — way beyond anything allocated.
    assert!(matches!(
        t.get(Handle(500), Rights::READ).unwrap_err(),
        HandleError::InvalidHandle
    ));
}

#[test]
fn close_beyond_allocated_returns_invalid() {
    let mut t = HandleTable::new();

    assert!(matches!(
        t.close(Handle(300)).unwrap_err(),
        HandleError::InvalidHandle
    ));
}

// ---------------------------------------------------------------------------
// Capacity limit
// ---------------------------------------------------------------------------

#[test]
fn capacity_limit_returns_table_full() {
    let mut t = HandleTable::new();

    // Insert up to the capacity limit.
    let mut count = 0u32;

    loop {
        match t.insert(ch(count), Rights::ALL) {
            Ok(_) => count += 1,
            Err(HandleError::TableFull) => break,
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    // Should have gotten at least 256 (base) + some overflow.
    assert!(count >= 256, "got only {} handles", count);
    // Should be capped at MAX_HANDLES.
    assert_eq!(count as usize, handle::MAX_HANDLES);
}

// ---------------------------------------------------------------------------
// Mixed base + overflow operations
// ---------------------------------------------------------------------------

#[test]
fn interleaved_insert_close_across_levels() {
    let mut t = HandleTable::new();

    // Fill to 300.
    for i in 0..300u32 {
        t.insert(ch(i), Rights::ALL).unwrap();
    }

    // Close from base and overflow.
    t.close(Handle(50)).unwrap();
    t.close(Handle(260)).unwrap();

    // Re-insert should fill the gaps (base first, then overflow).
    let h1 = t.insert(ch(8001), Rights::ALL).unwrap();
    let h2 = t.insert(ch(8002), Rights::ALL).unwrap();

    assert_eq!(h1.0, 50, "should reuse base gap first");
    assert_eq!(h2.0, 260, "should reuse overflow gap second");
}
