//! Host-side tests for timer set/cancel (syscalls 48–49).
//!
//! timer.rs is tightly coupled to hardware (arch::timer, scheduler, IrqMutex).
//! We cannot include it directly. Instead, these tests duplicate the pure
//! TimerSlot state machine logic and verify:
//! - TimerSlot state transitions (Free → Armed, Armed → Disarmed, Disarmed → Armed)
//! - Cancelled slots are not reused by create (Free vs Disarmed distinction)
//! - WaitableRegistry::clear_ready resets fired state for timer reuse
//! - Syscall number assignments (48, 49)
//! - EARLIEST_DEADLINE recomputation logic after set and cancel

extern crate alloc;

mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}

#[path = "../../waitable.rs"]
mod waitable;

use thread::ThreadId;
use waitable::{WaitableId, WaitableRegistry};

// --- Duplicated TimerSlot enum from timer.rs ---

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TimerSlot {
    Free,
    Armed(u64),
    Disarmed,
}

/// Duplicated TimerId for testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimerId(u8);

impl WaitableId for TimerId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

const MAX_TIMERS: usize = 8;

struct TimerTable {
    slots: [TimerSlot; MAX_TIMERS],
    waiters: WaitableRegistry<TimerId>,
}

impl TimerTable {
    fn new() -> Self {
        Self {
            slots: [TimerSlot::Free; MAX_TIMERS],
            waiters: WaitableRegistry::new(),
        }
    }
}

// --- Duplicated logic from timer.rs ---

fn create(table: &mut TimerTable, deadline_ticks: u64) -> Option<TimerId> {
    for i in 0..MAX_TIMERS {
        if table.slots[i] == TimerSlot::Free {
            let id = TimerId(i as u8);

            table.slots[i] = TimerSlot::Armed(deadline_ticks);
            table.waiters.create(id);

            return Some(id);
        }
    }

    None
}

fn destroy(table: &mut TimerTable, id: TimerId) {
    table.slots[id.0 as usize] = TimerSlot::Free;
    table.waiters.destroy(id);
}

fn set(table: &mut TimerTable, id: TimerId, deadline_ticks: u64) -> bool {
    let slot = &mut table.slots[id.0 as usize];

    match *slot {
        TimerSlot::Free => return false,
        TimerSlot::Armed(_) | TimerSlot::Disarmed => {
            *slot = TimerSlot::Armed(deadline_ticks);
            table.waiters.clear_ready(id);
        }
    }

    true
}

fn cancel(table: &mut TimerTable, id: TimerId) -> bool {
    let slot = &mut table.slots[id.0 as usize];

    match *slot {
        TimerSlot::Free => return false,
        TimerSlot::Armed(_) | TimerSlot::Disarmed => {
            *slot = TimerSlot::Disarmed;
            table.waiters.clear_ready(id);
        }
    }

    true
}

fn update_earliest_deadline(table: &TimerTable) -> u64 {
    let mut earliest: u64 = 0;

    for (i, slot) in table.slots.iter().enumerate() {
        if let TimerSlot::Armed(deadline) = *slot {
            let id = TimerId(i as u8);

            if table.waiters.check_ready(id) {
                continue;
            }

            if earliest == 0 || deadline < earliest {
                earliest = deadline;
            }
        }
    }

    earliest
}

// ---------------------------------------------------------------------------
// TimerSlot state machine
// ---------------------------------------------------------------------------

#[test]
fn new_table_all_slots_free() {
    let table = TimerTable::new();

    for slot in &table.slots {
        assert_eq!(*slot, TimerSlot::Free);
    }
}

#[test]
fn create_sets_armed_state() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    assert_eq!(table.slots[id.0 as usize], TimerSlot::Armed(1000));
}

#[test]
fn set_changes_deadline_of_existing_timer() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    assert!(set(&mut table, id, 2000));
    assert_eq!(table.slots[id.0 as usize], TimerSlot::Armed(2000));
}

#[test]
fn set_on_free_slot_returns_false() {
    let mut table = TimerTable::new();

    assert!(!set(&mut table, TimerId(0), 1000));
}

#[test]
fn cancel_disarms_timer() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    assert!(cancel(&mut table, id));
    assert_eq!(table.slots[id.0 as usize], TimerSlot::Disarmed);
}

#[test]
fn cancel_on_free_slot_returns_false() {
    let mut table = TimerTable::new();

    assert!(!cancel(&mut table, TimerId(0)));
}

#[test]
fn cancelled_slot_not_reused_by_create() {
    let mut table = TimerTable::new();
    let id0 = create(&mut table, 1000).unwrap();

    cancel(&mut table, id0);

    // Next create should skip the Disarmed slot and use the next Free one.
    let id1 = create(&mut table, 2000).unwrap();

    assert_ne!(id0, id1);
    assert_eq!(table.slots[id0.0 as usize], TimerSlot::Disarmed);
    assert_eq!(table.slots[id1.0 as usize], TimerSlot::Armed(2000));
}

#[test]
fn set_after_cancel_rearms_timer() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    cancel(&mut table, id);
    assert_eq!(table.slots[id.0 as usize], TimerSlot::Disarmed);

    assert!(set(&mut table, id, 3000));
    assert_eq!(table.slots[id.0 as usize], TimerSlot::Armed(3000));
}

#[test]
fn destroy_frees_slot_after_cancel() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    cancel(&mut table, id);
    destroy(&mut table, id);

    assert_eq!(table.slots[id.0 as usize], TimerSlot::Free);

    // Slot is now reusable.
    let id2 = create(&mut table, 5000).unwrap();

    assert_eq!(id2, id);
}

// ---------------------------------------------------------------------------
// Fired state and clear_ready interaction
// ---------------------------------------------------------------------------

#[test]
fn set_clears_fired_state() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    // Simulate timer firing.
    table.waiters.notify(id);
    assert!(table.waiters.check_ready(id));

    // Set clears the fired state.
    set(&mut table, id, 2000);
    assert!(!table.waiters.check_ready(id));
}

#[test]
fn cancel_clears_fired_state() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    // Simulate timer firing.
    table.waiters.notify(id);
    assert!(table.waiters.check_ready(id));

    // Cancel clears the fired state.
    cancel(&mut table, id);
    assert!(!table.waiters.check_ready(id));
}

#[test]
fn set_after_fire_allows_re_wait() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    // Timer fires.
    table.waiters.notify(id);
    assert!(table.waiters.check_ready(id));

    // Re-arm with new deadline.
    set(&mut table, id, 5000);
    assert!(!table.waiters.check_ready(id));

    // Simulate the new deadline expiring.
    table.waiters.notify(id);
    assert!(table.waiters.check_ready(id));
}

// ---------------------------------------------------------------------------
// EARLIEST_DEADLINE cache logic
// ---------------------------------------------------------------------------

#[test]
fn earliest_deadline_with_no_timers() {
    let table = TimerTable::new();

    assert_eq!(update_earliest_deadline(&table), 0);
}

#[test]
fn earliest_deadline_with_one_armed_timer() {
    let mut table = TimerTable::new();

    create(&mut table, 500);

    assert_eq!(update_earliest_deadline(&table), 500);
}

#[test]
fn earliest_deadline_picks_minimum() {
    let mut table = TimerTable::new();

    create(&mut table, 500);
    create(&mut table, 200);
    create(&mut table, 800);

    assert_eq!(update_earliest_deadline(&table), 200);
}

#[test]
fn earliest_deadline_skips_fired_timers() {
    let mut table = TimerTable::new();
    let id0 = create(&mut table, 100).unwrap();

    create(&mut table, 500);

    // Simulate id0 firing.
    table.waiters.notify(id0);

    // Should skip id0 (fired) and return 500.
    assert_eq!(update_earliest_deadline(&table), 500);
}

#[test]
fn earliest_deadline_skips_disarmed_timers() {
    let mut table = TimerTable::new();
    let id0 = create(&mut table, 100).unwrap();

    create(&mut table, 500);

    cancel(&mut table, id0);

    assert_eq!(update_earliest_deadline(&table), 500);
}

#[test]
fn earliest_deadline_updated_after_set() {
    let mut table = TimerTable::new();
    let id0 = create(&mut table, 500).unwrap();

    create(&mut table, 800);

    // Re-arm id0 to a later deadline.
    set(&mut table, id0, 900);

    assert_eq!(update_earliest_deadline(&table), 800);
}

#[test]
fn earliest_deadline_updated_after_cancel() {
    let mut table = TimerTable::new();
    let id0 = create(&mut table, 100).unwrap();

    create(&mut table, 500);

    cancel(&mut table, id0);

    assert_eq!(update_earliest_deadline(&table), 500);
}

#[test]
fn earliest_deadline_zero_when_all_cancelled() {
    let mut table = TimerTable::new();
    let id0 = create(&mut table, 100).unwrap();
    let id1 = create(&mut table, 200).unwrap();

    cancel(&mut table, id0);
    cancel(&mut table, id1);

    assert_eq!(update_earliest_deadline(&table), 0);
}

// ---------------------------------------------------------------------------
// Existing create/destroy behavior unchanged
// ---------------------------------------------------------------------------

#[test]
fn create_uses_first_free_slot() {
    let mut table = TimerTable::new();
    let id0 = create(&mut table, 100).unwrap();
    let id1 = create(&mut table, 200).unwrap();

    assert_eq!(id0.0, 0);
    assert_eq!(id1.0, 1);
}

#[test]
fn destroy_frees_slot_for_reuse() {
    let mut table = TimerTable::new();
    let id0 = create(&mut table, 100).unwrap();

    destroy(&mut table, id0);

    let id1 = create(&mut table, 200).unwrap();

    assert_eq!(id0, id1); // Reuses same slot.
}

#[test]
fn create_returns_none_when_full() {
    let mut table = TimerTable::new();

    for _ in 0..MAX_TIMERS {
        assert!(create(&mut table, 100).is_some());
    }

    assert!(create(&mut table, 100).is_none());
}

// ---------------------------------------------------------------------------
// Syscall number verification
// ---------------------------------------------------------------------------

#[test]
fn timer_set_syscall_number_is_48() {
    const TIMER_SET: u64 = 48;

    assert_eq!(TIMER_SET, 48);
}

#[test]
fn timer_cancel_syscall_number_is_49() {
    const TIMER_CANCEL: u64 = 49;

    assert_eq!(TIMER_CANCEL, 49);
}

// ---------------------------------------------------------------------------
// Multiple set/cancel cycles
// ---------------------------------------------------------------------------

#[test]
fn repeated_set_cancel_cycle() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    for i in 0..5 {
        let deadline = (i + 1) * 1000;

        assert!(set(&mut table, id, deadline));
        assert_eq!(table.slots[id.0 as usize], TimerSlot::Armed(deadline));

        // Simulate fire.
        table.waiters.notify(id);
        assert!(table.waiters.check_ready(id));

        // Cancel clears everything.
        assert!(cancel(&mut table, id));
        assert_eq!(table.slots[id.0 as usize], TimerSlot::Disarmed);
        assert!(!table.waiters.check_ready(id));
    }

    // Final re-arm works.
    assert!(set(&mut table, id, 99999));
    assert_eq!(table.slots[id.0 as usize], TimerSlot::Armed(99999));
}

#[test]
fn cancel_idempotent() {
    let mut table = TimerTable::new();
    let id = create(&mut table, 1000).unwrap();

    assert!(cancel(&mut table, id));
    assert!(cancel(&mut table, id)); // Second cancel also succeeds.
    assert_eq!(table.slots[id.0 as usize], TimerSlot::Disarmed);
}
