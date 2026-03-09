//! Host-side tests for the kernel handle table.
//!
//! Includes the kernel's handle.rs directly — it has zero external dependencies,
//! making it fully testable on the host.

#[path = "../../kernel/src/handle.rs"]
mod handle;
mod interrupt {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
#[path = "../../kernel/src/scheduling_context.rs"]
mod scheduling_context;
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}

use handle::*;

fn ch(id: u32) -> HandleObject {
    HandleObject::Channel(ChannelId(id))
}

// --- insert ---

#[test]
fn insert_returns_first_free_slot() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(1), Rights::READ).unwrap();

    assert_eq!(h.0, 0);
}

#[test]
fn insert_fills_sequentially() {
    let mut t = HandleTable::new();

    for i in 0..4u8 {
        let h = t.insert(ch(i as u32), Rights::READ).unwrap();

        assert_eq!(h.0, i);
    }
}

#[test]
fn insert_reuses_closed_slot() {
    let mut t = HandleTable::new();
    let h0 = t.insert(ch(0), Rights::READ).unwrap();
    let _h1 = t.insert(ch(1), Rights::READ).unwrap();

    t.close(h0).unwrap();

    let h2 = t.insert(ch(2), Rights::READ).unwrap();

    assert_eq!(h2.0, 0); // reused slot 0
}

#[test]
fn insert_table_full() {
    let mut t = HandleTable::new();

    for i in 0..256u32 {
        t.insert(ch(i), Rights::READ).unwrap();
    }

    let err = t.insert(ch(999), Rights::READ).unwrap_err();

    assert!(matches!(err, HandleError::TableFull));
}

// --- get ---

#[test]
fn get_valid_handle() {
    let mut t = HandleTable::new();

    t.insert(ch(42), Rights::READ_WRITE).unwrap();

    let obj = t.get(Handle(0), Rights::READ).unwrap();

    assert!(matches!(obj, HandleObject::Channel(ChannelId(42))));
}

#[test]
fn get_invalid_handle() {
    let t = HandleTable::new();
    let err = t.get(Handle(0), Rights::READ).unwrap_err();

    assert!(matches!(err, HandleError::InvalidHandle));
}

#[test]
fn get_insufficient_rights() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ).unwrap();

    let err = t.get(Handle(0), Rights::WRITE).unwrap_err();

    assert!(matches!(err, HandleError::InsufficientRights));
}

#[test]
fn get_read_write_satisfies_read() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ_WRITE).unwrap();

    assert!(t.get(Handle(0), Rights::READ).is_ok());
}

#[test]
fn get_read_write_satisfies_write() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ_WRITE).unwrap();

    assert!(t.get(Handle(0), Rights::WRITE).is_ok());
}

// --- close ---

#[test]
fn close_returns_object() {
    let mut t = HandleTable::new();

    t.insert(ch(7), Rights::READ).unwrap();

    let obj = t.close(Handle(0)).unwrap();

    assert!(matches!(obj, HandleObject::Channel(ChannelId(7))));
}

#[test]
fn close_makes_handle_invalid() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ).unwrap();
    t.close(Handle(0)).unwrap();

    let err = t.get(Handle(0), Rights::READ).unwrap_err();

    assert!(matches!(err, HandleError::InvalidHandle));
}

#[test]
fn close_invalid_handle() {
    let mut t = HandleTable::new();
    let err = t.close(Handle(0)).unwrap_err();

    assert!(matches!(err, HandleError::InvalidHandle));
}

#[test]
fn close_double_close() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ).unwrap();
    t.close(Handle(0)).unwrap();

    let err = t.close(Handle(0)).unwrap_err();

    assert!(matches!(err, HandleError::InvalidHandle));
}

// --- drain ---

#[test]
fn drain_empty_table() {
    let mut t = HandleTable::new();
    let items: Vec<_> = t.drain().collect();

    assert!(items.is_empty());
}

#[test]
fn drain_returns_all_and_clears() {
    let mut t = HandleTable::new();

    t.insert(ch(10), Rights::READ).unwrap();
    t.insert(ch(20), Rights::WRITE).unwrap();
    t.insert(ch(30), Rights::READ_WRITE).unwrap();

    let items: Vec<_> = t.drain().collect();

    assert_eq!(items.len(), 3);
    assert!(matches!(
        items[0],
        (Handle(0), HandleObject::Channel(ChannelId(10)))
    ));
    assert!(matches!(
        items[1],
        (Handle(1), HandleObject::Channel(ChannelId(20)))
    ));
    assert!(matches!(
        items[2],
        (Handle(2), HandleObject::Channel(ChannelId(30)))
    ));

    // Table is now empty.
    assert!(t.get(Handle(0), Rights::READ).is_err());
}

#[test]
fn drain_skips_closed_slots() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ).unwrap();
    t.insert(ch(2), Rights::READ).unwrap();
    t.insert(ch(3), Rights::READ).unwrap();

    t.close(Handle(1)).unwrap();

    let items: Vec<_> = t.drain().collect();

    assert_eq!(items.len(), 2);
    assert!(matches!(
        items[0],
        (Handle(0), HandleObject::Channel(ChannelId(1)))
    ));
    assert!(matches!(
        items[1],
        (Handle(2), HandleObject::Channel(ChannelId(3)))
    ));
}

// --- SchedulingContext handles ---

fn sc(id: u32) -> HandleObject {
    HandleObject::SchedulingContext(scheduling_context::SchedulingContextId(id))
}

#[test]
fn insert_and_get_scheduling_context() {
    let mut t = HandleTable::new();
    let h = t.insert(sc(42), Rights::READ_WRITE).unwrap();
    let obj = t.get(h, Rights::READ).unwrap();

    assert!(matches!(
        obj,
        HandleObject::SchedulingContext(scheduling_context::SchedulingContextId(42))
    ));
}

#[test]
fn drain_mixed_channel_and_scheduling_context() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ).unwrap();
    t.insert(sc(2), Rights::WRITE).unwrap();
    t.insert(ch(3), Rights::READ_WRITE).unwrap();

    let items: Vec<_> = t.drain().collect();

    assert_eq!(items.len(), 3);
    assert!(matches!(
        items[0],
        (Handle(0), HandleObject::Channel(ChannelId(1)))
    ));
    assert!(matches!(
        items[1],
        (
            Handle(1),
            HandleObject::SchedulingContext(scheduling_context::SchedulingContextId(2))
        )
    ));
    assert!(matches!(
        items[2],
        (Handle(2), HandleObject::Channel(ChannelId(3)))
    ));
}

// --- Interrupt handles ---

fn int(id: u8) -> HandleObject {
    HandleObject::Interrupt(interrupt::InterruptId(id))
}

#[test]
fn insert_and_get_interrupt() {
    let mut t = HandleTable::new();
    let h = t.insert(int(7), Rights::READ_WRITE).unwrap();
    let obj = t.get(h, Rights::WRITE).unwrap();

    assert!(matches!(
        obj,
        HandleObject::Interrupt(interrupt::InterruptId(7))
    ));
}

// --- Timer handles ---

fn tm(id: u8) -> HandleObject {
    HandleObject::Timer(timer::TimerId(id))
}

#[test]
fn insert_and_get_timer() {
    let mut t = HandleTable::new();
    let h = t.insert(tm(5), Rights::READ).unwrap();
    let obj = t.get(h, Rights::READ).unwrap();

    assert!(matches!(obj, HandleObject::Timer(timer::TimerId(5))));
}

#[test]
fn drain_mixed_all_handle_types() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ).unwrap();
    t.insert(int(2), Rights::READ_WRITE).unwrap();
    t.insert(sc(3), Rights::WRITE).unwrap();
    t.insert(tm(4), Rights::READ).unwrap();

    let items: Vec<_> = t.drain().collect();

    assert_eq!(items.len(), 4);
    assert!(matches!(
        items[0],
        (Handle(0), HandleObject::Channel(ChannelId(1)))
    ));
    assert!(matches!(
        items[1],
        (
            Handle(1),
            HandleObject::Interrupt(interrupt::InterruptId(2))
        )
    ));
    assert!(matches!(
        items[2],
        (
            Handle(2),
            HandleObject::SchedulingContext(scheduling_context::SchedulingContextId(3))
        )
    ));
    assert!(matches!(
        items[3],
        (Handle(3), HandleObject::Timer(timer::TimerId(4)))
    ));
}

// --- Thread handles ---

fn th(id: u64) -> HandleObject {
    HandleObject::Thread(thread::ThreadId(id))
}

#[test]
fn insert_and_get_thread() {
    let mut t = HandleTable::new();
    let h = t.insert(th(42), Rights::READ).unwrap();
    let obj = t.get(h, Rights::READ).unwrap();

    assert!(matches!(obj, HandleObject::Thread(thread::ThreadId(42))));
}

#[test]
fn drain_includes_thread_handles() {
    let mut t = HandleTable::new();

    t.insert(ch(1), Rights::READ).unwrap();
    t.insert(th(2), Rights::READ).unwrap();

    let items: Vec<_> = t.drain().collect();

    assert_eq!(items.len(), 2);
    assert!(matches!(
        items[0],
        (Handle(0), HandleObject::Channel(ChannelId(1)))
    ));
    assert!(matches!(
        items[1],
        (Handle(1), HandleObject::Thread(thread::ThreadId(2)))
    ));
}

// --- Rights ---

#[test]
fn rights_contains() {
    assert!(Rights::READ_WRITE.contains(Rights::READ));
    assert!(Rights::READ_WRITE.contains(Rights::WRITE));
    assert!(Rights::READ_WRITE.contains(Rights::READ_WRITE));
    assert!(!Rights::READ.contains(Rights::WRITE));
    assert!(!Rights::WRITE.contains(Rights::READ));
    assert!(Rights::READ.contains(Rights::READ));
}
